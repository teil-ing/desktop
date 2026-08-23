//! teil.ing API v1 client — counterpart of Swift APIService + UploadService.
//!
//! Response structs deserialize the API's snake_case JSON and serialize back to the
//! frontend as camelCase (matching src/types.ts). Auth is the `X-API-Key` header.

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

const BASE: &str = "https://teil.ing/api/v1";
/// Non-v1 routes (the video preflight/upload pair lives outside /api/v1).
const HOST: &str = "https://teil.ing";

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
    /// "image" | "video" — the server says to branch on this, not on mimeType.
    #[serde(default = "default_kind")]
    pub kind: String,
    /// Video processing state: "uploading" | "processing" | "ready" | "failed"; None for images.
    #[serde(default)]
    pub stream_status: Option<String>,
    #[serde(default)]
    pub duration_ms: Option<i64>,
    #[serde(default)]
    pub width: Option<i64>,
    #[serde(default)]
    pub height: Option<i64>,
    /// Token-signed HLS playlist; expires — never cache. None until streamStatus is "ready".
    #[serde(default)]
    pub video_url: Option<String>,
}

fn default_kind() -> String {
    "image".into()
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

/// Fetch the stored original behind an `imageUrl` (i.teil.ing). Public images need no
/// auth, but the key is sent anyway so private ones resolve too.
///
/// Own client: originals are multi-megabyte, so the shared 30s total timeout is as
/// wrong here as it is for uploads.
pub async fn download_file(key: &str, url: &str) -> Result<Vec<u8>> {
    let resp = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(15))
        .timeout(std::time::Duration::from_secs(180))
        .build()?
        .get(url)
        .header("X-API-Key", key)
        .send()
        .await
        .map_err(|e| {
            if e.is_timeout() {
                anyhow!("Download timed out. Check your connection speed.")
            } else {
                anyhow!("Could not reach teil.ing ({}).", root_cause(&e))
            }
        })?;
    let code = resp.status().as_u16();
    if !(200..300).contains(&code) {
        return Err(match code {
            401 | 403 => anyhow!("Not allowed to download this image."),
            404 => anyhow!("Image file not found."),
            c => status_error(c),
        });
    }
    Ok(resp
        .bytes()
        .await
        .map_err(|_| anyhow!("Download was interrupted."))?
        .to_vec())
}

/// POST /upload — multipart with `file`, plus `stripExif`/`private` fields ONLY when on
/// (matching the Swift UploadService contract: omission means off/public).
/// Returns (image id, share url).
pub async fn upload(key: &str, png: Vec<u8>, strip_exif: bool, is_private: bool) -> Result<(String, String)> {
    // e.g. screenshot-2026-07-20_15-30-45.png — local time, no ':' (invalid on Windows).
    let filename = format!("screenshot-{}.png", chrono::Local::now().format("%Y-%m-%d_%H-%M-%S"));
    let part = reqwest::multipart::Part::bytes(png)
        .file_name(filename)
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


// ---- Video upload (preflight + raw-body streaming) ------------------------
//
// Videos do NOT go through /api/v1/upload (image-only by magic-byte sniffing).
// The flow is: POST /api/videos/preflight (multipart, byteSize signed into a
// short-lived ticket) → POST /api/videos/upload (raw file body, metadata in
// headers, Content-Length must equal the preflighted byteSize exactly).

pub struct VideoMeta {
    pub duration_ms: u64,
    pub width: u32,
    pub height: u32,
}

/// Maps the video routes' status codes to user-facing messages.
fn video_error(code: u16, body: &str) -> anyhow::Error {
    let server_msg = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| {
            v.get("reason")
                .or_else(|| v.get("error"))
                .and_then(|m| m.as_str().map(str::to_string))
        });
    match code {
        401 => anyhow!("API key is invalid or expired."),
        403 if server_msg.as_deref().is_some_and(|m| m.contains("Pro")) => {
            anyhow!("Video uploads require a teil.ing Pro subscription.")
        }
        403 => anyhow!("The upload ticket was rejected — please retry."),
        411 => anyhow!("Upload rejected: missing Content-Length."),
        413 => anyhow!(server_msg
            .unwrap_or_else(|| "The recording is too large or exceeds your storage quota.".into())),
        429 => anyhow!("Too many video uploads. Please wait and try again."),
        400 => anyhow!("The server could not read the recording file."),
        422 => anyhow!(server_msg
            .unwrap_or_else(|| "The recording was rejected by content moderation.".into())),
        501 => anyhow!("Video uploads are not available right now."),
        502 => anyhow!("Upload failed on the server. Please try again."),
        c => anyhow!("Server error ({c}). Please try again."),
    }
}

