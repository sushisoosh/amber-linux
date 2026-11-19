//! Blocking in-process Whisper recognition shared by the server and Linux desktop.
//! Call only from a dedicated worker or blocking task, never a UI or audio callback.
//! Loading, switching and decoding share one lock and keep at most one model in memory.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, Once};

use amber_core::engine::{CancellationToken, ErrorKind, SttError, TranscribeOptions, Transcriber};
use amber_core::local::{self, Model};
use amber_core::imports::{self, SpeechModel};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperState};

pub struct Speech {
    dir: PathBuf,
    threads: AtomicU32,
    gpu: bool,
    /// The model in memory, by id. Held during a transcription, so a switch never pulls the
    /// model out from under one.
    loaded: Mutex<Option<Loaded>>,
}

/// A model with its one state, kept across requests: whisper.cpp detects the language with
/// the encoder context the state's previous run left - a fresh state means a full 30 s pass.
struct Loaded {
    id: String,
    /// Kept alive for the state.
    _ctx: WhisperContext,
    state: WhisperState,
}

impl Speech {
    pub fn new(dir: PathBuf, threads: u32, gpu: bool) -> Speech {
        // whisper.cpp reports every load and run on stderr; routed into `log`, which stays quiet.
        static QUIET: Once = Once::new();
        QUIET.call_once(whisper_rs::install_logging_hooks);
        Speech { dir, threads: AtomicU32::new(threads), gpu, loaded: Mutex::new(None) }
    }

    /// The engine for one request: [requested] if it is a downloaded model, else [default].
    pub fn engine(self: &Arc<Self>, requested: Option<&str>, default: &str) -> Result<Box<dyn Transcriber>, String> {
        let id = chosen(&self.dir, requested, default);
        let model = local::model(&id).ok_or_else(|| format!("unknown model {id}"))?;
        if !model_ready(&self.dir, model) {
            return Err(format!("The speech model {id} is not downloaded."));
        }
        self.engine_model(SpeechModel::whisper(&self.dir, model))
    }

    pub fn engine_model(self: &Arc<Self>, model: SpeechModel) -> Result<Box<dyn Transcriber>, String> {
        if model.is_onnx() { return Err("Not a Whisper model.".into()); }
        Ok(Box::new(Engine { speech: self.clone(), model }))
    }

    /// Applies to the next request without replacing the shared model owner.
    pub fn set_threads(&self, threads: u32) {
        self.threads.store(threads, Ordering::Relaxed);
    }

    /// Preloads a model. Native loading itself cannot be interrupted; cancellation is checked
    /// before and after it, and the lock remains held until native work really finishes.
    pub fn load(&self, id: &str, cancel: &CancellationToken) -> Result<(), SttError> {
        cancel.check()?;
        let model = local::model(id).ok_or_else(|| failure("Unknown speech model.", id))?;
        self.load_model(&SpeechModel::whisper(&self.dir, model), cancel)
    }

    pub fn load_model(&self, model: &SpeechModel, cancel: &CancellationToken) -> Result<(), SttError> {
        cancel.check()?;
        if model.is_onnx() { return Err(failure("Not a Whisper model.", &model.id)); }
        let threads = params(&TranscribeOptions::default(), 0, model, self.threads.load(Ordering::Relaxed)).threads;
        let mut loaded = self.loaded.lock().map_err(|_| failure("Restart Amber to recover the speech engine.", "model lock poisoned"))?;
        self.load_locked(&mut loaded, model, threads, cancel)?;
        Ok(())
    }

    fn load_locked(&self, loaded: &mut Option<Loaded>, model: &SpeechModel, threads: i32, cancel: &CancellationToken) -> Result<(), SttError> {
        cancel.check()?;
        if loaded.as_ref().is_none_or(|l| l.id != model.id) {
            if model.imported {
                let layout = local::Layout { root: self.dir.parent().unwrap_or(&self.dir).to_path_buf() };
                imports::verify(&layout, model).map_err(|e| failure("The imported speech model is missing or damaged.", e.to_string()))?;
                cancel.check()?;
            }
            *loaded = None;
            let mut cp = WhisperContextParameters::default();
            cp.use_gpu(self.gpu);
            cp.flash_attn(self.gpu && !model.short_context);
            let ctx = WhisperContext::new_with_params(&model.path, cp);
            cancel.check()?;
            let ctx = ctx.map_err(|e| failure("The speech model could not be loaded.", e.to_string()))?;
            let mut state = ctx.create_state().map_err(|e| failure("The speech model could not start.", e.to_string()))?;
            if model.short_context {
                prime(&mut state, threads, cancel);
            }
            cancel.check()?;
            *loaded = Some(Loaded { id: model.id.to_string(), _ctx: ctx, state });
        }
        Ok(())
    }

