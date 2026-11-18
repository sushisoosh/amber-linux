//! The local engine: whisper.cpp's `whisper-server.exe`, started with the chosen model on a
//! free port on 127.0.0.1. It keeps the model in memory between dictations; see `process`
//! for how it is tied to the app.

use std::process::{Child, Command};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::process::{self, Output};

use amber_core::local::{self, Layout};
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
}

impl Running {
    fn served(&self) -> Served {
        Served { url: self.base.clone(), runtime: self.key.runtime.clone(), model: self.key.model.clone(), snapshot_transcriber: None }
    }
}

/// The server a request goes to, and what it runs - which may still be the previous model
/// while the chosen one downloads, so the request has to fit the running one.
pub struct Served {
    pub url: String,
    pub runtime: String,
    pub model: String,
    pub snapshot_transcriber: Option<Box<dyn Transcriber>>,
}

#[derive(Default)]
pub struct Whisper {
    running: Arc<Mutex<Option<Running>>>,
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

    fn status_if(&self, own: bool, app: &tauri::AppHandle, status: Status) {
        if own {
            self.set_status(app, status);
        }
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
        let mut served = self.ensure_with(app, Some(&settings.model), Some(settings))?;
        // Both the client's context and its identity come from the model actually served,
        // including the old installed model retained while the requested one downloads.
        let slack = local::model(&served.model).filter(|m| m.short_context).map(|m| m.context_slack);
        served.snapshot_transcriber = Some(Box::new(SnapshotEngine {
            running: self.running.clone(),
            base: served.url.clone(),
            transcriber: WhisperServerTranscriber::new(&served.url, slack),
        }));
        Ok(served)
    }

    fn ensure_with(&self, app: &tauri::AppHandle, model: Option<&str>, snapshot: Option<&amber_core::settings::LocalSettings>) -> Result<Served, String> {
        use tauri::Manager;
        // The Engine page shows the PC's own model; a phone's switch is not its news.
        let own = model.is_none();
        let mut running = self.running.lock().unwrap();
        let core = app.state::<crate::state::Core>();
        let layout = &core.paths.layout;
        let settings = snapshot.cloned().unwrap_or_else(|| core.settings().local);
        let key = Key { runtime: settings.runtime.clone(), model: model.unwrap_or(&settings.model).to_string(), threads: settings.threads };

        let rt = local::runtime(&key.runtime).ok_or("Unknown runtime.")?;
        let model = local::model(&key.model).ok_or("Unknown model.")?;
        let missing = if !layout.runtime_installed(rt) {
            Some(format!("The local engine ({}) is not installed yet - download it in the settings.", rt.label))
        } else if !layout.model_installed(model) {
            Some(format!("The model {} is not downloaded yet - get it in the settings.", model.label))
        } else {
            None
        };

        if let Some(r) = running.as_mut() {
            let alive = matches!(r.child.try_wait(), Ok(None));
            if alive && (r.key == key || missing.is_some()) {
                // The configured one, or the previous one while the new one is not there yet.
                return Ok(r.served());
            }
            if !alive {
                crate::diag::warn(format!("local engine had exited: {}", self.last_output()));
            }
            process::stop(&mut r.child);
            *running = None;
        }
        if let Some(msg) = missing {
            self.status_if(own, app, Status::of("failed", Some(msg.clone())));
            return Err(msg);
        }

        self.status_if(own, app, Status::of("starting", Some(format!("Loading {}…", model.label))));
        // A cleanup model on the graphics card may have taken what is free there; the speech
        // model comes first - without it dictation itself breaks - and the other one reloads
        // after it, fitting itself into what is left.
        let moved = rt.id != "cpu" && core.llm.make_room_on_gpu(app);
        let started = Instant::now();
        let spawned = self.spawn(layout, rt, model, &key);
        if moved {
            core.llm.warm(app, true);
        }
        match spawned {
            Ok(r) => {
                let served = r.served();
                if model.short_context {
                    // Leaves the server at a short context for its first language detection
                    // (whisper.cpp detects with the previous request's context), and warms the
                    // GPU up.
                    amber_core::engine::WhisperServerTranscriber::new(&served.url, Some(model.context_slack)).reset_context();
                }
                crate::diag::info(format!("local engine ready: {} on {} in {} ms", model.label, rt.label, started.elapsed().as_millis()));
                *running = Some(r);
                let gpu = if rt.id == "cuda" { self.gpu_in_use() } else { None };
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
                self.status_if(own, app, Status::of("ready", Some(message)));
                Ok(served)
            }
            Err(e) => {
                crate::diag::error(format!("local engine failed to start: {e}"));
                self.status_if(own, app, Status::of("failed", Some(e.clone())));
                Err(e)
            }
        }
    }

