//! ONNX speech inference. Native libraries are loaded only when a model is requested.

use std::sync::{Arc, Mutex, OnceLock};

use amber_core::engine::{CancellationToken, ErrorKind, SttError, TranscribeOptions, Transcriber};
use amber_core::local::Layout;
use amber_core::onnx;
use amber_core::imports::{self, SpeechModel as Descriptor, Variant};
use transcribe_rs::onnx::{moonshine::{MoonshineModel, MoonshineVariant}, parakeet::ParakeetModel, Quantization};
use transcribe_rs::SpeechModel;

pub struct Speech {
    layout: Layout,
    loaded: Mutex<Option<Loaded>>,
}

struct Loaded {
    id: String,
    model: Box<dyn SpeechModel>,
}

impl Speech {
    pub fn new(layout: Layout) -> Self {
        Self { layout, loaded: Mutex::new(None) }
    }

    /// Serializes model construction and inference. Call only from blocking workers.
    pub fn load(&self, id: &str, cancel: &CancellationToken) -> Result<(), SttError> {
        cancel.check()?;
        let model = imports::resolve(&self.layout, id).map_err(|e| failure("Unknown ONNX speech model.", e.to_string()))?;
        if !model.is_onnx() { return Err(failure("Not an ONNX speech model.", id)); }
        let mut loaded = self.loaded.lock().map_err(|_| failure("Restart Amber to recover speech recognition.", "ONNX model lock poisoned"))?;
        cancel.check()?;
        if loaded.as_ref().is_some_and(|l| l.id == id) {
            return Ok(());
        }
        imports::verify(&self.layout, &model).map_err(|e| failure("The speech model is incomplete or damaged. Download or import it again.", e.to_string()))?;
        cancel.check()?;
        initialize_runtime(&self.layout)?;
        cancel.check()?;
        // Drop the previous sessions before native model construction allocates new weights.
        *loaded = None;
        let dir = &model.path;
        let native: Result<Box<dyn SpeechModel>, _> = match model.backend {
            Variant::ParakeetInt8 => ParakeetModel::load(dir, &Quantization::Int8).map(|m| Box::new(m) as Box<dyn SpeechModel>),
            // The upstream quantized Moonshine files have unsuffixed names. Quantization here
            // only selects a filename; these pinned files still contain int8 weights.
            Variant::MoonshineTiny => MoonshineModel::load(dir, MoonshineVariant::Tiny, &Quantization::FP32).map(|m| Box::new(m) as Box<dyn SpeechModel>),
            Variant::MoonshineBase => MoonshineModel::load(dir, MoonshineVariant::Base, &Quantization::FP32).map(|m| Box::new(m) as Box<dyn SpeechModel>),
            Variant::Whisper => unreachable!("checked above"),
        };
        cancel.check()?;
        *loaded = Some(Loaded { id: model.id, model: native.map_err(|e| failure("The ONNX speech model could not be loaded.", e.to_string()))? });
        Ok(())
    }

    pub fn engine(self: &Arc<Self>, id: &str) -> Result<Box<dyn Transcriber>, String> {
        let model = imports::resolve(&self.layout, id).map_err(|e| e.to_string())?;
        if !model.is_onnx() { return Err("Not an ONNX speech model.".into()); }
        Ok(Box::new(Engine { speech: self.clone(), model }))
    }

