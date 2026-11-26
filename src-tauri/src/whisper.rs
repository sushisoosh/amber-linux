//! The local engine: whisper.cpp's `whisper-server.exe`, started with the chosen model on a
//! free port on 127.0.0.1. It keeps the model in memory between dictations; see `process`
//! for how it is tied to the app.

use std::process::{Child, Command};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::process::{self, Output};

use amber_core::local::{self, Layout};
use amber_core::imports::{self, SpeechModel};
use amber_core::engine::{CancellationToken, ErrorKind, SttError, TranscribeOptions, Transcriber, WhisperServerTranscriber};
use serde::Serialize;

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    /// "stopped", "starting", "ready", "failed"
    pub state: &'static str,
    pub message: Option<String>,
}

impl Status {
    pub fn of(state: &'static str, message: Option<String>) -> Self {
        Status { state, message }
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
struct Key {
    runtime: String,
    model: String,
    threads: u32,
}

struct Running {
    child: Child,
    /// "http://127.0.0.1:<port>/<secret>": whisper-server has no key of its own, and any
    /// program or other account on this PC - or a web page - could otherwise use it, or make
    /// it load a file of their choosing. The random path prefix keeps them out.
    base: String,
    key: Key,
    model: SpeechModel,
    effective_runtime: String,
    threads: u32,
}

impl Running {
    fn served(&self, running: Arc<Mutex<Option<Running>>>, changes: Arc<Mutex<()>>) -> Served {
        let slack = self.model.short_context.then_some(self.model.context_slack);
        let transcriber = Box::new(SnapshotEngine {
            running, changes, base: self.base.clone(), transcriber: WhisperServerTranscriber::new(&self.base, slack),
            model: self.model.clone(),
        });
        Served { transcriber, runtime: self.effective_runtime.clone(), model: self.key.model.clone(), threads: self.threads }
    }
}

/// The server a request goes to, and what it runs - which may still be the previous model
/// while the chosen one downloads, so the request has to fit the running one.
pub struct Served {
    pub transcriber: Box<dyn Transcriber>,
    pub runtime: String,
    pub model: String,
    pub threads: u32,
}

#[derive(Default)]
pub struct Whisper {
    running: Arc<Mutex<Option<Running>>>,
    onnx: OnceLock<Arc<amber_onnx::Speech>>,
    changes: Arc<Mutex<()>>,
    /// The last lines the server printed - the reason when it dies.
    output: Output,
    status: Mutex<Option<Status>>,
}

/// How long a model may take to load - large-v3 on a slow CPU from a slow disk.
const START_TIMEOUT: Duration = Duration::from_secs(120);

impl Whisper {
    pub fn status(&self) -> Status {
        self.status.lock().unwrap().clone().unwrap_or(Status::of("stopped", None))
    }

    fn set_status(&self, app: &tauri::AppHandle, status: Status) {
        use tauri::Emitter;
        *self.status.lock().unwrap() = Some(status.clone());
        let _ = app.emit("local-status", status);
    }

    /// Returns the address of a server running the configured model, starting (or
    /// restarting) it first if needed. Blocks while the model loads.
    ///
    /// Reads the settings only once it holds the lock, so two quick changes in a row end with
    /// the newer one loaded. A running server is only replaced once its successor is fully
    /// downloaded - choosing a model that is still on its way keeps the old one working.
    pub fn ensure(&self, app: &tauri::AppHandle) -> Result<Served, String> {
        self.ensure_model(app, None)
    }

    /// Like [ensure], with [model] instead of the configured one - what the phone chose. One
    /// model at a time: the PC's own dictations load theirs again on their next use.
    pub fn ensure_model(&self, app: &tauri::AppHandle, model: Option<&str>) -> Result<Served, String> {
        self.ensure_with(app, model, None)
    }

    pub fn ensure_snapshot(&self, app: &tauri::AppHandle, settings: &amber_core::settings::LocalSettings) -> Result<Served, String> {
        self.ensure_with(app, Some(&settings.model), Some(settings))
    }

    fn ensure_with(&self, app: &tauri::AppHandle, model: Option<&str>, snapshot: Option<&amber_core::settings::LocalSettings>) -> Result<Served, String> {
        // Tests and phone requests also change the resident model shown by load/unload UI.
        let _change = self.changes.lock().unwrap();
        crate::imports::report_load_failure(self.ensure_locked(app, model, snapshot), |message| {
            self.set_status(app, Status::of("failed", Some(message.into())));
        })
    }

    fn ensure_locked(&self, app: &tauri::AppHandle, model: Option<&str>, snapshot: Option<&amber_core::settings::LocalSettings>) -> Result<Served, String> {
        use tauri::Manager;
        let mut running = self.running.lock().unwrap();
        let core = app.state::<crate::state::Core>();
        let layout = &core.paths.layout;
        let settings = snapshot.cloned().unwrap_or_else(|| core.settings().local);
        let key = Key { runtime: settings.runtime.clone(), model: model.unwrap_or(&settings.model).to_string(), threads: settings.threads };
        let model = imports::resolve(layout, &key.model).map_err(|e| e.to_string())?;

        if model.is_onnx() {
            if !imports::installed(layout, &model) {
                let message = format!("The model {} is missing or incomplete. Download or import it in the settings.", model.label);
                return Err(message);
            }
            if let Some(mut r) = running.take() { process::stop(&mut r.child); }
            let speech = self.onnx.get_or_init(|| Arc::new(amber_onnx::Speech::new(layout.clone())));
            if !speech.is_ready(&model.id) {
                self.set_status(app, Status::of("starting", Some(format!("Loading {}...", model.label))));
            }
            speech.load(&model.id, &CancellationToken::default()).map_err(|error| error.message)?;
            let transcriber = Box::new(OnnxEngine { transcriber: speech.engine(&model.id)?, changes: self.changes.clone() });
            self.set_status(app, Status::of("ready", Some(format!("{} on CPU (ONNX)", model.label))));
            return Ok(Served { transcriber, runtime: "onnx-cpu".into(), model: model.id.into(), threads: 0 });
        }

        let rt = local::runtime(&key.runtime).ok_or("Unknown runtime.")?;
        let missing = if !layout.runtime_installed(rt) {
            Some(format!("The local engine ({}) is not installed yet - download it in the settings.", rt.label))
        } else if !imports::installed(layout, &model) {
            Some(format!("The model {} is not downloaded yet - get it in the settings.", model.label))
        } else {
            None
        };

        if let Some(r) = running.as_mut() {
            let alive = matches!(r.child.try_wait(), Ok(None));
            if alive && (r.key == key && !(model.imported && missing.is_some()) || missing.is_some() && !model.imported) {
                // The configured one, or the previous one while the new one is not there yet.
                return Ok(r.served(self.running.clone(), self.changes.clone()));
            }
            if !alive {
                crate::diag::warn(format!("local engine had exited: {}", self.last_output()));
            }
            process::stop(&mut r.child);
            *running = None;
        }
        if let Some(msg) = missing {
            return Err(msg);
        }
        if let Some(speech) = self.onnx.get() { speech.unload(); }
        if model.imported { imports::verify(layout, &model).map_err(|e| e.to_string())?; }

        self.set_status(app, Status::of("starting", Some(format!("Loading {}...", model.label))));
        // A cleanup model on the graphics card may have taken what is free there; the speech
        // model comes first - without it dictation itself breaks - and the other one reloads
        // after it, fitting itself into what is left.
        let moved = rt.id != "cpu" && core.llm.make_room_on_gpu(app);
        let started = Instant::now();
        let spawned = self.spawn(layout, rt, &model, &key);
        if moved {
            core.llm.warm(app, true);
        }
        match spawned {
            Ok(mut r) => {
                let gpu = if rt.id == "cuda" { self.gpu_in_use() } else { None };
                r.effective_runtime = match (rt.id, gpu) {
                    ("cuda", Some(false)) => "cuda-cpu".into(),
                    ("cuda", None) => "cuda-unknown".into(),
                    _ => rt.id.into(),
                };
                let served = r.served(self.running.clone(), self.changes.clone());
                if model.short_context {
                    // Leaves the server at a short context for its first language detection
                    // (whisper.cpp detects with the previous request's context), and warms the
                    // GPU up.
                    amber_core::engine::WhisperServerTranscriber::new(&r.base, Some(model.context_slack)).reset_context();
                }
                crate::diag::info(format!("local engine ready: {} on {} in {} ms", model.label, rt.label, started.elapsed().as_millis()));
                *running = Some(r);
                let message = if gpu == Some(false) {
                    // whisper.cpp falls back to the CPU without complaint; say it out loud.
                    crate::diag::warn(format!("GPU not used, running on the CPU: {}", self.last_output()));
                    format!("{} - running on the CPU: the GPU could not be used (update the NVIDIA driver?)", model.label)
                } else {
                    if rt.id == "cuda" && gpu.is_none() {
                        crate::diag::info("local engine did not report its backend");
                    }
                    format!("{} on {}", model.label, rt.label)
                };
                self.set_status(app, Status::of("ready", Some(message)));
                Ok(served)
            }
            Err(e) => {
                crate::diag::error(format!("local engine failed to start: {e}"));
                Err(e)
            }
        }
    }

    fn spawn(&self, layout: &Layout, rt: &local::Runtime, model: &SpeechModel, key: &Key) -> Result<Running, String> {
        let port = process::free_port().map_err(|e| format!("No free port: {e}"))?;
        let threads = if key.threads == 0 { process::default_threads() } else { key.threads };
        let mut cmd = Command::new(layout.server_exe(rt));
        // whisper-server reads its arguments in the system code page; a user folder with other
        // characters would arrive mangled. So it runs in the models folder and gets only the
        // file name - its DLLs still load from the exe's own folder.
        let model_path = &model.path;
        let path = format!("/{}", amber_core::api::new_key());
        cmd.current_dir(model_path.parent().unwrap_or(&layout.root)).args(server_args(model, port, threads, &path));
        let child = process::spawn(cmd, &self.output).map_err(|e| format!("The local engine could not be started: {e}"))?;

        // Not the open port: this whisper-server opens it before the model has loaded and
        // answers 503 until then.
        let base = format!("http://127.0.0.1:{port}{path}");
        let deadline = Instant::now() + START_TIMEOUT;
        let mut running = Running { child, base: base.clone(), key: key.clone(), model: model.clone(), effective_runtime: key.runtime.clone(), threads };
        loop {
            if let Ok(Some(status)) = running.child.try_wait() {
                return Err(format!("The local engine stopped while loading ({status}). {}", self.explain()));
            }
            if amber_core::engine::local_server_ready(&base) {
                return Ok(running);
            }
            if Instant::now() > deadline {
                process::stop(&mut running.child);
                return Err("The local engine took too long to load the model.".into());
            }
            std::thread::sleep(Duration::from_millis(150));
        }
    }

    /// The server prints its backend while loading, before it opens the port; the reader
    /// threads may lag a moment behind.
    fn gpu_in_use(&self) -> Option<bool> {
        for _ in 0..60 {
            let out = self.output.lock().unwrap();
            if let Some(answer) = local::gpu_in_use(out.iter().map(String::as_str)) {
                if let Some(line) = out.iter().find(|l| l.contains("CUDA0") || l.contains("no GPU found")) {
                    crate::diag::info(format!("local engine: {}", line.trim()));
                }
                return Some(answer);
            }
            drop(out);
            std::thread::sleep(Duration::from_millis(50));
        }
        None
    }

    fn last_output(&self) -> String {
        process::last_lines(&self.output, 6)
    }

    /// The server's last words, turned into advice where they are recognizable.
    fn explain(&self) -> String {
        let out = self.last_output();
        let lower = out.to_lowercase();
        if lower.contains("cuda") && (lower.contains("error") || lower.contains("failed") || lower.contains("no device")) {
            format!("The GPU could not be used - update the NVIDIA driver or switch to the CPU engine. ({out})")
        } else if lower.contains("out of memory") || lower.contains("failed to allocate") {
            format!("Not enough memory for this model - pick a smaller one. ({out})")
        } else {
            out
        }
    }

    pub fn stop(&self, app: &tauri::AppHandle) {
        let _change = self.changes.lock().unwrap();
        if let Some(mut r) = self.running.lock().unwrap().take() {
            process::stop(&mut r.child);
            crate::diag::info("local engine stopped");
        }
        if let Some(speech) = self.onnx.get() { speech.unload(); }
        self.set_status(app, Status::of("stopped", None));
    }

    /// The model the server runs right now, without waiting on a load in progress.
    pub fn running_model(&self) -> Option<String> {
        let _change = self.changes.try_lock().ok()?;
        self.running.try_lock().ok()?.as_ref().map(|r| r.key.model.clone())
            .or_else(|| self.onnx.get().and_then(|speech| speech.running_model()))
    }

    /// Stops the server only if it runs from [runtime] - before that runtime's files are
    /// replaced or deleted.
    pub fn stop_runtime(&self, app: &tauri::AppHandle, runtime: &str) {
        let _change = self.changes.lock().unwrap();
        let mut running = self.running.lock().unwrap();
        if running.as_ref().is_some_and(|r| r.key.runtime == runtime) {
            if let Some(mut r) = running.take() { process::stop(&mut r.child); }
            self.set_status(app, Status::of("stopped", None));
        }
        if runtime == "onnx-cpu" {
            if let Some(speech) = self.onnx.get() { speech.unload(); }
            self.set_status(app, Status::of("stopped", None));
        }
    }

    /// Stops the server only if it has [model] loaded - before the file is deleted.
    pub fn stop_model(&self, app: &tauri::AppHandle, model: &str) {
        let _change = self.changes.lock().unwrap();
        let mut running = self.running.lock().unwrap();
        if running.as_ref().is_some_and(|r| r.key.model == model) {
            if let Some(mut r) = running.take() { process::stop(&mut r.child); }
            self.set_status(app, Status::of("stopped", None));
        }
        if self.onnx.get().is_some_and(|speech| speech.unload_model(model)) {
            self.set_status(app, Status::of("stopped", None));
        }
    }

    pub fn remove_imported(&self, app: &tauri::AppHandle, removal: imports::Removal<'_>) -> Result<(), String> {
        let _change = self.changes.lock().unwrap();
        let id = removal.id();
        let mut running = self.running.lock().unwrap();
        let mut stopped = false;
        if running.as_ref().is_some_and(|r| r.key.model == id) {
            if let Some(mut r) = running.take() { process::stop(&mut r.child); stopped = true; }
        }
        if self.onnx.get().is_some_and(|s| s.unload_model(id)) { stopped = true; }
        if stopped { self.set_status(app, Status::of("stopped", None)); }
        removal.remove().map_err(|e| e.to_string())
    }

    /// The server died and the next dictation will restart it - or it is running fine.
    /// Never waits: while another thread loads a model (and holds the lock), the answer is
    /// simply "not ready".
    /// Whether a server with [model] runs - not one the phone switched to.
    pub fn is_ready(&self, model: &str) -> bool {
        let Ok(_change) = self.changes.try_lock() else { return false };
        if self.onnx.get().is_some_and(|speech| speech.is_ready(model)) { return true; }
        match self.running.try_lock() {
            Ok(mut running) => running.as_mut().map(|r| r.key.model == model && matches!(r.child.try_wait(), Ok(None))).unwrap_or(false),
            Err(_) => false,
        }
    }
}

struct SnapshotEngine {
    running: Arc<Mutex<Option<Running>>>,
    changes: Arc<Mutex<()>>,
    base: String,
    transcriber: WhisperServerTranscriber,
    model: SpeechModel,
}

impl Transcriber for SnapshotEngine {
    fn transcribe(&self, wav: &[u8], options: &TranscribeOptions) -> Result<String, SttError> {
        self.transcribe_cancellable(wav, options, &CancellationToken::default())
    }

    fn transcribe_cancellable(&self, wav: &[u8], options: &TranscribeOptions, cancel: &CancellationToken) -> Result<String, SttError> {
        self.transcribe_detecting_cancellable(wav, options, cancel).map(|(text, _)| text)
    }

    fn transcribe_detecting(&self, wav: &[u8], options: &TranscribeOptions) -> Result<(String, Option<String>), SttError> {
        self.transcribe_detecting_cancellable(wav, options, &CancellationToken::default())
    }

    fn transcribe_detecting_cancellable(&self, wav: &[u8], options: &TranscribeOptions, cancel: &CancellationToken) -> Result<(String, Option<String>), SttError> {
        cancel.check()?;
        let _change = self.changes.lock().unwrap();
        with_snapshot(&self.running, &self.base, |r| &r.base, cancel, || {
            if self.model.imported { self.model.validate_request(options, 0)?; }
            self.transcriber.transcribe_detecting_cancellable(wav, options, cancel)
        })
    }
}

struct OnnxEngine {
    transcriber: Box<dyn Transcriber>,
    changes: Arc<Mutex<()>>,
}

impl Transcriber for OnnxEngine {
    fn transcribe(&self, wav: &[u8], options: &TranscribeOptions) -> Result<String, SttError> {
        self.transcribe_cancellable(wav, options, &CancellationToken::default())
    }

    fn transcribe_cancellable(&self, wav: &[u8], options: &TranscribeOptions, cancel: &CancellationToken) -> Result<String, SttError> {
        cancel.check()?;
        let _change = self.changes.lock().unwrap();
        cancel.check()?;
        self.transcriber.transcribe_cancellable(wav, options, cancel)
    }

    fn transcribe_detecting_cancellable(&self, wav: &[u8], options: &TranscribeOptions, cancel: &CancellationToken) -> Result<(String, Option<String>), SttError> {
        self.transcribe_cancellable(wav, options, cancel).map(|text| (text, None))
    }
}

/// Stop/replacement needs this same mutex. The secret URL identifies a particular helper,
/// not just a model name or reusable TCP port. A change before lock acquisition is a retry,
/// never permission to send a request under stale model metadata.
fn with_snapshot<T, R>(running: &Mutex<Option<T>>, base: &str, identity: impl FnOnce(&T) -> &str, cancel: &CancellationToken, infer: impl FnOnce() -> Result<R, SttError>) -> Result<R, SttError> {
    cancel.check()?;
    let running = running.lock().unwrap();
    cancel.check()?;
    if !running.as_ref().is_some_and(|r| identity(r) == base) {
        return Err(SttError::new(ErrorKind::Unreachable, "The local model was unloaded or changed before recognition started. Try again.", "snapshot helper replaced"));
    }
    let result = infer();
    cancel.check()?;
    result
}

/// whisper-server's arguments. It runs in the models folder, so the model is its file name;
/// a tuned (short-context) model runs without flash attention ([local::flash_attention]).
fn server_args(model: &SpeechModel, port: u16, threads: u32, path: &str) -> Vec<String> {
    let mut args = vec!["--model".to_string(), model.path.file_name().expect("managed model filename").to_string_lossy().into_owned()];
    args.extend(["--host", "127.0.0.1", "--port", &port.to_string(), "--threads", &threads.to_string()].map(String::from));
    args.extend(["--request-path".to_string(), path.to_string()]);
    if model.short_context {
        args.push("--no-flash-attn".into());
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tuned_model_starts_without_flash_attention() {
        let tuned = local::model("acft-large-v3-turbo-q8_0").unwrap();
        let args = server_args(&SpeechModel::whisper(std::path::Path::new("."), tuned), 5000, 8, "/k");
        assert!(args.contains(&"--no-flash-attn".to_string()), "{args:?}");
        assert!(args.windows(2).any(|w| w == ["--model", tuned.file]) && args.windows(2).any(|w| w == ["--port", "5000"]));
        let stock = local::model("large-v3-turbo-q5_0").unwrap();
        assert!(!server_args(&SpeechModel::whisper(std::path::Path::new("."), stock), 5000, 8, "/k").contains(&"--no-flash-attn".to_string()), "the full window keeps it");
    }

    #[test]
    fn snapshot_pins_selected_helper_through_success_failure_and_cancellation() {
        for (fail, cancelled) in [(false, false), (true, false), (false, true)] {
            let running = Mutex::new(Some("actual-fallback-helper"));
            let cancel = CancellationToken::default();
            let result = with_snapshot(&running, "actual-fallback-helper", |r| *r, &cancel, || {
                assert!(running.try_lock().is_err(), "model replacement must wait for inference");
                if cancelled { cancel.cancel(); }
                if fail { Err(SttError::new(ErrorKind::Server, "test failure", "test failure")) } else { Ok("recognized") }
            });
            assert!(running.try_lock().is_ok(), "every request exit releases the helper");
            match (fail, cancelled) {
                (_, true) => assert_eq!(result.unwrap_err().kind, ErrorKind::Cancelled),
                (true, false) => assert_eq!(result.unwrap_err().kind, ErrorKind::Server),
                _ => assert_eq!(result.unwrap(), "recognized"),
            }
        }
    }

    #[test]
    fn changed_or_stopped_helper_never_runs_under_snapshot_metadata() {
        for current in [None, Some("http://127.0.0.1:5000/new-secret")] {
            let running = Mutex::new(current);
            let result = with_snapshot(&running, "http://127.0.0.1:5000/old-secret", |r| *r, &CancellationToken::default(), || -> Result<(), SttError> {
                panic!("a replacement must not receive this snapshot's inference");
            });
            assert_eq!(result.unwrap_err().kind, ErrorKind::Unreachable);
            assert!(running.try_lock().is_ok());
        }
    }

    #[test]
    fn cancelled_snapshot_does_not_wait_for_model_or_start_inference() {
        let running = Mutex::new(Some("helper"));
        let _busy = running.lock().unwrap();
        let cancel = CancellationToken::default();
        cancel.cancel();
        let result = with_snapshot(&running, "helper", |r| *r, &cancel, || -> Result<(), SttError> {
            panic!("cancelled snapshots must not run inference");
        });
        assert_eq!(result.unwrap_err().kind, ErrorKind::Cancelled);
    }

    #[test]
    fn upload_entry_point_keeps_snapshot_guard_and_local_upload_metadata() {
        let base = "http://127.0.0.1:9/test-helper";
        let engine = SnapshotEngine {
            running: Arc::new(Mutex::new(None)),
            changes: Arc::new(Mutex::new(())),
            base: base.into(),
            transcriber: WhisperServerTranscriber::new(base, None),
            model: SpeechModel::whisper(std::path::Path::new("."), &local::MODELS[0]),
        };
        let options = TranscribeOptions::new("en", "");
        let cancel = CancellationToken::default();
        // With no matching helper, this must fail before making an HTTP request.
        let (result, upload) = engine.transcribe_with_upload(&[], &options, &cancel);
        assert_eq!(result.unwrap_err().kind, ErrorKind::Unreachable);
        assert!(upload.is_none());
        cancel.cancel();
        let (result, upload) = engine.transcribe_with_upload(&[], &options, &cancel);
        assert_eq!(result.unwrap_err().kind, ErrorKind::Cancelled);
        assert!(upload.is_none());
    }
}
