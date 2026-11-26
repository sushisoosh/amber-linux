//! Offline, copy-only cleanup GGUF imports. Structural checks and hashes are not a
//! trust guarantee: the user must trust the source before a native runtime loads it.

use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::imports::{ensure_regular, hex, no_follow, open_regular, read_bounded, reject_links, ImportError};
use crate::local::{self, Layout};

const DIRECTORY: &str = "cleanup-imports";
const MAX_MODELS: usize = 64;
const MAX_REGISTRY_BYTES: u64 = 1_048_576;
const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024 * 1024;
const MAX_METADATA_BYTES: u64 = 128 * 1024 * 1024;

fn invalid(message: impl Into<String>) -> ImportError { ImportError::Invalid(message.into()) }

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImportRequest {
    pub source_path: String,
    pub name: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImportedModel {
    pub id: String,
    pub name: String,
    pub size: u64,
    pub sha256: String,
    pub architecture: String,
}

impl ImportedModel {
    pub fn size_mb(&self) -> u32 { self.size.div_ceil(1_048_576) as u32 }
    pub fn note(&self) -> String {
        format!("Imported {} GGUF; embedded chat template. Integrity-checked, not publisher-verified. Speed, memory use and languages depend on the model.", self.architecture)
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Registry { version: u32, models: Vec<ImportedModel> }

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Pending { version: u32, id: String }

/// Owned metadata for the catalog and imported models; paths never point to the source.
#[derive(Clone, Debug)]
pub struct CleanupModel {
    pub id: String,
    pub label: String,
    pub path: PathBuf,
    manifest: Option<ImportedModel>,
}

pub fn is_import_id(id: &str) -> bool {
    id.strip_prefix("cleanup-import-").is_some_and(|s| s.len() == 32 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
}

fn validate_name(name: &str) -> Result<(), ImportError> {
    if name.is_empty() || name.trim() != name || name.chars().count() > 80 || name.chars().any(char::is_control) {
        return Err(invalid("Use a model name of 1 to 80 characters without control characters."));
    }
    Ok(())
}

fn validate_record(model: &ImportedModel) -> Result<(), ImportError> {
    validate_name(&model.name)?;
    if !is_import_id(&model.id) || model.size == 0 || model.size > MAX_FILE_BYTES
        || model.sha256.len() != 64 || !model.sha256.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        || model.architecture.is_empty() || model.architecture.len() > 64
        || !model.architecture.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_') {
        return Err(invalid("Invalid imported cleanup model manifest."));
    }
    Ok(())
}

pub fn list(layout: &Layout) -> Result<Vec<ImportedModel>, ImportError> {
    let path = layout.root.join(DIRECTORY).join("registry.json");
    reject_links(&path)?;
    if !path.try_exists()? { return Ok(Vec::new()); }
    let registry: Registry = serde_json::from_slice(&read_bounded(&path, MAX_REGISTRY_BYTES)?)?;
    if registry.version != 1 || registry.models.len() > MAX_MODELS { return Err(invalid("Unsupported or oversized cleanup import registry.")); }
    let mut ids = HashSet::new();
    for record in &registry.models {
        validate_record(record)?;
        if !ids.insert(&record.id) { return Err(invalid("Duplicate cleanup import ID.")); }
    }
    Ok(registry.models)
}

pub fn resolve(layout: &Layout, id: &str) -> Result<CleanupModel, ImportError> {
    if let Some(model) = local::llm_model(id) {
        return Ok(CleanupModel { id: id.into(), label: model.label.into(), path: layout.llm_model_path(model), manifest: None });
    }
    if !is_import_id(id) { return Err(invalid("Unknown cleanup model. Select a model under Cleanup.")); }
    let record = list(layout)?.into_iter().find(|m| m.id == id)
        .ok_or_else(|| invalid("This imported cleanup model is missing. Import it again or select another model under Cleanup."))?;
    Ok(CleanupModel { id: id.into(), label: record.name.clone(), path: layout.root.join(DIRECTORY).join(id).join("model.gguf"), manifest: Some(record) })
}

/// Cheap status checks; every cold native load additionally verifies the hash and GGUF.
pub fn installed(layout: &Layout, model: &CleanupModel) -> bool { available(layout, model).is_ok() }

pub fn available(layout: &Layout, model: &CleanupModel) -> Result<(), ImportError> {
    if let Some(record) = &model.manifest {
        check_install(layout, record, false).map_err(|e| invalid(format!("The imported cleanup model is unavailable: {e} Import it again or select another model under Cleanup.")))
    } else if local::llm_model(&model.id).is_some_and(|m| layout.llm_model_installed(m)) { Ok(()) }
    else { Err(invalid(format!("The cleanup model {} is not installed. Download it under Cleanup.", model.label))) }
}

pub fn verify(layout: &Layout, model: &CleanupModel) -> Result<(), ImportError> {
    if let Some(record) = &model.manifest {
        if !list(layout)?.contains(record) { return Err(invalid("The imported cleanup model was removed or changed. Select it again under Cleanup.")); }
        check_install(layout, record, true)
    } else { available(layout, model) }
}

fn check_directory(dir: &Path) -> Result<(), ImportError> {
    reject_links(dir)?;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if !["model.gguf", "manifest.json"].iter().any(|name| entry.file_name() == *name) {
            return Err(invalid("Unexpected file in imported cleanup model directory; nothing was removed."));
        }
        open_regular(&entry.path())?;
    }
    Ok(())
}

fn check_install(layout: &Layout, record: &ImportedModel, hash: bool) -> Result<(), ImportError> {
    validate_record(record)?;
    let dir = layout.root.join(DIRECTORY).join(&record.id);
    check_directory(&dir)?;
    let stored: ImportedModel = serde_json::from_slice(&read_bounded(&dir.join("manifest.json"), MAX_REGISTRY_BYTES)?)?;
    if &stored != record { return Err(invalid("The imported cleanup model manifest has changed.")); }
    let mut file = open_regular(&dir.join("model.gguf"))?;
    if file.metadata()?.len() != record.size { return Err(invalid("The imported cleanup model is incomplete or damaged.")); }
    if hash {
        let mut digest = Sha256::new();
        let copied = std::io::copy(&mut (&mut file).take(record.size + 1), &mut digest)?;
        if copied != record.size || hex(&digest.finalize()) != record.sha256 {
            return Err(invalid("The imported cleanup model checksum has changed. Import it again from a trusted source."));
        }
        file.rewind()?;
        if validate_gguf(&mut file, &AtomicBool::new(false))? != record.architecture {
            return Err(invalid("The imported cleanup model architecture has changed."));
        }
    }
    Ok(())
}

fn storage_lock(layout: &Layout) -> Result<File, ImportError> {
    let dir = layout.root.join(DIRECTORY);
    reject_links(&dir)?;
    fs::create_dir_all(&dir)?;
    let path = dir.join("registry.lock");
    reject_links(&path)?;
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    no_follow(&mut options);
    let lock = options.open(path)?;
    ensure_regular(&lock.metadata()?)?;
    lock.try_lock().map_err(|e| match e {
        std::fs::TryLockError::WouldBlock => ImportError::Busy,
        std::fs::TryLockError::Error(e) => ImportError::Io(e),
    })?;
    Ok(lock)
}

fn save_registry(layout: &Layout, models: &[ImportedModel]) -> Result<(), ImportError> {
    let bytes = serde_json::to_vec(&Registry { version: 1, models: models.to_vec() })?;
    if bytes.len() as u64 > MAX_REGISTRY_BYTES { return Err(invalid("Cleanup import registry is full.")); }
    let dir = layout.root.join(DIRECTORY);
    reject_links(&dir.join("registry.json"))?;
    let mut file = tempfile::NamedTempFile::new_in(&dir)?;
    file.write_all(&bytes)?;
    file.as_file().sync_all()?;
    file.persist(dir.join("registry.json")).map_err(|e| e.error)?;
    Ok(())
}

// Never recurse and never delete unknown files. Keep ownership metadata until data is gone.
fn delete_managed(dir: &Path) -> Result<(), ImportError> {
    for name in ["model.gguf", "manifest.json", "pending.json"] {
        let path = dir.join(name);
        reject_links(&path)?;
        if path.try_exists()? { open_regular(&path)?; fs::remove_file(path)?; }
    }
    fs::remove_dir(dir)?;
    Ok(())
}

fn recover_interrupted(layout: &Layout, registry: &[ImportedModel]) -> Result<(), ImportError> {
    for entry in fs::read_dir(layout.root.join(DIRECTORY))? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let staging = name.strip_prefix(".import-").is_some_and(|s| s.len() == 6 && s.bytes().all(|b| b.is_ascii_alphanumeric()));
        let orphan = is_import_id(name) && !registry.iter().any(|m| m.id == name);
        if !staging && !orphan { continue; }
        let dir = entry.path();
        reject_links(&dir)?;
        if !entry.file_type()?.is_dir() { continue; }
        if staging {
            if !dir.join("pending.json").try_exists()? { continue; }
            let marker: Pending = serde_json::from_slice(&read_bounded(&dir.join("pending.json"), MAX_REGISTRY_BYTES)?)?;
            if marker.version != 1 || !is_import_id(&marker.id) { return Err(invalid("Invalid interrupted cleanup import marker.")); }
            for file in fs::read_dir(&dir)? {
                let file = file?;
                if !["pending.json", "manifest.json", "model.gguf"].iter().any(|n| file.file_name() == *n) {
                    return Err(invalid("Unexpected file in interrupted cleanup import; nothing was removed."));
                }
                open_regular(&file.path())?;
            }
        } else {
            if !dir.join("manifest.json").try_exists()? { continue; }
            let record: ImportedModel = serde_json::from_slice(&read_bounded(&dir.join("manifest.json"), MAX_REGISTRY_BYTES)?)?;
            validate_record(&record)?;
            if record.id != name { return Err(invalid("Interrupted cleanup import manifest does not match its directory.")); }
            for file in fs::read_dir(&dir)? {
                let file = file?;
                if !["pending.json", "manifest.json", "model.gguf"].iter().any(|n| file.file_name() == *n) {
                    return Err(invalid("Unexpected file in interrupted cleanup import; nothing was removed."));
                }
                open_regular(&file.path())?;
            }
        }
        delete_managed(&dir)?;
    }
    Ok(())
}

pub fn import(layout: &Layout, request: &ImportRequest, cancel: &AtomicBool, progress: &mut dyn FnMut(u64, u64)) -> Result<ImportedModel, ImportError> {
    if cancel.load(Ordering::Relaxed) { return Err(ImportError::Cancelled); }
    validate_name(&request.name)?;
    let source_path = Path::new(&request.source_path);
    if !source_path.is_absolute() || !source_path.extension().is_some_and(|s| s.eq_ignore_ascii_case("gguf")) {
        return Err(invalid("Choose an absolute path to a single local .gguf file."));
    }
    let mut source = open_regular(source_path)?;
    let size = source.metadata()?.len();
    if size == 0 || size > MAX_FILE_BYTES { return Err(invalid("Cleanup models must be nonempty and at most 64 GiB.")); }
    let _lock = storage_lock(layout)?;
    let parent = layout.root.join(DIRECTORY);
    if source_path.canonicalize()?.starts_with(parent.canonicalize()?) {
        return Err(invalid("Choose the original GGUF outside Amber's managed cleanup imports folder."));
    }
    let mut registry = list(layout)?;
    recover_interrupted(layout, &registry)?;
    if registry.len() >= MAX_MODELS { return Err(invalid("Cleanup import registry is full (64 models). Remove an import first.")); }
    let architecture = validate_gguf(&mut source, cancel)?;
    source.rewind()?;
    let mut random = [0u8; 16];
    getrandom::fill(&mut random).map_err(|_| invalid("Could not generate a secure cleanup model ID."))?;
    let id = format!("cleanup-import-{}", hex(&random));
    let dest = parent.join(&id);
    if dest.try_exists()? || registry.iter().any(|m| m.id == id) { return Err(invalid("Model ID collision; retry the import.")); }
    let staging = tempfile::Builder::new().prefix(".import-").tempdir_in(&parent)?;
    let mut marker = OpenOptions::new().write(true).create_new(true).open(staging.path().join("pending.json"))?;
    marker.write_all(&serde_json::to_vec(&Pending { version: 1, id: id.clone() })?)?;
    marker.sync_all()?;
    drop(marker);
    let target = staging.path().join("model.gguf");
    let mut out = OpenOptions::new().create_new(true).write(true).open(&target)?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0u8; 256 * 1024];
    let mut done = 0;
    progress(0, size);
    loop {
        if cancel.load(Ordering::Relaxed) { return Err(ImportError::Cancelled); }
        let n = source.read(&mut buffer)?;
        if n == 0 { break; }
        done += n as u64;
        if done > size { return Err(invalid("The source GGUF changed during import.")); }
        out.write_all(&buffer[..n])?;
        digest.update(&buffer[..n]);
        progress(done, size);
    }
    if done != size || source.metadata()?.len() != size { return Err(invalid("The source GGUF changed during import.")); }
    out.sync_all()?;
    drop(out);
    if validate_gguf(&mut open_regular(&target)?, cancel)? != architecture { return Err(invalid("The source GGUF changed during import.")); }
    let record = ImportedModel { id, name: request.name.clone(), size, sha256: hex(&digest.finalize()), architecture };
    validate_record(&record)?;
    if registry.iter().any(|m| m.size == size && m.sha256 == record.sha256) {
        return Err(invalid("This cleanup model is already imported. Remove its entry under Cleanup first to replace a missing or damaged managed copy; your original GGUF is kept."));
    }
    let mut manifest = OpenOptions::new().write(true).create_new(true).open(staging.path().join("manifest.json"))?;
    manifest.write_all(&serde_json::to_vec(&record)?)?;
    manifest.sync_all()?;
    drop(manifest);
    if cancel.load(Ordering::Relaxed) { return Err(ImportError::Cancelled); }
    // Keeping the pending marker until publication makes interrupted staging recoverable.
    fs::rename(staging.path(), &dest)?;
    fs::remove_file(dest.join("pending.json"))?;
    registry.push(record.clone());
    if let Err(error) = save_registry(layout, &registry) {
        let _ = delete_managed(&dest);
        return Err(error);
    }
    Ok(record)
}

