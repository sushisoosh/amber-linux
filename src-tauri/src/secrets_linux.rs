//! Linux storage boundary. The wallet key is never cached or replaced on a read failure.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use secret_service::blocking::SecretService;
use secret_service::EncryptionType;
use zeroize::Zeroizing;

const MAGIC: &[u8] = b"AMBER\0LINUX\x01";
const NONCE_LEN: usize = 24;
const MAX_STORAGE_BYTES: u64 = 64 * 1024 * 1024;
const LOCKED: &str = "Secret Service wallet is locked; unlock it and retry, or use session-only credentials";
const UNAVAILABLE: &str = "Secret Service is unavailable; persistent credentials and history are disabled";
const MISSING: &str =
    "The wallet encryption key is missing while encrypted data exists; restore the original key or explicitly delete the encrypted data";

// Serializes key creation and writes within the single-instance desktop process. Values
// remember failed loads so a partial/empty in-memory map can never overwrite unread data.
type Paths = HashMap<PathBuf, Option<String>>;
fn paths() -> &'static Mutex<Paths> {
    static PATHS: OnceLock<Mutex<Paths>> = OnceLock::new();
    PATHS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(super) fn register(path: &Path) {
    paths().lock().unwrap().entry(path.to_path_buf()).or_default();
}

pub(super) fn block(path: &Path, error: &str) {
    paths().lock().unwrap().insert(path.to_path_buf(), Some(error.into()));
}

pub(super) fn status(path: &Path) -> Option<String> {
    match paths().try_lock() {
        Ok(known) => known.get(path).cloned().flatten(),
        Err(_) => Some("Encrypted storage is busy; a wallet operation is in progress".into()),
    }
}

fn keyring_error(error: secret_service::Error) -> String {
    match error {
        secret_service::Error::Locked | secret_service::Error::Prompt => LOCKED.into(),
        _ => UNAVAILABLE.into(),
    }
}

fn attributes() -> HashMap<&'static str, &'static str> {
    HashMap::from([("application", "app.amber.pc"), ("purpose", "linux-storage-key-v1")])
}

fn existing_key(service: &SecretService<'_>) -> Result<Option<Zeroizing<Vec<u8>>>, String> {
    let found = service.search_items(attributes()).map_err(keyring_error)?;
    if !found.locked.is_empty() {
        return Err(LOCKED.into());
    }
    if found.unlocked.len() > 1 {
        return Err("Multiple Amber encryption keys exist in the wallet; refusing to choose or replace one".into());
    }
    let Some(item) = found.unlocked.first() else { return Ok(None) };
    let key = Zeroizing::new(item.get_secret().map_err(keyring_error)?);
    if key.len() != 32 {
        return Err("The wallet encryption key is invalid; it was not replaced".into());
    }
    Ok(Some(key))
}

fn app_key(allow_create: bool) -> Result<Zeroizing<Vec<u8>>, String> {
    let service = SecretService::connect(EncryptionType::Dh).map_err(keyring_error)?;
    if let Some(key) = existing_key(&service)? {
        return Ok(key);
    }
    // Never use the transient "session" collection or create/unlock a collection implicitly.
    let collection = service.get_default_collection().map_err(keyring_error)?;
    if collection.is_locked().map_err(keyring_error)? {
        return Err(LOCKED.into());
    }
    if !allow_create {
        return Err(MISSING.into());
    }
    let mut key = Zeroizing::new(vec![0u8; 32]);
    getrandom::getrandom(&mut key).map_err(|_| "Secure random generation failed")?;
    collection
        .create_item("Amber Linux encrypted storage", attributes(), &key, false, "application/octet-stream")
        .map_err(keyring_error)?;
    // Re-read: another process creating an item concurrently must fail as ambiguous, never
    // replace the other process's key or encrypt under a key the wallet did not retain.
    let stored = existing_key(&service)?.ok_or(UNAVAILABLE)?;
    if *stored != *key {
        return Err("The wallet encryption key changed during creation; no data was written".into());
    }
    Ok(stored)
}