    pub fn unload(&self) {
        *self.loaded.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    pub fn unload_model(&self, id: &str) -> bool {
        let mut loaded = self.loaded.lock().unwrap_or_else(|e| e.into_inner());
        if loaded.as_ref().is_some_and(|l| l.id == id) {
            *loaded = None;
            true
        } else {
            false
        }
    }

    pub fn running_model(&self) -> Option<String> {
        self.loaded.try_lock().ok()?.as_ref().map(|l| l.id.to_string())
    }

    pub fn is_ready(&self, id: &str) -> bool {
        self.running_model().as_deref() == Some(id)
    }
}

fn initialize_runtime(layout: &Layout) -> Result<(), SttError> {
    static INITIALIZED: OnceLock<()> = OnceLock::new();
    static INIT: Mutex<()> = Mutex::new(());
    let _init = INIT.lock().map_err(|_| failure("Restart Amber to recover ONNX Runtime.", "runtime initialization lock poisoned"))?;
    if INITIALIZED.get().is_some() {
        return Ok(());
    }
    onnx::verify_runtime(layout).map_err(|e| failure("ONNX Runtime is missing or damaged. Use Install runtime in the local model settings.", e.to_string()))?;
    let path = onnx::runtime_library(layout);
    // Explicit absolute path, never the current directory or a globally installed DLL.
    let path = path.canonicalize().map_err(|e| failure("ONNX Runtime could not be located.", e.to_string()))?;
    ort::init_from(path)
        .map_err(|e| failure("ONNX Runtime could not start. On Windows, install the Microsoft Visual C++ 2019 or newer x64 runtime.", e.to_string()))?
        .with_telemetry(false)
        .commit();
    transcribe_rs::set_ort_accelerator(transcribe_rs::OrtAccelerator::CpuOnly);
    let _ = INITIALIZED.set(());
    Ok(())
}

fn failure(message: &str, detail: impl Into<String>) -> SttError {
    SttError::new(ErrorKind::Server, message, detail)
}

struct Engine {
    speech: Arc<Speech>,
    model: Descriptor,
}

impl Transcriber for Engine {
    fn transcribe(&self, wav: &[u8], options: &TranscribeOptions) -> Result<String, SttError> {
        self.transcribe_cancellable(wav, options, &CancellationToken::default())
    }

    fn transcribe_cancellable(&self, wav: &[u8], options: &TranscribeOptions, cancel: &CancellationToken) -> Result<String, SttError> {
        cancel.check()?;
        let (samples, rate) = amber_core::audio::decode_wav(wav).map_err(|e| SttError::new(ErrorKind::BadResponse, "Not a WAV recording.", e))?;
        cancel.check()?;
        if samples.is_empty() {
            return Ok(String::new());
        }
        if samples.iter().any(|s| !s.is_finite()) {
            return Err(SttError::new(ErrorKind::BadResponse, "The recording contains invalid audio.", "non-finite sample"));
        }
        let samples = amber_core::audio::resample(&samples, rate, 16_000);
        self.model.validate_request(options, samples.len())?;
        cancel.check()?;
        let mut loaded = self.speech.loaded.lock().map_err(|_| failure("Restart Amber to recover speech recognition.", "ONNX model lock poisoned"))?;
        cancel.check()?;
        let native = loaded.as_mut().filter(|l| l.id == self.model.id).ok_or_else(|| {
            SttError::new(ErrorKind::Unreachable, "The local model was unloaded or changed before recognition started. Try again.", "ONNX request handle is stale")
        })?;
        let mut texts = Vec::new();
        let mut start = 0;
        while start < samples.len() {
            cancel.check()?;
            let end = start + onnx::chunk_end(&samples[start..]);
            let chunk = &samples[start..end];
            // Only a tiny final tail needs padding. Never discard its speech just because
            // Moonshine has a minimum input length.
            let padded;
            let chunk = if self.model.max_audio_seconds.is_some() && chunk.len() < 1600 {
                padded = { let mut p = chunk.to_vec(); p.resize(1600, 0.0); p };
                &padded[..]
            } else { chunk };
            let result = native.model.transcribe(chunk, &transcribe_rs::TranscribeOptions::default());
            // Upstream synchronous runs cannot be interrupted. Keep the model lock until
            // native work finishes, then discard cancelled text before delivery.
            cancel.check()?;
            texts.push(result.map_err(|e| failure("ONNX speech recognition failed.", e.to_string()))?.text);
            start = end;
        }
        Ok(amber_core::chunking::join(&texts))
    }

