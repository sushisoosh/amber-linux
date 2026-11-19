//! Copy-only, offline speech imports. Hashes detect changed bytes, not publisher trust.
//! Only generated IDs and fixed filenames ever participate in managed paths.

use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::local::{self, Layout};
use crate::onnx;
use crate::engine::{ErrorKind, SttError, TranscribeOptions};

pub const MAX_MODELS: usize = 64;
const MAX_REGISTRY_BYTES: u64 = 1_048_576;
const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const MAX_MODEL_BYTES: u64 = 16 * 1024 * 1024 * 1024;
const MAX_TEXT_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    #[error("{0}")]
    Invalid(String),
    #[error("Could not access local model storage: {0}")]
    Io(#[from] std::io::Error),
    #[error("The imported model registry is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Model import cancelled.")]
    Cancelled,
    #[error("Another model import or deletion is running. Wait for it to finish or cancel the import, then try again.")]
    Busy,
}

fn invalid(message: impl Into<String>) -> ImportError { ImportError::Invalid(message.into()) }

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Variant { Whisper, ParakeetInt8, MoonshineTiny, MoonshineBase }

impl Variant {
    pub fn files(self) -> &'static [&'static str] {
        match self {
            Self::Whisper => &["model.bin"],
            Self::ParakeetInt8 => &["encoder-model.int8.onnx", "decoder_joint-model.int8.onnx", "nemo128.onnx", "vocab.txt"],
            Self::MoonshineTiny | Self::MoonshineBase => &["encoder_model.onnx", "decoder_model_merged.onnx", "tokenizer.json"],
        }
    }

    pub fn family(self) -> &'static str {
        match self { Self::Whisper => "whisper", Self::ParakeetInt8 => "parakeet", _ => "moonshine" }
    }

    pub fn max_audio_seconds(self) -> Option<u32> {
        matches!(self, Self::MoonshineTiny | Self::MoonshineBase).then_some(64)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImportRequest {
    pub source_path: String,
    pub name: String,
    pub variant: Variant,
    pub languages: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManifestFile {
    pub name: String,
    pub size: u64,
    pub sha256: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImportedModel {
    pub id: String,
    pub name: String,
    pub variant: Variant,
    /// Empty means multilingual Whisper; ONNX always declares explicit languages.
    pub languages: Vec<String>,
    pub files: Vec<ManifestFile>,
}

impl ImportedModel {
    pub fn size_mb(&self) -> u32 { self.files.iter().map(|f| f.size).sum::<u64>().div_ceil(1_000_000) as u32 }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Registry { version: u32, models: Vec<ImportedModel> }

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PendingImport { version: u32, id: String, variant: Variant }

/// Owned runtime metadata for either a pinned catalog entry or a custom import.
#[derive(Clone, Debug)]
pub struct SpeechModel {
    pub id: String,
    pub label: String,
    pub backend: Variant,
    pub languages: Vec<String>,
    pub max_audio_seconds: Option<u32>,
    pub short_context: bool,
    pub context_slack: u32,
    /// A file for Whisper, a directory for ONNX; never an original source path.
    pub path: PathBuf,
    pub imported: bool,
    manifest: Option<ImportedModel>,
}

impl SpeechModel {
    pub fn whisper(dir: &Path, model: &local::Model) -> Self {
        Self { id: model.id.into(), label: model.label.into(), backend: Variant::Whisper,
            languages: if model.id.contains(".en") { vec!["en".into()] } else { Vec::new() },
            max_audio_seconds: None, short_context: model.short_context, context_slack: model.context_slack,
            path: dir.join(model.file), imported: false, manifest: None }
    }

    pub fn is_onnx(&self) -> bool { self.backend != Variant::Whisper }
    pub fn supports_prompt(&self) -> bool { !self.is_onnx() }

    pub fn validate_request(&self, options: &TranscribeOptions, samples: usize) -> Result<(), SttError> {
        let normalized = TranscribeOptions::new(options.language.as_deref().unwrap_or("auto"), "");
        if !self.languages.is_empty() && normalized.among().iter().any(|code| !self.languages.contains(code)) {
            return Err(SttError::new(ErrorKind::BadResponse, format!("{} does not support the selected languages. Choose supported languages or automatic detection.", self.label),
                format!("supported languages: {}", self.languages.join(", "))));
        }
        if self.max_audio_seconds.is_some() && samples < 1600 {
            return Err(SttError::new(ErrorKind::BadResponse, "Moonshine needs at least 0.1 seconds of audio.", "Moonshine input below native duration limit"));
        }
        Ok(())
    }
}

pub fn is_import_id(id: &str) -> bool {
    id.strip_prefix("import-").is_some_and(|s| s.len() == 32 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
}

pub fn list(layout: &Layout) -> Result<Vec<ImportedModel>, ImportError> {
    let path = layout.root.join("imports/registry.json");
    reject_links(&path)?;
    if !path.exists() { return Ok(Vec::new()); }
    let registry: Registry = serde_json::from_slice(&read_bounded(&path, MAX_REGISTRY_BYTES)?)?;
    if registry.version != 1 || registry.models.len() > MAX_MODELS {
        return Err(invalid("Unsupported or oversized imported model registry."));
    }
    let mut ids = HashSet::new();
    for model in &registry.models {
        validate_record(model)?;
        if !ids.insert(&model.id) { return Err(invalid("Duplicate model ID in imported model registry.")); }
    }
    Ok(registry.models)
}

pub fn resolve(layout: &Layout, id: &str) -> Result<SpeechModel, ImportError> {
    if let Some(model) = local::model(id) { return Ok(SpeechModel::whisper(&layout.root.join("models"), model)); }
    if let Some(model) = onnx::model(id) {
        return Ok(SpeechModel { id: model.id.into(), label: model.label.into(), backend: match model.backend {
            onnx::Backend::Parakeet => Variant::ParakeetInt8,
            onnx::Backend::MoonshineTiny => Variant::MoonshineTiny,
            onnx::Backend::MoonshineBase => Variant::MoonshineBase,
        }, languages: model.languages.iter().map(|s| (*s).into()).collect(), max_audio_seconds: model.max_audio_seconds,
            short_context: false, context_slack: 0, path: onnx::model_dir(layout, model), imported: false, manifest: None });
    }
    if !is_import_id(id) { return Err(invalid("Unknown speech model.")); }
    let model = list(layout)?.into_iter().find(|m| m.id == id).ok_or_else(|| invalid("This imported speech model is missing. Import it again or select another model."))?;
    let dir = layout.root.join("imports").join(id);
    Ok(SpeechModel { id: id.into(), label: model.name.clone(), backend: model.variant,
        languages: model.languages.clone(), max_audio_seconds: model.variant.max_audio_seconds(),
        short_context: false, context_slack: 0,
        path: if model.variant == Variant::Whisper { dir.join("model.bin") } else { dir },
        imported: true, manifest: Some(model) })
}

/// Cheap status only; native loaders call verify before reading imported model bytes.
pub fn installed(layout: &Layout, model: &SpeechModel) -> bool {
    if let Some(record) = &model.manifest { return check_install(layout, record, false).is_ok(); }
    if let Some(m) = local::model(&model.id) { return layout.model_installed(m); }
    onnx::model(&model.id).is_some_and(|m| onnx::installed(layout, m))
}

pub fn verify(layout: &Layout, model: &SpeechModel) -> Result<(), ImportError> {
    if let Some(record) = &model.manifest {
        if !list(layout)?.contains(record) { return Err(invalid("The imported model was removed or changed.")); }
        return check_install(layout, record, true);
    }
    if let Some(m) = onnx::model(&model.id) { return onnx::verify(layout, m).map_err(|e| invalid(e.to_string())); }
    if installed(layout, model) { Ok(()) } else { Err(invalid("The speech model is not installed.")) }
}

fn validate_record(model: &ImportedModel) -> Result<(), ImportError> {
    if !is_import_id(&model.id) { return Err(invalid("Unsafe imported model ID.")); }
    validate_name_languages(&model.name, model.variant, &model.languages)?;
    if model.files.len() != model.variant.files().len() { return Err(invalid("Incomplete imported model manifest.")); }
    let mut names = HashSet::new();
    let mut total = 0u64;
    for file in &model.files {
        if !model.variant.files().contains(&file.name.as_str()) || !names.insert(&file.name) {
            return Err(invalid("Unsafe or duplicate filename in imported model manifest."));
        }
        if file.size == 0 || file.size > MAX_FILE_BYTES || ((file.name.ends_with(".json") || file.name.ends_with(".txt")) && file.size > MAX_TEXT_BYTES) {
            return Err(invalid("Imported model file exceeds the size limit."));
        }
        total += file.size;
        if file.sha256.len() != 64 || !file.sha256.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
            return Err(invalid("Invalid imported model checksum."));
        }
    }
    if total > MAX_MODEL_BYTES { return Err(invalid("Imported model exceeds the size limit.")); }
    Ok(())
}

fn validate_name_languages(name: &str, variant: Variant, languages: &[String]) -> Result<(), ImportError> {
    if name.trim() != name || name.is_empty() || name.chars().count() > 80 || name.chars().any(char::is_control) {
        return Err(invalid("Use a model name of 1 to 80 characters without control characters."));
    }
    let mut seen = HashSet::new();
    if languages.len() > 100 || (variant != Variant::Whisper && languages.is_empty()) || languages.iter().any(|s|
        !(2..=3).contains(&s.len()) || !s.bytes().all(|b| b.is_ascii_lowercase()) || !seen.insert(s)) {
        return Err(invalid("Use unique lowercase language codes (for example en or de). ONNX models need at least one language."));
    }
    Ok(())
}

fn check_install(layout: &Layout, record: &ImportedModel, hashes: bool) -> Result<(), ImportError> {
    validate_record(record)?;
    let dir = layout.root.join("imports").join(&record.id);
    check_directory(&dir, record)?;
    let stored: ImportedModel = serde_json::from_slice(&read_bounded(&dir.join("manifest.json"), MAX_REGISTRY_BYTES)?)?;
    if &stored != record { return Err(invalid("The imported model manifest has changed.")); }
    for file in &record.files {
        let path = dir.join(&file.name);
        let mut opened = open_regular(&path)?;
        if opened.metadata()?.len() != file.size { return Err(invalid("The imported model is incomplete or damaged.")); }
        if hashes {
            let mut hash = Sha256::new();
            std::io::copy(&mut opened, &mut hash)?;
            if hex(&hash.finalize()) != file.sha256 { return Err(invalid("The imported model checksum has changed. Import it again from a trusted source.")); }
            validate_native(&path, record.variant, &record.languages)?;
        }
    }
    Ok(())
}

fn check_directory(dir: &Path, record: &ImportedModel) -> Result<(), ImportError> {
    reject_links(dir)?;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_str().ok_or_else(|| invalid("Unsafe model filename."))?;
        if name != "manifest.json" && !record.variant.files().contains(&name) { return Err(invalid("Unexpected file in imported model directory.")); }
        open_regular(&entry.path())?;
    }
    Ok(())
}

/// Reserve storage without waiting, especially before a caller takes dictation/lifecycle locks.
fn storage_lock(layout: &Layout) -> Result<File, ImportError> {
    let dir = layout.root.join("imports");
    reject_links(&dir)?;
    fs::create_dir_all(&dir)?;
    let path = dir.join("registry.lock");
    reject_links(&path)?;
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    no_follow(&mut options);
    let lock = options.open(path)?;
    ensure_regular(&lock.metadata()?)?;
    lock.try_lock().map_err(|error| match error {
        std::fs::TryLockError::WouldBlock => ImportError::Busy,
        std::fs::TryLockError::Error(error) => ImportError::Io(error),
    })?;
    Ok(lock)
}

/// Called only while the registry lock is held, before any new staging is created. The
/// old importer used the same reserved six-character tempfile names without a marker.
fn recover_interrupted(layout: &Layout, registry: &[ImportedModel], source: &Path) -> Result<(), ImportError> {
    let source = source.canonicalize()?;
    let parent = layout.root.join("imports");
    for entry in fs::read_dir(&parent)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let staging = name.strip_prefix(".import-").is_some_and(|suffix| suffix.len() == 6 && suffix.bytes().all(|b| b.is_ascii_alphanumeric()));
        let orphan = is_import_id(name) && !registry.iter().any(|m| m.id == name);
        if !staging && !orphan { continue; }
        let dir = entry.path();
        let result = (|| {
            reject_links(&dir)?;
            if !entry.file_type()?.is_dir() { return Ok(()); }
            // A manually selected file/folder is never a recovery target, even when it
            // lives inside the managed tree. No original source path is persisted.
            let canonical = dir.canonicalize()?;
            if source.starts_with(&canonical) || canonical.starts_with(&source) { return Ok(()); }
            let mut files = Vec::new();
            for file in fs::read_dir(&dir)? {
                let file = file?;
                let filename = file.file_name().into_string().map_err(|_| invalid("Unexpected filename in interrupted import."))?;
                open_regular(&file.path())?;
                files.push(filename);
            }
            let variant = if orphan && !files.is_empty() {
                // A final directory is app-owned only when its manifest identifies it.
                if !files.iter().any(|f| f == "manifest.json") { return Ok(()); }
                let record: ImportedModel = serde_json::from_slice(&read_bounded(&dir.join("manifest.json"), MAX_REGISTRY_BYTES)?)?;
                validate_record(&record)?;
                if record.id != name { return Err(invalid("Interrupted import manifest does not match its directory.")); }
                Some(record.variant)
            } else if staging && files.iter().any(|f| f == "pending.json") && files.len() > 1 {
                let pending: PendingImport = serde_json::from_slice(&read_bounded(&dir.join("pending.json"), MAX_REGISTRY_BYTES)?)?;
                if pending.version != 1 || !is_import_id(&pending.id) { return Err(invalid("Invalid interrupted import marker.")); }
                Some(pending.variant)
            } else { None };
            for file in &files {
                let known = variant.map_or_else(|| [Variant::Whisper, Variant::ParakeetInt8, Variant::MoonshineTiny].iter().any(|v| v.files().contains(&file.as_str())),
                    |v| v.files().contains(&file.as_str()));
                if !known && file != "manifest.json" && !(staging && file == "pending.json") {
                    return Err(invalid("Unexpected file in interrupted import; nothing in this directory was removed."));
                }
            }
            // Never recurse. Keep ownership metadata until every model file is gone,
            // so a permission/in-use failure leaves a recognizable, retryable directory.
            for file in files.iter().filter(|f| *f != "manifest.json" && *f != "pending.json") { fs::remove_file(dir.join(file))?; }
            for file in ["manifest.json", "pending.json"] {
                if files.iter().any(|f| f == file) { fs::remove_file(dir.join(file))?; }
            }
            fs::remove_dir(&dir)?;
            Ok::<_, ImportError>(())
        })();
        result.map_err(|error| invalid(format!("Could not recover interrupted model import {name}: {error} Check the local models folder and its permissions, then retry.")))?;
    }
    Ok(())
}

fn save_registry(layout: &Layout, models: &[ImportedModel]) -> Result<(), ImportError> {
    let bytes = serde_json::to_vec(&Registry { version: 1, models: models.to_vec() })?;
    if bytes.len() as u64 > MAX_REGISTRY_BYTES || models.len() > MAX_MODELS { return Err(invalid("Imported model registry is full.")); }
    let dir = layout.root.join("imports");
    reject_links(&dir.join("registry.json"))?;
    let mut file = tempfile::NamedTempFile::new_in(&dir)?;
    file.write_all(&bytes)?;
    file.as_file().sync_all()?;
    file.persist(dir.join("registry.json")).map_err(|e| e.error)?;
    Ok(())
}

/// Import never loads native code, installs a runtime, or accesses the network.
pub fn import(layout: &Layout, request: &ImportRequest, cancel: &AtomicBool, progress: &mut dyn FnMut(u64, u64)) -> Result<ImportedModel, ImportError> {
    if cancel.load(Ordering::Relaxed) { return Err(ImportError::Cancelled); }
    validate_name_languages(&request.name, request.variant, &request.languages)?;
    let source = Path::new(&request.source_path);
    if !source.is_absolute() { return Err(invalid("Choose an absolute local model path.")); }
    reject_links(source)?;
    if request.variant == Variant::Whisper {
        if !source.extension().is_some_and(|s| s.eq_ignore_ascii_case("bin")) { return Err(invalid("Whisper imports require a GGML .bin file, not GGUF.")); }
    } else if !source.is_dir() { return Err(invalid("Choose the folder containing the supported ONNX model files.")); }
    let _lock = storage_lock(layout)?;
    let mut registry = list(layout)?;
    recover_interrupted(layout, &registry, source)?;
    if registry.len() >= MAX_MODELS { return Err(invalid("Imported model registry is full (64 models). Delete an import first.")); }
    let mut sources = Vec::new();
    let mut total = 0u64;
    for name in request.variant.files() {
        let path = if request.variant == Variant::Whisper { source.to_path_buf() } else { source.join(name) };
        let file = open_regular(&path)?;
        let size = file.metadata()?.len();
        let limit = if name.ends_with(".json") || name.ends_with(".txt") { MAX_TEXT_BYTES } else { MAX_FILE_BYTES };
        if size == 0 || size > limit { return Err(invalid(format!("{name} is empty or exceeds the import size limit."))); }
        total += size;
        if total > MAX_MODEL_BYTES { return Err(invalid("The model exceeds the 16 GiB import limit.")); }
        sources.push((*name, file, size));
    }
    let mut random = [0u8; 16];
    getrandom::fill(&mut random).map_err(|_| invalid("Could not generate a secure model ID."))?;
    let id = format!("import-{}", hex(&random));
    let parent = layout.root.join("imports");
    let dest = parent.join(&id);
    if dest.try_exists()? || registry.iter().any(|m| m.id == id) { return Err(invalid("Model ID collision; retry the import.")); }
    let staging = tempfile::Builder::new().prefix(".import-").tempdir_in(&parent)?;
    let mut pending = OpenOptions::new().write(true).create_new(true).open(staging.path().join("pending.json"))?;
    pending.write_all(&serde_json::to_vec(&PendingImport { version: 1, id: id.clone(), variant: request.variant })?)?;
    pending.sync_all()?;
    drop(pending);
    let mut record = ImportedModel { id, name: request.name.clone(), variant: request.variant, languages: request.languages.clone(), files: Vec::new() };
    let mut done = 0;
    let mut buffer = vec![0u8; 256 * 1024];
    for (name, mut source, size) in sources {
        let target = staging.path().join(name);
        let mut out = OpenOptions::new().create_new(true).write(true).open(&target)?;
        let mut hash = Sha256::new();
        let mut copied = 0;
        loop {
            if cancel.load(Ordering::Relaxed) { return Err(ImportError::Cancelled); }
            let n = source.read(&mut buffer)?;
            if n == 0 { break; }
            copied += n as u64;
            if copied > size { return Err(invalid("The source model changed during import.")); }
            out.write_all(&buffer[..n])?;
            hash.update(&buffer[..n]);
            done += n as u64;
            progress(done, total);
        }
        if copied != size || source.metadata()?.len() != size { return Err(invalid("The source model changed during import.")); }
        out.sync_all()?;
        drop(out);
        validate_native(&target, request.variant, &request.languages)?;
        record.files.push(ManifestFile { name: name.into(), size, sha256: hex(&hash.finalize()) });
    }
    validate_record(&record)?;
    if registry.iter().any(|m| m.variant == record.variant && m.files == record.files) { return Err(invalid("This model is already imported.")); }
    let mut manifest = OpenOptions::new().write(true).create_new(true).open(staging.path().join("manifest.json"))?;
    manifest.write_all(&serde_json::to_vec(&record)?)?;
    manifest.sync_all()?;
    drop(manifest);
    fs::remove_file(staging.path().join("pending.json"))?;
    if cancel.load(Ordering::Relaxed) { return Err(ImportError::Cancelled); }
    fs::rename(staging.path(), &dest)?;
    registry.push(record.clone());
    if let Err(error) = save_registry(layout, &registry) {
        let _ = fs::remove_dir_all(&dest);
        return Err(error);
    }
    Ok(record)
}

/// The caller holds its speech lifecycle lock and unloads this model first.
pub fn remove(layout: &Layout, id: &str) -> Result<(), ImportError> {
    remove_with(layout, id, |dir| fs::remove_dir_all(dir))
}

fn remove_with(layout: &Layout, id: &str, delete: impl FnOnce(&Path) -> std::io::Result<()>) -> Result<(), ImportError> {
    prepare_remove(layout, id)?.remove_with(delete)
}

/// Acquired before broad application locks; its OS reservation excludes a racing import.
pub struct Removal<'a> {
    _lock: File,
    layout: &'a Layout,
    id: String,
}

pub fn prepare_remove<'a>(layout: &'a Layout, id: &str) -> Result<Removal<'a>, ImportError> {
    if !is_import_id(id) { return Err(invalid("Unsafe imported model ID.")); }
    let lock = storage_lock(layout)?;
    Ok(Removal { _lock: lock, layout, id: id.into() })
}