    /// Waits for native work before releasing the model. Never call from the UI thread.
    pub fn unload(&self) {
        *self.loaded.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    /// Waits for an in-flight run before a model file can safely be removed.
    pub fn unload_model(&self, model: &str) -> bool {
        let mut loaded = self.loaded.lock().unwrap_or_else(|e| e.into_inner());
        if loaded.as_ref().is_some_and(|l| l.id == model) {
            *loaded = None;
            true
        } else {
            false
        }
    }

    /// A busy worker is not ready; these status probes never wait for native inference.
    pub fn running_model(&self) -> Option<String> {
        self.loaded.try_lock().ok()?.as_ref().map(|l| l.id.clone())
    }

    pub fn is_ready(&self, model: &str) -> bool {
        self.running_model().as_deref() == Some(model)
    }
}

fn failure(message: &str, detail: impl Into<String>) -> SttError {
    SttError::new(ErrorKind::Server, message, detail)
}

/// Downloads reach their final name only after checksum verification; `.part` is not ready.
pub fn model_ready(dir: &Path, model: &Model) -> bool {
    dir.join(model.file).is_file()
}

struct Engine {
    speech: Arc<Speech>,
    model: SpeechModel,
}

impl Transcriber for Engine {
    fn transcribe(&self, wav: &[u8], options: &TranscribeOptions) -> Result<String, SttError> {
        self.transcribe_cancellable(wav, options, &CancellationToken::default())
    }

    fn transcribe_detecting(&self, wav: &[u8], options: &TranscribeOptions) -> Result<(String, Option<String>), SttError> {
        self.transcribe_detecting_cancellable(wav, options, &CancellationToken::default())
    }

    fn transcribe_cancellable(&self, wav: &[u8], options: &TranscribeOptions, cancel: &CancellationToken) -> Result<String, SttError> {
        self.transcribe_detecting_cancellable(wav, options, cancel).map(|(text, _)| text)
    }

