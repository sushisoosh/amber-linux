//! What the whole app shares: settings, paths, keys, and the parts that hold state.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;

use amber_core::gesture::{Gesture, Mode};
use amber_core::connections::{self, Connection, Credential};
use amber_core::local::Layout;
use amber_core::settings::Settings;

use crate::capture::Recorder;
use crate::dictation::Dictation;
use crate::whisper::Whisper;

pub struct Paths {
    pub settings: PathBuf,
    pub secrets: PathBuf,
    pub logs: PathBuf,
    pub layout: Layout,
}

pub struct Core {
    pub paths: Paths,
    settings: RwLock<Settings>,
    pub settings_changes: Mutex<()>,
    keys: Mutex<HashMap<String, String>>,
    credential_changes: Mutex<()>,
    pub dictation: Dictation,
    pub testing: crate::testing::Testing,
    pub whisper: Whisper,
    pub hotkey_gesture: Mutex<Gesture>,
    pub pointer_gesture: Mutex<Gesture>,
    /// A Stream Deck key: held to talk, tapped to toggle.
    pub deck_gesture: Mutex<Gesture>,
    pub control: crate::control::Control,
    /// Running downloads by id, with their cancel switch.
    pub downloads: Mutex<HashMap<String, Arc<AtomicBool>>>,
    /// The settings window's microphone meter.
    pub mic_test: Mutex<Option<(Recorder, Instant)>>,
    pub phone: crate::phone::PhoneApi,
    /// cloudflared, for the phone API from anywhere.
    pub tunnel: crate::tunnel::Tunnel,
    /// The smart cleanup's local model.
    pub llm: crate::llm::Llm,
    /// How long recognition takes per engine and model on this PC.
    pub estimates: amber_core::estimate::Estimates,
    /// History, statistics and the last failed recording.
    pub memory: crate::memory::Memory,
    epoch: Instant,
}

impl Core {
    pub fn new(paths: Paths) -> Self {
        #[cfg(not(target_os = "linux"))]
        let (settings, problem) = Settings::load(&paths.settings);
        #[cfg(target_os = "linux")]
        let (settings, problem) = Settings::load_linux(&paths.settings);
        if let Some(problem) = problem {
            crate::diag::warn(problem);
        }
        // Register all stores before loading any of them: an archive alone must prevent
        // replacement of a missing shared wallet key, even with ordinary history disabled.
        crate::secrets::register(&paths.secrets);
        crate::secrets::register(&paths.layout.root.join("history.bin"));
        let testing = crate::testing::Testing::new(paths.layout.root.join("testing-tables.bin"));
        let keys = crate::secrets::load(&paths.secrets);
        let mode = settings.hotkey.mode;
        let paths_root = paths.layout.root.clone();
        let settings_snapshot = settings.clone();
        Core {
            paths,
            settings: RwLock::new(settings),
            settings_changes: Mutex::new(()),
            keys: Mutex::new(keys),
            credential_changes: Mutex::new(()),
            dictation: Dictation::default(),
            testing,
            whisper: Whisper::default(),
            hotkey_gesture: Mutex::new(Gesture::new(mode)),
            // The bubble always offers both: a click toggles, a long press talks.
            pointer_gesture: Mutex::new(Gesture::for_pointer(Mode::Both)),
            deck_gesture: Mutex::new(Gesture::new(Mode::Both)),
            control: crate::control::Control::default(),
            downloads: Mutex::new(HashMap::new()),
            mic_test: Mutex::new(None),
            phone: crate::phone::PhoneApi::default(),
            tunnel: crate::tunnel::Tunnel::default(),
            llm: crate::llm::Llm::default(),
            estimates: amber_core::estimate::Estimates::load(&paths_root.join("estimates.json")),
            memory: crate::memory::Memory::load(&paths_root, &settings_snapshot),
            epoch: Instant::now(),
        }
    }

    pub fn settings(&self) -> Settings {
        self.settings.read().unwrap().clone()
    }

    /// Changes, sanitizes and saves the settings; returns the new ones.
    pub fn update_settings(&self, change: impl FnOnce(&mut Settings)) -> Settings {
        match self.try_update_settings(change) {
            Ok(settings) => settings,
            Err(e) => {
                crate::diag::error(&e);
                self.settings()
            }
        }
    }