impl Removal<'_> {
    pub fn id(&self) -> &str { &self.id }

    pub fn remove(self) -> Result<(), ImportError> { self.remove_with(|dir| fs::remove_dir_all(dir)) }

    fn remove_with(self, delete: impl FnOnce(&Path) -> std::io::Result<()>) -> Result<(), ImportError> {
        let mut registry = list(self.layout)?;
        let index = registry.iter().position(|m| m.id == self.id).ok_or_else(|| invalid("Unknown imported model."))?;
        let dir = self.layout.root.join("imports").join(&self.id);
        reject_links(&dir)?;
        if dir.exists() { check_directory(&dir, &registry[index])?; }
        // Retain the registry entry until deletion succeeds, so partial/failed deletion can
        // be retried. The manifest/length checks reject an interrupted deletion as incomplete.
        if dir.exists() { delete(&dir)?; }
        registry.remove(index);
        save_registry(self.layout, &registry)?;
        Ok(())
    }
}

fn hex(bytes: &[u8]) -> String { bytes.iter().map(|b| format!("{b:02x}")).collect() }

fn no_follow(options: &mut OpenOptions) {
    #[cfg(unix)] {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)] {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x00200000); // FILE_FLAG_OPEN_REPARSE_POINT
    }
}

fn ensure_regular(meta: &fs::Metadata) -> Result<(), ImportError> {
    if !meta.is_file() || is_link(meta) { return Err(invalid("Model files must be regular files, not links or devices.")); }
    Ok(())
}