/// Step 1: registers the exact byte size and returns the one-shot upload ticket
/// (15-minute TTL). No frames are sent — server-side moderation runs after upload.
pub async fn preflight_video(key: &str, byte_size: u64) -> Result<String> {
    let form = reqwest::multipart::Form::new().text("byteSize", byte_size.to_string());
    let resp = client()?
        .post(format!("{HOST}/api/videos/preflight"))
        .header("X-API-Key", key)
        .multipart(form)
        .send()
        .await
        .map_err(reach_error)?;
    let code = resp.status().as_u16();
    let body = resp.text().await.unwrap_or_default();
    if code != 200 {
        return Err(video_error(code, &body));
    }
    let v: serde_json::Value =
        serde_json::from_str(&body).map_err(|_| anyhow!("Unexpected preflight response."))?;
    if v.get("ok").and_then(|b| b.as_bool()) == Some(false) {
        return Err(video_error(422, &body));
    }
    v.get("ticket")
        .and_then(|t| t.as_str())
        .map(str::to_string)
        .ok_or_else(|| anyhow!("Preflight returned no upload ticket."))
}

/// Step 2: streams the file from disk as the raw request body. `on_progress`
/// receives (bytes_sent, total) as chunks leave the reader.
pub async fn upload_video(
    key: &str,
    path: &std::path::Path,
    filename: &str,
    meta: &VideoMeta,
    ticket: &str,
    mut on_progress: impl FnMut(u64, u64) + Send + 'static,
) -> Result<(String, String)> {
    use futures_util::TryStreamExt;

    // Content-Length MUST equal the preflighted byteSize — both come from disk.
    let total = tokio::fs::metadata(path)
        .await
        .map_err(|e| anyhow!("Could not read the recording file: {e}"))?
        .len();
    let file = tokio::fs::File::open(path)
        .await
        .map_err(|e| anyhow!("Could not open the recording file: {e}"))?;
    let mut sent: u64 = 0;
    let stream = tokio_util::io::ReaderStream::with_capacity(file, 256 * 1024).inspect_ok(
        move |chunk| {
            sent += chunk.len() as u64;
            on_progress(sent, total);
        },
    );

    // No tight total timeout: a 500 MiB upload on a slow uplink takes a while, and
    // the server spools + relays to the stream host before answering.
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(15))
        .timeout(std::time::Duration::from_secs(90 * 60))
        .build()?;
    let resp = client
        .post(format!("{HOST}/api/videos/upload"))
        .header("X-API-Key", key)
        .header("X-Upload-Ticket", ticket)
        .header("X-Filename", filename)
        .header("X-Duration-Ms", meta.duration_ms.to_string())
        .header("X-Width", meta.width.to_string())
        .header("X-Height", meta.height.to_string())
        .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
        .header(reqwest::header::CONTENT_LENGTH, total)
        .body(reqwest::Body::wrap_stream(stream))
        .send()
        .await
        .map_err(reach_error)?;

    let code = resp.status().as_u16();
    if code == 201 {
        let body: UploadResponse = resp.json().await.map_err(reach_error)?;
        let share_url = body
            .share_url
            .or_else(|| body.slug.as_ref().map(|s| format!("https://teil.ing/v/{s}")))
            .ok_or_else(|| anyhow!("Upload succeeded but no share URL was returned."))?;
        Ok((body.id, share_url))
    } else {
        let body = resp.text().await.unwrap_or_default();
        Err(video_error(code, &body))
    }
}
