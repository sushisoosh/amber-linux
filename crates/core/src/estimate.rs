//! Successful request latency, independent of transcript history. Only online aggregates and
//! settings fingerprints are kept, indefinitely: no audio, text, timestamps or sample lists.
//! Time spent loading models or on failed attempts is not part of a successful route's latency.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::profiles::{self, Profile};
use crate::settings::{EngineKind, Settings};

const VERSION: u64 = 2;
const MAX_SIZE: f64 = 1_000_000_000.0;
const MAX_MS: f64 = 86_400_000.0;
const MAX_SAMPLES: u64 = 1 << 53;

#[derive(Clone, Copy, Debug)]
pub struct Line {
    pub overhead: f64,
    pub slope: f64,
}

impl Line {
    fn predict(self, size: f64) -> f64 {
        (self.overhead + self.slope * size).clamp(1.0, MAX_MS)
    }
}

/// Bivariate Welford moments. Variance is M2 / (n - 1), covariance is co_moment / (n - 1).
/// Outlying durations are winsorized before updating, so moments describe the robust sample.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq)]
pub struct Moments {
    pub samples: u64,
    pub mean_size: f64,
    pub mean_ms: f64,
    pub m2_size: f64,
    pub m2_ms: f64,
    pub co_moment: f64,
}

impl Moments {
    fn valid(&self) -> bool {
        let n = self.samples as f64;
        (1..=MAX_SAMPLES).contains(&self.samples)
            && [self.mean_size, self.mean_ms, self.m2_size, self.m2_ms, self.co_moment].iter().all(|v| v.is_finite())
            && self.mean_size > 0.0 && self.mean_size <= MAX_SIZE
            && self.mean_ms > 0.0 && self.mean_ms <= MAX_MS
            && (0.0..=n * MAX_SIZE.powi(2)).contains(&self.m2_size)
            && (0.0..=n * MAX_MS.powi(2)).contains(&self.m2_ms)
            && self.co_moment.abs() <= (self.m2_size.sqrt() * self.m2_ms.sqrt()) * (1.0 + 1e-9)
            && (self.samples != 1 || (self.m2_size == 0.0 && self.m2_ms == 0.0 && self.co_moment == 0.0))
    }

    pub fn stddev_ms(&self) -> f64 {
        if self.samples < 2 { 0.0 } else { (self.m2_ms / (self.samples - 1) as f64).sqrt() }
    }

    fn line(&self, default: Line) -> Line {
        // Identical/near-identical sizes cannot identify a slope separately from overhead.
        if self.samples >= 4 && self.m2_size / self.samples as f64 > (self.mean_size * 0.1).powi(2) {
            let slope = (self.co_moment / self.m2_size).max(0.0).min(self.mean_ms / self.mean_size);
            Line { overhead: (self.mean_ms - slope * self.mean_size).max(0.0), slope }
        } else {
            let weight = self.samples as f64 / (self.samples as f64 + 3.0);
            let slope = default.slope.min(self.mean_ms / self.mean_size);
            Line {
                overhead: default.overhead * (1.0 - weight) + (self.mean_ms - slope * self.mean_size) * weight,
                slope: default.slope * (1.0 - weight) + slope * weight,
            }
        }
    }

    fn add(&mut self, size: f64, mut ms: f64, default: Line) {
        if self.samples >= 8 {
            let line = self.line(default);
            let predicted = line.predict(size);
            let residual = (self.m2_ms - 2.0 * line.slope * self.co_moment + line.slope.powi(2) * self.m2_size).max(0.0);
            let sigma = (residual / (self.samples - 1) as f64).sqrt();
            // A pause/suspend or a single slow request must not dominate indefinitely. A wide
            // floor tolerates real variation; clipping (not rejection) still permits drift.
            let band = (6.0 * sigma).max(predicted * 3.0).max(1_000.0);
            ms = ms.clamp((predicted - band).max(1.0), (predicted + band).min(MAX_MS));
        }
        self.samples += 1;
        let dx = size - self.mean_size;
        let dy = ms - self.mean_ms;
        self.mean_size += dx / self.samples as f64;
        self.mean_ms += dy / self.samples as f64;
        self.m2_size = (self.m2_size + dx * (size - self.mean_size)).max(0.0);
        self.m2_ms = (self.m2_ms + dy * (ms - self.mean_ms)).max(0.0);
        self.co_moment += dx * (ms - self.mean_ms);
    }
}