fn is_link(meta: &fs::Metadata) -> bool {
    #[cfg(windows)] {
        use std::os::windows::fs::MetadataExt;
        if meta.file_attributes() & 0x400 != 0 { return true; } // Any reparse point, including junctions.
    }
    meta.file_type().is_symlink()
}

fn reject_links(path: &Path) -> Result<(), ImportError> {
    let mut prefix = PathBuf::new();
    for component in path.components() {
        if matches!(component, Component::ParentDir) { return Err(invalid("Parent-directory paths are not allowed for model imports.")); }
        prefix.push(component);
        match fs::symlink_metadata(&prefix) {
            Ok(meta) if is_link(&meta) => return Err(invalid("Symbolic links and junctions are not allowed for model imports.")),
            Ok(_) => {},
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {},
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

fn open_regular(path: &Path) -> Result<File, ImportError> {
    reject_links(path)?;
    let mut options = OpenOptions::new();
    options.read(true);
    no_follow(&mut options);
    let file = options.open(path)?;
    ensure_regular(&file.metadata()?)?;
    Ok(file)
}

fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>, ImportError> {
    let file = open_regular(path)?;
    if file.metadata()?.len() > limit { return Err(invalid("The model metadata exceeds its size limit.")); }
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit { return Err(invalid("The model metadata exceeds its size limit.")); }
    Ok(bytes)
}

fn validate_native(path: &Path, variant: Variant, languages: &[String]) -> Result<(), ImportError> {
    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
    if variant == Variant::Whisper {
        let mut file = open_regular(path)?;
        let mut header = [0u8; 48];
        file.read_exact(&mut header).map_err(|_| invalid("Truncated Whisper GGML model header."))?;
        let values: Vec<u32> = header.chunks_exact(4).map(|v| u32::from_le_bytes(v.try_into().unwrap())).collect();
        if values[0] != 0x67676d6c || !(50_000..=60_000).contains(&values[1]) || values[2] != 1500
            || !(128..=2048).contains(&values[3]) || !(1..=32).contains(&values[4]) || !(1..=64).contains(&values[5])
            || !(1..=2048).contains(&values[6]) || !(128..=2048).contains(&values[7])
            || !(1..=32).contains(&values[8]) || !(1..=64).contains(&values[9]) || ![80, 128].contains(&values[10])
            || file.metadata()?.len() < 1_000_000 {
            return Err(invalid("Not a supported Whisper GGML model. GGUF, Git LFS pointers and truncated files are not supported."));
        }
        if values[1] == 51_864 && languages != ["en"] { return Err(invalid("This English-only Whisper model must declare language en.")); }
    } else if name.ends_with(".onnx") {
        let file = open_regular(path)?;
        let end = file.metadata()?.len();
        let mut reader = BufReader::new(file);
        let mut fields = 0;
        protobuf(&mut reader, end, Proto::Model, 0, &mut fields)?;
    } else if name == "tokenizer.json" {
        let value: serde_json::Value = serde_json::from_slice(&read_bounded(path, MAX_TEXT_BYTES)?)?;
        if !value.get("model").and_then(|m| m.get("vocab")).is_some_and(|v|
            v.as_object().is_some_and(|v| !v.is_empty()) || v.as_array().is_some_and(|v| !v.is_empty())) {
            return Err(invalid("The Moonshine tokenizer must contain a model vocabulary."));
        }
    } else if name == "vocab.txt" {
        let bytes = read_bounded(path, MAX_TEXT_BYTES)?;
        let text = std::str::from_utf8(&bytes).map_err(|_| invalid("Parakeet vocabulary must be UTF-8 text."))?;
        if text.contains('\0') || text.lines().count() < 2 || text.lines().any(|l| l.len() > 4096) {
            return Err(invalid("Invalid Parakeet vocabulary."));
        }
    }
    Ok(())
}

#[derive(Clone, Copy, PartialEq)]
enum Proto { Model, Graph, Node, Attribute, Tensor, Sparse, Training, Function }

/// Walk only ONNX's message-bearing fields, skipping tensor payloads without allocating
/// them. All external tensors are refused, including those in subgraphs and functions.
/// This is structural screening, not a promise that native inference can load the graph.
fn protobuf<R: Read + Seek>(reader: &mut R, end: u64, kind: Proto, depth: usize, fields: &mut usize) -> Result<(), ImportError> {
    if depth > 64 { return Err(invalid("ONNX nesting exceeds the import limit.")); }
    let mut version = false;
    let mut graph = false;
    while reader.stream_position()? < end {
        *fields += 1;
        if *fields > 2_000_000 { return Err(invalid("ONNX metadata exceeds the import limit.")); }
        let tag = varint(reader, end)?;
        let field = tag >> 3;
        let wire = tag & 7;
        if field == 0 || field > 0x1fff_ffff { return Err(invalid("Invalid ONNX protobuf field.")); }
        if kind == Proto::Tensor && field == 13 { return Err(invalid("ONNX external tensor data is not supported. Use self-contained ONNX files from a trusted source.")); }
        let child = match (kind, field) {
            (Proto::Model, 7) | (Proto::Attribute, 6 | 11) | (Proto::Training, 1 | 2) => Some(Proto::Graph),
            (Proto::Model, 20) => Some(Proto::Training), (Proto::Model, 25) => Some(Proto::Function),
            (Proto::Graph, 1) | (Proto::Function, 7) => Some(Proto::Node),
            (Proto::Graph, 5) | (Proto::Attribute, 5 | 10) | (Proto::Sparse, 1 | 2) => Some(Proto::Tensor),
            (Proto::Graph, 15) | (Proto::Attribute, 22 | 23) => Some(Proto::Sparse),
            (Proto::Node, 5) | (Proto::Function, 11) => Some(Proto::Attribute),
            _ => None,
        };
        if child.is_some() && wire != 2 { return Err(invalid("Invalid ONNX message encoding.")); }
        if (kind == Proto::Model && field == 1 || kind == Proto::Tensor && field == 14) && wire != 0 {
            return Err(invalid("Invalid ONNX version or data-location encoding."));
        }
        match wire {
            0 => {
                let value = varint(reader, end)?;
                if kind == Proto::Model && field == 1 {
                    if version || !(1..=13).contains(&value) { return Err(invalid("Unsupported or duplicate ONNX IR version.")); }
                    version = true;
                }
                if kind == Proto::Tensor && field == 14 && value != 0 { return Err(invalid("ONNX external tensor data is not supported.")); }
            },
            1 | 2 | 5 => {
                let length = match wire { 1 => 8, 5 => 4, _ => varint(reader, end)? };
                let next = reader.stream_position()?.checked_add(length).filter(|n| *n <= end).ok_or_else(|| invalid("Truncated ONNX protobuf data."))?;
                if kind == Proto::Model && field == 7 {
                    if graph || length == 0 { return Err(invalid("Missing or duplicate ONNX graph.")); }
                    graph = true;
                }
                if let Some(child) = child { protobuf(reader, next, child, depth + 1, fields)?; }
                else { reader.seek(SeekFrom::Start(next))?; }
            },
            _ => return Err(invalid("Unsupported ONNX protobuf encoding.")),
        }
    }
    if kind == Proto::Model && (!version || !graph) { return Err(invalid("Not an ONNX model with an IR version and graph.")); }
    Ok(())
}

fn varint<R: Read + Seek>(reader: &mut R, end: u64) -> Result<u64, ImportError> {
    let mut value = 0;
    for shift in (0..70).step_by(7) {
        if reader.stream_position()? >= end { return Err(invalid("Truncated ONNX protobuf integer.")); }
        let mut byte = [0];
        reader.read_exact(&mut byte)?;
        if shift == 63 && byte[0] > 1 { return Err(invalid("Oversized ONNX protobuf integer.")); }
        value |= ((byte[0] & 127) as u64) << shift;
        if byte[0] & 128 == 0 { return Ok(value); }
    }
    Err(invalid("Invalid ONNX protobuf integer."))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn whisper(path: &Path) {
        let mut file = File::create(path).unwrap();
        for value in [0x67676d6cu32, 51865, 1500, 384, 6, 4, 448, 384, 6, 4, 80, 1] {
            file.write_all(&value.to_le_bytes()).unwrap();
        }
        file.set_len(1_000_001).unwrap();
    }

    fn fixture() -> (tempfile::TempDir, Layout, ImportRequest) {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source.bin");
        whisper(&source);
        let layout = Layout { root: temp.path().join("managed") };
        let request = ImportRequest { source_path: source.to_string_lossy().into_owned(), name: "My Whisper".into(), variant: Variant::Whisper, languages: vec![] };
        (temp, layout, request)
    }

    fn run(layout: &Layout, request: &ImportRequest) -> Result<ImportedModel, ImportError> {
        import(layout, request, &AtomicBool::new(false), &mut |_, _| {})
    }

    fn save_raw(layout: &Layout, value: serde_json::Value) {
        fs::create_dir_all(layout.root.join("imports")).unwrap();
        fs::write(layout.root.join("imports/registry.json"), serde_json::to_vec(&value).unwrap()).unwrap();
    }

    #[test]
    fn import_roundtrip_is_copy_only_private_and_survives_restart() {
        let (_temp, layout, request) = fixture();
        let source_hash = local::sha256_file(Path::new(&request.source_path)).unwrap();
        assert!(list(&layout).unwrap().is_empty());
        let record = run(&layout, &request).unwrap();
        assert!(is_import_id(&record.id));
        assert_eq!(record.files[0].sha256, source_hash);
        assert_eq!(list(&layout).unwrap(), vec![record.clone()]);
        let fresh = Layout { root: layout.root.clone() };
        let model = resolve(&fresh, &record.id).unwrap();
        assert!(model.imported && !model.is_onnx() && !model.short_context);
        assert!(installed(&fresh, &model));
        verify(&fresh, &model).unwrap();
        let saved = fs::read_to_string(layout.root.join("imports/registry.json")).unwrap();
        assert!(!saved.contains(&request.source_path) && !saved.contains("sourcePath"));
        assert!(model.path.ends_with(format!("{}/model.bin", record.id)));
        remove(&layout, &record.id).unwrap();
        assert!(list(&layout).unwrap().is_empty());
        assert!(!model.path.exists());
        assert!(resolve(&layout, &record.id).is_err());
        assert_eq!(local::sha256_file(Path::new(&request.source_path)).unwrap(), source_hash);
    }

    #[test]
    fn source_and_registry_corruption_fail_closed() {
        let (_temp, layout, request) = fixture();
        let record = run(&layout, &request).unwrap();
        assert!(run(&layout, &request).unwrap_err().to_string().contains("already imported"));
        let model = resolve(&layout, &record.id).unwrap();
        let mut file = OpenOptions::new().write(true).open(&model.path).unwrap();
        file.seek(SeekFrom::Start(100)).unwrap();
        file.write_all(b"modified").unwrap();
        drop(file);
        assert!(installed(&layout, &model), "status is a cheap length check");
        assert!(verify(&layout, &model).is_err());
        fs::remove_file(&model.path).unwrap();
        assert!(!installed(&layout, &model));
        assert!(verify(&layout, &model).is_err());
        remove(&layout, &record.id).unwrap();
        assert!(list(&layout).unwrap().is_empty(), "broken imports remain removable");
    }

    #[test]
    fn failed_or_interrupted_deletion_retains_a_retryable_registry_entry() {
        let (_temp, layout, request) = fixture();
        let record = run(&layout, &request).unwrap();
        let model = resolve(&layout, &record.id).unwrap();
        let denied = || std::io::Error::new(std::io::ErrorKind::PermissionDenied, "fixture deletion failure");
        assert!(remove_with(&layout, &record.id, |_| Err(denied())).is_err());
        assert_eq!(list(&layout).unwrap(), [record.clone()]);
        verify(&layout, &model).unwrap();
        assert!(remove_with(&layout, &record.id, |dir| {
            fs::remove_file(dir.join("model.bin"))?;
            Err(denied())
        }).is_err());
        assert_eq!(list(&layout).unwrap(), [record.clone()]);
        assert!(!installed(&layout, &model));
        assert!(verify(&layout, &model).is_err());
        remove(&layout, &record.id).unwrap();
        assert!(list(&layout).unwrap().is_empty());
        assert!(Path::new(&request.source_path).exists());
    }

    #[test]
    fn next_import_recovers_interrupted_copy_and_publish_without_touching_other_files() {
        let (_temp, layout, request) = fixture();
        let installed_record = run(&layout, &request).unwrap();
        let parent = layout.root.join("imports");
        let mut abandoned = Vec::new();
        for marker in [false, true] {
            let stage = tempfile::Builder::new().prefix(".import-").tempdir_in(&parent).unwrap().keep();
            fs::copy(&request.source_path, stage.join("model.bin")).unwrap();
            OpenOptions::new().write(true).open(stage.join("model.bin")).unwrap().set_len(512).unwrap();
            if marker {
                let pending = PendingImport { version: 1, id: "import-11111111111111111111111111111111".into(), variant: Variant::Whisper };
                fs::write(stage.join("pending.json"), serde_json::to_vec(&pending).unwrap()).unwrap();
            }
            abandoned.push(stage);
        }
        let mut orphan = installed_record.clone();
        orphan.id = "import-22222222222222222222222222222222".into();
        let unpublished = parent.join(&orphan.id);
        fs::create_dir(&unpublished).unwrap();
        fs::copy(&request.source_path, unpublished.join("model.bin")).unwrap();
        fs::write(unpublished.join("manifest.json"), serde_json::to_vec(&orphan).unwrap()).unwrap();
        abandoned.push(unpublished);
        let unrelated = parent.join("my-model-backup");
        fs::create_dir(&unrelated).unwrap();
        fs::copy(&request.source_path, unrelated.join("model.bin")).unwrap();
        fs::write(parent.join("notes.txt"), b"keep this unrelated file").unwrap();
        let source_hash = local::sha256_file(Path::new(&request.source_path)).unwrap();

        assert!(run(&layout, &request).unwrap_err().to_string().contains("already imported"));
        for dir in abandoned { assert!(!dir.exists(), "abandoned copy must be reclaimed before retry"); }
        assert_eq!(list(&layout).unwrap(), [installed_record.clone()]);
        verify(&layout, &resolve(&layout, &installed_record.id).unwrap()).unwrap();
        assert_eq!(local::sha256_file(Path::new(&request.source_path)).unwrap(), source_hash);
        assert_eq!(local::sha256_file(&unrelated.join("model.bin")).unwrap(), source_hash);
        assert_eq!(fs::read(parent.join("notes.txt")).unwrap(), b"keep this unrelated file");
    }

    #[cfg(unix)]
    #[test]
    fn recovery_permission_failure_is_visible_and_retryable() {
        use std::os::unix::fs::PermissionsExt;
        if unsafe { libc::geteuid() } == 0 { return; } // root intentionally bypasses directory permissions.
        let (_temp, layout, request) = fixture();
        fs::create_dir_all(layout.root.join("imports")).unwrap();
        let stage = tempfile::Builder::new().prefix(".import-").tempdir_in(layout.root.join("imports")).unwrap().keep();
        fs::copy(&request.source_path, stage.join("model.bin")).unwrap();
        fs::set_permissions(&stage, fs::Permissions::from_mode(0o500)).unwrap();
        let result = run(&layout, &request);
        fs::set_permissions(&stage, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(result.unwrap_err().to_string().contains("Could not recover interrupted model import"));
        assert!(stage.join("model.bin").exists());
        assert!(list(&layout).unwrap().is_empty());
        run(&layout, &request).unwrap();
        assert!(!stage.exists());
        assert!(Path::new(&request.source_path).exists());
    }

    #[test]
    fn import_and_deletion_reservations_reject_races_without_waiting() {
        use std::sync::mpsc;
        use std::time::Duration;
        let (_temp, layout, request) = fixture();
        let record = run(&layout, &request).unwrap();
        let (copy_started, copying) = mpsc::channel();
        let (finish_copy, wait) = mpsc::channel();
        let worker_layout = layout.clone();
        let worker_request = request.clone();
        let worker = std::thread::spawn(move || {
            let mut first = true;
            import(&worker_layout, &worker_request, &AtomicBool::new(false), &mut |_, _| {
                if first {
                    first = false;
                    copy_started.send(()).unwrap();
                    wait.recv().unwrap();
                }
            })
        });
        copying.recv_timeout(Duration::from_secs(5)).unwrap();
        let (attempted, attempt) = mpsc::channel();
        let deletion_layout = layout.clone();
        let id = record.id.clone();
        let deletion = std::thread::spawn(move || {
            // The desktop performs this exact reservation before when_idle/lifecycle.
            let result = prepare_remove(&deletion_layout, &id);
            attempted.send(matches!(result, Err(ImportError::Busy))).unwrap();
        });
        let refused_immediately = attempt.recv_timeout(Duration::from_secs(2));
        finish_copy.send(()).unwrap();
        deletion.join().unwrap();
        assert!(refused_immediately.unwrap(), "deletion must not wait for the copy under broad locks");
        assert!(worker.join().unwrap().unwrap_err().to_string().contains("already imported"));

        let removal = prepare_remove(&layout, &record.id).unwrap();
        let result = import(&layout, &request, &AtomicBool::new(false), &mut |_, _| panic!("a reserved deletion excludes new copies"));
        assert!(matches!(result, Err(ImportError::Busy)));
        removal.remove().unwrap();
        assert!(list(&layout).unwrap().is_empty());
        run(&layout, &request).unwrap();
    }

    #[test]
    fn invalid_ids_filenames_duplicates_versions_and_limits_are_rejected() {
        let (_temp, layout, request) = fixture();
        let record = run(&layout, &request).unwrap();
        for id in ["../outside", "import-../../secret", "import-ABCDEF01234567890123456789012345", "import-123", "C:\\model"] {
            assert!(!is_import_id(id));
            assert!(remove(&layout, id).is_err());
        }
        let original = serde_json::to_value(&record).unwrap();
        for value in [
            serde_json::json!({"version":2,"models":[original.clone()]}),
            serde_json::json!({"version":1,"models":[original.clone(), original.clone()]}),
            serde_json::json!({"version":1,"models":vec![original.clone();MAX_MODELS+1]}),
        ] {
            save_raw(&layout, value);
            assert!(list(&layout).is_err());
        }
        for bad_name in ["../secret", "C:\\secret", "/absolute", "model.bin:secret", "MODEL.BIN", "a/b"] {
            let mut bad = original.clone();
            bad["files"][0]["name"] = bad_name.into();
            save_raw(&layout, serde_json::json!({"version":1,"models":[bad]}));
            assert!(list(&layout).is_err());
        }
        let mut bad = original.clone();
        bad["files"][0]["size"] = (MAX_FILE_BYTES + 1).into();
        save_raw(&layout, serde_json::json!({"version":1,"models":[bad]}));
        assert!(list(&layout).is_err());
        fs::write(layout.root.join("imports/registry.json"), vec![b' '; MAX_REGISTRY_BYTES as usize + 1]).unwrap();
        assert!(list(&layout).is_err());
        fs::write(layout.root.join("imports/registry.json"), br#"{"version":1,"version":1,"models":[]}"#).unwrap();
        assert!(list(&layout).is_err());
    }

    #[test]
    fn cancellation_and_registry_failure_never_publish_partial_imports() {
        let (_temp, layout, request) = fixture();
        let cancel = AtomicBool::new(false);
        let result = import(&layout, &request, &cancel, &mut |_, _| {
            assert!(list(&layout).unwrap().is_empty());
            cancel.store(true, Ordering::Relaxed);
        });
        assert!(matches!(result, Err(ImportError::Cancelled)));
        assert!(list(&layout).unwrap().is_empty());
        assert!(!fs::read_dir(layout.root.join("imports")).unwrap().any(|e| e.unwrap().file_type().unwrap().is_dir()));
        let result = import(&layout, &request, &AtomicBool::new(false), &mut |_, _| {
            let _ = fs::create_dir(layout.root.join("imports/registry.json"));
        });
        assert!(result.is_err());
        assert_eq!(fs::read_dir(layout.root.join("imports")).unwrap().filter(|e| e.as_ref().unwrap().file_type().unwrap().is_dir()).count(), 1);
        assert!(Path::new(&request.source_path).exists());
    }

    #[test]
    fn whisper_screening_rejects_wrong_formats_headers_and_language_claims() {
        let (_temp, layout, mut request) = fixture();
        let source = PathBuf::from(&request.source_path);
        for bytes in [b"GGUF pretend model".as_slice(), b"version https://git-lfs.github.com/spec/v1", b"lmgg"] {
            fs::write(&source, bytes).unwrap();
            assert!(run(&layout, &request).is_err());
        }
        whisper(&source);
        let mut file = OpenOptions::new().write(true).open(&source).unwrap();
        file.seek(SeekFrom::Start(4)).unwrap();
        file.write_all(&51_864u32.to_le_bytes()).unwrap();
        drop(file);
        assert!(run(&layout, &request).unwrap_err().to_string().contains("English-only"));
        request.languages = vec!["en".into()];
        run(&layout, &request).unwrap();
        for languages in [vec!["en".into(), "en".into()], vec!["../secret".into()], vec!["EN".into()]] {
            request.languages = languages;
            assert!(run(&layout, &request).is_err());
        }
    }

    fn message(field: u64, value: &[u8]) -> Vec<u8> {
        fn integer(mut value: u64) -> Vec<u8> {
            let mut out = Vec::new();
            loop {
                let byte = (value & 127) as u8;
                value >>= 7;
                out.push(byte | if value == 0 { 0 } else { 128 });
                if value == 0 { break; }
            }
            out
        }
        [integer(field * 8 + 2), integer(value.len() as u64), value.to_vec()].concat()
    }

    fn graph_model(graph: &[u8]) -> Vec<u8> { [vec![8, 8], message(7, graph)].concat() }

    #[test]
    fn onnx_whitelist_roundtrip_and_unsupported_external_tensors() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("moonshine");
        fs::create_dir(&source).unwrap();
        let layout = Layout { root: temp.path().join("managed") };
        let request = ImportRequest { source_path: source.to_string_lossy().into_owned(), name: "My Moonshine".into(), variant: Variant::MoonshineTiny, languages: vec!["en".into()] };
        let native = graph_model(&message(2, b"graph"));
        fs::write(source.join("encoder_model.onnx"), &native).unwrap();
        fs::write(source.join("decoder_model_merged.onnx"), &native).unwrap();
        fs::write(source.join("tokenizer.json"), br#"{"model":{"vocab":{"word":0}}}"#).unwrap();
        fs::write(source.join("run.sh"), b"must never copy or run").unwrap();
        let record = run(&layout, &request).unwrap();
        let model = resolve(&layout, &record.id).unwrap();
        assert!(model.is_onnx() && !model.supports_prompt());
        assert_eq!(model.languages, ["en"]);
        verify(&layout, &model).unwrap();
        assert!(!model.path.join("run.sh").exists());
        assert!(!onnx::runtime_installed(&layout), "import never provisions a runtime");
        remove(&layout, &record.id).unwrap();
        let external = message(13, &message(1, b"../../private-file"));
        let direct = graph_model(&message(5, &external));
        let nested = graph_model(&message(1, &message(5, &message(6, &message(5, &external)))));
        let sparse = graph_model(&message(15, &message(1, &external)));
        let function = [native.clone(), message(25, &message(11, &message(5, &external)))].concat();
        for bytes in [direct, nested, sparse, function, graph_model(&message(5, &[112, 1]))] {
            fs::write(source.join("encoder_model.onnx"), bytes).unwrap();
            assert!(run(&layout, &request).unwrap_err().to_string().contains("external tensor"));
            assert!(list(&layout).unwrap().is_empty());
        }
        for bytes in [b"not onnx".to_vec(), vec![8, 8, 58, 127], vec![8, 8], [native.clone(), native].concat()] {
            fs::write(source.join("encoder_model.onnx"), bytes).unwrap();
            assert!(run(&layout, &request).is_err());
        }
    }

    #[cfg(unix)]
    #[test]
    fn symlink_sources_storage_parents_and_manifests_are_rejected() {
        use std::os::unix::fs::symlink;
        let (temp, layout, mut request) = fixture();
        let original = PathBuf::from(&request.source_path);
        let link = temp.path().join("link.bin");
        symlink(&original, &link).unwrap();
        request.source_path = link.to_string_lossy().into_owned();
        assert!(run(&layout, &request).is_err());
        request.source_path = original.to_string_lossy().into_owned();
        fs::create_dir_all(&layout.root).unwrap();
        let external = temp.path().join("external");
        fs::create_dir(&external).unwrap();
        symlink(&external, layout.root.join("imports")).unwrap();
        assert!(run(&layout, &request).is_err());
        assert!(fs::read_dir(&external).unwrap().next().is_none());
        fs::remove_file(layout.root.join("imports")).unwrap();
        let record = run(&layout, &request).unwrap();
        let model = resolve(&layout, &record.id).unwrap();
        fs::remove_file(&model.path).unwrap();
        symlink(&original, &model.path).unwrap();
        assert!(!installed(&layout, &model));
        assert!(verify(&layout, &model).is_err());
        assert!(remove(&layout, &record.id).is_err());
        assert!(original.exists());
    }

    #[test]
    fn concurrent_imports_do_not_lose_registry_entries() {
        let (_temp, layout, request) = fixture();
        let first_layout = layout.clone();
        let first_request = request.clone();
        let first = std::thread::spawn(move || run(&first_layout, &first_request));
        let second = std::thread::spawn(move || run(&layout, &request));
        let results = [first.join().unwrap(), second.join().unwrap()];
        assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1, "duplicate bytes are only installed once");
        let error = results.iter().find_map(|r| r.as_ref().err()).unwrap();
        assert!(matches!(error, ImportError::Busy) || error.to_string().contains("already imported"));
    }
}
