//! Everything the user can set, with defaults that work on first start. API keys are not in
//! here - they live encrypted next to it (see the app's `secrets`).

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::Path;

use crate::gesture::Mode;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    pub engine: EngineKind,
    pub local: LocalSettings,
    pub server: ServerSettings,
    /// Named endpoint/account choices; credentials remain in the encrypted store.
    pub connections: Vec<crate::connections::Connection>,
    /// "auto", an ISO 639-1 code ("de"), or several to choose among ("de,en"). A fixed language
    /// makes Whisper translate everything else into it, so "auto" is the default.
    pub language: String,
    /// Names and jargon, handed to the engine as a prompt.
    pub vocabulary: String,
    /// Device name; empty means the system's default microphone.
    pub microphone: String,
    pub hotkey: HotkeySettings,
    pub bubble: BubbleSettings,
    pub insert: InsertSettings,
    /// Short sounds when a dictation starts and ends.
    pub sounds: bool,
    pub autostart: bool,
    pub auto_update: bool,
    /// Keep the recognized text in the diagnostics log. Off: only lengths and timings.
    pub log_text: bool,
    /// Set once the first-run setup has been seen.
    pub onboarded: bool,
    /// Amber on the phone may use this PC as its server.
    pub phone: PhoneSettings,
    pub cleanup: CleanupSettings,
    /// Writing style and smart-cleanup choice per program.
    pub profiles: Vec<crate::profiles::Profile>,
    /// Keep the last dictations on this PC (encrypted for the Windows account).
    pub history: bool,
    /// How many days the history keeps a dictation.
    pub history_days: u32,
    /// Count words and timings per dictation for the statistics - never the text.
    pub statistics: bool,
    /// Spoken phrases that insert a saved text.
    pub snippets: Vec<crate::snippets::Snippet>,
    /// Explicit first-keyword transformations, separate from ordinary cleanup.
    pub instructions: Vec<crate::instructions::Instruction>,
    pub discord: DiscordSettings,
    /// Spoken requests in terminals become shell commands.
    pub terminal: TerminalSettings,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            engine: EngineKind::Local,
            local: LocalSettings::default(),
            server: ServerSettings::default(),
            connections: Vec::new(),
            language: "auto".into(),
            vocabulary: String::new(),
            microphone: String::new(),
            hotkey: HotkeySettings::default(),
            bubble: BubbleSettings::default(),
            insert: InsertSettings::default(),
            sounds: true,
            autostart: true,
            auto_update: true,
            log_text: false,
            onboarded: false,
            phone: PhoneSettings::default(),
            cleanup: CleanupSettings::default(),
            profiles: crate::profiles::defaults(),
            history: true,
            history_days: 7,
            statistics: true,
            snippets: Vec::new(),
            instructions: Vec::new(),
            discord: DiscordSettings::default(),
            terminal: TerminalSettings::default(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum EngineKind {
    Local,
    Server,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct LocalSettings {
    /// "cpu" or "cuda", see `local::RUNTIMES`.
    pub runtime: String,
    /// A model id from `local::MODELS`.
    pub model: String,
    /// 0 = choose from the CPU.
    pub threads: u32,
    /// Start the local engine with the app, so the first dictation does not wait for loading.
    pub keep_loaded: bool,
    /// Recognize long dictations in parts while still speaking (see `chunking`).
    pub chunking: bool,
}

impl Default for LocalSettings {
    fn default() -> Self {
        LocalSettings { runtime: "cpu".into(), model: "futo-multilingual-244".into(), threads: 0, keep_loaded: true, chunking: true }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct ServerSettings {
    /// None uses the original provider fields. A missing saved connection must fail closed.
    pub connection_id: Option<String>,
    /// A speech-capable provider id, see `providers`.
    pub provider: String,
    /// Only used for "custom"; known providers have a fixed address.
    pub base_url: String,
    pub model: String,
    /// Only outgoing recognition audio; local engines and retained recordings stay lossless.
    pub upload_format: UploadFormat,
    /// A server that cannot be reached hands the recording to a downloaded local model -
    /// never the other way round, so the voice only ever stays more private.
    pub local_fallback: bool,
}

impl Default for ServerSettings {
    fn default() -> Self {
        ServerSettings { connection_id: None, provider: "groq".into(), base_url: String::new(), model: "whisper-large-v3-turbo".into(), upload_format: UploadFormat::Wav, local_fallback: true }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum UploadFormat {
    #[default]
    Wav,
    Opus,
}

impl UploadFormat {
    pub fn as_str(self) -> &'static str {
        match self { Self::Wav => "wav", Self::Opus => "opus" }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct HotkeySettings {
    /// Tauri accelerator syntax, e.g. "Ctrl+Alt+Space". Empty = no hotkey.
    pub dictate: String,
    pub mode: Mode,
    /// Shows or hides the bubble. Empty = none.
    pub toggle_bubble: String,
}

impl Default for HotkeySettings {
    fn default() -> Self {
        HotkeySettings { dictate: "Ctrl+Alt+Space".into(), mode: Mode::Both, toggle_bubble: String::new() }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum BubbleVisibility {
    Always,
    WhileDictating,
    Hidden,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Edge {
    Left,
    Right,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct BubbleSettings {
    pub visibility: BubbleVisibility,
    pub edge: Edge,
    /// Vertical position as a fraction of the monitor's work area, 0 = top.
    pub y: f64,
    /// Monitor the bubble sits on, by name; empty = primary.
    pub monitor: String,
    /// Diameter in logical pixels.
    pub size: u32,
    /// 0.3..1, at rest; dictating, the bubble is fully visible.
    pub opacity: f64,
    /// Programs the bubble stays away from (games, apps with their own voice input), by exe.
    pub hidden_in: Vec<String>,
    /// Out of the way while a program fills the whole screen (games, videos).
    pub hide_fullscreen: bool,
}

impl Default for BubbleSettings {
    fn default() -> Self {
        BubbleSettings {
            visibility: BubbleVisibility::Always,
            edge: Edge::Right,
            y: 0.62,
            monitor: String::new(),
            size: 56,
            opacity: 1.0,
            hidden_in: Vec::new(),
            hide_fullscreen: true,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum InsertMethod {
    /// Clipboard + Ctrl+V; works almost everywhere, fast for long text.
    Paste,
    /// Unicode keystrokes; leaves the clipboard alone, slower, some apps drop characters.
    Type,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct InsertSettings {
    pub method: InsertMethod,
    /// Linux opt-in. Older Linux versions ignored `method`, so it is not consent to paste.
    pub paste_on_completion: bool,
    /// After pasting, put back the text that was on the clipboard before.
    pub restore_clipboard: bool,
    /// A space after the text, so the next dictation does not stick to it.
    pub trailing_space: bool,
}

impl Default for InsertSettings {
    fn default() -> Self {
        InsertSettings { method: InsertMethod::Paste, paste_on_completion: false, restore_clipboard: true, trailing_space: true }
    }
}

/// Terminal mode: in a terminal (or with its hotkey, or after a spoken prefix) a request is
/// turned into a shell command and typed without running it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct TerminalSettings {
    pub enabled: bool,
    /// Requests by themselves in terminal programs; off: only by the hotkey or a start word.
    pub automatic: bool,
    /// A risky command comes commented out ("# rm -rf build"); off, as it is - Enter runs it.
    pub safe_mode: bool,
    /// Press Enter after a harmless command: it runs at once. Off by default, behind a warning.
    pub run_commands: bool,
    /// A hotkey of its own: a request anywhere. Empty: none.
    pub hotkey: String,
    /// Words that make one dictation a request when it starts with them ("Befehl: ...").
    pub prefixes: Vec<String>,
}

impl Default for TerminalSettings {
    fn default() -> Self {
        TerminalSettings { enabled: true, automatic: true, safe_mode: true, run_commands: false, hotkey: String::new(), prefixes: vec!["Befehl".into(), "Command".into()] }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct PhoneSettings {
    /// Off until the user turns it on: it opens a port to the home network.
    pub enabled: bool,
    pub port: u16,
    /// The PC's cleanup (fillers, smart cleanup) also for what the phone dictates.
    pub cleanup: bool,
    /// Reachable from anywhere through a Cloudflare Tunnel (cloudflared, the user's token).
    pub tunnel: bool,
    /// The tunnel's public hostname, "amber.example.com", as set up in Cloudflare.
    pub public_host: String,
    /// The ways in, the first made by 0.4.x; their secrets are in the PC's secret store
    /// under [PhoneKey::id].
    pub keys: Vec<PhoneKey>,
}

/// An access key to the phone API: its name, the models it may use, its quota.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PhoneKey {
    /// Where its secret is stored: "phone-api" for the first, "phone-api-..." for the others.
    pub id: String,
    pub name: String,
    /// The model ids it may use; None for every model offered.
    pub models: Option<Vec<String>>,
    /// Its requests a day; None for the usual.
    pub per_day: Option<u32>,
}

impl Default for PhoneSettings {
    fn default() -> Self {
        PhoneSettings {
            enabled: false,
            port: crate::api::DEFAULT_PORT,
            cleanup: true,
            tunnel: false,
            public_host: String::new(),
            keys: vec![PhoneKey { id: "phone-api".into(), name: "Phone".into(), models: None, per_day: None }],
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct CleanupSettings {
    pub connection_id: Option<String>,
    /// Drop "äh", "ähm" and the like - rules, local, instant.
    pub fillers: bool,
    /// "neue Zeile", "Komma", "Fragezeichen" ... become the marks they name.
    pub spoken_punctuation: bool,
    /// The commas German needs before "dass", "weil", "um ... zu" (see `commas`).
    pub german_commas: bool,
    /// Let a chat model tidy up: self-corrections, false starts, punctuation.
    pub smart: bool,
    /// Where that model runs.
    pub engine: EngineKind,
    /// "cpu" or "cuda", see `local::LLM_RUNTIMES`.
    pub local_runtime: String,
    /// See `local::LLM_MODELS`.
    pub local_model: String,
    /// A cleanup-capable provider id; credentials are shared per provider.
    pub provider: String,
    /// Only for "custom".
    pub base_url: String,
    pub model: String,
}

impl Default for CleanupSettings {
    fn default() -> Self {
        CleanupSettings {
            connection_id: None,
            fillers: true,
            spoken_punctuation: true,
            german_commas: true,
            smart: false,
            engine: EngineKind::Local,
            local_runtime: "cuda".into(),
            local_model: crate::local::LLM_MODELS[0].id.into(),
            provider: "groq".into(),
            base_url: String::new(),
            model: "openai/gpt-oss-20b".into(),
        }
    }
}

/// Muted in Discord while Amber listens: Amber presses Discord's own microphone switch.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct DiscordSettings {
    pub mute_while_dictating: bool,
}

pub const BUBBLE_SIZES: std::ops::RangeInclusive<u32> = 40..=96;

impl Settings {
    /// Linux excludes unsupported integrations, listeners and command execution.
    /// Guarded paste remains a separate opt-in in the existing insertion settings.
    pub fn for_linux(mut self, first_start: bool) -> Self {
        if first_start {
            self.autostart = false;
            self.history = false;
            self.statistics = false;
        }
        self.auto_update = false;
        self.log_text = false;
        self.local.runtime = "cpu".into();
        self.cleanup.local_runtime = "cpu".into();
        self.phone.enabled = false;
        self.phone.tunnel = false;
        self.terminal.enabled = false;
        self.terminal.automatic = false;
        self.terminal.run_commands = false;
        self.terminal.hotkey.clear();
        self.discord.mute_while_dictating = false;
        self.profiles.clear();
        self.bubble.visibility = BubbleVisibility::Hidden;
        self.hotkey.toggle_bubble.clear();
        self
    }

    /// Values out of range are pulled back in, so a hand-edited file cannot break the app.
    pub fn sanitized(mut self) -> Self {
        let d = Settings::default();
        if crate::local::runtime(&self.local.runtime).is_none() {
            self.local.runtime = d.local.runtime;
        }
        if crate::local::model(&self.local.model).is_none() && crate::onnx::model(&self.local.model).is_none() && !crate::imports::is_import_id(&self.local.model) {
            self.local.model = d.local.model;
        }
        self.local.threads = self.local.threads.min(64);
        self.connections = crate::connections::sanitize(std::mem::take(&mut self.connections));
        self.instructions = crate::instructions::sanitize(std::mem::take(&mut self.instructions));
        if !crate::providers::by_id(&self.server.provider).is_some_and(|p| p.supports_speech()) {
            self.server.provider = d.server.provider.clone();
            self.server.model = d.server.model.clone();
        }
        if self.server.model.trim().is_empty() {
            let provider = self.server.connection_id.as_ref().and_then(|id| self.connections.iter().find(|c| &c.id == id)).map(|c| c.provider.as_str())
                .or_else(|| self.server.connection_id.is_none().then_some(self.server.provider.as_str()));
            if let Some(p) = provider.and_then(crate::providers::by_id) {
                self.server.model = p.default_model().to_string();
            }
        }
        let mut seen = std::collections::HashSet::new();
        self.terminal.prefixes = std::mem::take(&mut self.terminal.prefixes)
            .into_iter()
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty() && seen.insert(p.to_lowercase()))
            .collect();
        // The key of 0.4.x first; a name and an id once each, a quota of at least one.
        // A name given twice gets a number: a key is never dropped for its name.
        let mut ids = std::collections::HashSet::new();
        self.phone.keys.retain(|k| !k.name.trim().is_empty() && k.id.starts_with("phone-api") && ids.insert(k.id.clone()));
        let mut names = std::collections::HashSet::new();
        for k in &mut self.phone.keys {
            let base = k.name.trim().to_string();
            let mut name = base.clone();
            let mut n = 2;
            while !names.insert(name.to_lowercase()) {
                name = format!("{base} {n}");
                n += 1;
            }
            k.name = name;
            k.per_day = k.per_day.map(|d| d.max(1));
        }
        if self.phone.keys.is_empty() {
            self.phone.keys.push(PhoneKey { id: "phone-api".into(), name: "Phone".into(), models: None, per_day: None });
        }
        // "de", or several to choose among ("de,en"): two-letter codes, each once, in order.
        let codes: Vec<String> = crate::engine::TranscribeOptions::new(&self.language, "")
            .among()
            .into_iter()
            .filter(|c| c.len() == 2 && c.chars().all(|ch| ch.is_ascii_lowercase()))
            .collect();
        self.language = if codes.is_empty() { "auto".into() } else { codes.join(",") };
        self.bubble.size = self.bubble.size.clamp(*BUBBLE_SIZES.start(), *BUBBLE_SIZES.end());
        self.bubble.opacity = if self.bubble.opacity.is_finite() { self.bubble.opacity.clamp(0.3, 1.0) } else { 1.0 };
        self.bubble.y = if self.bubble.y.is_finite() { self.bubble.y.clamp(0.0, 1.0) } else { d.bubble.y };
        let mut seen = std::collections::HashSet::new();
        self.bubble.hidden_in = std::mem::take(&mut self.bubble.hidden_in)
            .into_iter()
            .map(|e| crate::profiles::exe_name(&e))
            .filter(|e| !e.is_empty() && seen.insert(e.clone()))
            .collect();
        self.vocabulary = self.vocabulary.chars().take(800).collect();
        self.phone.public_host = crate::api::public_host(&self.phone.public_host);
        if self.phone.port < 1024 {
            self.phone.port = d.phone.port;
        }
        self.profiles = crate::profiles::sanitize(std::mem::take(&mut self.profiles));
        self.history_days = self.history_days.clamp(1, 90);
        self.snippets = crate::snippets::sanitize(std::mem::take(&mut self.snippets));
        let c = &mut self.cleanup;
        if crate::local::llm_runtime(&c.local_runtime).is_none() {
            c.local_runtime = d.cleanup.local_runtime.clone();
        }
        if crate::local::llm_model(&c.local_model).is_none() {
            c.local_model = d.cleanup.local_model.clone();
        }
        match crate::providers::by_id(&c.provider).filter(|p| p.supports_cleanup()) {
            None => {
                c.provider = d.cleanup.provider.clone();
                c.model = d.cleanup.model.clone();
            }
            Some(p) if c.model.trim().is_empty() => {
                let provider = if let Some(id) = &c.connection_id {
                    self.connections.iter().find(|c| &c.id == id).and_then(|c| crate::providers::by_id(&c.provider))
                } else { Some(p) };
                c.model = provider.and_then(|p| p.chat_models.first()).map(|m| m.id.to_string()).unwrap_or_default();
            },
            Some(_) => {}
        }
        self
    }

    /// Reads the file; a missing file is a first start. A broken one is kept aside as
    /// `settings.broken.json` and the defaults take over, instead of the app not starting.
    pub fn load(path: &Path) -> (Settings, Option<String>) {
        Self::load_for_platform(path, false)
    }

    pub fn load_linux(path: &Path) -> (Settings, Option<String>) {
        Self::load_for_platform(path, true)
    }

    fn load_for_platform(path: &Path, linux: bool) -> (Settings, Option<String>) {
        let defaults = || if linux { Settings::default().for_linux(true) } else { Settings::default() };
        let raw = match std::fs::read_to_string(path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (defaults(), None),
            Err(e) => return (defaults(), Some(format!("settings unreadable: {e}"))),
        };
        let parsed = if linux {
            serde_json::from_str::<serde_json::Value>(&raw).and_then(|mut value| {
                if let Some(fields) = value.as_object_mut() {
                    for name in ["history", "statistics", "autostart"] {
                        fields.entry(name).or_insert(serde_json::Value::Bool(false));
                    }
                }
                serde_json::from_value::<Settings>(value)
            })
        } else {
            serde_json::from_str::<Settings>(&raw)
        };
        match parsed {
            Ok(s) => {
                let s = s.sanitized();
                (if linux { s.for_linux(false) } else { s }, None)
            }
            Err(e) => {
                let _ = std::fs::rename(path, path.with_file_name("settings.broken.json"));
                (defaults(), Some(format!("settings broken, reset to defaults: {e}")))
            }
        }
    }

    /// Write to a temporary file and rename it over the old one: a crash mid-write leaves the
    /// previous settings, never half a file.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        {
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut f = options.open(&tmp)?;
            f.write_all(serde_json::to_string_pretty(self).expect("settings serialize").as_bytes())?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_connection_defaults_use_the_selected_adapter_not_the_legacy_provider() {
        let mut s = Settings::default();
        s.connections = vec![
            crate::connections::Connection { id: "conn_speech".into(), name: "Speech account".into(), provider: "openai".into(), base_url: crate::providers::OPENAI.base_url.into() },
            crate::connections::Connection { id: "conn_text".into(), name: "Text account".into(), provider: "anthropic".into(), base_url: crate::providers::ANTHROPIC.base_url.into() },
        ];
        s.server.connection_id = Some("conn_speech".into());
        s.server.model.clear();
        s.cleanup.connection_id = Some("conn_text".into());
        s.cleanup.model.clear();
        let s = s.sanitized();
        assert_eq!(s.server.provider, "groq", "dormant provider choice stays intact");
        assert_eq!(s.server.model, crate::providers::OPENAI.default_model());
        assert_eq!(s.cleanup.model, crate::providers::ANTHROPIC.chat_models[0].id);
        crate::connections::validate_settings(&s).unwrap();
    }

    #[test]
    fn new_rules_are_opt_in_and_missing_imports_or_connections_are_not_rerouted() {
        let mut s: Settings = serde_json::from_str("{}").unwrap();
        assert!(s.connections.is_empty() && s.instructions.is_empty());
        assert!(s.server.connection_id.is_none() && s.cleanup.connection_id.is_none());
        s.local.model = format!("import-{}", "a".repeat(32));
        s.server.connection_id = Some("conn_missing".into());
        s.server.model.clear();
        let s = s.sanitized();
        assert!(s.local.model.starts_with("import-"), "a missing import must fail at lookup, not switch to another model");
        assert_eq!(s.server.connection_id.as_deref(), Some("conn_missing"));
        assert!(s.server.model.is_empty());
        assert!(crate::connections::validate_settings(&s).is_err());
        let mut old = serde_json::to_value(crate::testing::Config::from(&Settings::default())).unwrap();
        old.as_object_mut().unwrap().remove("connections");
        old.as_object_mut().unwrap().remove("instructions");
        let restored: crate::testing::Config = serde_json::from_value(old).unwrap();
        assert!(restored.connections.is_empty() && restored.instructions.is_empty());
    }

    #[test]
    fn provider_capabilities_and_defaults_survive_settings_round_trip() {
        for provider in crate::providers::ALL {
            let mut settings = Settings::default();
            settings.server.provider = provider.id.into();
            settings.server.model.clear();
            settings.cleanup.provider = provider.id.into();
            settings.cleanup.model.clear();
            let saved = serde_json::to_vec(&settings).unwrap();
            let settings = serde_json::from_slice::<Settings>(&saved).unwrap().sanitized();
            assert_eq!(settings.server.provider, if provider.supports_speech() { provider.id } else { "groq" });
            assert_eq!(settings.cleanup.provider, if provider.supports_cleanup() { provider.id } else { "groq" });
            if provider.supports_speech() { assert_eq!(settings.server.model, provider.default_model()); }
            if provider.supports_cleanup() {
                assert_eq!(settings.cleanup.model, provider.chat_models.first().map(|m| m.id).unwrap_or_default());
            }
        }
    }

    #[test]
    fn upload_compression_is_opt_in_and_round_trips() {
        let old: Settings = serde_json::from_str(r#"{"server":{"provider":"tinfoil","model":"voxtral-small-24b"}}"#).unwrap();
        assert_eq!(old.server.upload_format, UploadFormat::Wav);
        let mut settings = old;
        settings.server.upload_format = UploadFormat::Opus;
        let saved = serde_json::to_value(&settings).unwrap();
        assert_eq!(saved["server"]["uploadFormat"], "opus");
        let restored: Settings = serde_json::from_value(saved).unwrap();
        assert_eq!(restored.sanitized().server.upload_format, UploadFormat::Opus);
    }

    #[test]
    fn linux_missing_partial_and_corrupt_settings_never_imply_privacy_opt_in() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let check = |s: Settings| assert!(!s.history && !s.statistics && !s.autostart && !s.auto_update);
        check(Settings::load_linux(&path).0);
        std::fs::write(&path, "{}").unwrap();
        check(Settings::load_linux(&path).0);
        std::fs::write(&path, "{broken").unwrap();
        let (s, error) = Settings::load_linux(&path);
        assert!(error.is_some());
        check(s);
        assert!(dir.path().join("settings.broken.json").exists());
        std::fs::write(&path, r#"{"history":true,"statistics":true,"autostart":true}"#).unwrap();
        let (s, error) = Settings::load_linux(&path);
        assert!(error.is_none());
        assert!(s.history && s.statistics && s.autostart);
    }

    #[test]
    fn linux_defaults_are_private_and_unsupported_features_stay_off() {
        let mut s = Settings::default();
        s.phone.enabled = true;
        s.phone.tunnel = true;
        s.terminal.run_commands = true;
        s.local.runtime = "cuda".into();
        s.log_text = true;
        let s = s.for_linux(true);
        assert!(!s.history && !s.statistics && !s.autostart && !s.auto_update && !s.log_text);
        assert!(!s.phone.enabled && !s.phone.tunnel && !s.terminal.enabled && !s.terminal.run_commands);
        assert!(s.profiles.is_empty());
        assert_eq!(s.local.runtime, "cpu");
        assert_eq!(s.cleanup.local_runtime, "cpu");
        assert_eq!(s.bubble.visibility, BubbleVisibility::Hidden);
        let mut opted_in = s;
        opted_in.history = true;
        opted_in.autostart = true;
        opted_in.statistics = true;
        let opted_in = opted_in.for_linux(false);
        assert!(opted_in.history && opted_in.autostart && opted_in.statistics);
    }

    #[test]
    fn linux_paste_requires_explicit_consent_even_with_legacy_paste_settings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        assert!(!Settings::load_linux(&path).0.insert.paste_on_completion);
        std::fs::write(&path, r#"{"insert":{"method":"paste"}}"#).unwrap();
        let (mut s, error) = Settings::load_linux(&path);
        assert!(error.is_none());
        assert!(!s.insert.paste_on_completion);
        s.insert.paste_on_completion = true;
        s.save(&path).unwrap();
        assert!(Settings::load_linux(&path).0.insert.paste_on_completion);
        assert_eq!(Settings::default().insert.method, InsertMethod::Paste);
    }

    #[test]
    fn terminal_mode_is_on_with_its_prefixes() {
        let t = Settings::default().terminal;
        assert!(t.enabled && t.automatic && t.hotkey.is_empty());
        // Settings of 0.4.5 without the switch: automatic, as they were.
        let old: TerminalSettings = serde_json::from_str(r#"{"enabled":true,"hotkey":"","prefixes":["Befehl"]}"#).unwrap();
        assert!(old.automatic);
        assert!(!old.run_commands, "never on without being switched on");
        assert!(old.safe_mode, "risky commands commented out unless switched off");
        assert_eq!(t.prefixes, ["Befehl", "Command"]);
        let mut s = Settings::default();
        s.terminal.prefixes = vec![" Befehl ".into(), "".into(), "befehl".into(), "Kommando".into()];
        assert_eq!(s.sanitized().terminal.prefixes, ["Befehl", "Kommando"]);
    }

    #[test]
    fn the_phone_key_of_0_4_becomes_the_first_of_several() {
        let s = Settings::default().sanitized();
        assert_eq!(s.phone.keys, [PhoneKey { id: "phone-api".into(), name: "Phone".into(), models: None, per_day: None }]);
        let mut s = Settings::default();
        s.phone.keys = vec![
            PhoneKey { id: "phone-api".into(), name: "Phone".into(), models: None, per_day: None },
            PhoneKey { id: "phone-api-2".into(), name: "phone".into(), models: None, per_day: None },
            PhoneKey { id: "phone-api-3".into(), name: " ".into(), models: None, per_day: None },
            PhoneKey { id: "phone-api".into(), name: "Laptop".into(), models: None, per_day: Some(0) },
            PhoneKey { id: "phone-api-4".into(), name: "Laptop".into(), models: Some(vec![]), per_day: Some(0) },
        ];
        let s = s.sanitized();
        // A name given twice gets a number - never a key silently lost (and its device locked out).
        assert_eq!(
            s.phone.keys.iter().map(|k| (k.id.as_str(), k.name.as_str())).collect::<Vec<_>>(),
            [("phone-api", "Phone"), ("phone-api-2", "phone 2"), ("phone-api-4", "Laptop")]
        );
        assert_eq!(s.phone.keys[2].per_day, Some(1), "at least one a day");
    }

    #[test]
    fn several_languages_survive_saving() {
        // Review finding: the sanitizer turned "de,en" into "auto" on every save.
        let s = |l: &str| Settings { language: l.into(), ..Settings::default() }.sanitized().language;
        assert_eq!(s("de,en"), "de,en");
        assert_eq!(s(" DE-de , en_US,de "), "de,en");
        assert_eq!(s("de"), "de");
        assert_eq!(s("auto"), "auto");
        assert_eq!(s("klingon,, ,"), "auto");
        assert_eq!(s("de,klingon"), "de");
    }

    #[test]
    fn round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let mut s = Settings::default();
        s.engine = EngineKind::Server;
        s.vocabulary = "Tauri, Groq".into();
        s.bubble.edge = Edge::Left;
        s.save(&path).unwrap();
        let (back, err) = Settings::load(&path);
        assert!(err.is_none());
        assert_eq!(back, s);
    }

    #[test]
    fn missing_file_is_a_first_start() {
        let dir = tempfile::tempdir().unwrap();
        let (s, err) = Settings::load(&dir.path().join("settings.json"));
        assert_eq!(s, Settings::default());
        assert!(err.is_none());
        assert!(!s.onboarded);
    }

    #[test]
    fn broken_file_is_kept_aside() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, "{ not json").unwrap();
        let (s, err) = Settings::load(&path);
        assert_eq!(s, Settings::default());
        assert!(err.is_some());
        assert!(dir.path().join("settings.broken.json").exists());
    }

    #[test]
    fn partial_and_unknown_fields_are_fine() {
        let s: Settings = serde_json::from_str(r#"{"sounds":false,"futureThing":1,"bubble":{"edge":"left"}}"#).unwrap();
        assert!(!s.sounds);
        assert_eq!(s.bubble.edge, Edge::Left);
        assert_eq!(s.bubble.size, BubbleSettings::default().size);
    }

    #[test]
    fn sanitize_pulls_values_back() {
        let mut s = Settings::default();
        s.bubble.size = 500;
        s.bubble.opacity = 0.0;
        s.bubble.y = f64::NAN;
        s.language = "Deutsch".into();
        s.local.model = "gone".into();
        s.server.provider = "nope".into();
        s.server.model = " ".into();
        s.phone.port = 80;
        s.cleanup.local_model = "gone".into();
        s.cleanup.provider = "openai".into();
        s.cleanup.model = "".into();
        let s = s.sanitized();
        assert_eq!(s.cleanup.local_model, CleanupSettings::default().local_model);
        assert_eq!(s.cleanup.model, "gpt-4.1-nano");
        assert!(s.cleanup.fillers && !s.cleanup.smart, "fillers on, the model only when asked for");
        assert_eq!(s.phone.port, 8765);
        assert!(!s.phone.enabled, "the phone API is off unless turned on");
        assert_eq!(s.bubble.size, 96);
        assert_eq!(s.bubble.opacity, 0.3);
        assert_eq!(s.bubble.y, BubbleSettings::default().y);
        assert_eq!(s.language, "auto");
        assert_eq!(s.local.model, LocalSettings::default().model);
        assert_eq!(s.server.provider, "groq");
        assert_eq!(s.server.model, "whisper-large-v3-turbo");

        let mut s = Settings::default();
        s.language = " DE ".into();
        assert_eq!(s.sanitized().language, "de");
    }
}
