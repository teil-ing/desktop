//! teil.ing API v1 client — counterpart of Swift APIService + UploadService.
//!
//! Response structs deserialize the API's snake_case JSON and serialize back to the
//! frontend as camelCase (matching src/types.ts). Auth is the `X-API-Key` header.

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

const BASE: &str = "https://teil.ing/api/v1";

// ---- Models --------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ImageResponse {
    pub id: String,
    pub slug: String,
    pub original_filename: String,
    pub mime_type: String,
    pub file_size: i64,
    pub image_url: Option<String>,
    pub thumbnail_url: Option<String>,
    pub has_password: bool,
    pub is_private: bool,
    pub view_count: i64,
    pub max_views: Option<i64>,
    pub valid_until: Option<String>,
    #[serde(default)]
    pub is_edited: bool,
    pub created_at: String,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageListResponse {
    pub images: Vec<ImageResponse>,
    pub limit: i64,
    pub offset: i64,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuotaResponse {
    pub storage_used: i64,
    pub storage_quota: Option<i64>,
    pub tier: String,
    pub image_count: i64,
}

/// Raw 201 body. Tolerant of snake/camel key styles and a missing shareUrl (derived from slug).
#[derive(Deserialize)]
pub struct UploadResponse {
    #[serde(alias = "imageId", alias = "image_id")]
    pub id: String,
    pub slug: Option<String>,
    #[serde(alias = "shareUrl", alias = "url")]
    pub share_url: Option<String>,
}

/// PATCH body. The API expects camelCase keys (Swift encodes with useDefaultKeys),
/// so this serializes camelCase and only includes present (Some) fields.
#[derive(Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ImageUpdateRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remove_password: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub private: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_views: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub valid_for_days: Option<i64>,
}

// ---- HTTP helpers --------------------------------------------------------

fn client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(15))
        .timeout(std::time::Duration::from_secs(30))
        .build()?)
}

/// Deepest source of a transport error — the actual DNS/TLS/OS cause, which
/// reqwest's own Display hides behind "error sending request".
fn root_cause(e: &dyn std::error::Error) -> String {
    let mut cur = e;
    while let Some(s) = cur.source() {
        cur = s;
    }
    cur.to_string()
}

fn reach_error(e: reqwest::Error) -> anyhow::Error {
    if e.is_timeout() {
        anyhow!("Connection to teil.ing timed out.")
    } else {
        anyhow!("Could not reach teil.ing ({}).", root_cause(&e))
    }
}

/// Map non-2xx status codes to messages mirroring the Swift APIError strings.
fn status_error(code: u16) -> anyhow::Error {
    match code {
        401 => anyhow!("API key is invalid or expired."),
        404 => anyhow!("Image not found."),
        429 => anyhow!("Too many requests. Please wait and try again."),
        c => anyhow!("Server error ({c}). Please try again."),
    }
}

// ---- Endpoints -----------------------------------------------------------

pub async fn validate(key: &str) -> Result<bool> {
    let resp = client()?
        .get(format!("{BASE}/images"))
        .header("X-API-Key", key)
        .send()
        .await
        .map_err(reach_error)?;
    match resp.status().as_u16() {
        200 => Ok(true),
        401 | 403 => Ok(false),
        c => Err(status_error(c)),
    }
}

pub async fn list_images(key: &str, limit: i64, offset: i64) -> Result<ImageListResponse> {
    let resp = client()?
        .get(format!("{BASE}/images?limit={limit}&offset={offset}"))
        .header("X-API-Key", key)
        .send()
        .await
        .map_err(reach_error)?;
    let code = resp.status().as_u16();
    if !(200..300).contains(&code) {
        return Err(status_error(code));
    }
    Ok(resp.json().await.map_err(|_| anyhow!("Failed to parse response"))?)
}