/// Reserve before taking the desktop's dictation/model lifecycle locks.
pub struct Removal<'a> { _lock: File, layout: &'a Layout, id: String }

pub fn prepare_remove<'a>(layout: &'a Layout, id: &str) -> Result<Removal<'a>, ImportError> {
    if !is_import_id(id) { return Err(invalid("Unsafe imported cleanup model ID.")); }
    Ok(Removal { _lock: storage_lock(layout)?, layout, id: id.into() })
}

impl Removal<'_> {
    pub fn id(&self) -> &str { &self.id }

    pub fn remove(self) -> Result<(), ImportError> {
        let mut registry = list(self.layout)?;
        let index = registry.iter().position(|m| m.id == self.id).ok_or_else(|| invalid("Unknown imported cleanup model."))?;
        let dir = self.layout.root.join(DIRECTORY).join(&self.id);
        reject_links(&dir)?;
        if dir.try_exists()? { check_directory(&dir)?; delete_managed(&dir)?; }
        registry.remove(index);
        save_registry(self.layout, &registry)
    }
}

/// A bounded metadata reader. Tensor data is never allocated or interpreted during import.
struct Gguf<'a> { reader: BufReader<&'a mut File>, position: u64, limit: u64, values: u64, cancel: &'a AtomicBool }

