//! Checksum-pinned multi-file speech models. No native inference or runtime downloads.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use crate::local::{self, DownloadError, Layout};
use crate::engine::{ErrorKind, SttError, TranscribeOptions};

static DOWNLOAD: Mutex<()> = Mutex::new(());

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    Parakeet,
    MoonshineTiny,
    MoonshineBase,
}

pub struct File {
    /// Local filename expected by transcribe-rs.
    pub name: &'static str,
    /// Repository-relative download path.
    pub path: &'static str,
    pub sha256: &'static str,
    pub size: u64,
}

pub struct Model {
    pub id: &'static str,
    pub label: &'static str,
    pub note: &'static str,
    pub backend: Backend,
    pub languages: &'static [&'static str],
    pub max_audio_seconds: Option<u32>,
    pub repo: &'static str,
    pub revision: &'static str,
    pub files: &'static [File],
}

impl Model {
    pub fn size_mb(&self) -> u32 {
        self.files.iter().map(|f| f.size).sum::<u64>().div_ceil(1_000_000) as u32
    }

    pub fn supports_prompt(&self) -> bool {
        false
    }

    pub fn url(&self, file: &File) -> String {
        format!("https://huggingface.co/{}/resolve/{}/{}", self.repo, self.revision, file.path)
    }

    fn marker(&self) -> String {
        let files = self.files.iter().map(|f| format!("{}:{}:{}", f.name, f.size, f.sha256)).collect::<Vec<_>>().join("\n");
        format!("{}\n{}\n{}\n{files}\n", self.id, self.repo, self.revision)
    }
}