    fn spawn(&self, layout: &Layout, rt: &local::Runtime, model: &local::Model, key: &Key) -> Result<Running, String> {
        let port = process::free_port().map_err(|e| format!("No free port: {e}"))?;
        let threads = if key.threads == 0 { process::default_threads() } else { key.threads };
        let mut cmd = Command::new(layout.server_exe(rt));
        // whisper-server reads its arguments in the system code page; a user folder with other
        // characters would arrive mangled. So it runs in the models folder and gets only the
        // file name - its DLLs still load from the exe's own folder.
        let model_path = layout.model_path(model);
        let path = format!("/{}", amber_core::api::new_key());
        cmd.current_dir(model_path.parent().unwrap_or(&layout.root)).args(server_args(model, port, threads, &path));
        let child = process::spawn(cmd, &self.output).map_err(|e| format!("The local engine could not be started: {e}"))?;

        // Not the open port: this whisper-server opens it before the model has loaded and
        // answers 503 until then.
        let base = format!("http://127.0.0.1:{port}{path}");
        let deadline = Instant::now() + START_TIMEOUT;
        let mut running = Running { child, base: base.clone(), key: key.clone() };
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
        if let Some(mut r) = self.running.lock().unwrap().take() {
            process::stop(&mut r.child);
            crate::diag::info("local engine stopped");
        }
        self.set_status(app, Status::of("stopped", None));
    }

    /// The model the server runs right now, without waiting on a load in progress.
    pub fn running_model(&self) -> Option<String> {
        self.running.try_lock().ok()?.as_ref().map(|r| r.key.model.clone())
    }

    /// Stops the server only if it runs from [runtime] - before that runtime's files are
    /// replaced or deleted.
    pub fn stop_runtime(&self, app: &tauri::AppHandle, runtime: &str) {
        let uses = self.running.lock().unwrap().as_ref().is_some_and(|r| r.key.runtime == runtime);
        if uses {
            self.stop(app);
        }
    }

    /// Stops the server only if it has [model] loaded - before the file is deleted.
    pub fn stop_model(&self, app: &tauri::AppHandle, model: &str) {
        let uses = self.running.lock().unwrap().as_ref().is_some_and(|r| r.key.model == model);
        if uses {
            self.stop(app);
        }
    }

    /// The server died and the next dictation will restart it - or it is running fine.
    /// Never waits: while another thread loads a model (and holds the lock), the answer is
    /// simply "not ready".
    /// Whether a server with [model] runs - not one the phone switched to.
    pub fn is_ready(&self, model: &str) -> bool {
        match self.running.try_lock() {
            Ok(mut running) => running.as_mut().map(|r| r.key.model == model && matches!(r.child.try_wait(), Ok(None))).unwrap_or(false),
            Err(_) => false,
        }
    }
}

struct SnapshotEngine {
    running: Arc<Mutex<Option<Running>>>,
    base: String,
    transcriber: WhisperServerTranscriber,
}

impl Transcriber for SnapshotEngine {
    fn transcribe(&self, wav: &[u8], options: &TranscribeOptions) -> Result<String, SttError> {
        self.transcribe_cancellable(wav, options, &CancellationToken::default())
    }

    fn transcribe_cancellable(&self, wav: &[u8], options: &TranscribeOptions, cancel: &CancellationToken) -> Result<String, SttError> {
        with_snapshot(&self.running, &self.base, |r| &r.base, cancel, || {
            self.transcriber.transcribe_cancellable(wav, options, cancel)
        })
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
        return Err(SttError::new(ErrorKind::Unreachable, "The local model changed before recognition started. Try the test again.", "snapshot helper replaced"));
    }
    let result = infer();
    cancel.check()?;
    result
}

/// whisper-server's arguments. It runs in the models folder, so the model is its file name;
/// a tuned (short-context) model runs without flash attention ([local::flash_attention]).
fn server_args(model: &local::Model, port: u16, threads: u32, path: &str) -> Vec<String> {
    let mut args = vec!["--model".to_string(), model.file.to_string()];
    args.extend(["--host", "127.0.0.1", "--port", &port.to_string(), "--threads", &threads.to_string()].map(String::from));
    args.extend(["--request-path".to_string(), path.to_string()]);
    if !local::flash_attention(model) {
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
        let args = server_args(tuned, 5000, 8, "/k");
        assert!(args.contains(&"--no-flash-attn".to_string()), "{args:?}");
        assert!(args.windows(2).any(|w| w == ["--model", tuned.file]) && args.windows(2).any(|w| w == ["--port", "5000"]));
        let stock = local::model("large-v3-turbo-q5_0").unwrap();
        assert!(!server_args(stock, 5000, 8, "/k").contains(&"--no-flash-attn".to_string()), "the full window keeps it");
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
            base: base.into(),
            transcriber: WhisperServerTranscriber::new(base, None),
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
