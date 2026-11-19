//! Desktop entry points for copy-only local speech model imports.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use amber_core::imports::{self, ImportedModel, ImportRequest, Variant};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_dialog::DialogExt;

use crate::state::Core;

/// Load-now is fire-and-forget, so every loader error must also reach visible status.
pub(crate) fn report_load_failure<T>(result: Result<T, String>, failed: impl FnOnce(&str)) -> Result<T, String> {
    result.inspect_err(|message| failed(message))
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportResult {
    #[serde(flatten)]
    model: ImportedModel,
    selection_error: Option<String>,
}

#[tauri::command]
pub async fn browse_local_model(app: AppHandle, variant: Variant) -> Result<Option<String>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let dialog = app.dialog().file().set_title("Import a trusted local speech model");
        let path = if variant == Variant::Whisper {
            dialog.add_filter("Whisper GGML model", &["bin"]).blocking_pick_file()
        } else {
            dialog.blocking_pick_folder()
        };
        path.map(|p| {
            p.into_path().map_err(|_| "Choose a local file or folder.".to_string())?
                .into_os_string().into_string().map_err(|_| "The model path must use valid Unicode characters.".to_string())
        }).transpose()
    }).await.map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn import_local_model(app: AppHandle, request: ImportRequest, select_after_import: bool) -> Result<ImportResult, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let core = app.state::<Core>();
        let cancel = Arc::new(AtomicBool::new(false));
        {
            let mut active = core.downloads.lock().unwrap();
            if active.contains_key("model-import") { return Err("Another model import is already running.".into()); }
            active.insert("model-import".into(), cancel.clone());
        }
        let _ = app.emit("local-models-changed", ());
        let result = imports::import(&core.paths.layout, &request, &cancel, &mut |done, total| {
            let _ = app.emit("model-import-progress", serde_json::json!({ "done": done, "total": total }));
        }).map_err(|e| e.to_string()).map(|model| {
            let selection_error = if select_after_import {
                match core.try_update_settings(|settings| settings.local.model = model.id.clone()) {
                    Ok(settings) => {
                        let _ = app.emit("settings-changed", &settings);
                        if settings.engine == amber_core::settings::EngineKind::Local && settings.local.keep_loaded {
                            crate::commands::start_local(app.clone());
                        }
                        None
                    },
                    Err(error) => Some(error),
                }
            } else { None };
            ImportResult { model, selection_error }
        });
        core.downloads.lock().unwrap().remove("model-import");
        let _ = app.emit("local-models-changed", ());
        result
    }).await.map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn cancel_model_import(app: AppHandle) {
    if let Some(cancel) = app.state::<Core>().downloads.lock().unwrap().get("model-import") {
        cancel.store(true, Ordering::Relaxed);
    }
}

#[tauri::command]
pub async fn remove_imported_model(app: AppHandle, id: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let core = app.state::<Core>();
        let removal = imports::prepare_remove(&core.paths.layout, &id).map_err(|e| e.to_string())?;
        let result = core.dictation.when_idle(|| core.whisper.remove_imported(&app, removal))?;
        let _ = app.emit("local-models-changed", ());
        result
    }).await.map_err(|e| e.to_string())?
}

/// This is the only import-related action allowed to download anything, and only on demand.
#[tauri::command]
pub async fn setup_onnx_runtime(app: AppHandle) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let core = app.state::<Core>();
        let cancel = Arc::new(AtomicBool::new(false));
        {
            let mut active = core.downloads.lock().unwrap();
            if active.contains_key("onnx-runtime") { return Err("ONNX Runtime setup is already running.".into()); }
            active.insert("onnx-runtime".into(), cancel.clone());
        }
        let _ = app.emit("local-models-changed", ());
        let result = amber_core::onnx::setup_runtime(&core.paths.layout, &cancel, &mut |done, total| {
            let _ = app.emit("onnx-runtime-progress", serde_json::json!({ "done": done, "total": total }));
        }).map_err(|e| e.to_string());
        core.downloads.lock().unwrap().remove("onnx-runtime");
        let _ = app.emit("local-models-changed", ());
        result
    }).await.map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn cancel_onnx_runtime_setup(app: AppHandle) {
    if let Some(cancel) = app.state::<Core>().downloads.lock().unwrap().get("onnx-runtime") {
        cancel.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_result_keeps_model_identity_when_selection_cannot_be_saved() {
        let result = ImportResult {
            model: ImportedModel { id: "import-0123456789abcdef0123456789abcdef".into(), name: "My model".into(),
                variant: Variant::Whisper, languages: vec![], files: vec![] },
            selection_error: Some("Settings were not saved: disk full".into()),
        };
        let value = serde_json::to_value(&result).unwrap();
        assert_eq!(value["id"], result.model.id);
        assert_eq!(value["name"], "My model");
        assert_eq!(value["selectionError"], "Settings were not saved: disk full");
        assert!(value.get("model").is_none(), "the existing imported model response remains flattened");
    }

    #[test]
    fn missing_selection_and_verification_errors_publish_failed_before_returning() {
        let layout = amber_core::local::Layout {
            root: std::env::temp_dir().join(format!("amber-missing-import-{}", amber_core::api::new_key())),
        };
        let mut status = crate::whisper::Status::of("ready", Some("Previous model".into()));
        let missing = imports::resolve(&layout, "import-0123456789abcdef0123456789abcdef").map_err(|e| e.to_string());
        let result = report_load_failure(missing, |message| status = crate::whisper::Status::of("failed", Some(message.into())));
        let message = result.unwrap_err();
        assert_eq!(status.state, "failed");
        assert_eq!(status.message.as_deref(), Some(message.as_str()));
        assert!(message.contains("Import it again or select another model"));

        let model = imports::resolve(&layout, amber_core::local::MODELS[0].id).unwrap();
        let result = report_load_failure(imports::verify(&layout, &model).map_err(|e| e.to_string()), |message| {
            status = crate::whisper::Status::of("failed", Some(message.into()));
        });
        assert_eq!(status.message.as_deref(), Some(result.unwrap_err().as_str()));
        assert_eq!(status.state, "failed");
        let ok = report_load_failure(Ok(42), |_| panic!("successful load must not publish failure"));
        assert_eq!(ok, Ok(42));
    }
}