// Pinned HF API LFS SHA-256 metadata; small non-LFS vocabulary/tokenizer hashes computed
// from these exact revisions. Parakeet weights: CC-BY-4.0; Moonshine weights: MIT.
pub const MODELS: [Model; 4] = [
    Model {
        id: "parakeet-v3-int8",
        label: "Parakeet v3 (int8)",
        note: "NVIDIA Parakeet, ONNX by istupakov (CC BY 4.0); 25 languages including German; CPU; automatic language, no language forcing or vocabulary prompt",
        backend: Backend::Parakeet,
        languages: &["bg", "hr", "cs", "da", "nl", "en", "et", "fi", "fr", "de", "el", "hu", "it", "lv", "lt", "mt", "pl", "pt", "ro", "ru", "sk", "sl", "es", "sv", "uk"],
        max_audio_seconds: None,
        repo: "istupakov/parakeet-tdt-0.6b-v3-onnx",
        revision: "8f23f0c03c8761650bdb5b40aaf3e40d2c15f1ce",
        files: &[
            File { name: "encoder-model.int8.onnx", path: "encoder-model.int8.onnx", size: 652183999, sha256: "6139d2fa7e1b086097b277c7149725edbab89cc7c7ae64b23c741be4055aff09" },
            File { name: "decoder_joint-model.int8.onnx", path: "decoder_joint-model.int8.onnx", size: 18202004, sha256: "eea7483ee3d1a30375daedc8ed83e3960c91b098812127a0d99d1c8977667a70" },
            File { name: "nemo128.onnx", path: "nemo128.onnx", size: 139764, sha256: "a9fde1486ebfcc08f328d75ad4610c67835fea58c73ba57e3209a6f6cf019e9f" },
            File { name: "vocab.txt", path: "vocab.txt", size: 93939, sha256: "d58544679ea4bc6ac563d1f545eb7d474bd6cfa467f0a6e2c1dc1c7d37e3c35d" },
        ],
    },
    Model {
        id: "parakeet-v2-int8",
        label: "Parakeet v2 (int8)",
        note: "NVIDIA Parakeet, ONNX by istupakov (CC BY 4.0); English only; CPU; no vocabulary prompt",
        backend: Backend::Parakeet,
        languages: &["en"],
        max_audio_seconds: None,
        repo: "istupakov/parakeet-tdt-0.6b-v2-onnx",
        revision: "0bbb45a3365852604aef28b538a8f066f4ccaa85",
        files: &[
            File { name: "encoder-model.int8.onnx", path: "encoder-model.int8.onnx", size: 652184014, sha256: "3e0581fda6ab843888b51e56d7ee78b6d5bc3237ec113af1f732d1d5286aa155" },
            File { name: "decoder_joint-model.int8.onnx", path: "decoder_joint-model.int8.onnx", size: 8998286, sha256: "a449f49acd68979d418651dd2dcb737cc0f1bf0225e009e29ee326354edbf7d3" },
            File { name: "nemo128.onnx", path: "nemo128.onnx", size: 139764, sha256: "a9fde1486ebfcc08f328d75ad4610c67835fea58c73ba57e3209a6f6cf019e9f" },
            File { name: "vocab.txt", path: "vocab.txt", size: 9384, sha256: "ec182b70dd42113aff6c5372c75cac58c952443eb22322f57bbd7f53977d497d" },
        ],
    },
    Model {
        id: "moonshine-tiny-int8",
        label: "Moonshine Tiny (int8)",
        note: "Moonshine AI (MIT); English only; small CPU model; long recordings split into <=30-second chunks (native limit 64 seconds); no vocabulary prompt",
        backend: Backend::MoonshineTiny,
        languages: &["en"],
        max_audio_seconds: Some(64),
        repo: "moonshine-ai/moonshine",
        revision: "48b4e427b587bcf67797a5be706d6ddc4a298149",
        files: &[
            File { name: "encoder_model.onnx", path: "onnx/merged/tiny/quantized/encoder_model.onnx", size: 7937661, sha256: "c6fc4b7bc5af75c0591fd157a1f3829b533d18e9769a888fd95a62e470dd4f4a" },
            File { name: "decoder_model_merged.onnx", path: "onnx/merged/tiny/quantized/decoder_model_merged.onnx", size: 20243286, sha256: "eed87831c3a6103534aae7d47a5d485025c659a1323901513961c39fe8a1a367" },
            File { name: "tokenizer.json", path: "ctranslate2/tiny/tokenizer.json", size: 1985534, sha256: "ed2324b3f699d8ba18a4030f33ba205d50b30ff46ff73f2b7d22661cc850efb4" },
        ],
    },
    Model {
        id: "moonshine-base-int8",
        label: "Moonshine Base (int8)",
        note: "Moonshine AI (MIT); English only; CPU; long recordings split into <=30-second chunks (native limit 64 seconds); no vocabulary prompt",
        backend: Backend::MoonshineBase,
        languages: &["en"],
        max_audio_seconds: Some(64),
        repo: "moonshine-ai/moonshine",
        revision: "48b4e427b587bcf67797a5be706d6ddc4a298149",
        files: &[
            File { name: "encoder_model.onnx", path: "onnx/merged/base/quantized/encoder_model.onnx", size: 20513063, sha256: "1dd9ab0a7f987113d30affcba5a068d11c8f90fa0223caa3e491ade431ad9751" },
            File { name: "decoder_model_merged.onnx", path: "onnx/merged/base/quantized/decoder_model_merged.onnx", size: 42498870, sha256: "cc9f3cd6698a369c6008b41aa60aa3fb3322e7f03c9bdf19d8e6b7200afca4f3" },
            File { name: "tokenizer.json", path: "ctranslate2/tiny/tokenizer.json", size: 1985534, sha256: "ed2324b3f699d8ba18a4030f33ba205d50b30ff46ff73f2b7d22661cc850efb4" },
        ],
    },
];

pub fn model(id: &str) -> Option<&'static Model> {
    MODELS.iter().find(|m| m.id == id)
}

/// Bound native work and memory, preferring the quietest 20 ms frame in the final two
/// seconds. The caller consumes exactly this prefix, without overlap or dropped samples.
pub fn chunk_end(samples: &[f32]) -> usize {
    const MAX: usize = 30 * 16_000;
    if samples.len() <= MAX {
        return samples.len();
    }
    const SEARCH: usize = 2 * 16_000;
    const FRAME: usize = 320;
    let mut cut = MAX;
    let mut quietest = f32::INFINITY;
    for (i, frame) in samples[MAX - SEARCH..MAX].chunks_exact(FRAME).enumerate() {
        let energy = frame.iter().map(|s| s * s).sum::<f32>();
        if energy <= quietest {
            quietest = energy;
            cut = MAX - SEARCH + i * FRAME + FRAME / 2;
        }
    }
    cut
}