    fn transcribe_detecting_cancellable(&self, wav: &[u8], options: &TranscribeOptions, cancel: &CancellationToken) -> Result<(String, Option<String>), SttError> {
        // These backends do not report a detected language. Never invent one from a hint.
        self.transcribe_cancellable(wav, options, cancel).map(|text| (text, None))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake(Box<dyn FnMut(&[f32]) -> String + Send>);

    impl SpeechModel for Fake {
        fn capabilities(&self) -> transcribe_rs::ModelCapabilities {
            transcribe_rs::ModelCapabilities {
                name: "test", engine_id: "test", sample_rate: 16_000, languages: &["en"],
                supports_timestamps: false, supports_translation: false, supports_streaming: false,
            }
        }

        fn transcribe_raw(&mut self, samples: &[f32], _: &transcribe_rs::TranscribeOptions) -> Result<transcribe_rs::TranscriptionResult, transcribe_rs::TranscribeError> {
            Ok(transcribe_rs::TranscriptionResult { text: self.0(samples), segments: None })
        }
    }

    #[test]
    fn construction_status_and_unload_need_no_native_library() {
        let dir = tempfile::tempdir().unwrap();
        let speech = Arc::new(Speech::new(Layout { root: dir.path().into() }));
        assert_eq!(speech.running_model(), None);
        assert!(!speech.is_ready("parakeet-v3-int8"));
        assert!(!speech.unload_model("parakeet-v3-int8"));
        speech.unload();
        let cancel = CancellationToken::default();
        cancel.cancel();
        assert_eq!(speech.load("parakeet-v3-int8", &cancel).unwrap_err().kind, ErrorKind::Cancelled);
        let engine = speech.engine("parakeet-v3-int8").unwrap();
        assert_eq!(engine.transcribe_cancellable(b"invalid", &TranscribeOptions::default(), &cancel).unwrap_err().kind, ErrorKind::Cancelled);
        assert!(speech.load("parakeet-v3-int8", &CancellationToken::default()).is_err());
    }

    #[test]
    fn stale_handles_cannot_reload_a_second_backend() {
        let dir = tempfile::tempdir().unwrap();
        let speech = Arc::new(Speech::new(Layout { root: dir.path().into() }));
        let engine = speech.engine("moonshine-tiny-int8").unwrap();
        let wav = amber_core::audio::encode_wav(&vec![0.0; 1600], 16_000);
        assert_eq!(engine.transcribe(&wav, &TranscribeOptions::default()).unwrap_err().kind, ErrorKind::Unreachable);
        assert_eq!(speech.running_model(), None);
    }

    #[test]
    fn speech_is_shareable_and_busy_status_does_not_block() {
        fn shared<T: Send + Sync>() {}
        shared::<Speech>();
        let dir = tempfile::tempdir().unwrap();
        let speech = Speech::new(Layout { root: dir.path().into() });
        let _busy = speech.loaded.lock().unwrap();
        assert_eq!(speech.running_model(), None);
    }

    #[test]
    fn unload_waits_for_native_work_and_cancelled_text_is_discarded() {
        let dir = tempfile::tempdir().unwrap();
        let speech = Arc::new(Speech::new(Layout { root: dir.path().into() }));
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        *speech.loaded.lock().unwrap() = Some(Loaded { id: "moonshine-tiny-int8".into(), model: Box::new(Fake(Box::new(move |_| {
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            "cancelled text".into()
        }))) });
        let engine = speech.engine("moonshine-tiny-int8").unwrap();
        let cancel = CancellationToken::default();
        let request_cancel = cancel.clone();
        let worker = std::thread::spawn(move || engine.transcribe_cancellable(
            &amber_core::audio::encode_wav(&vec![0.0; 1600], 16_000), &TranscribeOptions::default(), &request_cancel));
        entered_rx.recv().unwrap();
        assert!(speech.loaded.try_lock().is_err(), "native work pins the model");
        assert_eq!(speech.running_model(), None, "status cannot block");
        let unload = { let speech = speech.clone(); std::thread::spawn(move || speech.unload()) };
        cancel.cancel();
        release_tx.send(()).unwrap();
        assert_eq!(worker.join().unwrap().unwrap_err().kind, ErrorKind::Cancelled);
        unload.join().unwrap();
        assert_eq!(speech.running_model(), None);
    }

    #[test]
    fn long_moonshine_audio_runs_bounded_chunks_and_preserves_a_short_tail() {
        let dir = tempfile::tempdir().unwrap();
        let speech = Arc::new(Speech::new(Layout { root: dir.path().into() }));
        let sizes = Arc::new(Mutex::new(Vec::new()));
        let seen = sizes.clone();
        *speech.loaded.lock().unwrap() = Some(Loaded { id: "moonshine-tiny-int8".into(), model: Box::new(Fake(Box::new(move |s| {
            seen.lock().unwrap().push(s.len());
            "chunk".into()
        }))) });
        let engine = speech.engine("moonshine-tiny-int8").unwrap();
        let samples = vec![0.0; 30 * 16_000 + 1];
        let text = engine.transcribe(&amber_core::audio::encode_wav(&samples, 16_000), &TranscribeOptions::default()).unwrap();
        assert_eq!(text, "chunk chunk");
        let sizes = sizes.lock().unwrap();
        assert_eq!(sizes.len(), 2);
        assert!(sizes.iter().all(|n| (1600..=30 * 16_000).contains(n)));
        assert!(sizes.iter().sum::<usize>() >= samples.len());
    }

    #[test]
    #[ignore = "requires existing AMBER_TEST_ONNX_ROOT, AMBER_TEST_ONNX_WAV and AMBER_TEST_ORT_ARCHIVE; never downloads"]
    fn imported_moonshine_roundtrip_native_inference_and_delete() {
        use std::path::PathBuf;
        use std::sync::atomic::AtomicBool;
        let source_layout = Layout { root: PathBuf::from(std::env::var_os("AMBER_TEST_ONNX_ROOT").expect("set existing model root")) };
        let source_model = onnx::model("moonshine-tiny-int8").unwrap();
        onnx::verify(&source_layout, source_model).unwrap();
        let wav_path = PathBuf::from(std::env::var_os("AMBER_TEST_ONNX_WAV").expect("set public JFK fixture"));
        assert_eq!(amber_core::local::sha256_file(&wav_path).unwrap(), "59dfb9a4acb36fe2a2affc14bacbee2920ff435cb13cc314a08c13f66ba7860e");
        let wav = std::fs::read(wav_path).unwrap();
        let temp = tempfile::tempdir().unwrap();
        let layout = Layout { root: temp.path().into() };
        let record = imports::import(&layout, &imports::ImportRequest {
            source_path: onnx::model_dir(&source_layout, source_model).to_string_lossy().into_owned(),
            name: "Imported Moonshine smoke".into(), variant: Variant::MoonshineTiny, languages: vec!["en".into()],
        }, &AtomicBool::new(false), &mut |_, _| {}).unwrap();
        assert!(!onnx::runtime_installed(&layout));
        let descriptor = imports::resolve(&layout, &record.id).unwrap();
        imports::verify(&layout, &descriptor).unwrap();
        let archive = PathBuf::from(std::env::var_os("AMBER_TEST_ORT_ARCHIVE").expect("set existing Microsoft runtime package"));
        onnx::install_runtime(&archive, &layout, &AtomicBool::new(false)).unwrap();
        for _ in 0..2 {
            let speech = Arc::new(Speech::new(layout.clone()));
            speech.load(&record.id, &CancellationToken::default()).unwrap();
            let engine = speech.engine(&record.id).unwrap();
            let text = engine.transcribe(&wav, &TranscribeOptions::new("en", "")).unwrap();
            println!("Imported native transcript: {text}");
            assert!(text.to_lowercase().contains("ask not what your country can do for you"));
            speech.unload();
            assert_eq!(engine.transcribe(&wav, &TranscribeOptions::default()).unwrap_err().kind, ErrorKind::Unreachable);
        }
        imports::remove(&layout, &record.id).unwrap();
        assert!(imports::resolve(&layout, &record.id).is_err());
        assert!(Arc::new(Speech::new(layout)).engine(&record.id).is_err());
        onnx::verify(&source_layout, source_model).unwrap();
    }

    #[test]
    #[ignore = "requires existing Microsoft package at AMBER_TEST_ORT_ARCHIVE; no speech models or downloads"]
    fn official_runtime_initializes_lazily_without_a_speech_model() {
        let archive = std::path::PathBuf::from(std::env::var_os("AMBER_TEST_ORT_ARCHIVE").expect("set AMBER_TEST_ORT_ARCHIVE"));
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout { root: dir.path().into() };
        onnx::install_runtime(&archive, &layout, &std::sync::atomic::AtomicBool::new(false)).unwrap();
        initialize_runtime(&layout).unwrap();
        ort::session::Session::builder().unwrap();
    }

    #[test]
    #[ignore = "requires AMBER_TEST_ONNX_ROOT and AMBER_TEST_ONNX_WAV; downloads only with AMBER_TEST_ONNX_DOWNLOAD=1 and AMBER_TEST_ORT_ARCHIVE"]
    fn moonshine_tiny_public_jfk_lifecycle() {
        use std::path::PathBuf;
        use std::sync::atomic::AtomicBool;
        use std::time::Instant;

        let layout = Layout { root: PathBuf::from(std::env::var_os("AMBER_TEST_ONNX_ROOT").expect("set AMBER_TEST_ONNX_ROOT to a test-only data directory")) };
        let wav_path = PathBuf::from(std::env::var_os("AMBER_TEST_ONNX_WAV").expect("set AMBER_TEST_ONNX_WAV to the public JFK fixture"));
        // Public fixture: https://github.com/cjpais/transcribe-rs/blob/abc6f582967c40839c1d05ec5acc98d1068d9d58/samples/jfk.wav
        assert_eq!(amber_core::local::sha256_file(&wav_path).unwrap(), "59dfb9a4acb36fe2a2affc14bacbee2920ff435cb13cc314a08c13f66ba7860e", "use the pinned public JFK fixture, not user audio");
        let wav = std::fs::read(wav_path).unwrap();
        let (samples, rate) = amber_core::audio::decode_wav(&wav).unwrap();
        println!("Public JFK audio: {:.3}s, {rate} Hz", samples.len() as f64 / rate as f64);
        let model = onnx::model("moonshine-tiny-int8").unwrap();
        if std::env::var("AMBER_TEST_ONNX_DOWNLOAD").as_deref() == Ok("1") {
            let archive = PathBuf::from(std::env::var_os("AMBER_TEST_ORT_ARCHIVE").expect("set AMBER_TEST_ORT_ARCHIVE to the existing official package; runtime downloads are not permitted by this test"));
            let cancel = AtomicBool::new(false);
            let started = Instant::now();
            onnx::install_runtime(&archive, &layout, &cancel).unwrap();
            onnx::verify_runtime(&layout).unwrap();
            println!("Runtime installed from existing archive: {:?}", started.elapsed());
            let started = Instant::now();
            let mut last_report = 0;
            onnx::download(&layout, model, &cancel, &mut |done, total| {
                if done - last_report >= 5_000_000 || Some(done) == total {
                    println!("Moonshine Tiny download: {done}/{} bytes", total.unwrap_or(0));
                    last_report = done;
                }
            }).unwrap();
            println!("Verified model download: {:?}", started.elapsed());
            assert!(!onnx::model_dir(&layout, model).with_extension("new").exists());
        }
        onnx::verify_runtime(&layout).unwrap();
        onnx::verify(&layout, model).unwrap();
        let speech = Arc::new(Speech::new(layout));
        let options = TranscribeOptions::new("en", "");
        for pass in 1..=2 {
            assert_eq!(speech.running_model(), None);
            let started = Instant::now();
            speech.load(model.id, &CancellationToken::default()).unwrap();
            println!("Load {pass}: {:?}", started.elapsed());
            assert!(speech.is_ready(model.id));
            let engine = speech.engine(model.id).unwrap();
            let started = Instant::now();
            let text = engine.transcribe(&wav, &options).unwrap();
            println!("Transcript {pass} ({:?}): {text}", started.elapsed());
            let normalized = text.to_ascii_lowercase().split(|c: char| !c.is_ascii_alphanumeric())
                .filter(|word| !word.is_empty()).collect::<Vec<_>>().join(" ");
            for phrase in ["fellow americans", "ask not what your country can do for you", "ask what you can do for your country"] {
                assert!(normalized.contains(phrase), "missing {phrase:?} in {text:?}");
            }
            let started = Instant::now();
            speech.unload();
            println!("Unload {pass}: {:?}", started.elapsed());
            assert_eq!(speech.running_model(), None);
            let error = engine.transcribe(&wav, &options).unwrap_err();
            assert_eq!(error.kind, ErrorKind::Unreachable);
            println!("Stale handle {pass}: {:?}: {}", error.kind, error.detail);
            assert_eq!(speech.running_model(), None, "a stale handle must not reload the model");
        }
    }
}