impl Gguf<'_> {
    fn advance(&mut self, count: u64) -> Result<(), ImportError> {
        if self.cancel.load(Ordering::Relaxed) { return Err(ImportError::Cancelled); }
        self.position = self.position.checked_add(count).filter(|n| *n <= self.limit)
            .ok_or_else(|| invalid("Truncated or oversized GGUF metadata."))?;
        Ok(())
    }
    fn bytes<const N: usize>(&mut self) -> Result<[u8; N], ImportError> {
        self.advance(N as u64)?;
        let mut bytes = [0; N];
        self.reader.read_exact(&mut bytes)?;
        Ok(bytes)
    }
    fn u32(&mut self) -> Result<u32, ImportError> { Ok(u32::from_le_bytes(self.bytes()?)) }
    fn u64(&mut self) -> Result<u64, ImportError> { Ok(u64::from_le_bytes(self.bytes()?)) }
    fn skip(&mut self, length: u64) -> Result<(), ImportError> {
        self.advance(length)?;
        self.reader.seek_relative(length as i64)?;
        Ok(())
    }
    fn string(&mut self, max: u64) -> Result<String, ImportError> {
        let length = self.u64()?;
        if length > max { return Err(invalid("GGUF string exceeds the import limit.")); }
        self.advance(length)?;
        let mut bytes = vec![0; length as usize];
        self.reader.read_exact(&mut bytes)?;
        String::from_utf8(bytes).map_err(|_| invalid("GGUF metadata must be UTF-8."))
    }
    fn value(&mut self, kind: u32, array: bool) -> Result<(), ImportError> {
        self.values += 1;
        if self.values > 4_000_000 { return Err(invalid("GGUF metadata has too many values.")); }
        match kind {
            0 | 1 | 7 => self.skip(1),
            2 | 3 => self.skip(2),
            4..=6 => self.skip(4),
            10..=12 => self.skip(8),
            8 => { let length = self.u64()?; self.skip(length) },
            9 if !array => {
                let kind = self.u32()?;
                let count = self.u64()?;
                if kind > 12 || kind == 9 || count > 2_000_000 { return Err(invalid("Unsupported or oversized GGUF metadata array.")); }
                for _ in 0..count { self.value(kind, true)?; }
                Ok(())
            },
            _ => Err(invalid("Unsupported GGUF metadata type.")),
        }
    }
}