pub fn validate_request(model: &Model, options: &TranscribeOptions, samples: usize) -> Result<(), SttError> {
    let normalized = TranscribeOptions::new(options.language.as_deref().unwrap_or("auto"), "");
    if normalized.among().iter().any(|code| !model.languages.contains(&code.as_str())) {
        return Err(SttError::new(ErrorKind::BadResponse,
            format!("{} does not support the selected languages. Choose supported languages or automatic detection.", model.label),
            format!("supported languages: {}", model.languages.join(", "))));
    }
    if model.max_audio_seconds.is_some() && samples < 1600 {
        return Err(SttError::new(ErrorKind::BadResponse, "Moonshine needs at least 0.1 seconds of audio.", "Moonshine input below native duration limit"));
    }
    // Neither backend accepts vocabulary prompts or forced language decoding. The catalog
    // states this explicitly; supported language hints are validated, not passed as controls.
    Ok(())
}

pub fn model_dir(layout: &Layout, model: &Model) -> PathBuf {
    layout.root.join("models").join(model.id)
}

/// A quick status probe, not a substitute for full integrity verification before loading.
pub fn installed(layout: &Layout, model: &Model) -> bool {
    let dir = model_dir(layout, model);
    fs::read_to_string(dir.join(".installed")).is_ok_and(|s| s == model.marker())
        && model.files.iter().all(|f| fs::metadata(dir.join(f.name)).is_ok_and(|m| m.is_file() && m.len() == f.size))
}

pub fn verify(layout: &Layout, model: &Model) -> Result<(), DownloadError> {
    if !installed(layout, model) {
        return Err(DownloadError::Package(format!("{} is incomplete; download it again.", model.label)));
    }
    verify_files(&model_dir(layout, model), model)
}

fn verify_files(dir: &Path, model: &Model) -> Result<(), DownloadError> {
    for file in model.files {
        let path = dir.join(file.name);
        if fs::metadata(&path)?.len() != file.size || local::sha256_file(&path)? != file.sha256 {
            return Err(DownloadError::Checksum);
        }
    }
    Ok(())
}

/// The caller serializes downloads/removal for this model and unloads it before replacement.
/// Every file is hashed in staging; the final directory becomes visible only as a full set.
pub fn download(layout: &Layout, model: &Model, cancel: &AtomicBool, progress: &mut dyn FnMut(u64, Option<u64>)) -> Result<(), DownloadError> {
    // Different model downloads share one runtime; serialize installation as well as each
    // staging path. Cancellation is rechecked after waiting for another download.
    let _download = DOWNLOAD.lock().map_err(|_| DownloadError::Package("Restart Amber to recover ONNX downloads.".into()))?;
    if cancel.load(Ordering::Relaxed) { return Err(DownloadError::Cancelled); }
    if !runtime_supported() {
        return Err(DownloadError::Package("ONNX models currently require x64 Windows or x64 GNU/Linux.".into()));
    }
    let needs_runtime = verify_runtime(layout).is_err();
    let model_verified = verify(layout, model).is_ok();
    let runtime_bytes = if needs_runtime { RUNTIME_ARCHIVE_SIZE } else { 0 };
    let total = runtime_bytes + if model_verified { 0 } else { model.files.iter().map(|f| f.size).sum::<u64>() };
    if needs_runtime {
        let archive = layout.root.join("downloads").join("onnxruntime-1.24.4.nupkg");
        local::download_verified(RUNTIME_URL, RUNTIME_SHA256, &archive, cancel, &mut |done, _| progress(done, Some(total)))?;
        install_runtime(&archive, layout, cancel)?;
        let _ = fs::remove_file(archive);
    }
    if cancel.load(Ordering::Relaxed) { return Err(DownloadError::Cancelled); }
    if model_verified {
        progress(total, Some(total));
        return Ok(());
    }
    download_with(layout, model, cancel, &mut |done, _| progress(runtime_bytes + done, Some(total)), &mut |file, dest, report| {
        local::download_verified(&model.url(file), file.sha256, dest, cancel, report)
    })
}

/// Explicit runtime setup for offline-imported models. Import and native load never call it.
pub fn setup_runtime(layout: &Layout, cancel: &AtomicBool, progress: &mut dyn FnMut(u64, Option<u64>)) -> Result<(), DownloadError> {
    let _download = DOWNLOAD.lock().map_err(|_| DownloadError::Package("Restart Amber to recover ONNX downloads.".into()))?;
    if cancel.load(Ordering::Relaxed) { return Err(DownloadError::Cancelled); }
    if verify_runtime(layout).is_ok() { return Ok(()); }
    if !runtime_supported() { return Err(DownloadError::Package("ONNX models currently require x64 Windows or x64 GNU/Linux.".into())); }
    let archive = layout.root.join("downloads").join("onnxruntime-1.24.4.nupkg");
    local::download_verified(RUNTIME_URL, RUNTIME_SHA256, &archive, cancel, progress)?;
    install_runtime(&archive, layout, cancel)?;
    let _ = fs::remove_file(archive);
    Ok(())
}

