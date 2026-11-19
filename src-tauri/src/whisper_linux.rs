//! Linux local recognition uses the shared in-process CPU engine, with no helper process
//! or listening socket. All loading/inference methods run on the existing blocking paths.

use std::sync::{Arc, Mutex, OnceLock};

use amber_core::engine::{CancellationToken, ErrorKind, SttError, TranscribeOptions, Transcriber};
use amber_core::imports;
use amber_inference::Speech;
use serde::Serialize;
use tauri::{Emitter, Manager};

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub state: &'static str,
    pub message: Option<String>,
}

impl Status {
    pub fn of(state: &'static str, message: Option<String>) -> Self {
        Self { state, message }
    }
}

pub struct Served {
    pub transcriber: Box<dyn Transcriber>,
    pub runtime: String,
    pub model: String,
}

#[derive(Default)]
pub struct Whisper {
    // Never replace the Speech owner: outstanding engine handles must share its model lock.
    speech: OnceLock<Arc<Speech>>,
    onnx: OnceLock<Arc<amber_onnx::Speech>>,
    changes: Arc<Mutex<()>>,
    status: Mutex<Option<Status>>,
}

impl Whisper {
    pub fn status(&self) -> Status {
        self.status.lock().unwrap().clone().unwrap_or_else(|| Status::of("stopped", None))
    }

    fn set_status(&self, app: &tauri::AppHandle, status: Status) {
        *self.status.lock().unwrap() = Some(status.clone());
        let _ = app.emit("local-status", status);
    }

    pub fn ensure(&self, app: &tauri::AppHandle) -> Result<Served, String> {
        self.ensure_model(app, None)
    }

    /// Preloading is blocking and not interruptible while native model construction runs.
    /// Actual transcription uses the dictation's per-run CancellationToken through Transcriber.
    pub fn ensure_model(&self, app: &tauri::AppHandle, requested: Option<&str>) -> Result<Served, String> {
        self.ensure_with(app, requested, None)
    }

    pub fn ensure_snapshot(&self, app: &tauri::AppHandle, settings: &amber_core::settings::LocalSettings) -> Result<Served, String> {
        self.ensure_with(app, Some(&settings.model), Some(settings))
    }

    fn ensure_with(&self, app: &tauri::AppHandle, requested: Option<&str>, snapshot: Option<&amber_core::settings::LocalSettings>) -> Result<Served, String> {
        let _change = self.changes.lock().unwrap();
        crate::imports::report_load_failure(self.ensure_locked(app, requested, snapshot), |message| {
            self.set_status(app, Status::of("failed", Some(message.into())));
        })
    }

    fn ensure_locked(&self, app: &tauri::AppHandle, requested: Option<&str>, snapshot: Option<&amber_core::settings::LocalSettings>) -> Result<Served, String> {
        let core = app.state::<crate::state::Core>();
        let settings = snapshot.cloned().unwrap_or_else(|| core.settings().local);
        let layout = &core.paths.layout;
        let id = requested.unwrap_or(&settings.model);
        let model = imports::resolve(layout, id).map_err(|e| e.to_string())?;
        // Tests and phone requests also change the resident model shown by load/unload UI.
        if model.is_onnx() {
            if !imports::installed(layout, &model) {
                let message = format!("The model {} is missing or incomplete. Download or import it in the settings.", model.label);
                return Err(message);
            }
            if let Some(speech) = self.speech.get() { speech.unload(); }
            let speech = self.onnx.get_or_init(|| Arc::new(amber_onnx::Speech::new(layout.clone())));
            if !speech.is_ready(id) {
                self.set_status(app, Status::of("starting", Some(format!("Loading {}...", model.label))));
            }
            speech.load(id, &CancellationToken::default()).map_err(|error| error.message)?;
            let transcriber = Box::new(SnapshotEngine {
                transcriber: speech.engine(id)?, owner: Owner::Onnx(speech.clone()),
                changes: self.changes.clone(), threads: settings.threads, model: id.into(),
            });
            self.set_status(app, Status::of("ready", Some(format!("{} on CPU (ONNX)", model.label))));
            return Ok(Served { transcriber, runtime: "onnx-cpu".into(), model: id.into() });
        }
        if !imports::installed(layout, &model) {
            let message = format!("The model {} is missing or incomplete. Download or import it in the settings.", model.label);
            return Err(message);
        }
        if let Some(speech) = self.onnx.get() { speech.unload(); }
        let speech = self.speech.get_or_init(|| Arc::new(Speech::new(layout.root.join("models"), settings.threads, false)));
        speech.set_threads(settings.threads);
        if !speech.is_ready(&model.id) {
            self.set_status(app, Status::of("starting", Some(format!("Loading {}...", model.label))));
        }
        speech.load_model(&model, &CancellationToken::default()).map_err(|error| error.message)?;
        let transcriber = Box::new(SnapshotEngine {
            transcriber: speech.engine_model(model.clone())?, owner: Owner::Whisper(speech.clone()),
            changes: self.changes.clone(), threads: settings.threads, model: model.id.clone(),
        });
        self.set_status(app, Status::of("ready", Some(format!("{} on CPU (built in)", model.label))));
        Ok(Served { transcriber, runtime: "cpu".into(), model: model.id.into() })
    }