    pub fn try_update_settings(&self, change: impl FnOnce(&mut Settings)) -> Result<Settings, String> {
        let _credentials = self.credential_guard();
        let mut s = self.settings.write().unwrap();
        let mut next = s.clone();
        change(&mut next);
        if next.cleanup.local_model != s.cleanup.local_model && next.cleanup.local_model.starts_with("cleanup-import-") {
            let model = amber_core::cleanup_imports::resolve(&self.paths.layout, &next.cleanup.local_model).map_err(|e| e.to_string())?;
            amber_core::cleanup_imports::available(&self.paths.layout, &model).map_err(|e| e.to_string())?;
        }
        let next = next.sanitized();
        #[cfg(target_os = "linux")]
        let next = next.for_linux(false);
        connections::validate_settings(&next)?;
        let revoked: Vec<_> = s.connections.iter().filter(|old| {
            !next.connections.iter().any(|new| old.same_credential_binding(new))
        }).filter_map(|old| old.credential_slot().ok()).collect();
        next.save(&self.paths.settings).map_err(|e| format!("Settings were not saved: {e}"))?;
        *s = next.clone();
        drop(s);
        if !revoked.is_empty() {
            let mut keys = self.keys.lock().unwrap();
            let changed = revoked.iter().fold(false, |changed, id| keys.remove(id).is_some() || changed);
            if changed {
                let snapshot = keys.clone();
                drop(keys);
                crate::secrets::save(&self.paths.secrets, &snapshot).map_err(|e| format!(
                    "Settings were saved and old connection credentials were revoked in this session, but encrypted credential persistence failed. {e}"))?;
            }
        }
        Ok(next)
    }

    pub fn key(&self, provider: &str) -> Option<String> {
        self.keys.lock().unwrap().get(provider).cloned().filter(|k| !k.is_empty())
    }

    pub fn has_key(&self, provider: &str) -> bool {
        if provider == "custom" {
            self.key_for(provider, &self.settings().server.base_url).is_some()
        } else {
            self.key(provider).is_some()
        }
    }

    /// A custom credential is never forwarded to another scheme, host or port.
    pub fn key_for(&self, provider: &str, base: &str) -> Option<String> {
        if provider != "custom" {
            return self.key(provider);
        }
        let origin = amber_core::providers::credential_origin(base).ok()?;
        self.key(&format!("custom:{origin}"))
    }

    pub fn set_key(&self, provider: &str, key: Option<String>) -> Result<(), String> {
        self.set_key_for(provider, None, key)
    }

    pub fn set_key_for(&self, provider: &str, base: Option<&str>, key: Option<String>) -> Result<(), String> {
        let _change = self.credential_guard();
        if amber_core::providers::by_id(provider).is_some() { self.testing.snapshot().ensure_idle()?; }
        let storage_id = if provider == "custom" {
            let settings = self.settings();
            format!("custom:{}", amber_core::providers::credential_origin(base.unwrap_or(&settings.server.base_url))?)
        } else {
            provider.to_string()
        };
        let mut keys = self.keys.lock().unwrap();
        if provider == "custom" {
            // Legacy unbound credentials must be re-entered for a known origin.
            keys.remove("custom");
        }
        match key.map(|k| k.trim().to_string()).filter(|k| !k.is_empty()) {
            Some(k) => keys.insert(storage_id, k),
            None => keys.remove(&storage_id),
        };
        let snapshot = keys.clone();
        drop(keys);
        crate::secrets::save(&self.paths.secrets, &snapshot).map_err(|e| {
            format!("The credential change is active in this session, but persistence failed or could not be confirmed. Do not rely on it surviving a restart. {e}")
        })
    }

    /// Testing takes this before its session lock, so a key cannot change between its
    /// settings snapshot and entering the active state. Do not acquire in reverse order.
    pub fn credential_guard(&self) -> std::sync::MutexGuard<'_, ()> {
        self.credential_changes.lock().unwrap()
    }

    pub fn connection_key(&self, expected: &Connection) -> Result<Option<String>, String> {
        let settings = self.settings.read().unwrap();
        let current = connections::find(&settings.connections, &expected.id)?;
        if !current.same_endpoint(expected) {
            return Err("This connection changed after it was selected. Reload the settings or start a new test; no request was made.".into());
        }
        let keys = self.keys.lock().unwrap();
        let Some(value) = keys.get(&current.credential_slot()?) else { return Ok(None) };
        let credential: Credential = serde_json::from_str(value).map_err(|_| "This connection's saved credential is invalid. Save its key again.".to_string())?;
        Ok(credential.key_for(current).map(str::to_string))
    }

    pub fn set_connection_key(&self, expected: &Connection, key: Option<String>) -> Result<bool, String> {
        let _change = self.credential_guard();
        self.testing.snapshot().ensure_idle()?;
        let settings = self.settings.read().unwrap();
        let current = connections::find(&settings.connections, &expected.id)?;
        if !current.same_endpoint(expected) { return Err("This connection changed. Reload it before saving a key.".into()); }
        let key = key.map(|k| connections::validate_key(&k)).transpose()?.filter(|k| !k.is_empty());
        let value = key.as_deref().map(|key| Credential::new(current, key)
            .and_then(|c| serde_json::to_string(&c).map_err(|_| "Could not encode the connection credential.".to_string()))).transpose()?;
        let mut keys = self.keys.lock().unwrap();
        let slot = current.credential_slot()?;
        match value { Some(value) => { keys.insert(slot, value); }, None => { keys.remove(&slot); } }
        let snapshot = keys.clone();
        drop(keys);
        drop(settings);
        crate::secrets::save(&self.paths.secrets, &snapshot).map_err(|e| format!(
            "The connection credential changed in this session, but encrypted persistence failed or could not be confirmed. Do not rely on it surviving a restart. {e}"))?;
        Ok(key.is_some())
    }

    /// Milliseconds since start, the gesture machine's clock.
    pub fn now_ms(&self) -> u64 {
        self.epoch.elapsed().as_millis() as u64
    }
}