pub const RUNTIME_VERSION: &str = "1.24.4";
pub const RUNTIME_ARCHIVE_SIZE: u64 = 125194303;
const RUNTIME_URL: &str = "https://api.nuget.org/v3-flatcontainer/microsoft.ml.onnxruntime/1.24.4/microsoft.ml.onnxruntime.1.24.4.nupkg";
const RUNTIME_SHA256: &str = "4b978d5065b85e7004b6c6f60ca494bd978fbe6836cbf0a0b52d82b61ab99638";

// Actual native bytes from the versioned Microsoft NuGet artifact. Its SHA-512 was also
// checked against NuGet's packageHash metadata. Keep MIT and bundled third-party notices.
#[cfg(windows)]
const RUNTIME_NATIVE: &[File] = &[
    File { name: "onnxruntime.dll", path: "runtimes/win-x64/native/onnxruntime.dll", size: 14203464, sha256: "b95efb2113b603bbbf3f191061c5516a871ed546893c820e4f3b7b6c358dbf2a" },
    File { name: "onnxruntime_providers_shared.dll", path: "runtimes/win-x64/native/onnxruntime_providers_shared.dll", size: 22088, sha256: "f2540b89707b47895c2a732bfd04e34a695c580d22301ef44c0f01f09b001673" },
];
#[cfg(not(windows))]
const RUNTIME_NATIVE: &[File] = &[
    File { name: "libonnxruntime.so", path: "runtimes/linux-x64/native/libonnxruntime.so", size: 22159232, sha256: "d132535d051344ff5c64c9c200004150559049a81ed330eb4422c1962fb6b7e4" },
    File { name: "libonnxruntime_providers_shared.so", path: "runtimes/linux-x64/native/libonnxruntime_providers_shared.so", size: 14632, sha256: "086ec1d5388f64153d9c63470d126693db9a182c8ce236d3a1119068471b8a0d" },
];
const RUNTIME_NOTICES: &[File] = &[
    File { name: "LICENSE", path: "LICENSE", size: 1094, sha256: "c250d6278f0b47a6439fb7592b08b58a55eb9f535aa49a1db63211c3f982b674" },
    File { name: "ThirdPartyNotices.txt", path: "ThirdPartyNotices.txt", size: 331175, sha256: "fb0af774b4d7cffc5b9d046f2aaeade2f37df2f80abf8033c95dfffcc77a8866" },
];

fn runtime_dir(layout: &Layout) -> PathBuf {
    layout.root.join("runtime").join(format!("onnx-{RUNTIME_VERSION}"))
}

fn runtime_supported() -> bool {
    cfg!(all(target_arch = "x86_64", any(windows, all(target_os = "linux", target_env = "gnu"))))
}

pub fn runtime_library(layout: &Layout) -> PathBuf {
    runtime_dir(layout).join(RUNTIME_NATIVE[0].name)
}

/// Native dependencies: x64 Windows with VC++ 2015-2022 redist; x64 GNU/Linux with
/// glibc >=2.27 and GCC 7-era libstdc++ (CXXABI_1.3.11). No CUDA/Vulkan or .NET needed.
pub fn runtime_installed(layout: &Layout) -> bool {
    if !runtime_supported() { return false; }
    let dir = runtime_dir(layout);
    fs::read_to_string(dir.join(".installed")).is_ok_and(|s| s == RUNTIME_SHA256)
        && RUNTIME_NATIVE.iter().chain(RUNTIME_NOTICES).all(|f| fs::metadata(dir.join(f.name)).is_ok_and(|m| m.is_file() && m.len() == f.size))
}

pub fn verify_runtime(layout: &Layout) -> Result<(), DownloadError> {
    if !runtime_installed(layout) {
        return Err(DownloadError::Package("ONNX Runtime 1.24.4 is not installed for this platform.".into()));
    }
    let dir = runtime_dir(layout);
    for file in RUNTIME_NATIVE.iter().chain(RUNTIME_NOTICES) {
        if local::sha256_file(&dir.join(file.name))? != file.sha256 { return Err(DownloadError::Checksum); }
    }
    Ok(())
}

