//! API keys, encrypted with Windows DPAPI or a Linux Secret Service-backed app key. They never
//! go into settings.json, the log or the frontend - the settings window only learns whether a
//! key is set.

use std::collections::HashMap;
use std::path::Path;

#[cfg(target_os = "linux")]
#[path = "secrets_linux.rs"]
mod linux;

pub(crate) const KEYS: &[u8] = b"api-keys";
pub(crate) const HISTORY: &[u8] = b"history";

pub fn load(path: &Path) -> HashMap<String, String> {
    match load_checked(path) {
        Ok(keys) => keys,
        Err(e) => {
            crate::diag::warn(format!("API keys unavailable: {e}. Existing storage was not changed"));
            HashMap::new()
        }
    }
}

pub fn load_checked(path: &Path) -> Result<HashMap<String, String>, String> {
    let Some(plain) = read_encrypted(path, KEYS)? else { return Ok(HashMap::new()) };
    #[cfg(target_os = "linux")]
    let plain = zeroize::Zeroizing::new(plain);
    serde_json::from_slice(&plain).map_err(|_| {
        let error = "API key storage is corrupt (invalid data); existing storage was not changed".to_string();
        block(path, &error);
        error
    })
}

pub fn save(path: &Path, keys: &HashMap<String, String>) -> Result<(), String> {
    let plain = serde_json::to_vec(keys).map_err(|e| e.to_string())?;
    #[cfg(target_os = "linux")]
    let plain = zeroize::Zeroizing::new(plain);
    write_encrypted(path, &plain, KEYS)
}

/// Register both history and credentials during startup, even when history is disabled.
/// A missing wallet key must not be replaced while either encrypted file still exists.
pub(crate) fn register(path: &Path) {
    #[cfg(target_os = "linux")]
    linux::register(path);
    #[cfg(not(target_os = "linux"))]
    let _ = path;
}

pub(crate) fn block(path: &Path, error: &str) {
    #[cfg(target_os = "linux")]
    linux::block(path, error);
    #[cfg(not(target_os = "linux"))]
    let _ = (path, error);
}

pub fn status(path: &Path) -> Option<String> {
    #[cfg(target_os = "linux")]
    return linux::status(path);
    #[cfg(not(target_os = "linux"))]
    {
        let _ = path;
        None
    }
}

/// A nonblocking UI snapshot, not a claim that a currently locked wallet is available.
pub fn storage_status(path: &Path) -> String {
    status(path).unwrap_or_else(|| {
        if cfg!(target_os = "linux") {
            "Encrypted persistence requires an unlocked Secret Service wallet; session-only credentials remain available".into()
        } else if cfg!(windows) {
            "Credentials use Windows DPAPI encryption".into()
        } else {
            "Persistent credentials are not supported on this platform".into()
        }
    })
}

pub(crate) fn private_dir(path: &Path) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    return linux::private_dir(path);
    #[cfg(not(target_os = "linux"))]
    std::fs::create_dir_all(path).map_err(|e| e.to_string())
}

#[cfg(target_os = "linux")]
pub(crate) fn remove_private(path: &Path) -> Result<(), String> {
    linux::remove_private(path)
}

/// No key creation or unlock prompt. Persistent storage requires an unlocked wallet.
pub fn check_available() -> Result<(), String> {
    #[cfg(target_os = "linux")]
    return linux::check_available();
    #[cfg(windows)]
    return Ok(());
    #[cfg(not(any(windows, target_os = "linux")))]
    Err("Secure storage is not supported on this platform".into())
}

pub(crate) fn read_encrypted(path: &Path, purpose: &[u8]) -> Result<Option<Vec<u8>>, String> {
    #[cfg(target_os = "linux")]
    return linux::read(path, purpose);
    #[cfg(not(target_os = "linux"))]
    {
        let _ = purpose;
        match std::fs::read(path) {
            Ok(blob) => unprotect(&blob).map(Some),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("Cannot read encrypted storage: {e}")),
        }
    }
}

pub(crate) fn write_encrypted(path: &Path, data: &[u8], purpose: &[u8]) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    return linux::write(path, data, purpose);
    #[cfg(not(target_os = "linux"))]
    {
        let _ = purpose;
        write_private(path, &protect(data)?)
    }
}

pub(crate) fn remove_encrypted(path: &Path) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    return linux::remove(path);
    #[cfg(not(target_os = "linux"))]
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("Cannot delete encrypted storage: {e}")),
    }
}

pub(crate) fn write_private(path: &Path, data: &[u8]) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    return linux::write_private(path, data);
    #[cfg(not(target_os = "linux"))]
    {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, data).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, path).map_err(|e| e.to_string())
    }
}

#[cfg(windows)]
pub(crate) fn protect(data: &[u8]) -> Result<Vec<u8>, String> {
    use windows::Win32::Security::Cryptography::{CryptProtectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB};
    unsafe {
        let input = CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 };
        let mut output = CRYPT_INTEGER_BLOB::default();
        CryptProtectData(&input, windows::core::w!("Amber"), None, None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut output)
            .map_err(|e| e.to_string())?;
        Ok(take_blob(output))
    }
}

#[cfg(windows)]
pub(crate) fn unprotect(data: &[u8]) -> Result<Vec<u8>, String> {
    use windows::Win32::Security::Cryptography::{CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB};
    unsafe {
        let input = CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 };
        let mut output = CRYPT_INTEGER_BLOB::default();
        CryptUnprotectData(&input, None, None, None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut output).map_err(|e| e.to_string())?;
        Ok(take_blob(output))
    }
}

/// Copies DPAPI's output and frees its buffer.
#[cfg(windows)]
unsafe fn take_blob(blob: windows::Win32::Security::Cryptography::CRYPT_INTEGER_BLOB) -> Vec<u8> {
    use windows::Win32::Foundation::{LocalFree, HLOCAL};
    let out = std::slice::from_raw_parts(blob.pbData, blob.cbData as usize).to_vec();
    let _ = LocalFree(Some(HLOCAL(blob.pbData as *mut _)));
    out
}

#[cfg(not(any(windows, target_os = "linux")))]
pub(crate) fn protect(_data: &[u8]) -> Result<Vec<u8>, String> {
    Err("Secure storage is not supported on this platform".into())
}

#[cfg(not(any(windows, target_os = "linux")))]
pub(crate) fn unprotect(_data: &[u8]) -> Result<Vec<u8>, String> {
    Err("Secure storage is not supported on this platform".into())
}