    fn transcribe_detecting_cancellable(
        &self,
        wav: &[u8],
        options: &TranscribeOptions,
        cancel: &CancellationToken,
    ) -> Result<(String, Option<String>), SttError> {
        cancel.check()?;
        let (samples, rate) = amber_core::audio::decode_wav(wav).map_err(|e| SttError::new(ErrorKind::BadResponse, "Not a WAV recording.", e))?;
        cancel.check()?;
        if samples.is_empty() {
            return Ok((String::new(), None));
        }
        if samples.iter().any(|sample| !sample.is_finite()) {
            return Err(SttError::new(ErrorKind::BadResponse, "The recording contains invalid audio.", "non-finite sample"));
        }
        let samples = amber_core::audio::resample(&samples, rate, amber_core::audio::TARGET_RATE);
        cancel.check()?;
        if self.model.imported { self.model.validate_request(options, samples.len())?; }
        let plan = params(options, samples.len(), &self.model, self.speech.threads.load(Ordering::Relaxed));

        let mut loaded = self.speech.loaded.lock().map_err(|_| failure("Restart Amber to recover the speech engine.", "model lock poisoned"))?;
        self.speech.load_locked(&mut loaded, &self.model, plan.threads, cancel)?;
        let state = &mut loaded.as_mut().expect("loaded above").state;

        let run = |state: &mut WhisperState, language: Option<&str>| -> Result<String, SttError> {
            cancel.check()?;
            let mut fp = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
            // No retries at higher temperatures: on noise and silence they run each 30 s window
            // up to five times over (a 10-minute recording took 3.5 min instead of 1 on a CPU).
            fp.set_temperature(0.0);
            fp.set_temperature_inc(0.0);
            // The explicit prompt belongs to this request; previous or aborted text does not.
            fp.set_no_context(true);
            fp.set_language(language);
            if let Some(prompt) = &plan.prompt {
                fp.set_initial_prompt(prompt);
            }
            if plan.audio_ctx > 0 {
                fp.set_audio_ctx(plan.audio_ctx as i32);
            }
            fp.set_n_threads(plan.threads);
            fp.set_no_timestamps(plan.no_timestamps);
            fp.set_print_progress(false);
            fp.set_print_realtime(false);
            fp.set_print_special(false);
            fp.set_print_timestamps(false);
            let result = full(state, fp, &samples, cancel);
            // Native abort can leave partial segments even when it reports success.
            cancel.check()?;
            result.map_err(|e| failure("Recognition failed.", e.to_string()))?;
            Ok(state.as_iter().filter_map(|s| s.to_str_lossy().ok().map(|t| t.into_owned())).collect::<Vec<_>>().join(""))
        };
        let text = if plan.among.len() > 1 {
            // Several languages: one run with detection, then the probabilities on this
            // dictation's own context (the detection before the run used the previous one's),
            // and again in the likeliest allowed language when Whisper chose another - short
            // German speech is easily taken for Turkish or English.
            let text = run(state, None)?;
            let detected = whisper_rs::get_lang_str(state.full_lang_id_from_state());
            let (_, probabilities) = state.lang_detect(0, plan.threads as usize).map_err(|e| failure("Recognition failed.", e.to_string()))?;
            cancel.check()?;
            let by_code: serde_json::Map<String, serde_json::Value> = probabilities
                .iter()
                .enumerate()
                .filter_map(|(id, p)| whisper_rs::get_lang_str(id as i32).map(|code| (code.to_string(), serde_json::json!(p))))
                .collect();
            let audio_ms = samples.len() as u64 * 1000 / amber_core::audio::TARGET_RATE as u64;
            let picked = amber_core::engine::pick_language(&serde_json::Value::Object(by_code), &plan.among, audio_ms);
            if detected == Some(picked.as_str()) {
                text
            } else {
                run(state, Some(&picked))?
            }
        } else {
            run(state, plan.language.as_deref())?
        };
        // Priming changes the state's language to English; capture this run's first.
        let language = whisper_rs::get_lang_str_full(state.full_lang_id_from_state()).map(str::to_string);
        if self.model.short_context && plan.audio_ctx == 0 {
            // A full window left the state at 30 s: the next short dictation's language
            // detection would pay for it.
            prime(state, plan.threads, cancel);
        }
        cancel.check()?;
        Ok((text, language))
    }
}

/// Leaves [state] at a short encoder context, as the desktop's `reset_context` does for its
/// whisper-server: 1.2 s of silence, the language given, a context of 256.
fn prime(state: &mut WhisperState, threads: i32, cancel: &CancellationToken) {
    let mut fp = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    fp.set_language(Some("en"));
    fp.set_no_context(true);
    fp.set_audio_ctx(256);
    fp.set_n_threads(threads);
    fp.set_no_timestamps(true);
    fp.set_print_progress(false);
    fp.set_print_realtime(false);
    fp.set_print_special(false);
    fp.set_print_timestamps(false);
    let _ = full(state, fp, &vec![0.0; amber_core::audio::TARGET_RATE as usize * 6 / 5], cancel);
}

unsafe extern "C" fn abort(data: *mut std::ffi::c_void) -> bool {
    // SAFETY: full() keeps the borrowed token alive until all native work returns.
    unsafe { &*data.cast::<CancellationToken>() }.is_cancelled()
}

fn full(state: &mut WhisperState, mut params: FullParams<'_, '_>, samples: &[f32], cancel: &CancellationToken) -> Result<(), whisper_rs::WhisperError> {
    // whisper-rs 0.16's safe abort wrapper miscasts its double-boxed userdata and leaks it.
    // SAFETY: the callback only reads an atomic; whisper_full_with_state is synchronous and
    // invokes it only during this call. No callback state is retained after native work ends.
    unsafe {
        params.set_abort_callback(Some(abort));
        params.set_abort_callback_user_data((cancel as *const CancellationToken).cast_mut().cast());
    }
    state.full(params, samples)
}

/// A language code whisper knows ("de-DE" -> "de"), or None for detection: "auto", empty, or
/// unknown - an unknown id would put a start token where the language token belongs.
pub fn whisper_language(value: &str) -> Option<String> {
    let code: String = value.split(['-', '_']).next().unwrap_or("").chars().filter(|c| *c != '\0').collect::<String>().trim().to_lowercase();
    if code.is_empty() || code == "auto" {
        return None;
    }
    whisper_rs::get_lang_id(&code).map(|_| code)
}

/// The languages in [value] ("de,en", "de-DE, en_US") whisper knows, each once, in order.
pub fn whisper_languages(value: &str) -> Vec<String> {
    let mut codes = Vec::new();
    for code in value.split(',').filter_map(whisper_language) {
        if !codes.contains(&code) {
            codes.push(code);
        }
    }
    codes
}

/// How one request is run.
#[derive(Debug, PartialEq)]
struct ParamsPlan {
    /// One language; None for detection - among [among] when that holds several.
    language: Option<String>,
    among: Vec<String>,
    prompt: Option<String>,
    /// Encoder positions; 0 for the model's full 30 s window.
    audio_ctx: u32,
    threads: i32,
    no_timestamps: bool,
}

/// How much of a prompt goes in: its end - whisper reads only the last ~223 tokens, and its
/// tokenizer slows down with the square of the length.
const PROMPT_CHARS: usize = 800;

fn params(options: &TranscribeOptions, samples: usize, m: &SpeechModel, threads: u32) -> ParamsPlan {
    let cores = std::thread::available_parallelism().map_or(4, |n| n.get() as u32);
    let threads = if threads == 0 { local::default_threads() } else { threads }.min(cores).max(1);
    let prompt = options.prompt.as_deref().map(|p| {
        let clean: Vec<char> = p.chars().filter(|c| *c != '\0').collect();
        clean[clean.len().saturating_sub(PROMPT_CHARS)..].iter().collect::<String>()
    });
    let languages = options.language.as_deref().map(whisper_languages).unwrap_or_default();
    ParamsPlan {
        language: (languages.len() == 1).then(|| languages[0].clone()),
        among: if languages.len() > 1 { languages } else { Vec::new() },
        prompt: prompt.filter(|p| !p.trim().is_empty()),
        // Only tuned (ACFT) models may stop where the speech stops; a stock model loses words.
        audio_ctx: if m.short_context { local::audio_ctx_for(samples, m.context_slack) } else { 0 },
        threads: threads as i32,
        no_timestamps: true,
    }
}

/// [requested] if it is a downloaded model of the catalog, else [default].
fn chosen(dir: &Path, requested: Option<&str>, default: &str) -> String {
    // An explicit custom selection must fail closed, never run the default instead.
    if let Some(id) = requested.filter(|id| id.starts_with("import-")) { return id.into(); }
    requested.and_then(local::model).filter(|m| model_ready(dir, m)).map_or_else(|| default.to_string(), |m| m.id.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use amber_core::engine::TranscribeOptions;

    fn params(options: &TranscribeOptions, samples: usize, m: &Model, threads: u32) -> ParamsPlan {
        super::params(options, samples, &SpeechModel::whisper(Path::new("."), m), threads)
    }

    #[test]
    fn params_follow_the_model_and_the_request() {
        let tuned = amber_core::local::model("futo-multilingual-244").unwrap();
        let p = params(&TranscribeOptions::new("de", "Tauri"), 16_000 * 3, tuned, 4);
        assert_eq!(p.language.as_deref(), Some("de"));
        assert_eq!(p.prompt.as_deref(), Some("Tauri"));
        assert!(p.audio_ctx > 0 && p.audio_ctx < 1500, "a tuned model stops where the speech stops: {}", p.audio_ctx);
        assert!(p.no_timestamps);
        assert_eq!(p.threads, 4.min(std::thread::available_parallelism().unwrap().get() as i32));
        let stock = amber_core::local::model("large-v3-turbo-q5_0").unwrap();
        let p = params(&TranscribeOptions::new("auto", ""), 16_000 * 3, stock, 0);
        assert_eq!(p.audio_ctx, 0, "a stock model keeps its 30 s window");
        assert_eq!(p.language, None);
        assert!((1..=8).contains(&p.threads), "0: half the logical cores, capped by available cores: {}", p.threads);
    }

    #[test]
    fn a_language_list_is_kept_to_choose_among() {
        let m = local::model("futo-multilingual-74").unwrap();
        let p = params(&TranscribeOptions::new("de,en", ""), 16_000, m, 4);
        assert_eq!((p.language, p.among), (None, vec!["de".to_string(), "en".to_string()]));
        // A code whisper does not know falls away; one left is a fixed language.
        let p = params(&TranscribeOptions::new("de,xx", ""), 16_000, m, 4);
        assert_eq!((p.language.as_deref(), p.among.len()), (Some("de"), 0));
    }

    #[test]
    fn languages_prompts_and_threads_are_made_safe() {
        let m = amber_core::local::model("futo-multilingual-244").unwrap();
        let lang = |l: &str| params(&TranscribeOptions { language: Some(l.into()), prompt: None }, 16_000, m, 4).language;
        assert_eq!(lang("de-DE").as_deref(), Some("de"));
        assert_eq!(lang("Auto"), None);
        assert_eq!(lang("xx"), None, "not a language whisper knows: detected instead");
        assert_eq!(lang("de\0"), Some("de".into()));
        // The prompt: no NUL (whisper-rs would panic), and only its end - whisper reads ~223 tokens.
        let long = format!("{}Ende\0", "a".repeat(10_000));
        let p = params(&TranscribeOptions { language: None, prompt: Some(long) }, 16_000, m, 4).prompt.unwrap();
        assert!(p.chars().count() <= 800 && p.ends_with("Ende") && !p.contains('\0'));
        // Threads: never more than the machine has (an absurd count aborts whisper.cpp).
        let cores = std::thread::available_parallelism().unwrap().get() as i32;
        assert!(params(&TranscribeOptions::default(), 16_000, m, 100_000).threads <= cores);
        assert!(params(&TranscribeOptions::default(), 16_000, m, u32::MAX).threads >= 1);
    }

    #[test]
    fn an_unknown_or_missing_model_gets_the_default() {
        let dir = tempfile::tempdir().unwrap();
        let default = "futo-multilingual-74";
        std::fs::write(dir.path().join(amber_core::local::model(default).unwrap().file), b"x").unwrap();
        assert_eq!(chosen(dir.path(), Some("large-v3-q5_0"), default), default, "not downloaded");
        assert_eq!(chosen(dir.path(), Some("whisper-1"), default), default, "not a model at all");
        assert_eq!(chosen(dir.path(), None, default), default);
        let other = amber_core::local::model("futo-multilingual-244").unwrap();
        std::fs::write(dir.path().join(other.file), b"x").unwrap();
        assert_eq!(chosen(dir.path(), Some(other.id), default), other.id, "downloaded: taken");
        let imported = "import-0123456789abcdef0123456789abcdef";
        assert_eq!(chosen(dir.path(), Some(imported), default), imported, "imports never silently fall back");
        assert!(Arc::new(Speech::new(dir.path().into(), 0, false)).engine(Some(imported), default).is_err());
    }

    #[test]
    fn speech_is_shared_across_threads() {
        fn shared<T: Send + Sync>() {}
        shared::<Speech>();
    }

    #[test]
    fn native_abort_callback_reads_the_live_request_token() {
        let cancel = CancellationToken::default();
        let data = (&cancel as *const CancellationToken).cast_mut().cast();
        // SAFETY: exactly the pointer and lifetime supplied by full(), with no model needed.
        assert!(!unsafe { abort(data) });
        let other_thread = cancel.clone();
        std::thread::spawn(move || other_thread.cancel()).join().unwrap();
        assert!(unsafe { abort(data) });
    }

    #[test]
    fn cancelled_requests_never_decode_or_load_a_model() {
        let dir = tempfile::tempdir().unwrap();
        let model = local::model("futo-multilingual-74").unwrap();
        std::fs::write(dir.path().join(model.file), b"not a model").unwrap();
        let speech = Arc::new(Speech::new(dir.path().into(), 0, false));
        let engine = speech.engine(None, model.id).unwrap();
        let cancel = CancellationToken::default();
        cancel.cancel();
        let error = engine.transcribe_detecting_cancellable(b"not a WAV", &TranscribeOptions::default(), &cancel).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Cancelled);
        assert_eq!(speech.load(model.id, &cancel).unwrap_err().kind, ErrorKind::Cancelled);
        assert_eq!(speech.running_model(), None);
    }

    #[test]
    fn cancellation_survives_waiting_for_the_model_lock() {
        let dir = tempfile::tempdir().unwrap();
        let speech = Arc::new(Speech::new(dir.path().into(), 0, false));
        let loaded = speech.loaded.lock().unwrap();
        let cancel = CancellationToken::default();
        let worker = {
            let speech = speech.clone();
            let cancel = cancel.clone();
            std::thread::spawn(move || speech.load("futo-multilingual-74", &cancel))
        };
        assert!(!speech.is_ready("futo-multilingual-74"), "a status probe must not wait on the worker");
        cancel.cancel();
        drop(loaded);
        assert_eq!(worker.join().unwrap().unwrap_err().kind, ErrorKind::Cancelled);
        assert_eq!(speech.running_model(), None);
    }

    #[test]
    fn partial_downloads_cannot_be_selected() {
        let dir = tempfile::tempdir().unwrap();
        let model = local::model("futo-multilingual-74").unwrap();
        std::fs::write(dir.path().join(model.file).with_extension("part"), b"partial").unwrap();
        assert!(!model_ready(dir.path(), model));
        let speech = Arc::new(Speech::new(dir.path().into(), 0, false));
        assert!(speech.engine(None, model.id).is_err());
        assert!(speech.engine(None, "unknown").is_err());
    }

    #[test]
    fn empty_or_invalid_recordings_do_not_load_a_model() {
        let dir = tempfile::tempdir().unwrap();
        let model = local::model("futo-multilingual-74").unwrap();
        std::fs::write(dir.path().join(model.file), b"not a model").unwrap();
        let speech = Arc::new(Speech::new(dir.path().into(), 0, false));
        let engine = speech.engine(None, model.id).unwrap();
        let options = TranscribeOptions::default();
        assert_eq!(engine.transcribe(b"broken", &options).unwrap_err().kind, ErrorKind::BadResponse);
        let empty = amber_core::audio::encode_wav(&[], 16_000);
        assert_eq!(engine.transcribe_detecting(&empty, &options).unwrap(), (String::new(), None));
        let mut invalid_float = amber_core::audio::encode_wav(&[0.0, 0.0], 16_000);
        invalid_float[20..22].copy_from_slice(&3u16.to_le_bytes());
        invalid_float[28..32].copy_from_slice(&64_000u32.to_le_bytes());
        invalid_float[32..34].copy_from_slice(&4u16.to_le_bytes());
        invalid_float[34..36].copy_from_slice(&32u16.to_le_bytes());
        invalid_float[44..48].copy_from_slice(&f32::NAN.to_le_bytes());
        assert_eq!(engine.transcribe(&invalid_float, &options).unwrap_err().kind, ErrorKind::BadResponse);
        assert_eq!(speech.running_model(), None);
        speech.unload();
        assert!(!speech.unload_model(model.id));
    }

    #[test]
    fn language_lists_are_normalized_without_unknown_codes() {
        assert_eq!(whisper_languages("de-DE, en_US, de, xx, auto, \0fr"), ["de", "en", "fr"]);
        assert!(whisper_languages("auto, xx, ,\0").is_empty());
    }

    #[test]
    #[ignore = "requires an existing futo-multilingual-74 in AMBER_TEST_MODEL_DIR"]
    fn a_short_dictation_after_a_long_one_stays_quick() {
        let cache = PathBuf::from(std::env::var_os("AMBER_TEST_MODEL_DIR").expect("set AMBER_TEST_MODEL_DIR to already downloaded models"));
        let m = amber_core::local::model("futo-multilingual-74").unwrap();
        assert!(model_ready(&cache, m));
        let speech = std::sync::Arc::new(Speech::new(cache, 0, false));
        let engine = speech.engine(None, m.id).unwrap();
        let wav = |secs: usize| amber_core::audio::encode_wav(&vec![0.0; 16_000 * secs], 16_000);
        engine.transcribe(&wav(40), &TranscribeOptions::new("auto", "")).unwrap();
        let t = std::time::Instant::now();
        engine.transcribe(&wav(3), &TranscribeOptions::new("auto", "")).unwrap();
        let auto = t.elapsed();
        let t = std::time::Instant::now();
        engine.transcribe(&wav(3), &TranscribeOptions::new("de", "")).unwrap();
        let fixed = t.elapsed();
        eprintln!("3 s after 40 s: auto {auto:?}, de {fixed:?}");
        assert!(auto < fixed * 2 + std::time::Duration::from_millis(300), "detection ran a full window: {auto:?} vs {fixed:?}");
    }

    #[test]
    #[ignore = "requires an existing futo-multilingual-74 in AMBER_TEST_MODEL_DIR"]
    fn ten_minutes_of_silence() {
        let cache = PathBuf::from(std::env::var_os("AMBER_TEST_MODEL_DIR").expect("set AMBER_TEST_MODEL_DIR to already downloaded models"));
        let m = amber_core::local::model("futo-multilingual-74").unwrap();
        assert!(model_ready(&cache, m));
        let speech = std::sync::Arc::new(Speech::new(cache, 0, false));
        let engine = speech.engine(None, m.id).unwrap();
        let wav = amber_core::audio::encode_wav(&vec![0.0; 16_000 * 600], 16_000);
        let started = std::time::Instant::now();
        let text = engine.transcribe(&wav, &TranscribeOptions::new("de", "")).unwrap();
        assert!(text.trim().len() < 200, "silence, not a novel: {text}");
        assert!(started.elapsed().as_secs() < 60, "took {:?}", started.elapsed());
    }
}