/// Whitelist extraction from the checksum-pinned official package, never arbitrary paths.
pub fn install_runtime(archive: &Path, layout: &Layout, cancel: &AtomicBool) -> Result<(), DownloadError> {
    if cancel.load(Ordering::Relaxed) { return Err(DownloadError::Cancelled); }
    if !runtime_supported() {
        return Err(DownloadError::Package("ONNX models currently require x64 Windows or x64 GNU/Linux.".into()));
    }
    if local::sha256_file(archive)? != RUNTIME_SHA256 { return Err(DownloadError::Checksum); }
    let dest = runtime_dir(layout);
    let staging = dest.with_extension("new");
    if staging.exists() { fs::remove_dir_all(&staging)?; }
    fs::create_dir_all(&staging)?;
    let result = (|| {
        let mut zip = zip::ZipArchive::new(fs::File::open(archive)?).map_err(|e| DownloadError::Package(e.to_string()))?;
        for file in RUNTIME_NATIVE.iter().chain(RUNTIME_NOTICES) {
            if cancel.load(Ordering::Relaxed) { return Err(DownloadError::Cancelled); }
            let mut entry = zip.by_name(file.path).map_err(|e| DownloadError::Package(e.to_string()))?;
            if entry.size() != file.size { return Err(DownloadError::Checksum); }
            let path = staging.join(file.name);
            let mut out = fs::File::create(&path)?;
            std::io::copy(&mut entry, &mut out)?;
            out.sync_all()?;
            drop(out);
            if local::sha256_file(&path)? != file.sha256 { return Err(DownloadError::Checksum); }
        }
        if cancel.load(Ordering::Relaxed) { return Err(DownloadError::Cancelled); }
        fs::write(staging.join(".installed"), RUNTIME_SHA256)?;
        local::replace_dir(&staging, &dest)?;
        Ok(())
    })();
    if result.is_err() { let _ = fs::remove_dir_all(staging); }
    result
}

