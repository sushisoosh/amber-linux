//! What the whole app shares: settings, paths, keys, and the parts that hold state.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;

use amber_core::gesture::{Gesture, Mode};
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
        let mut s = self.settings.write().unwrap();
        let mut next = s.clone();
        change(&mut next);
        let next = next.sanitized();
        #[cfg(target_os = "linux")]
        let next = next.for_linux(false);
        next.save(&self.paths.settings).map_err(|e| format!("Settings were not saved: {e}"))?;
        *s = next.clone();
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
        let _change = self.credential_changes.lock().unwrap();
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

    /// Milliseconds since start, the gesture machine's clock.
    pub fn now_ms(&self) -> u64 {
        self.epoch.elapsed().as_millis() as u64
    }
}
