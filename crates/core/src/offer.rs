//! What Amber PC offers the phone to choose from: every downloaded model, and the cloud model
//! set in the app - the PC's own choice marked as the default.

use crate::api::{ModelInfo, ModelKind};
use crate::settings::{EngineKind, Settings};

/// What this PC has ready: downloaded models (with their engine installed), and whether the
/// cloud models set in the app can run (a provider and its key).
pub struct Installed {
    pub speech: Vec<String>,
    pub imported_speech: Vec<ModelInfo>,
    /// Where the local speech models run: "NVIDIA GPU (CUDA)".
    pub speech_runtime: String,
    pub llms: Vec<String>,
    pub imported_llms: Vec<ModelInfo>,
    pub llm_runtime: String,
    pub cloud_speech: bool,
    pub cloud_cleanup: bool,
}

/// The models the phone may choose among, local ones first, the PC's own as the default.
pub fn pc_models(s: &Settings, installed: &Installed) -> Vec<ModelInfo> {
    let mut speech: Vec<ModelInfo> = installed
        .speech
        .iter()
        .filter_map(|id| crate::local::model(id))
        .map(|m| ModelInfo {
            id: m.id.into(),
            kind: ModelKind::Speech,
            label: m.label.into(),
            note: format!("on this PC · {}", installed.speech_runtime),
            default: s.engine == EngineKind::Local && s.local.model == m.id,
        })
        .collect();
    speech.extend(installed.speech.iter().filter_map(|id| crate::onnx::model(id)).map(|m| ModelInfo {
        id: m.id.into(), kind: ModelKind::Speech, label: m.label.into(),
        note: format!("on this PC - ONNX CPU; {}", m.note),
        default: s.engine == EngineKind::Local && s.local.model == m.id,
    }));
    speech.extend(installed.imported_speech.iter().map(|m| ModelInfo {
        id: m.id.clone(), label: m.label.clone(), note: m.note.clone(), kind: ModelKind::Speech,
        default: s.engine == EngineKind::Local && s.local.model == m.id,
    }));
    if installed.cloud_speech && !s.server.model.trim().is_empty() {
      if let Ok(target) = crate::connections::resolve_speech(s) {
        speech.push(ModelInfo {
            id: s.server.model.clone(),
            kind: ModelKind::Speech,
            label: s.server.model.clone(),
            note: format!("through this PC · {}", target.connection.map_or(target.provider.name, |c| &c.name)),
            default: s.engine == EngineKind::Server,
        });
      }
    }
    let mut cleanup: Vec<ModelInfo> = installed
        .llms
        .iter()
        .filter_map(|id| crate::local::llm_model(id))
        .map(|m| ModelInfo {
            id: m.id.into(),
            kind: ModelKind::Cleanup,
            label: m.label.into(),
            note: format!("on this PC · {}", installed.llm_runtime),
            default: s.cleanup.engine == EngineKind::Local && s.cleanup.local_model == m.id,
        })
        .collect();
    cleanup.extend(installed.imported_llms.iter().map(|m| ModelInfo {
        id: m.id.clone(), label: m.label.clone(), note: m.note.clone(), kind: ModelKind::Cleanup,
        default: s.cleanup.engine == EngineKind::Local && s.cleanup.local_model == m.id,
    }));
    if installed.cloud_cleanup && !s.cleanup.model.trim().is_empty() {
      if let Ok(target) = crate::connections::resolve_cleanup(s) {
        let label = target.provider.chat_model(&s.cleanup.model).map_or(s.cleanup.model.clone(), |m| m.label.to_string());
        cleanup.push(ModelInfo {
            id: s.cleanup.model.clone(),
            kind: ModelKind::Cleanup,
            label,
            note: format!("through this PC · {}", target.connection.map_or(target.provider.name, |c| &c.name)),
            default: s.cleanup.engine == EngineKind::Server,
        });
      }
    }
    // The PC's own may not be ready (a model still downloading): then the first is.
    for list in [&mut speech, &mut cleanup] {
        if !list.iter().any(|m| m.default) {
            if let Some(first) = list.first_mut() {
                first.default = true;
            }
        }
    }
    speech.into_iter().chain(cleanup).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::ModelKind;
    use crate::settings::{EngineKind, Settings};

    fn installed() -> Installed {
        Installed {
            speech: vec!["acft-large-v3-turbo-q8_0".into(), "futo-multilingual-74".into()],
            imported_speech: Vec::new(),
            speech_runtime: "NVIDIA GPU (CUDA)".into(),
            llms: vec!["qwen3-4b-instruct-2507-q4km".into()],
            imported_llms: Vec::new(),
            llm_runtime: "NVIDIA GPU (CUDA)".into(),
            cloud_speech: true,
            cloud_cleanup: true,
        }
    }

    #[test]
    fn cleanup_imports_are_offered_with_their_saved_identity_and_default() {
        let mut s = Settings::default();
        s.cleanup.engine = EngineKind::Local;
        s.cleanup.local_model = "cleanup-import-0123456789abcdef0123456789abcdef".into();
        let mut installed = installed();
        installed.imported_llms.push(ModelInfo { id: s.cleanup.local_model.clone(), kind: ModelKind::Cleanup,
            label: "My cleanup model".into(), note: "Imported llama GGUF on this PC".into(), default: false });
        let offered = pc_models(&s, &installed);
        let defaults: Vec<_> = offered.iter().filter(|m| m.kind == ModelKind::Cleanup && m.default).collect();
        assert_eq!(defaults.len(), 1);
        assert_eq!(defaults[0].id, s.cleanup.local_model);
        assert_eq!(defaults[0].label, "My cleanup model");
    }

    #[test]
    fn every_downloaded_model_and_the_cloud_one() {
        let mut s = Settings::default();
        s.engine = EngineKind::Local;
        s.local.model = "acft-large-v3-turbo-q8_0".into();
        s.server.provider = "groq".into();
        s.server.model = "whisper-large-v3-turbo".into();
        s.cleanup.engine = EngineKind::Local;
        s.cleanup.local_model = "qwen3-4b-instruct-2507-q4km".into();
        s.cleanup.provider = "groq".into();
        s.cleanup.model = "openai/gpt-oss-20b".into();
        let list = pc_models(&s, &installed());
        let ids = |k: ModelKind| list.iter().filter(|m| m.kind == k).map(|m| (m.id.as_str(), m.default)).collect::<Vec<_>>();
        assert_eq!(ids(ModelKind::Speech), [("acft-large-v3-turbo-q8_0", true), ("futo-multilingual-74", false), ("whisper-large-v3-turbo", false)]);
        assert_eq!(ids(ModelKind::Cleanup), [("qwen3-4b-instruct-2507-q4km", true), ("openai/gpt-oss-20b", false)]);
        assert!(list[0].note.contains("on this PC") && list[2].note.contains("Groq"), "{:?}", list);
    }

    #[test]
    fn the_pcs_own_engine_is_the_default() {
        let mut s = Settings::default();
        s.engine = EngineKind::Server;
        s.server.provider = "groq".into();
        s.server.model = "whisper-large-v3-turbo".into();
        s.cleanup.engine = EngineKind::Server;
        s.cleanup.provider = "groq".into();
        s.cleanup.model = "openai/gpt-oss-20b".into();
        let list = pc_models(&s, &installed());
        let default = |k: ModelKind| list.iter().find(|m| m.kind == k && m.default).map(|m| m.id.clone());
        assert_eq!(default(ModelKind::Speech).as_deref(), Some("whisper-large-v3-turbo"));
        assert_eq!(default(ModelKind::Cleanup).as_deref(), Some("openai/gpt-oss-20b"));
        // No key for the cloud, nothing downloaded: nothing to offer.
        let none = Installed { speech: vec![], llms: vec![], cloud_speech: false, cloud_cleanup: false, ..installed() };
        assert!(pc_models(&s, &none).is_empty());
    }

    #[test]
    fn onnx_models_are_offered_without_claiming_the_whisper_gpu_runtime() {
        let mut s = Settings::default();
        s.local.model = "parakeet-v3-int8".into();
        let mut installed = installed();
        installed.speech.extend(["parakeet-v3-int8".into(), "moonshine-tiny-int8".into()]);
        let models = pc_models(&s, &installed);
        let parakeet = models.iter().find(|m| m.id == "parakeet-v3-int8").unwrap();
        assert!(parakeet.default);
        assert!(parakeet.note.contains("ONNX CPU") && !parakeet.note.contains("CUDA"));
        assert!(models.iter().any(|m| m.id == "moonshine-tiny-int8" && !m.default));
    }

    #[test]
    fn imported_models_and_saved_connection_names_are_advertised_without_source_paths() {
        let mut s = Settings::default();
        s.local.model = format!("import-{}", "b".repeat(32));
        let mut installed = installed();
        installed.imported_speech.push(ModelInfo { id: s.local.model.clone(), kind: ModelKind::Speech,
            label: "My English model".into(), note: "Imported Moonshine on this PC".into(), default: false });
        s.connections.push(crate::connections::Connection { id: "conn_personal".into(), name: "Personal OpenAI".into(), provider: "openai".into(), base_url: crate::providers::OPENAI.base_url.into() });
        s.server.connection_id = Some("conn_personal".into());
        s.server.model = "whisper-1".into();
        let models = pc_models(&s, &installed);
        assert!(models.iter().any(|m| m.id == s.local.model && m.default && m.label == "My English model"));
        assert!(models.iter().find(|m| m.id == "whisper-1").unwrap().note.contains("Personal OpenAI"));
        s.server.connection_id = Some("conn_missing".into());
        assert!(!pc_models(&s, &installed).iter().any(|m| m.id == "whisper-1"));
    }
}