#[cfg(test)]
mod connection_tests {
    use super::*;

    fn fixture() -> (Core, Connection) {
        // No directories or encrypted files are created, so no wallet is contacted.
        let root = std::env::temp_dir().join(format!("amber-connection-test-{}", amber_core::api::new_key()));
        let core = Core::new(Paths { settings: root.join("settings.json"), secrets: root.join("secrets.bin"),
            logs: root.join("logs"), layout: Layout { root } });
        let c = Connection { id: "conn_first".into(), name: "Private server".into(), provider: "custom".into(), base_url: "https://example.com/v1".into() };
        core.settings.write().unwrap().connections.push(c.clone());
        (core, c)
    }

    #[test]
    fn connection_credentials_never_fall_back_to_provider_or_same_origin_keys() {
        let (core, c) = fixture();
        let mut second = c.clone();
        second.id = "conn_second".into();
        core.settings.write().unwrap().connections.push(second.clone());
        core.keys.lock().unwrap().extend([
            ("custom".into(), "unbound-legacy".into()),
            ("custom:https://example.com".into(), "bound-legacy".into()),
            ("groq".into(), "built-in-key".into()),
        ]);
        assert_eq!(core.connection_key(&c).unwrap(), None);
        core.keys.lock().unwrap().insert(c.credential_slot().unwrap(), serde_json::to_string(&Credential::new(&c, "connection-only").unwrap()).unwrap());
        assert_eq!(core.connection_key(&c).unwrap().as_deref(), Some("connection-only"));
        assert_eq!(core.connection_key(&second).unwrap(), None);
        assert_eq!(core.key_for("custom", &c.base_url).as_deref(), Some("bound-legacy"));
    }

    #[test]
    fn stale_snapshot_and_key_save_fail_closed_after_path_adapter_or_identity_changes() {
        let (core, c) = fixture();
        core.keys.lock().unwrap().insert(c.credential_slot().unwrap(), serde_json::to_string(&Credential::new(&c, "connection-only").unwrap()).unwrap());
        let mut renamed = c.clone();
        renamed.name = "Renamed".into();
        core.settings.write().unwrap().connections[0] = renamed;
        assert_eq!(core.connection_key(&c).unwrap().as_deref(), Some("connection-only"));
        for current in [
            Connection { base_url: "https://example.com/new/v1".into(), ..c.clone() },
            Connection { provider: "openai".into(), base_url: String::new(), ..c.clone() },
            Connection { id: "conn_other".into(), ..c.clone() },
        ] {
            core.settings.write().unwrap().connections[0] = current;
            assert!(core.connection_key(&c).is_err());
            assert!(core.set_connection_key(&c, Some("must-not-save".into())).is_err());
            assert!(!core.keys.lock().unwrap()[&c.credential_slot().unwrap()].contains("must-not-save"));
        }
        core.settings.write().unwrap().connections.clear();
        assert!(core.connection_key(&c).is_err());
    }

    #[test]
    fn invalid_connection_selection_cannot_be_saved_or_silently_replaced() {
        let (core, _) = fixture();
        assert!(core.try_update_settings(|s| s.server.connection_id = Some("conn_missing".into())).is_err());
        assert!(core.settings().server.connection_id.is_none());
        assert!(!core.paths.settings.exists());
    }

    #[test]
    fn missing_or_unsafe_cleanup_import_selection_is_not_saved_or_normalized() {
        let (core, _) = fixture();
        let original = core.settings().cleanup.local_model;
        for id in ["cleanup-import-0123456789abcdef0123456789abcdef", "cleanup-import-../../outside"] {
            assert!(core.try_update_settings(|s| s.cleanup.local_model = id.into()).is_err());
            assert_eq!(core.settings().cleanup.local_model, original);
            assert!(!core.paths.settings.exists());
        }
    }
}
