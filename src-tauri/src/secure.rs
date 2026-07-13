//! OS keychain wrapper — macOS Keychain / Windows Credential Manager.
//! Counterpart of the Swift KeychainService.

use keyring::Entry;

const SERVICE: &str = "ing.teil.client.crossplatform";
const USER: &str = "api-key";

fn entry() -> anyhow::Result<Entry> {
    Ok(Entry::new(SERVICE, USER)?)
}

pub fn get_api_key() -> Option<String> {
    entry().ok()?.get_password().ok()
}

pub fn set_api_key(key: &str) -> anyhow::Result<()> {
    entry()?.set_password(key)?;
    Ok(())
}

pub fn delete_api_key() {
    if let Ok(e) = entry() {
        let _ = e.delete_credential();
    }
}

/// Masked key for the Account section: last 8 chars visible (Swift maskedKey).
/// Uses a FIXED-length bullet run so a long key doesn't overflow the Settings window.
pub fn masked() -> Option<String> {
    let key = get_api_key()?;
    let visible = 8;
    if key.len() <= visible {
        return Some("\u{2022}".repeat(key.len()));
    }
    Some(format!("{}{}", "\u{2022}".repeat(8), &key[key.len() - visible..]))
}
