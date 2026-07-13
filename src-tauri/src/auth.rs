//! Browser-based sign-in (connect flow) — counterpart of Swift AuthService.
//!
//! `begin` opens https://teil.ing/connect in the default browser, where the user
//! signs in with any teil.ing method and approves this device. The website then
//! redirects to teiling://connect?code=…&state=…, delivered here through the
//! deep-link plugin → `handle_callback`. The one-time code plus the locally held
//! PKCE verifier are exchanged at POST /api/app/exchange for a device-scoped API
//! key, which goes straight into the OS keychain. The key never travels through
//! the browser, and a hijacked teiling:// handler can't redeem the code without
//! the verifier.

use base64::Engine;
use rand::RngCore;
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_opener::OpenerExt;

use crate::{secure, AppState};

const HOST: &str = "https://teil.ing";

/// The in-flight sign-in attempt (Swift: PendingFlow, minus the continuation —
/// completion is reported to the frontend via "auth-changed"/"auth-feedback").
pub struct PendingSignin {
    state: String,
    verifier: String,
}

fn b64url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn random_b64url(byte_count: usize) -> String {
    let mut buf = vec![0u8; byte_count];
    rand::thread_rng().fill_bytes(&mut buf);
    b64url(&buf)
}

/// Starts the browser handshake. A new attempt supersedes a stale one
/// (e.g. the user closed the browser tab and clicked the button again).
pub fn begin(app: &AppHandle) -> Result<(), String> {
    let verifier = random_b64url(32);
    let state = random_b64url(24);
    let challenge = b64url(&Sha256::digest(verifier.as_bytes()));

    let device = whoami::devicename();
    let url = url::Url::parse_with_params(
        &format!("{HOST}/connect"),
        &[
            ("device", device.as_str()),
            ("state", state.as_str()),
            ("challenge", challenge.as_str()),
        ],
    )
    .map_err(|e| e.to_string())?;

    *app.state::<AppState>().pending_signin.lock().unwrap() =
        Some(PendingSignin { state, verifier });

    app.opener()
        .open_url(url.to_string(), None::<&str>)
        .map_err(|e| e.to_string())
}

/// Routes a deep-link URL. Consumes the pending sign-in when scheme, host, and
/// state all match; anything else is a stray or forged callback and is ignored.
pub fn handle_callback(app: &AppHandle, url: &url::Url) {
    if url.scheme() != "teiling" || url.host_str() != Some("connect") {
        return;
    }
    let query: std::collections::HashMap<String, String> = url
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();

    let flow = {
        let app_state = app.state::<AppState>();
        let mut pending = app_state.pending_signin.lock().unwrap();
        match pending.as_ref() {
            Some(p) if Some(p.state.as_str()) == query.get("state").map(String::as_str) => {
                pending.take()
            }
            _ => return, // mismatched state — keep waiting
        }
    };
    let Some(flow) = flow else { return };

    if query.contains_key("error") {
        finish_failed(app, "The connection request was denied in the browser.");
        return;
    }
    let code = match query.get("code").filter(|c| !c.is_empty()) {
        Some(c) => c.clone(),
        None => {
            finish_failed(app, "The sign-in callback was incomplete. Please try again.");
            return;
        }
    };

    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        match exchange(&code, &flow.verifier).await {
            Ok(key) => {
                if let Err(e) = secure::set_api_key(&key) {
                    finish_failed(&app, &format!("Could not store the API key: {e}"));
                    return;
                }
                eprintln!("[teil.ing] browser sign-in complete");
                let _ = app.emit("auth-changed", ());
                let _ = app.emit("auth-feedback", serde_json::json!({"kind": "succeeded"}));
                // Bring the popover up so the user sees they are signed in.
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.show();
                    let _ = w.set_focus();
                }
            }
            Err(message) => finish_failed(&app, &message),
        }
    });
}

fn finish_failed(app: &AppHandle, message: &str) {
    eprintln!("[teil.ing] browser sign-in failed: {message}");
    let _ = app.emit(
        "auth-feedback",
        serde_json::json!({"kind": "failed", "message": message}),
    );
}

/// POST /api/app/exchange {code, verifier} → 201 {key} (Swift: AuthService.exchange).
async fn exchange(code: &str, verifier: &str) -> Result<String, String> {
    #[derive(serde::Deserialize)]
    struct ExchangeResponse {
        key: String,
    }
    #[derive(serde::Deserialize)]
    struct ExchangeError {
        error: String,
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| e.to_string())?;

    let resp = client
        .post(format!("{HOST}/api/app/exchange"))
        .json(&serde_json::json!({"code": code, "verifier": verifier}))
        .send()
        .await
        .map_err(|_| "Could not reach teil.ing. Check your connection and try again.".to_string())?;

    match resp.status().as_u16() {
        201 => resp
            .json::<ExchangeResponse>()
            .await
            .map(|r| r.key)
            .map_err(|_| "Unexpected server response. Try again later.".to_string()),
        429 => Err("Too many sign-in attempts. Please wait a few minutes and try again.".into()),
        400 => {
            let message = resp.json::<ExchangeError>().await.ok().map(|e| e.error);
            Err(message.unwrap_or_else(|| "Sign-in could not be completed. Please try again.".into()))
        }
        status => Err(format!("Unexpected server response ({status}). Try again later.")),
    }
}