fn can_create(known: &Paths) -> Result<bool, String> {
    for path in known.keys() {
        match fs::symlink_metadata(path) {
            Ok(_) => return Ok(false),
            Err(e) if e.kind() == ErrorKind::NotFound => (),
            Err(e) => return Err(format!("Cannot inspect encrypted storage: {e}")),
        }
    }
    Ok(true)
}

pub(super) fn check_available() -> Result<(), String> {
    let known = paths().lock().unwrap();
    let service = SecretService::connect(EncryptionType::Dh).map_err(keyring_error)?;
    if existing_key(&service)?.is_some() {
        return Ok(());
    }
    let collection = service.get_default_collection().map_err(keyring_error)?;
    if collection.is_locked().map_err(keyring_error)? {
        return Err(LOCKED.into());
    }
    if !can_create(&known)? {
        return Err(MISSING.into());
    }
    Ok(())
}

fn aad(purpose: &[u8]) -> Vec<u8> {
    [MAGIC, purpose].concat()
}

fn seal(data: &[u8], purpose: &[u8], key: &[u8]) -> Result<Vec<u8>, String> {
    let cipher = XChaCha20Poly1305::new_from_slice(key).map_err(|_| "Invalid encryption key")?;
    let mut nonce = [0; NONCE_LEN];
    getrandom::getrandom(&mut nonce).map_err(|_| "Secure random generation failed")?;
    let ciphertext = cipher
        .encrypt(XNonce::from_slice(&nonce), Payload { msg: data, aad: &aad(purpose) })
        .map_err(|_| "Encryption failed; no data was written")?;
    Ok([MAGIC, &nonce, &ciphertext].concat())
}

fn validate_envelope(blob: &[u8]) -> Result<(), String> {
    if !blob.starts_with(MAGIC) {
        return Err("Unsupported encrypted storage format; plaintext and Windows DPAPI data are not imported on Linux".into());
    }
    if blob.len() < MAGIC.len() + NONCE_LEN + 16 {
        return Err("Encrypted storage is corrupt (truncated envelope); existing storage was not changed".into());
    }
    Ok(())
}

fn open(blob: &[u8], purpose: &[u8], key: &[u8]) -> Result<Vec<u8>, String> {
    validate_envelope(blob)?;
    let cipher = XChaCha20Poly1305::new_from_slice(key).map_err(|_| "Invalid encryption key")?;
    let nonce = &blob[MAGIC.len()..MAGIC.len() + NONCE_LEN];
    cipher
        .decrypt(XNonce::from_slice(nonce), Payload { msg: &blob[MAGIC.len() + NONCE_LEN..], aad: &aad(purpose) })
        .map_err(|_| "Encrypted storage failed authentication (corrupt data or wrong wallet key); existing storage was not changed".into())
}

pub(super) fn read(path: &Path, purpose: &[u8]) -> Result<Option<Vec<u8>>, String> {
    let mut known = paths().lock().unwrap();
    known.entry(path.to_path_buf()).or_default();
    let result = (|| {
        let Some(blob) = read_private(path)? else { return Ok(None) };
        validate_envelope(&blob)?;
        open(&blob, purpose, &app_key(false)?).map(Some)
    })();
    known.insert(path.to_path_buf(), result.as_ref().err().cloned());
    result
}

pub(super) fn write(path: &Path, data: &[u8], purpose: &[u8]) -> Result<(), String> {
    let mut known = paths().lock().unwrap();
    if let Some(error) = known.entry(path.to_path_buf()).or_default() {
        return Err(format!("Storage is blocked until explicitly reloaded or deleted: {error}"));
    }
    let result = write_with_key(path, data, purpose, &known, app_key);
    known.insert(path.to_path_buf(), result.as_ref().err().cloned());
    result
}