fn multimodal(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.contains("vision") || lower.contains("audio") || lower.contains("projector") || lower.contains("mmproj")
        || lower.starts_with("clip") || lower.contains("_vl") || lower.ends_with("vl") || lower == "mllama"
        || lower.contains("omni") || lower.starts_with("whisper") || lower.starts_with("wavtokenizer")
}

fn validate_gguf(file: &mut File, cancel: &AtomicBool) -> Result<String, ImportError> {
    let size = file.metadata()?.len();
    let mut g = Gguf { reader: BufReader::new(file), position: 0, limit: size.min(MAX_METADATA_BYTES), values: 0, cancel };
    if g.bytes::<4>()? != *b"GGUF" || !matches!(g.u32()?, 2 | 3) {
        return Err(invalid("Not a supported GGUF v2/v3 model. GGML, Git LFS pointers and other formats are not supported."));
    }
    let tensors = g.u64()?;
    let metadata = g.u64()?;
    if tensors == 0 || tensors > 200_000 || metadata > 65_536 { return Err(invalid("Invalid or oversized GGUF tensor/metadata count.")); }
    let mut keys = HashSet::new();
    let mut architecture = None;
    let mut template = false;
    let mut alignment = 32u64;
    for _ in 0..metadata {
        let key = g.string(1024)?;
        if key.is_empty() || !keys.insert(key.clone()) { return Err(invalid("Empty or duplicate GGUF metadata key.")); }
        if key.starts_with("split.") { return Err(invalid("Split GGUF models are not supported. Choose a single-file GGUF.")); }
        if multimodal(&key) { return Err(invalid("Multimodal GGUF models and projectors are not supported. Choose a text-only model.")); }
        if key.starts_with("adapter.") { return Err(invalid("GGUF adapters are not supported. Choose a complete language model.")); }
        let kind = g.u32()?;
        match key.as_str() {
            "general.architecture" => {
                if kind != 8 { return Err(invalid("Invalid GGUF architecture metadata.")); }
                let value = g.string(64)?;
                if value.is_empty() || !value.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_') || multimodal(&value) {
                    return Err(invalid("Unsupported GGUF architecture. Choose a text-only language model."));
                }
                architecture = Some(value);
            },
            "general.type" => {
                if kind != 8 || g.string(64)? != "model" { return Err(invalid("GGUF adapters and projectors are not supported.")); }
            },
            "general.alignment" => {
                if kind != 4 { return Err(invalid("Invalid GGUF tensor alignment.")); }
                alignment = g.u32()? as u64;
                if !alignment.is_power_of_two() || alignment > 4096 { return Err(invalid("Unsupported GGUF tensor alignment.")); }
            },
            "tokenizer.chat_template" => {
                if kind != 8 { return Err(invalid("The GGUF must contain one embedded chat template.")); }
                template = !g.string(1_048_576)?.trim().is_empty();
            },
            _ => g.value(kind, false)?,
        }
    }
    let architecture = architecture.ok_or_else(|| invalid("The GGUF has no language model architecture."))?;
    if !template { return Err(invalid("The GGUF has no embedded chat template. Choose a chat/instruct model with tokenizer.chat_template.")); }
    let mut names = HashSet::new();
    let mut ranges = Vec::new();
    for _ in 0..tensors {
        let name = g.string(256)?;
        if name.is_empty() || !names.insert(name.clone()) || multimodal(&name) { return Err(invalid("Unsupported or duplicate GGUF tensor name.")); }
        let dimensions = g.u32()?;
        if !(1..=4).contains(&dimensions) { return Err(invalid("Invalid GGUF tensor dimensions.")); }
        let mut elements = 1u64;
        let mut row = 0;
        for d in 0..dimensions {
            let length = g.u64()?;
            if length == 0 { return Err(invalid("Empty GGUF tensor dimension.")); }
            if d == 0 { row = length; }
            elements = elements.checked_mul(length).filter(|n| *n <= 1 << 40).ok_or_else(|| invalid("Oversized GGUF tensor."))?;
        }
        // GGML block sizes for scalar, common legacy/K/IQ and ternary quantizations.
        let (block, bytes) = match g.u32()? {
            0 | 26 => (1, 4), 1 | 25 | 30 => (1, 2), 24 => (1, 1), 27 | 28 => (1, 8),
            2 | 20 => (32, 18), 3 => (32, 20), 6 => (32, 22), 7 => (32, 24), 8 => (32, 34), 9 => (32, 36),
            10 => (256, 84), 11 => (256, 110), 12 => (256, 144), 13 => (256, 176), 14 => (256, 210), 15 => (256, 292),
            16 => (256, 66), 17 => (256, 74), 18 => (256, 98), 19 => (256, 50), 21 => (256, 110),
            22 => (256, 82), 23 => (256, 136), 29 => (256, 56), 34 => (256, 54), 35 => (256, 66),
            _ => return Err(invalid("This GGUF tensor quantization is not supported by the importer. Use a standard F16, Q4_K_M or Q8_0 model.")),
        };
        if row % block != 0 { return Err(invalid("GGUF tensor dimensions do not match its quantization.")); }
        let offset = g.u64()?;
        let end = offset.checked_add(elements / block * bytes).filter(|n| *n <= size).ok_or_else(|| invalid("Truncated GGUF tensor data."))?;
        if offset % alignment != 0 { return Err(invalid("Misaligned GGUF tensor data.")); }
        ranges.push((offset, end));
    }
    let start = g.position.div_ceil(alignment) * alignment;
    ranges.sort_unstable();
    let mut end = 0;
    for (offset, next) in ranges {
        if offset < end || start.checked_add(next).is_none_or(|n| n > size) { return Err(invalid("Overlapping or truncated GGUF tensor data.")); }
        end = next;
    }
    Ok(architecture)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::SeekFrom;

    fn string(bytes: &mut Vec<u8>, text: &str) {
        bytes.extend((text.len() as u64).to_le_bytes());
        bytes.extend(text.as_bytes());
    }

    fn gguf(path: &Path, extra: &[(&str, &str)], template: bool) {
        let mut bytes = b"GGUF".to_vec();
        bytes.extend(3u32.to_le_bytes());
        bytes.extend(1u64.to_le_bytes());
        bytes.extend((1 + u64::from(template) + extra.len() as u64).to_le_bytes());
        let mut values = vec![("general.architecture", "llama")];
        if template { values.push(("tokenizer.chat_template", "{% for message in messages %}{{ message.content }}{% endfor %}")); }
        values.extend_from_slice(extra);
        for (key, value) in values {
            string(&mut bytes, key);
            bytes.extend(8u32.to_le_bytes());
            string(&mut bytes, value);
        }
        string(&mut bytes, "token_embd.weight");
        bytes.extend(1u32.to_le_bytes());
        bytes.extend(32u64.to_le_bytes());
        bytes.extend(0u32.to_le_bytes());
        bytes.extend(0u64.to_le_bytes());
        bytes.resize(bytes.len().div_ceil(32) * 32 + 128, 0);
        fs::write(path, bytes).unwrap();
    }

    fn fixture() -> (tempfile::TempDir, Layout, ImportRequest) {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source.gguf");
        gguf(&source, &[], true);
        let layout = Layout { root: temp.path().join("managed") };
        let request = ImportRequest { source_path: source.to_string_lossy().into_owned(), name: "My instruct model".into() };
        (temp, layout, request)
    }

    fn run(layout: &Layout, request: &ImportRequest) -> Result<ImportedModel, ImportError> {
        import(layout, request, &AtomicBool::new(false), &mut |_, _| {})
    }

    #[test]
    fn copy_only_roundtrip_hashes_persistence_and_managed_deletion() {
        let (_temp, layout, request) = fixture();
        let original_hash = local::sha256_file(Path::new(&request.source_path)).unwrap();
        let mut progress = Vec::new();
        let record = import(&layout, &request, &AtomicBool::new(false), &mut |done, total| progress.push((done, total))).unwrap();
        assert!(is_import_id(&record.id));
        assert_eq!(record.sha256, original_hash);
        assert_eq!(progress.first(), Some(&(0, record.size)));
        assert_eq!(progress.last(), Some(&(record.size, record.size)));
        assert!(progress.windows(2).all(|p| p[0].0 <= p[1].0 && p[0].1 == p[1].1));
        let fresh = Layout { root: layout.root.clone() };
        assert_eq!(list(&fresh).unwrap(), [record.clone()]);
        let model = resolve(&fresh, &record.id).unwrap();
        assert!(model.path.ends_with(format!("{}/model.gguf", record.id)));
        assert!(installed(&fresh, &model));
        verify(&fresh, &model).unwrap();
        let saved = fs::read_to_string(layout.root.join(DIRECTORY).join("registry.json")).unwrap();
        assert!(!saved.contains(&request.source_path) && !saved.contains("sourcePath"));
        assert!(run(&layout, &request).unwrap_err().to_string().contains("already imported"));
        prepare_remove(&layout, &record.id).unwrap().remove().unwrap();
        assert!(list(&layout).unwrap().is_empty());
        assert!(!model.path.exists());
        assert!(resolve(&layout, &record.id).unwrap_err().to_string().contains("missing"));
        assert_eq!(local::sha256_file(Path::new(&request.source_path)).unwrap(), original_hash);
    }

    #[test]
    fn imported_sizes_use_the_same_mib_unit_as_the_catalog() {
        let mut model = ImportedModel { id: String::new(), name: String::new(), size: 1_048_576, sha256: String::new(), architecture: String::new() };
        assert_eq!(model.size_mb(), 1);
        model.size += 1;
        assert_eq!(model.size_mb(), 2);
        model.size = 3 * 1024 * 1024 * 1024;
        assert_eq!(model.size_mb(), 3072);
    }

    #[test]
    fn changed_missing_or_unexpected_managed_files_fail_closed_and_remain_removable() {
        let (_temp, layout, request) = fixture();
        let record = run(&layout, &request).unwrap();
        let model = resolve(&layout, &record.id).unwrap();
        let mut file = OpenOptions::new().write(true).open(&model.path).unwrap();
        file.seek(SeekFrom::End(-1)).unwrap();
        file.write_all(&[1]).unwrap();
        drop(file);
        assert!(installed(&layout, &model));
        assert!(verify(&layout, &model).unwrap_err().to_string().contains("checksum"));
        let extra = model.path.with_file_name("unrelated.txt");
        fs::write(&extra, b"keep").unwrap();
        assert!(prepare_remove(&layout, &record.id).unwrap().remove().is_err());
        assert!(extra.exists());
        fs::remove_file(extra).unwrap();
        fs::remove_file(&model.path).unwrap();
        assert!(!installed(&layout, &model));
        assert!(available(&layout, &model).unwrap_err().to_string().contains("unavailable"));
        prepare_remove(&layout, &record.id).unwrap().remove().unwrap();
        assert!(Path::new(&request.source_path).exists());
    }

    #[test]
    fn cancellation_and_publication_failure_leave_no_partial_import() {
        let (_temp, layout, request) = fixture();
        let cancel = AtomicBool::new(false);
        let result = import(&layout, &request, &cancel, &mut |done, _| {
            assert!(list(&layout).unwrap().is_empty());
            if done > 0 { cancel.store(true, Ordering::Relaxed); }
        });
        assert!(matches!(result, Err(ImportError::Cancelled)));
        assert!(list(&layout).unwrap().is_empty());
        assert!(!fs::read_dir(layout.root.join(DIRECTORY)).unwrap().any(|e| e.unwrap().file_type().unwrap().is_dir()));
        let result = import(&layout, &request, &AtomicBool::new(false), &mut |_, _| {
            let _ = fs::create_dir(layout.root.join(DIRECTORY).join("registry.json"));
        });
        assert!(result.is_err());
        assert_eq!(fs::read_dir(layout.root.join(DIRECTORY)).unwrap().filter(|e| e.as_ref().unwrap().file_type().unwrap().is_dir()).count(), 1);
        assert!(Path::new(&request.source_path).exists());
    }

    #[test]
    fn malformed_unsupported_and_truncated_gguf_are_rejected() {
        let (_temp, layout, request) = fixture();
        let source = Path::new(&request.source_path);
        for extra in [vec![("split.count", "2")], vec![("clip.projector_type", "mlp")], vec![("general.type", "adapter")],
            vec![("general.architecture", "qwen2vl")], vec![("tokenizer.chat_template", "duplicate")]] {
            gguf(source, &extra, true);
            assert!(run(&layout, &request).is_err());
        }
        gguf(source, &[], false);
        assert!(run(&layout, &request).unwrap_err().to_string().contains("chat template"));
        for bytes in [b"GGML".as_slice(), b"version https://git-lfs.github.com/spec/v1", b"GGUF\x03\0\0\0"] {
            fs::write(source, bytes).unwrap();
            assert!(run(&layout, &request).is_err());
        }
        for offset in [8, 16, 24] {
            gguf(source, &[], true);
            let mut file = OpenOptions::new().write(true).open(source).unwrap();
            file.seek(SeekFrom::Start(offset)).unwrap();
            file.write_all(&u64::MAX.to_le_bytes()).unwrap();
            drop(file);
            assert!(run(&layout, &request).is_err(), "oversized count or string at {offset}");
        }
        gguf(source, &[], true);
        let file = OpenOptions::new().write(true).open(source).unwrap();
        file.set_len(file.metadata().unwrap().len() - 1).unwrap();
        drop(file);
        assert!(run(&layout, &request).unwrap_err().to_string().contains("truncated"));
        assert!(list(&layout).unwrap().is_empty());
    }

    #[test]
    fn tensor_quantization_dimensions_offsets_and_payload_lengths_are_checked() {
        let (_temp, _layout, request) = fixture();
        let source = Path::new(&request.source_path);
        let original = fs::read(source).unwrap();
        let tensor = original.windows(b"token_embd.weight".len()).position(|w| w == b"token_embd.weight").unwrap() + b"token_embd.weight".len();
        let data = original.len() - 128;
        for (kind, row, size) in [(0u32, 32u64, 128), (1, 32, 64), (8, 32, 34), (9, 32, 36), (12, 256, 144), (14, 256, 210), (30, 32, 64)] {
            let mut bytes = original.clone();
            bytes[tensor + 4..tensor + 12].copy_from_slice(&row.to_le_bytes());
            bytes[tensor + 12..tensor + 16].copy_from_slice(&kind.to_le_bytes());
            bytes.resize(data + size, 0);
            fs::write(source, &bytes).unwrap();
            assert_eq!(validate_gguf(&mut open_regular(source).unwrap(), &AtomicBool::new(false)).unwrap(), "llama");
            bytes.pop();
            fs::write(source, &bytes).unwrap();
            assert!(validate_gguf(&mut open_regular(source).unwrap(), &AtomicBool::new(false)).is_err());
        }
        for (row, kind, offset) in [(0u64, 0u32, 0u64), (31, 12, 0), (32, 999, 0), (32, 0, 1), (32, 0, u64::MAX - 31)] {
            let mut bytes = original.clone();
            bytes[tensor + 4..tensor + 12].copy_from_slice(&row.to_le_bytes());
            bytes[tensor + 12..tensor + 16].copy_from_slice(&kind.to_le_bytes());
            bytes[tensor + 16..tensor + 24].copy_from_slice(&offset.to_le_bytes());
            fs::write(source, &bytes).unwrap();
            assert!(validate_gguf(&mut open_regular(source).unwrap(), &AtomicBool::new(false)).is_err());
        }
    }

    #[test]
    fn unsafe_ids_registry_corruption_and_managed_sources_are_rejected() {
        let (_temp, layout, mut request) = fixture();
        let record = run(&layout, &request).unwrap();
        for id in ["../source.gguf", "cleanup-import-../source", "cleanup-import-ABCDEF0123456789abcdef0123456789", "import-0123456789abcdef0123456789abcdef"] {
            assert!(!is_import_id(id));
            assert!(prepare_remove(&layout, id).is_err());
            assert!(resolve(&layout, id).is_err());
        }
        request.source_path = resolve(&layout, &record.id).unwrap().path.to_string_lossy().into_owned();
        assert!(run(&layout, &request).unwrap_err().to_string().contains("original GGUF"));
        let path = layout.root.join(DIRECTORY).join("registry.json");
        for bytes in [serde_json::to_vec(&Registry { version: 2, models: vec![] }).unwrap(),
            serde_json::to_vec(&Registry { version: 1, models: vec![record.clone(), record] }).unwrap(),
            vec![b' '; MAX_REGISTRY_BYTES as usize + 1]] {
            fs::write(&path, bytes).unwrap();
            assert!(list(&layout).is_err());
        }
    }

    #[test]
    fn storage_reservations_exclude_import_and_remove_without_waiting() {
        let (_temp, layout, request) = fixture();
        let record = run(&layout, &request).unwrap();
        let reservation = prepare_remove(&layout, &record.id).unwrap();
        assert!(matches!(run(&layout, &request), Err(ImportError::Busy)));
        assert!(matches!(prepare_remove(&layout, &record.id), Err(ImportError::Busy)));
        reservation.remove().unwrap();
        run(&layout, &request).unwrap();
    }

    #[test]
    fn interrupted_copy_and_publish_are_recovered_without_deleting_unowned_files() {
        let (_temp, layout, request) = fixture();
        let record = run(&layout, &request).unwrap();
        let parent = layout.root.join(DIRECTORY);
        let stage = tempfile::Builder::new().prefix(".import-").tempdir_in(&parent).unwrap().keep();
        fs::write(stage.join("pending.json"), serde_json::to_vec(&Pending { version: 1, id: record.id.clone() }).unwrap()).unwrap();
        fs::write(stage.join("model.gguf"), b"incomplete").unwrap();
        let mut orphan = record.clone();
        orphan.id = "cleanup-import-11111111111111111111111111111111".into();
        let unpublished = parent.join(&orphan.id);
        fs::create_dir(&unpublished).unwrap();
        fs::write(unpublished.join("manifest.json"), serde_json::to_vec(&orphan).unwrap()).unwrap();
        fs::write(unpublished.join("model.gguf"), b"unpublished").unwrap();
        fs::write(unpublished.join("pending.json"), serde_json::to_vec(&Pending { version: 1, id: orphan.id.clone() }).unwrap()).unwrap();
        let unrelated = parent.join("my-backup");
        fs::create_dir(&unrelated).unwrap();
        fs::write(unrelated.join("model.gguf"), b"keep").unwrap();
        assert!(run(&layout, &request).unwrap_err().to_string().contains("already imported"));
        assert!(!stage.exists() && !unpublished.exists());
        assert!(unrelated.join("model.gguf").exists());
        assert!(Path::new(&request.source_path).exists());
        assert_eq!(list(&layout).unwrap(), [record]);
    }

    #[cfg(unix)]
    #[test]
    fn linked_sources_storage_and_managed_copies_are_rejected() {
        use std::os::unix::fs::symlink;
        let (temp, layout, mut request) = fixture();
        let source = PathBuf::from(&request.source_path);
        let link = temp.path().join("link.gguf");
        symlink(&source, &link).unwrap();
        request.source_path = link.to_string_lossy().into_owned();
        assert!(run(&layout, &request).is_err());
        request.source_path = source.to_string_lossy().into_owned();
        fs::create_dir_all(&layout.root).unwrap();
        let outside = temp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        symlink(&outside, layout.root.join(DIRECTORY)).unwrap();
        assert!(run(&layout, &request).is_err());
        assert!(fs::read_dir(&outside).unwrap().next().is_none());
        fs::remove_file(layout.root.join(DIRECTORY)).unwrap();
        let record = run(&layout, &request).unwrap();
        let model = resolve(&layout, &record.id).unwrap();
        fs::remove_file(&model.path).unwrap();
        symlink(&source, &model.path).unwrap();
        assert!(!installed(&layout, &model));
        assert!(verify(&layout, &model).is_err());
        assert!(prepare_remove(&layout, &record.id).unwrap().remove().is_err());
        assert!(source.exists());
    }
}