fn download_with(
    layout: &Layout,
    model: &Model,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64, Option<u64>),
    fetch: &mut dyn FnMut(&File, &Path, &mut dyn FnMut(u64, Option<u64>)) -> Result<(), DownloadError>,
) -> Result<(), DownloadError> {
    if cancel.load(Ordering::Relaxed) {
        return Err(DownloadError::Cancelled);
    }
    let dest = model_dir(layout, model);
    let staging = dest.with_extension("new");
    fs::create_dir_all(&staging)?;
    let marker = staging.join(".installed");
    if marker.exists() { fs::remove_file(&marker)?; }
    let result = (|| {
        let total = model.files.iter().map(|f| f.size).sum();
        let mut done = 0;
        for file in model.files {
            if cancel.load(Ordering::Relaxed) {
                return Err(DownloadError::Cancelled);
            }
            let path = staging.join(file.name);
            let complete = fs::metadata(&path).is_ok_and(|m| m.is_file() && m.len() == file.size)
                && local::sha256_file(&path)? == file.sha256;
            if !complete {
                // Only discard invalid completed files; the downloader owns resumable partials.
                if path.exists() { fs::remove_file(&path)?; }
                fetch(file, &path, &mut |bytes, _| progress(done + bytes, Some(total)))?;
            }
            done += file.size;
            progress(done, Some(total));
        }
        // Recheck the complete set, including lengths, before writing the installation marker.
        verify_files(&staging, model)?;
        if cancel.load(Ordering::Relaxed) {
            return Err(DownloadError::Cancelled);
        }
        fs::write(&marker, model.marker())?;
        local::replace_dir(&staging, &dest)?;
        progress(total, Some(total));
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(marker);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Model {
        Model {
            id: "test-model", label: "Test", note: "", backend: Backend::Parakeet,
            languages: &["en"], max_audio_seconds: None, repo: "test/repo", revision: "test-revision",
            files: &[
                File { name: "one.onnx", path: "one.onnx", size: 3, sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad" },
                File { name: "two.json", path: "two.json", size: 3, sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad" },
            ],
        }
    }

    #[test]
    fn catalog_is_pinned_complete_and_separate_from_whisper() {
        for model in &MODELS {
            assert!(local::model(model.id).is_none());
            assert_eq!(model.revision.len(), 40);
            assert!(model.revision.chars().all(|c| c.is_ascii_hexdigit()));
            assert!(!model.supports_prompt());
            assert!(model.size_mb() > 20);
            let mut names = std::collections::HashSet::new();
            for file in model.files {
                assert!(names.insert(file.name));
                assert!(file.size > 0);
                assert_eq!(file.sha256.len(), 64);
                assert!(file.sha256.chars().all(|c| c.is_ascii_hexdigit()));
                assert!(!file.name.contains(['/', '\\']));
                assert!(model.url(file).contains(&format!("/resolve/{}/", model.revision)));
            }
        }
        assert!(model("parakeet-v3-int8").unwrap().languages.contains(&"de"));
        assert_eq!(model("moonshine-tiny-int8").unwrap().max_audio_seconds, Some(64));
    }

    #[test]
    fn install_is_all_or_nothing_and_verification_detects_changed_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout { root: dir.path().into() };
        let model = fixture();
        let cancel = AtomicBool::new(false);
        let dest = model_dir(&layout, &model);
        download_with(&layout, &model, &cancel, &mut |_, _| {}, &mut |_, path, _| {
            assert!(!installed(&layout, &model));
            fs::write(path, b"abc")?;
            Ok(())
        }).unwrap();
        assert!(installed(&layout, &model));
        verify(&layout, &model).unwrap();
        let result = download_with(&layout, &model, &cancel, &mut |_, _| {}, &mut |file, path, _| {
            fs::write(path, if file.name == "one.onnx" { b"abc" } else { b"bad" })?;
            Ok(())
        });
        assert!(matches!(result, Err(DownloadError::Checksum)));
        verify(&layout, &model).unwrap();
        let staging = dest.with_extension("new");
        assert_eq!(fs::read(staging.join("one.onnx")).unwrap(), b"abc");
        assert_eq!(fs::read(staging.join("two.json")).unwrap(), b"bad");
        assert!(!staging.join(".installed").exists());
        fs::write(dest.join("one.onnx"), b"bad").unwrap();
        assert!(installed(&layout, &model), "status only checks lengths");
        assert!(matches!(verify(&layout, &model), Err(DownloadError::Checksum)));
        fs::remove_file(dest.join("two.json")).unwrap();
        assert!(!installed(&layout, &model));
    }

    #[test]
    fn failed_second_file_reuses_completed_and_partial_staging_on_retry() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout { root: dir.path().into() };
        let model = fixture();
        let cancel = AtomicBool::new(false);
        let dest = model_dir(&layout, &model);
        let staging = dest.with_extension("new");
        let result = download_with(&layout, &model, &cancel, &mut |_, _| {}, &mut |file, path, report| {
            if file.name == "two.json" {
                fs::write(path.with_extension("part"), b"a")?;
                report(1, Some(file.size));
                return Err(DownloadError::Network("connection lost".into()));
            }
            fs::write(path, b"abc")?;
            Ok(())
        });
        assert!(matches!(result, Err(DownloadError::Network(_))));
        assert!(!dest.exists());
        assert_eq!(fs::read(staging.join("one.onnx")).unwrap(), b"abc");
        assert_eq!(fs::read(staging.join("two.part")).unwrap(), b"a");
        assert!(!staging.join(".installed").exists());

        let mut fetched = Vec::new();
        let mut progress = Vec::new();
        download_with(&layout, &model, &cancel, &mut |done, total| progress.push((done, total)), &mut |file, path, report| {
            fetched.push(file.name);
            assert_eq!(fs::read(path.with_extension("part"))?, b"a");
            report(1, Some(file.size));
            fs::write(path, b"abc")?;
            fs::remove_file(path.with_extension("part"))?;
            Ok(())
        }).unwrap();
        assert_eq!(fetched, ["two.json"]);
        assert_eq!(progress.first(), Some(&(3, Some(6))));
        assert!(progress.contains(&(4, Some(6))));
        assert_eq!(progress.last(), Some(&(6, Some(6))));
        verify(&layout, &model).unwrap();
        assert!(!staging.exists());
    }

    #[test]
    fn file_checksum_failure_keeps_completed_staging_for_retry() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout { root: dir.path().into() };
        let model = fixture();
        let cancel = AtomicBool::new(false);
        let result = download_with(&layout, &model, &cancel, &mut |_, _| {}, &mut |file, path, _| {
            if file.name == "two.json" { return Err(DownloadError::Checksum); }
            fs::write(path, b"abc")?;
            Ok(())
        });
        assert!(matches!(result, Err(DownloadError::Checksum)));
        assert!(!installed(&layout, &model));
        download_with(&layout, &model, &cancel, &mut |_, _| {}, &mut |file, path, _| {
            assert_eq!(file.name, "two.json");
            fs::write(path, b"abc")?;
            Ok(())
        }).unwrap();
        verify(&layout, &model).unwrap();
    }

    #[test]
    fn corrupt_staged_files_are_refetched_and_stale_marker_is_removed() {
        for bad in [b"bad".as_slice(), b"ab", b"abcd"] {
            let dir = tempfile::tempdir().unwrap();
            let layout = Layout { root: dir.path().into() };
            let model = fixture();
            let staging = model_dir(&layout, &model).with_extension("new");
            fs::create_dir_all(&staging).unwrap();
            fs::write(staging.join(".installed"), model.marker()).unwrap();
            fs::write(staging.join("one.onnx"), bad).unwrap();
            fs::write(staging.join("one.part"), b"a").unwrap();
            fs::write(staging.join("two.json"), b"abc").unwrap();
            let mut fetched = Vec::new();
            download_with(&layout, &model, &AtomicBool::new(false), &mut |_, _| {}, &mut |file, path, _| {
                fetched.push(file.name);
                assert!(!staging.join(".installed").exists());
                assert!(!path.exists());
                assert_eq!(fs::read(path.with_extension("part"))?, b"a");
                fs::write(path, b"abc")?;
                fs::remove_file(path.with_extension("part"))?;
                Ok(())
            }).unwrap();
            assert_eq!(fetched, ["one.onnx"]);
            verify(&layout, &model).unwrap();
        }
    }

    #[test]
    fn final_checksum_rechecks_reused_files_before_replacing_existing_model() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout { root: dir.path().into() };
        let model = fixture();
        let cancel = AtomicBool::new(false);
        download_with(&layout, &model, &cancel, &mut |_, _| {}, &mut |_, path, _| {
            fs::write(path, b"abc")?;
            Ok(())
        }).unwrap();
        let staging = model_dir(&layout, &model).with_extension("new");
        fs::create_dir_all(&staging).unwrap();
        fs::write(staging.join("one.onnx"), b"abc").unwrap();
        let result = download_with(&layout, &model, &cancel, &mut |_, _| {}, &mut |file, path, _| {
            assert_eq!(file.name, "two.json");
            fs::write(path, b"abc")?;
            fs::write(staging.join("one.onnx"), b"bad")?;
            Ok(())
        });
        assert!(matches!(result, Err(DownloadError::Checksum)));
        verify(&layout, &model).unwrap();
        assert_eq!(fs::read(staging.join("two.json")).unwrap(), b"abc");
        assert!(!staging.join(".installed").exists());
        download_with(&layout, &model, &cancel, &mut |_, _| {}, &mut |file, path, _| {
            assert_eq!(file.name, "one.onnx");
            fs::write(path, b"abc")?;
            Ok(())
        }).unwrap();
        verify(&layout, &model).unwrap();
    }

    #[test]
    fn cancellation_never_installs_a_partial_set() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout { root: dir.path().into() };
        let model = fixture();
        let cancel = AtomicBool::new(false);
        let result = download_with(&layout, &model, &cancel, &mut |_, _| {}, &mut |file, path, _| {
            if file.name == "two.json" {
                fs::write(path.with_extension("part"), b"a")?;
                cancel.store(true, Ordering::Relaxed);
                return Err(DownloadError::Cancelled);
            }
            fs::write(path, b"abc")?;
            Ok(())
        });
        assert!(matches!(result, Err(DownloadError::Cancelled)));
        let dest = model_dir(&layout, &model);
        let staging = dest.with_extension("new");
        assert!(!dest.exists());
        assert_eq!(fs::read(staging.join("one.onnx")).unwrap(), b"abc");
        assert!(!staging.join("two.json").exists());
        assert_eq!(fs::read(staging.join("two.part")).unwrap(), b"a");
        assert!(!staging.join(".installed").exists());
        assert!(matches!(download_with(&layout, &model, &cancel, &mut |_, _| {}, &mut |_, _, _| {
            panic!("cancelled download must not fetch");
        }), Err(DownloadError::Cancelled)));

        cancel.store(false, Ordering::Relaxed);
        let result = download_with(&layout, &model, &cancel, &mut |_, _| {}, &mut |file, path, _| {
            assert_eq!(file.name, "two.json");
            assert_eq!(fs::read(path.with_extension("part"))?, b"a");
            fs::write(path, b"abc")?;
            fs::remove_file(path.with_extension("part"))?;
            cancel.store(true, Ordering::Relaxed);
            Ok(())
        });
        assert!(matches!(result, Err(DownloadError::Cancelled)));
        assert!(!dest.exists() && !staging.join(".installed").exists());
        assert_eq!(fs::read(staging.join("two.json")).unwrap(), b"abc");
        cancel.store(false, Ordering::Relaxed);
        download_with(&layout, &model, &cancel, &mut |_, _| {}, &mut |_, _, _| {
            panic!("completed staged files must be reused");
        }).unwrap();
        verify(&layout, &model).unwrap();
    }

    #[test]
    fn chunking_is_bounded_lossless_and_uses_quiet_boundaries() {
        let mut audio = vec![0.5; 90 * 16_000 + 37];
        audio[29 * 16_000..29 * 16_000 + 320].fill(0.0);
        assert_eq!(chunk_end(&audio), 29 * 16_000 + 160);
        let mut done = 0;
        while done < audio.len() {
            let end = chunk_end(&audio[done..]);
            assert!((1..=30 * 16_000).contains(&end));
            done += end;
        }
        assert_eq!(done, audio.len());
        assert_eq!(chunk_end(&[]), 0);
        assert_eq!(chunk_end(&[0.1; 37]), 37);
    }

    #[test]
    fn language_and_audio_limits_are_checked_without_models() {
        let tiny = model("moonshine-tiny-int8").unwrap();
        assert!(validate_request(tiny, &TranscribeOptions::new("en-US", "vocabulary is unsupported"), 1600).is_ok());
        assert!(validate_request(tiny, &TranscribeOptions::new("de,en", ""), 1600).is_err());
        assert!(validate_request(tiny, &TranscribeOptions::default(), 1599).is_err());
        assert!(validate_request(tiny, &TranscribeOptions::default(), 64 * 16_000).is_ok());
        assert!(validate_request(tiny, &TranscribeOptions::default(), 90 * 16_000).is_ok(), "long recordings are chunked");
        assert!(validate_request(model("parakeet-v3-int8").unwrap(), &TranscribeOptions::new("de,en", ""), 90 * 16_000).is_ok());
        assert!(validate_request(model("parakeet-v2-int8").unwrap(), &TranscribeOptions::new("de", ""), 1600).is_err());
    }

    #[test]
    fn runtime_rejects_unverified_archives_and_checks_cancel_before_io() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout { root: dir.path().join("data") };
        let archive = dir.path().join("untrusted.zip");
        fs::write(&archive, b"not a Microsoft runtime").unwrap();
        assert!(!runtime_installed(&layout));
        assert!(verify_runtime(&layout).is_err());
        assert!(matches!(install_runtime(&archive, &layout, &AtomicBool::new(false)), Err(DownloadError::Checksum)));
        assert!(!layout.root.exists());
        assert!(matches!(install_runtime(Path::new("missing"), &layout, &AtomicBool::new(true)), Err(DownloadError::Cancelled)));
    }

    #[test]
    #[ignore = "requires existing Microsoft NuGet package at AMBER_TEST_ORT_ARCHIVE; never downloads"]
    fn official_runtime_archive_installs_only_verified_platform_files() {
        let archive = PathBuf::from(std::env::var_os("AMBER_TEST_ORT_ARCHIVE").expect("set AMBER_TEST_ORT_ARCHIVE"));
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout { root: dir.path().into() };
        install_runtime(&archive, &layout, &AtomicBool::new(false)).unwrap();
        verify_runtime(&layout).unwrap();
        assert_eq!(fs::read_dir(runtime_dir(&layout)).unwrap().count(), RUNTIME_NATIVE.len() + RUNTIME_NOTICES.len() + 1);
        assert!(runtime_library(&layout).is_file());
    }
}