fn write_with_key(
    path: &Path,
    data: &[u8],
    purpose: &[u8],
    known: &Paths,
    get_key: impl FnOnce(bool) -> Result<Zeroizing<Vec<u8>>, String>,
) -> Result<(), String> {
    let old = read_private(path)?;
    if let Some(blob) = &old {
        validate_envelope(blob)?;
    }
    let key = get_key(can_create(known)?)?;
    if let Some(blob) = &old {
        // A fresh load of this path is not required to preserve wrong-key/corrupt data.
        let _plain = Zeroizing::new(open(blob, purpose, &key)?);
    }
    write_private(path, &seal(data, purpose, &key)?)
}

pub(super) fn remove(path: &Path) -> Result<(), String> {
    let mut known = paths().lock().unwrap();
    let result = remove_private(path);
    known.insert(path.to_path_buf(), result.as_ref().err().cloned());
    result
}

pub(super) fn remove_private(path: &Path) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        no_symlink_ancestors(dir)?;
    }
    match fs::remove_file(path) {
        Ok(()) => path
            .parent()
            .map_or(Ok(()), |dir| File::open(dir).and_then(|f| f.sync_all()))
            .map_err(|e| format!("Cannot sync storage deletion: {e}")),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("Cannot delete private storage: {e}")),
    }
}

fn check_owner(metadata: &fs::Metadata) -> Result<(), String> {
    if metadata.uid() != unsafe { libc::geteuid() } {
        return Err("Private storage is not owned by the current user".into());
    }
    Ok(())
}

fn no_symlink_ancestors(path: &Path) -> Result<(), String> {
    if !path.is_absolute() || path.components().any(|c| c == std::path::Component::ParentDir) {
        return Err("Private storage requires an absolute path without '..'".into());
    }
    for dir in path.ancestors() {
        match fs::symlink_metadata(dir) {
            Ok(meta) if meta.file_type().is_symlink() => return Err("Private storage refuses symbolic links".into()),
            Ok(_) => (),
            Err(e) if e.kind() == ErrorKind::NotFound => (),
            Err(e) => return Err(format!("Cannot inspect private storage: {e}")),
        }
    }
    Ok(())
}

pub(super) fn private_dir(path: &Path) -> Result<(), String> {
    no_symlink_ancestors(path)?;
    fs::DirBuilder::new().recursive(true).mode(0o700).create(path).map_err(|e| e.to_string())?;
    let dir = OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY).open(path).map_err(|e| e.to_string())?;
    check_owner(&dir.metadata().map_err(|e| e.to_string())?)?;
    dir.set_permissions(fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())
}

fn read_private(path: &Path) -> Result<Option<Vec<u8>>, String> {
    no_symlink_ancestors(path)?;
    let file = match OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK).open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("Cannot read private storage: {e}")),
    };
    let meta = file.metadata().map_err(|e| e.to_string())?;
    check_owner(&meta)?;
    if !meta.is_file() || meta.nlink() != 1 {
        return Err("Private storage must be a regular file with no hard links".into());
    }
    if meta.len() > MAX_STORAGE_BYTES {
        return Err("Encrypted storage exceeds the 64 MiB safety limit; existing storage was not changed".into());
    }
    file.set_permissions(fs::Permissions::from_mode(0o600)).map_err(|e| e.to_string())?;
    let mut blob = Vec::new();
    file.take(MAX_STORAGE_BYTES + 1).read_to_end(&mut blob).map_err(|e| e.to_string())?;
    if blob.len() as u64 > MAX_STORAGE_BYTES {
        return Err("Encrypted storage exceeds the 64 MiB safety limit; existing storage was not changed".into());
    }
    Ok(Some(blob))
}