pub async fn get_quota(key: &str) -> Result<QuotaResponse> {
    let resp = client()?
        .get(format!("{BASE}/quota"))
        .header("X-API-Key", key)
        .send()
        .await
        .map_err(reach_error)?;
    let code = resp.status().as_u16();
    if !(200..300).contains(&code) {
        return Err(status_error(code));
    }
    Ok(resp.json().await.map_err(|_| anyhow!("Failed to parse response"))?)
}

pub async fn get_image_details(key: &str, id: &str) -> Result<ImageResponse> {
    let resp = client()?
        .get(format!("{BASE}/images/{id}"))
        .header("X-API-Key", key)
        .send()
        .await
        .map_err(reach_error)?;
    let code = resp.status().as_u16();
    if !(200..300).contains(&code) {
        return Err(status_error(code));
    }
    Ok(resp.json().await.map_err(|_| anyhow!("Failed to parse response"))?)
}

pub async fn update_image(key: &str, id: &str, update: &ImageUpdateRequest) -> Result<()> {
    let code = client()?
        .patch(format!("{BASE}/images/{id}"))
        .header("X-API-Key", key)
        .json(update)
        .send()
        .await
        .map_err(reach_error)?
        .status()
        .as_u16();
    if (200..300).contains(&code) {
        Ok(())
    } else {
        Err(status_error(code))
    }
}

pub async fn delete_image(key: &str, id: &str) -> Result<()> {
    let code = client()?
        .delete(format!("{BASE}/images/{id}"))
        .header("X-API-Key", key)
        .send()
        .await
        .map_err(reach_error)?
        .status()
        .as_u16();
    if (200..300).contains(&code) {
        Ok(())
    } else {
        Err(status_error(code))
    }
}

/// POST /upload — multipart with `file`, plus `stripExif`/`private` fields ONLY when on
/// (matching the Swift UploadService contract: omission means off/public).
/// Returns (image id, share url).
pub async fn upload(key: &str, png: Vec<u8>, strip_exif: bool, is_private: bool) -> Result<(String, String)> {
    let part = reqwest::multipart::Part::bytes(png)
        .file_name("screenshot.png")
        .mime_str("image/png")?;
    let mut form = reqwest::multipart::Form::new().part("file", part);
    if strip_exif {
        form = form.text("stripExif", "true");
    }
    if is_private {
        form = form.text("private", "true");
    }

    // Uploads get their own client: the shared 30s total timeout is too tight for
    // multi-megabyte PNGs on slow uplinks (surfaced as spurious "could not reach").
    let resp = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(15))
        .timeout(std::time::Duration::from_secs(180))
        .build()?
        .post(format!("{BASE}/upload"))
        .header("X-API-Key", key)
        .multipart(form)
        .send()
        .await
        .map_err(|e| {
            if e.is_timeout() {
                anyhow!("Upload timed out. Check your connection speed.")
            } else {
                anyhow!("Upload failed: could not reach teil.ing ({}).", root_cause(&e))
            }
        })?;

    match resp.status().as_u16() {
        201 => {
            let text = resp.text().await.map_err(|_| anyhow!("Failed to read upload response"))?;
            let raw: UploadResponse = serde_json::from_str(&text).map_err(|e| {
                // Log the actual body so any remaining shape mismatch is diagnosable.
                eprintln!(
                    "[teil.ing] upload response parse error: {e} | body: {}",
                    text.chars().take(500).collect::<String>()
                );
                anyhow!("Failed to parse upload response")
            })?;
            let share_url = raw
                .share_url
                .or_else(|| raw.slug.map(|s| format!("https://teil.ing/i/{s}")))
                .ok_or_else(|| anyhow!("Upload response missing share URL"))?;
            Ok((raw.id, share_url))
        }
        401 => Err(anyhow!("API key is invalid or expired. Please update your key.")),
        413 => Err(anyhow!("Storage quota exceeded. Free up space or upgrade your plan.")),
        429 => Err(anyhow!("Too many uploads. Please wait and try again.")),
        400 => Err(anyhow!("The image could not be uploaded.")),
        c => Err(anyhow!("Server error ({c}). Please try again.")),
    }
}
