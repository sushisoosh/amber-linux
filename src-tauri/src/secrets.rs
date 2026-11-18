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
pub(crate) const TESTING_TABLES: &[u8] = b"testing-tables";

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

/// Register every encrypted file before any wallet operation, including disabled stores.
/// A missing wallet key must not be replaced while any encrypted file still exists.
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

/// Testing archives have an 8 MiB plaintext bound; cap ciphertext before decryption too.
/// Linux's shared private reader already enforces this 64 MiB envelope bound.
pub(crate) fn read_encrypted_bounded(path: &Path, purpose: &[u8]) -> Result<Option<Vec<u8>>, String> {
    #[cfg(target_os = "linux")]
    return linux::read(path, purpose);
    #[cfg(not(target_os = "linux"))]
    {
        use std::io::Read;
        let _ = purpose;
        const LIMIT: u64 = 64 * 1024 * 1024;
        let file = match std::fs::File::open(path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("Cannot read encrypted storage: {e}")),
        };
        let meta = file.metadata().map_err(|e| e.to_string())?;
        if !meta.is_file() || meta.len() > LIMIT { return Err("Encrypted storage exceeds its safety limit or is not a regular file".into()); }
        let mut blob = Vec::new();
        file.take(LIMIT + 1).read_to_end(&mut blob).map_err(|e| e.to_string())?;
        if blob.len() as u64 > LIMIT { return Err("Encrypted storage exceeds the 64 MiB safety limit".into()); }
        unprotect(&blob).map(Some)
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
    #[cfg(windows)]
    return write_private_with(path, data, replace_private);
    #[cfg(not(any(windows, target_os = "linux")))]
    write_private_with(path, data, |tmp, path| std::fs::rename(tmp, path).map_err(|e| e.to_string()))
}

#[cfg(any(not(target_os = "linux"), test))]
fn write_private_with(path: &Path, data: &[u8], replace: impl FnOnce(&Path, &Path) -> Result<(), String>) -> Result<(), String> {
    use std::io::Write;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let tmp = path.with_extension("tmp");
    let prepared = (|| {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(data)?;
        file.sync_all()
    })();
    // The encrypted temporary file is flushed and closed before durable replacement.
    let result = prepared.map_err(|e: std::io::Error| e.to_string()).and_then(|_| replace(&tmp, path));
    if result.is_err() { let _ = std::fs::remove_file(&tmp); }
    result
}

#[cfg(windows)]
fn replace_private(from: &Path, to: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH};

    let mut from: Vec<u16> = from.as_os_str().encode_wide().collect();
    let mut to: Vec<u16> = to.as_os_str().encode_wide().collect();
    if from.contains(&0) || to.contains(&0) { return Err("Private storage path contains a null character".into()); }
    from.push(0);
    to.push(0);
    // Same-directory replacement, not a copy/delete or deferred move. ReplaceFileW's
    // similarly named WRITE_THROUGH flag is unsupported; MoveFileExW supports this flag.
    unsafe { MoveFileExW(PCWSTR(from.as_ptr()), PCWSTR(to.as_ptr()), MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH) }
        .map_err(|e| format!("Private storage replacement failed or durability could not be confirmed: {e}"))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("amber-replacement-test-{}-{at}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir(&dir).unwrap();
        dir
    }

    #[test]
    fn replacement_failure_is_reported_and_keeps_old_ciphertext() {
        let dir = temp_dir();
        let path = dir.join("mock.bin");
        std::fs::write(&path, b"old mock ciphertext").unwrap();
        let error = write_private_with(&path, b"new mock ciphertext", |from, to| {
            assert_eq!(from.parent(), to.parent());
            assert_eq!(std::fs::read(from).unwrap(), b"new mock ciphertext");
            assert_eq!(std::fs::read(to).unwrap(), b"old mock ciphertext");
            Err("Mock durable replacement failed".into())
        }).unwrap_err();
        assert_eq!(error, "Mock durable replacement failed");
        assert_eq!(std::fs::read(&path).unwrap(), b"old mock ciphertext");
        assert!(!path.with_extension("tmp").exists());
        write_private_with(&path, b"confirmed mock ciphertext", |from, to| {
            std::fs::rename(from, to).map_err(|e| e.to_string())
        }).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"confirmed mock ciphertext");
        assert!(!path.with_extension("tmp").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn replacement_uncertainty_and_preparation_failure_never_report_success() {
        let dir = temp_dir();
        let path = dir.join("mock.bin");
        std::fs::write(&path, b"old mock ciphertext").unwrap();
        let error = write_private_with(&path, b"unconfirmed mock ciphertext", |from, to| {
            std::fs::rename(from, to).unwrap();
            Err("Mock durability could not be confirmed".into())
        }).unwrap_err();
        assert_eq!(error, "Mock durability could not be confirmed");
        assert_eq!(std::fs::read(&path).unwrap(), b"unconfirmed mock ciphertext");
        assert!(!path.with_extension("tmp").exists());
        std::fs::create_dir(path.with_extension("tmp")).unwrap();
        assert!(write_private_with(&path, b"must not replace", |_, _| panic!("preparation failed before replacement")).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"unconfirmed mock ciphertext");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