pub(super) fn write_private(path: &Path, data: &[u8]) -> Result<(), String> {
    if data.len() as u64 > MAX_STORAGE_BYTES {
        return Err("Private storage exceeds the 64 MiB safety limit; existing storage was not changed".into());
    }
    no_symlink_ancestors(path)?;
    let dir = path.parent().ok_or("Private storage has no parent directory")?;
    private_dir(dir)?;
    if let Ok(meta) = fs::symlink_metadata(path) {
        check_owner(&meta)?;
        if !meta.is_file() || meta.nlink() != 1 {
            return Err("Private storage must be a regular file with no hard links".into());
        }
    }
    let mut random = [0; 16];
    getrandom::getrandom(&mut random).map_err(|_| "Secure random generation failed")?;
    let suffix: String = random.iter().map(|b| format!("{b:02x}")).collect();
    let tmp = dir.join(format!(".amber-write-{suffix}.tmp"));
    let mut file =
        OpenOptions::new().write(true).create_new(true).mode(0o600).custom_flags(libc::O_NOFOLLOW).open(&tmp).map_err(|e| e.to_string())?;
    let result = (|| {
        file.write_all(data)?;
        file.sync_all()?;
        fs::rename(&tmp, path)?;
        Ok(())
    })()
    .map_err(|e: std::io::Error| format!("Private storage write failed: {e}"));
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result?;
    File::open(dir)
        .and_then(|f| f.sync_all())
        .map_err(|e| format!("Private storage was replaced, but durability could not be confirmed: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> PathBuf {
        let mut random = [0u8; 8];
        getrandom::getrandom(&mut random).unwrap();
        std::env::temp_dir().join(format!("amber-storage-test-{}-{}", std::process::id(), u64::from_le_bytes(random)))
    }

    #[test]
    fn authenticated_envelope_is_randomized_and_purpose_bound() {
        let key = [7; 32];
        let a = seal(b"dummy-api-key", b"api-keys", &key).unwrap();
        let b = seal(b"dummy-api-key", b"api-keys", &key).unwrap();
        assert_ne!(a, b);
        assert!(!a.windows(b"dummy-api-key".len()).any(|w| w == b"dummy-api-key"));
        assert_eq!(open(&a, b"api-keys", &key).unwrap(), b"dummy-api-key");
        assert!(open(&a, b"history", &key).unwrap_err().contains("authentication"));
        assert!(open(&a, b"api-keys", &[8; 32]).unwrap_err().contains("authentication"));
        for i in 0..a.len() {
            let mut altered = a.clone();
            altered[i] ^= 1;
            assert!(open(&altered, b"api-keys", &key).is_err());
        }
        for len in 0..a.len() {
            assert!(open(&a[..len], b"api-keys", &key).is_err());
        }
        assert!(open(b"{\"key\":\"plaintext\"}", b"api-keys", &key).unwrap_err().contains("not imported"));
    }

    #[test]
    fn private_atomic_replacement_has_restrictive_permissions() {
        let dir = temp_dir();
        let path = dir.join("nested/history.bin");
        write_private(&path, b"old encrypted data").unwrap();
        write_private(&path, b"new encrypted data").unwrap();
        assert_eq!(read_private(&path).unwrap().unwrap(), b"new encrypted data");
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(fs::metadata(path.parent().unwrap()).unwrap().permissions().mode() & 0o777, 0o700);
        assert_eq!(fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn symlinks_and_hardlinks_are_refused() {
        let dir = temp_dir();
        let original = dir.join("original");
        write_private(&original, b"unchanged").unwrap();
        let link = dir.join("link");
        std::os::unix::fs::symlink(&original, &link).unwrap();
        assert!(write_private(&link, b"replaced").is_err());
        assert!(read_private(&link).is_err());
        fs::remove_file(&link).unwrap();
        fs::hard_link(&original, &link).unwrap();
        assert!(write_private(&link, b"replaced").is_err());
        assert!(read_private(&link).is_err());
        assert_eq!(fs::read(&original).unwrap(), b"unchanged");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_existing_file_blocks_key_creation_even_for_a_different_store() {
        let dir = temp_dir();
        let history = dir.join("history.bin");
        let keys = dir.join("secrets.bin");
        let known = HashMap::from([(history.clone(), None), (keys, None)]);
        assert!(can_create(&known).unwrap());
        write_private(&history, b"unreadable old data").unwrap();
        assert!(!can_create(&known).unwrap());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn failed_load_never_overwrites_and_clear_does_not_require_a_wallet() {
        let dir = temp_dir();
        let path = dir.join("history.bin");
        write_private(&path, b"old unsupported data").unwrap();
        assert!(read(&path, b"history").unwrap_err().contains("not imported"));
        assert!(write(&path, b"new data", b"history").unwrap_err().contains("blocked"));
        assert_eq!(fs::read(&path).unwrap(), b"old unsupported data");
        remove(&path).unwrap();
        assert!(paths().lock().unwrap().get(&path).unwrap().is_none());
        assert!(!path.exists());
        remove(&path).unwrap();
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn keyring_errors_do_not_expose_service_details() {
        assert_eq!(keyring_error(secret_service::Error::Locked), LOCKED);
        assert_eq!(keyring_error(secret_service::Error::Unavailable), UNAVAILABLE);
        assert_eq!(keyring_error(secret_service::Error::Crypto("sensitive")), UNAVAILABLE);
    }

    #[test]
    fn status_does_not_wait_for_an_in_progress_wallet_operation() {
        let _busy = paths().lock().unwrap();
        assert!(status(Path::new("/unused/amber.bin")).unwrap().contains("busy"));
    }

    #[test]
    fn oversized_ciphertext_is_rejected_before_reading_it() {
        let dir = temp_dir();
        let path = dir.join("history.bin");
        write_private(&path, b"small").unwrap();
        OpenOptions::new().write(true).open(&path).unwrap().set_len(MAX_STORAGE_BYTES + 1).unwrap();
        assert!(read_private(&path).unwrap_err().contains("safety limit"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn locked_missing_and_wrong_keys_do_not_create_or_replace_files() {
        let dir = temp_dir();
        let path = dir.join("history.bin");
        let known = HashMap::from([(path.clone(), None)]);
        let old = seal(b"old private history", b"history", &[3; 32]).unwrap();
        assert_eq!(
            write_with_key(&path, b"new transcript", b"history", &known, |allow_create| {
                assert!(allow_create);
                Err(LOCKED.into())
            })
            .unwrap_err(),
            LOCKED
        );
        assert!(!path.exists());
        write_private(&path, &old).unwrap();
        for error in [LOCKED, MISSING, UNAVAILABLE] {
            assert_eq!(
                write_with_key(&path, b"new transcript", b"history", &known, |allow_create| {
                    assert!(!allow_create, "old ciphertext prohibits a replacement key");
                    Err(error.into())
                })
                .unwrap_err(),
                error
            );
            assert_eq!(fs::read(&path).unwrap(), old);
        }
        assert!(write_with_key(&path, b"new transcript", b"history", &known, |_| Ok(Zeroizing::new(vec![4; 32])))
            .unwrap_err()
            .contains("authentication"));
        assert_eq!(fs::read(&path).unwrap(), old);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn missing_keyring_never_writes_plaintext() {
        const CHILD: &str = "AMBER_TEST_MISSING_KEYRING";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "secrets::linux::tests::missing_keyring_never_writes_plaintext", "--nocapture"])
                .env(CHILD, "1")
                .env("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent/amber-test-dbus")
                .output()
                .unwrap();
            assert!(output.status.success(), "{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
            assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
            return;
        }
        let dir = temp_dir();
        let path = dir.join("secrets.bin");
        assert_eq!(write(&path, b"dummy-credential", b"api-keys").unwrap_err(), UNAVAILABLE);
        assert!(!path.exists());
        assert!(!dir.exists());
        let old = seal(b"previous-credential", b"api-keys", &[9; 32]).unwrap();
        write_private(&path, &old).unwrap();
        assert_eq!(read(&path, b"api-keys").unwrap_err(), UNAVAILABLE);
        assert!(write(&path, b"dummy-credential", b"api-keys").is_err());
        assert_eq!(fs::read(&path).unwrap(), old);
        remove(&path).unwrap();
        fs::remove_dir_all(dir).unwrap();
    }
}