#[derive(Serialize, Deserialize, Clone, Copy)]
struct LegacyRate {
    per_ms: f64,
    samples: u32,
}

#[derive(Serialize, Default)]
struct Store {
    version: u64,
    models: BTreeMap<String, Moments>,
    /// Old EWMA has no variance or size distribution. Keep it as a prior, not fake samples.
    legacy: BTreeMap<String, LegacyRate>,
}

pub struct Estimates {
    path: Option<PathBuf>,
    store: Mutex<Store>,
}

impl Estimates {
    pub fn load(path: &Path) -> Self {
        let mut store = Store { version: VERSION, ..Store::default() };
        if let Some(value) = std::fs::read(path).ok().and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok()) {
            // A downgrade must not erase a newer application's statistics on its first run.
            if value.get("version").is_some_and(|v| v.as_u64() != Some(VERSION)) {
                return Self { path: None, store: Mutex::new(store) };
            }
            if value.get("version").and_then(|v| v.as_u64()) == Some(VERSION) {
                if let Some(models) = value.get("models").and_then(|v| v.as_object()) {
                    for (key, value) in models {
                        if let Ok(moments) = serde_json::from_value::<Moments>(value.clone()) {
                            if valid_fingerprint(key) && moments.valid() { store.models.insert(key.clone(), moments); }
                        }
                    }
                }
            }
            let legacy = if value.get("version").is_none() { value.as_object() }
                else if value.get("version").and_then(|v| v.as_u64()) == Some(VERSION) { value.get("legacy").and_then(|v| v.as_object()) }
                else { None };
            if let Some(legacy) = legacy {
                for (key, value_) in legacy {
                    if let Ok(rate) = serde_json::from_value::<LegacyRate>(value_.clone()) {
                        if rate.samples > 0 && rate.per_ms.is_finite() && (0.0..=1_000.0).contains(&rate.per_ms) {
                            let key = if value.get("version").is_none() { fingerprint(&key) } else { key.clone() };
                            if valid_fingerprint(&key) { store.legacy.insert(key, rate); }
                        }
                    }
                }
            }
        }
        Self { path: Some(path.into()), store: Mutex::new(store) }
    }

    pub fn in_memory() -> Self {
        Self { path: None, store: Mutex::new(Store { version: VERSION, ..Store::default() }) }
    }

    pub fn expected_ms(&self, route: &Route, size: u64) -> u64 {
        self.prediction(route, size).0
    }

    /// The model list must not label an unmeasured default as measured.
    pub fn measured_ms(&self, route: &Route, size: u64) -> Option<u64> {
        let (ms, measured) = self.prediction(route, size);
        measured.then_some(ms)
    }

    fn prediction(&self, route: &Route, size: u64) -> (u64, bool) {
        if size == 0 { return (0, false); }
        let store = self.store.lock().unwrap();
        let legacy = route.legacy.as_ref().and_then(|key| store.legacy.get(key));
        let default = legacy.map(|r| Line { overhead: 200.0, slope: r.per_ms }).unwrap_or(route.default);
        let moments = store.models.get(&route.key);
        let line = moments.map(|m| m.line(default)).unwrap_or(default);
        (line.predict((size as f64).min(MAX_SIZE)).round() as u64, moments.is_some() || legacy.is_some())
    }

    /// Only a completed, non-cancelled model request is eligible. Test runs never learn.
    /// Callers pass the successful attempt's own duration, not time spent on failed routes.
    pub fn observe(&self, route: &Route, size: u64, ms: u64, eligible: bool) {
        if !eligible || size == 0 || size as f64 > MAX_SIZE || ms == 0 || ms as f64 > MAX_MS { return; }
        // Keep update + replacement under one lock so concurrent callers cannot overwrite a
        // newer snapshot with an older one or collide on the temporary filename.
        let mut store = self.store.lock().unwrap();
        let moments = store.models.entry(route.key.clone()).or_default();
        if moments.samples >= MAX_SAMPLES { return; }
        moments.add(size as f64, ms as f64, route.default);
        if let Some(path) = &self.path {
            if let Ok(text) = serde_json::to_vec(&*store) {
                let tmp = path.with_extension("json.tmp");
                if std::fs::write(&tmp, text).is_ok() { let _ = std::fs::rename(&tmp, path); }
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct Route {
    key: String,
    legacy: Option<String>,
    default: Line,
}

fn valid_fingerprint(key: &str) -> bool {
    key.len() == 64 && key.bytes().all(|b| b.is_ascii_hexdigit())
}

fn fingerprint(value: &impl Serialize) -> String {
    format!("{:x}", Sha256::digest(serde_json::to_vec(value).expect("estimate identity serializes")))
}

/// Runtime and threads describe the actual backend, not a pending settings selection. ONNX
/// manages its own threads and does not use Whisper's setting. Never include a local port/key.
pub fn local_speech(s: &Settings, runtime: &str, model: &str, threads: u32) -> Route {
    Route {
        key: fingerprint(&("speech-v2", "local", runtime, model, threads, &s.language, &s.vocabulary, s.local.chunking)),
        legacy: Some(fingerprint(&format!("local:{runtime}:{model}"))),
        default: Line { overhead: 200.0, slope: 0.1 },
    }
}

pub fn server_speech(s: &Settings) -> Option<Route> {
    let target = crate::connections::resolve_speech(s).ok()?;
    // Native adapters always send WAV. OpenAI-compatible endpoints negotiate dynamically;
    // their requested format policy and discovery cost belong to the endpoint-specific route.
    let format = if target.provider.speech_api == Some(crate::providers::SpeechApi::OpenAi) {
        s.server.upload_format
    } else { crate::settings::UploadFormat::Wav };
    Some(Route {
        key: fingerprint(&("speech-v2", target.provider.id, &target.base_url, &s.server.connection_id,
            &s.server.model, format, &s.language, &s.vocabulary)),
        legacy: Some(fingerprint(&format!("{}::{}", target.provider.id, s.server.model))),
        default: Line { overhead: 200.0, slope: 0.05 },
    })
}

/// Used before speech is known (None), then with the real pre-cleanup text. Rules/snippets,
/// empty requests and disabled instructions are not model work and must not train it.
pub fn cleanup_wanted(s: &Settings, profile: Option<&Profile>, recognized: Option<&str>, snippets: bool) -> bool {
    if !profiles::smart_cleanup(s.cleanup.smart, profile) { return false; }
    let Some(text) = recognized else { return true };
    if let Some(selected) = crate::instructions::match_start(&s.instructions, text) {
        return s.cleanup.smart && !selected.body.is_empty();
    }
    if snippets && crate::snippets::find(&s.snippets, text).is_some() { return false; }
    !crate::text::by_rule(text, crate::text::Rules {
        fillers: s.cleanup.fillers, spoken_punctuation: s.cleanup.spoken_punctuation,
        german_commas: s.cleanup.german_commas, language: (s.language != "auto").then_some(s.language.as_str()),
    }).is_empty()
}

/// Prompts and addresses affect execution, but only their fingerprint is persisted. Never
/// include dictated text (even hashed), window titles, display names, or credential material.
pub fn cleanup(s: &Settings, profile: Option<&Profile>, recognized: Option<&str>, local: Option<(&str, &str, u32)>) -> Option<Route> {
    let selected = recognized.and_then(|text| crate::instructions::match_start(&s.instructions, text));
    let mode = selected.as_ref().map(|m| (&m.instruction.prompt, m.instruction.output));
    let style = profile.map(|p| p.style).unwrap_or_default();
    let instruction = profile.map(|p| p.instruction.as_str()).unwrap_or("");
    let route = match s.cleanup.engine {
        EngineKind::Local => {
            let (runtime, model, threads) = local?;
            fingerprint(&("local", runtime, model, threads))
        }
        EngineKind::Server => {
            let target = crate::connections::resolve_cleanup(s).ok()?;
            if s.cleanup.model.trim().is_empty() { return None; }
            fingerprint(&(target.provider.id, &target.base_url, &s.cleanup.connection_id, &s.cleanup.model))
        }
    };
    Some(Route {
        key: fingerprint(&("cleanup-v2", route, mode, style, instruction, &s.language, &s.vocabulary,
            s.cleanup.fillers, s.cleanup.spoken_punctuation, s.cleanup.german_commas)),
        legacy: None,
        default: Line { overhead: 400.0, slope: if s.cleanup.engine == EngineKind::Local { 12.0 } else { 4.0 } },
    })
}

/// A conservative initial text-size prior; the actual pre-cleanup character count replaces it.
pub fn predicted_chars(audio_ms: u64) -> u64 {
    audio_ms.saturating_mul(15).saturating_add(999) / 1_000
}

/// Working progress is never completion. Mirrored in the bubble's pure, tested helper.
pub fn progress(elapsed_ms: f64, expected_ms: f64) -> f64 {
    if !elapsed_ms.is_finite() || !expected_ms.is_finite() || expected_ms <= 0.0 { return 0.0; }
    let t = elapsed_ms / expected_ms;
    if t <= 0.9 { return t.max(0.0); }
    (0.9 + 0.08 * (1.0 - (-(t - 0.9) * 2.0).exp())).min(0.98)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route() -> Route { local_speech(&Settings::default(), "cpu", "test", 4) }

    #[test]
    fn defaults_sparse_and_constant_sizes_remain_finite() {
        let e = Estimates::in_memory();
        let r = route();
        assert_eq!(e.expected_ms(&r, 10_000), 1_200);
        assert_eq!(e.measured_ms(&r, 10_000), None);
        e.observe(&r, 10_000, 600, true);
        assert_eq!(e.expected_ms(&r, 10_000), 1_050, "one sample only partly replaces the prior");
        for _ in 0..100 { e.observe(&r, 10_000, 600, true); }
        assert!((600..620).contains(&e.expected_ms(&r, 10_000)));
        assert!(e.expected_ms(&r, 1) > 0);
        assert_eq!(e.expected_ms(&r, 0), 0);
    }

    #[test]
    fn regression_learns_fixed_cost_and_size_slope_with_welford_variance() {
        let e = Estimates::in_memory();
        let r = route();
        for x in [1_000, 2_000, 3_000, 4_000] { e.observe(&r, x, 500 + x / 2, true); }
        assert_eq!(e.expected_ms(&r, 10_000), 5_500);
        let m = e.store.lock().unwrap().models[&r.key];
        assert_eq!(m.samples, 4);
        assert_eq!(m.mean_size, 2_500.0);
        assert_eq!(m.mean_ms, 1_750.0);
        assert_eq!(m.m2_size, 5_000_000.0);
        assert_eq!(m.m2_ms, 1_250_000.0);
        assert_eq!(m.co_moment, 2_500_000.0);
        assert!((m.stddev_ms() - 645.4972243679028).abs() < 1e-6);
        assert!(m.valid());
    }

    #[test]
    fn invalid_ineligible_and_outlying_observations_do_not_poison_models() {
        let e = Estimates::in_memory();
        let r = route();
        for (x, y, eligible) in [(100, 30, false), (0, 30, true), (100, 0, true), (u64::MAX, 10, true), (100, u64::MAX, true)] {
            e.observe(&r, x, y, eligible);
        }
        assert!(e.store.lock().unwrap().models.is_empty());
        for _ in 0..20 { e.observe(&r, 10_000, 1_200, true); }
        e.observe(&r, 10_000, 3_600_000, true);
        assert!(e.expected_ms(&r, 10_000) < 1_500);
        assert_eq!(e.store.lock().unwrap().models[&r.key].samples, 21);
    }

    #[test]
    fn roundtrip_legacy_and_history_are_independent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("estimates.json");
        let r = route();
        std::fs::write(&path, r#"{"local:cpu:test":{"per_ms":0.3,"samples":8},"bad":{"per_ms":-1,"samples":4}}"#).unwrap();
        let e = Estimates::load(&path);
        assert_eq!(e.measured_ms(&r, 10_000), Some(3_200));
        for x in [1_000, 2_000, 3_000, 4_000] { e.observe(&r, x, 500 + x / 2, true); }
        let mut history = crate::history::History::default();
        history.add(crate::history::Entry {
            at: 1, text: "Private history text".into(), program: "editor".into(), engine: "cpu".into(), inserted: false, timings: None,
        }, 1, 1);
        history.prune(u64::MAX, 1);
        assert!(history.entries().is_empty());
        history.clear();
        let loaded = Estimates::load(&path);
        assert_eq!(loaded.expected_ms(&r, 10_000), 5_500);
        let value: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(value["version"], VERSION);
        assert_eq!(loaded.store.lock().unwrap().models[&r.key], e.store.lock().unwrap().models[&r.key]);
        assert_eq!(loaded.store.lock().unwrap().legacy.len(), 1);
        assert!(!std::fs::read_to_string(path).unwrap().contains("local:cpu:test"));
    }

    #[test]
    fn malformed_entries_are_isolated_and_unknown_versions_are_not_migrated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("estimates.json");
        let r = route();
        let e = Estimates::load(&path);
        e.observe(&r, 100, 200, true);
        let mut value: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        value["models"][fingerprint(&"bad")] = serde_json::json!({"samples":2,"mean_size":1,"mean_ms":1,"m2_size":-1,"m2_ms":2,"co_moment":0});
        value["models"][fingerprint(&"bad covariance")] = serde_json::json!({"samples":2,"mean_size":1,"mean_ms":1,"m2_size":1,"m2_ms":2,"co_moment":99});
        std::fs::write(&path, value.to_string()).unwrap();
        assert_eq!(Estimates::load(&path).store.lock().unwrap().models.len(), 1);
        for raw in ["broken", "{\"version\":99}", "{\"version\":2,\"models\":[]}"] {
            std::fs::write(&path, raw).unwrap();
            assert_eq!(Estimates::load(&path).measured_ms(&r, 100), None);
        }
        std::fs::write(&path, "{\"version\":99}").unwrap();
        Estimates::load(&path).observe(&r, 10, 50, true);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"version\":99}");
    }

    #[test]
    fn identities_follow_execution_settings_not_names_or_transcripts() {
        let mut s = Settings::default();
        let original = local_speech(&s, "cpu", "test", 4).key;
        s.history = false;
        s.history_days = 90;
        s.statistics = false;
        s.microphone = "different device".into();
        assert_eq!(original, local_speech(&s, "cpu", "test", 4).key);
        assert_ne!(original, local_speech(&s, "cpu", "test", 8).key);
        assert_ne!(original, local_speech(&s, "onnx-cpu", "test", 0).key);
        s.vocabulary = "private vocabulary".into();
        assert_ne!(original, local_speech(&s, "cpu", "test", 4).key);
        s.server.provider = "custom".into();
        s.server.base_url = "https://private.example/v1".into();
        let server = server_speech(&s).unwrap().key;
        s.server.upload_format = crate::settings::UploadFormat::Opus;
        assert_ne!(server, server_speech(&s).unwrap().key);
        s.server.base_url = "https://other.example/v1".into();
        assert_ne!(server, server_speech(&s).unwrap().key);
        s.server.provider = "deepgram".into();
        let native = server_speech(&s).unwrap().key;
        s.server.upload_format = crate::settings::UploadFormat::Wav;
        assert_eq!(native, server_speech(&s).unwrap().key, "native adapters always send WAV");
        let local = Some(("cpu", "chat", 4));
        let cleanup_key = cleanup(&s, None, Some("a private transcript"), local).unwrap().key;
        assert_eq!(cleanup_key, cleanup(&s, None, Some("another transcript"), local).unwrap().key);
        let profile = Profile { instruction: "private profile instruction".into(), ..Profile::default() };
        assert_ne!(cleanup_key, cleanup(&s, Some(&profile), None, local).unwrap().key);
        assert!(valid_fingerprint(&cleanup_key) && valid_fingerprint(&server));
    }

    #[test]
    fn cleanup_excludes_disabled_empty_snippet_and_inactive_instruction_work() {
        let mut s = Settings::default();
        assert!(!cleanup_wanted(&s, None, None, true));
        s.cleanup.smart = true;
        assert!(cleanup_wanted(&s, None, None, true));
        assert!(!cleanup_wanted(&s, None, Some(""), true));
        assert!(!cleanup_wanted(&s, Some(&Profile { cleanup: profiles::CleanupChoice::Never, ..Profile::default() }), Some("Hello"), true));
        s.snippets.push(crate::snippets::Snippet { trigger: "signature".into(), text: "private saved text".into() });
        assert!(!cleanup_wanted(&s, None, Some("signature"), true));
        s.instructions.push(crate::instructions::Instruction {
            id: "test".into(), name: "Test".into(), keyword: "rewrite".into(), prompt: "private instruction".into(),
            enabled: true, output: crate::instructions::InstructionOutput::Text,
        });
        assert!(!cleanup_wanted(&s, None, Some("rewrite:"), true));
        assert!(cleanup_wanted(&s, None, Some("rewrite: hello"), true));
        let ordinary = cleanup(&s, None, Some("hello"), Some(("cpu", "model", 4))).unwrap();
        let transformed = cleanup(&s, None, Some("rewrite: hello"), Some(("cpu", "model", 4))).unwrap();
        assert_ne!(ordinary.key, transformed.key);
    }

    #[test]
    fn saved_connections_follow_resolved_routes_without_persisting_addresses_or_prompts() {
        let mut s = Settings::default();
        s.vocabulary = "private vocabulary".into();
        s.connections.push(crate::connections::Connection {
            id: "conn_a".into(), name: "Private account name".into(), provider: "custom".into(), base_url: "https://private.example/v1".into(),
        });
        s.server.connection_id = Some("conn_a".into());
        s.cleanup.connection_id = Some("conn_a".into());
        s.cleanup.engine = EngineKind::Server;
        let speech = server_speech(&s).unwrap();
        let profile = Profile { instruction: "private style instruction".into(), ..Profile::default() };
        let clean = cleanup(&s, Some(&profile), None, None).unwrap();
        s.connections[0].name = "Another display name".into();
        s.server.provider = "openai".into();
        s.server.base_url = "https://unused.example/v1".into();
        assert_eq!(speech.key, server_speech(&s).unwrap().key);
        s.connections[0].base_url = "https://private.example/other".into();
        assert_ne!(speech.key, server_speech(&s).unwrap().key);
        assert_ne!(clean.key, cleanup(&s, Some(&profile), None, None).unwrap().key);
        let changed_path = server_speech(&s).unwrap();
        s.connections[0].id = "conn_b".into();
        s.server.connection_id = Some("conn_b".into());
        assert_ne!(changed_path.key, server_speech(&s).unwrap().key);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("estimates.json");
        let e = Estimates::load(&path);
        e.observe(&speech, 100, 50, true);
        e.observe(&clean, 100, 200, true);
        let stored = std::fs::read_to_string(&path).unwrap();
        for private in ["private", "example", "conn_a", "vocabulary", "instruction", "transcript", "timestamp"] {
            assert!(!stored.contains(private), "no raw identity or individual record: {private}");
        }
        assert_eq!(Estimates::load(&path).store.lock().unwrap().models.len(), 2);
    }

    #[test]
    fn nonphysical_regressions_cannot_produce_negative_overhead_or_slope() {
        for points in [[(100, 1_000), (200, 800), (300, 600), (400, 400)], [(100, 100), (200, 300), (300, 500), (400, 700)]] {
            let e = Estimates::in_memory();
            let r = route();
            for (x, y) in points { e.observe(&r, x, y, true); }
            let m = e.store.lock().unwrap().models[&r.key];
            let line = m.line(r.default);
            assert!(line.overhead >= 0.0 && line.slope >= 0.0);
            assert!(e.expected_ms(&r, 1) > 0);
            assert!(e.expected_ms(&r, u64::MAX) <= MAX_MS as u64);
        }
    }

    #[test]
    fn the_ring_never_claims_done() {
        assert_eq!(progress(500.0, 1_000.0), 0.5);
        assert_eq!(progress(100.0, 0.0), 0.0);
        assert_eq!(progress(f64::NAN, 100.0), 0.0);
        for elapsed in [1_000.0, 10_000.0, 1e20] { assert!(progress(elapsed, 1_000.0) <= 0.98); }
    }
}
