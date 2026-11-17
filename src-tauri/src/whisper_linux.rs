//! Linux local recognition uses the shared in-process CPU engine, with no helper process
//! or listening socket. All loading/inference methods run on the existing blocking paths.

use std::sync::{Arc, Mutex, OnceLock};

use amber_core::engine::{CancellationToken, Transcriber};
use amber_core::local;
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
    changes: Mutex<()>,
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
        let _change = self.changes.lock().unwrap();
        let core = app.state::<crate::state::Core>();
        let settings = core.settings().local;
        let layout = &core.paths.layout;
        let model = local::model(requested.unwrap_or(&settings.model)).ok_or("Unknown speech model.")?;
        let own = requested.is_none();
        if !layout.model_installed(model) {
            let message = format!("The model {} is not downloaded yet - get it in the settings.", model.label);
            if own {
                self.set_status(app, Status::of("failed", Some(message.clone())));
            }
            return Err(message);
        }
        let speech = self.speech.get_or_init(|| Arc::new(Speech::new(layout.root.join("models"), settings.threads, false)));
        speech.set_threads(settings.threads);
        if own && !speech.is_ready(model.id) {
            self.set_status(app, Status::of("starting", Some(format!("Loading {}...", model.label))));
        }
        if let Err(error) = speech.load(model.id, &CancellationToken::default()) {
            if own {
                self.set_status(app, Status::of("failed", Some(error.message.clone())));
            }
            return Err(error.message);
        }
        let transcriber = speech.engine(Some(model.id), model.id)?;
        if own {
            self.set_status(app, Status::of("ready", Some(format!("{} on CPU (built in)", model.label))));
        }
        Ok(Served { transcriber, runtime: "cpu".into(), model: model.id.into() })
    }

    pub fn stop(&self, app: &tauri::AppHandle) {
        let _change = self.changes.lock().unwrap();
        if let Some(speech) = self.speech.get() {
            speech.unload();
        }
        self.set_status(app, Status::of("stopped", None));
    }

    pub fn stop_runtime(&self, app: &tauri::AppHandle, runtime: &str) {
        if runtime == "cpu" {
            self.stop(app);
        }
    }

    pub fn stop_model(&self, app: &tauri::AppHandle, model: &str) {
        let _change = self.changes.lock().unwrap();
        if self.speech.get().is_some_and(|speech| speech.unload_model(model)) {
            self.set_status(app, Status::of("stopped", None));
        }
    }

    pub fn running_model(&self) -> Option<String> {
        self.speech.get()?.running_model()
    }

    pub fn is_ready(&self, model: &str) -> bool {
        self.speech.get().is_some_and(|speech| speech.is_ready(model))
    }
}