    pub fn remove_imported(&self, app: &tauri::AppHandle, removal: imports::Removal<'_>) -> Result<(), String> {
        let _change = self.changes.lock().unwrap();
        let id = removal.id();
        if self.speech.get().is_some_and(|s| s.unload_model(id)) || self.onnx.get().is_some_and(|s| s.unload_model(id)) {
            self.set_status(app, Status::of("stopped", None));
        }
        removal.remove().map_err(|e| e.to_string())
    }

    pub fn stop(&self, app: &tauri::AppHandle) {
        let _change = self.changes.lock().unwrap();
        if let Some(speech) = self.speech.get() {
            speech.unload();
        }
        if let Some(speech) = self.onnx.get() { speech.unload(); }
        self.set_status(app, Status::of("stopped", None));
    }

    pub fn stop_runtime(&self, app: &tauri::AppHandle, runtime: &str) {
        if runtime == "cpu" || runtime == "onnx-cpu" {
            self.stop(app);
        }
    }

    pub fn stop_model(&self, app: &tauri::AppHandle, model: &str) {
        let _change = self.changes.lock().unwrap();
        if self.speech.get().is_some_and(|speech| speech.unload_model(model))
            || self.onnx.get().is_some_and(|speech| speech.unload_model(model)) {
            self.set_status(app, Status::of("stopped", None));
        }
    }

    pub fn running_model(&self) -> Option<String> {
        let _change = self.changes.try_lock().ok()?;
        self.speech.get().and_then(|s| s.running_model()).or_else(|| self.onnx.get().and_then(|s| s.running_model()))
    }

    pub fn is_ready(&self, model: &str) -> bool {
        self.running_model().as_deref() == Some(model)
    }
}

enum Owner {
    Whisper(Arc<Speech>),
    Onnx(Arc<amber_onnx::Speech>),
}

/// Every handle keeps the cross-backend lifecycle lock during inference. An old Whisper
/// handle must not silently reload itself alongside a newly selected ONNX model.
struct SnapshotEngine {
    transcriber: Box<dyn Transcriber>,
    owner: Owner,
    changes: Arc<Mutex<()>>,
    threads: u32,
    model: String,
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
        cancel.check()?;
        let ready = match &self.owner {
            Owner::Whisper(speech) => { speech.set_threads(self.threads); speech.is_ready(&self.model) }
            Owner::Onnx(speech) => speech.is_ready(&self.model),
        };
        if !ready {
            return Err(SttError::new(ErrorKind::Unreachable, "The local model was unloaded or changed before recognition started. Try again.", "local request handle is stale"));
        }
        self.transcriber.transcribe_detecting_cancellable(wav, options, cancel)
    }
}
