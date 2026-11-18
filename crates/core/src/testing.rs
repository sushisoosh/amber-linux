//! Speech comparison tables and explicit transcript archives. Audio is session-only and
//! never enters an archive or the serialized settings-window payload.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Write;
use std::sync::Arc;

use crate::settings::{CleanupSettings, EngineKind, LocalSettings, ServerSettings, Settings};

pub const RESULT_LIMIT: usize = 20;
pub const AUDIO_BYTES_LIMIT: usize = 64 * 1024 * 1024;
pub const TABLE_LIMIT: usize = 10;
pub const TABLE_NAME_LIMIT: usize = 80;
pub const TABLE_BYTES_LIMIT: usize = 8 * 1024 * 1024;
const MAX_ID: u64 = (1 << 53) - 1;

/// Exactly the prepared PCM WAV used by the original run, never a compressed upload.
#[derive(Debug)]
pub struct Recording {
    pub wav: Vec<u8>,
    pub audio_ms: u64,
    pub prepared_audio_ms: u64,
    pub microphone: Option<String>,
    pub microphone_fallback: bool,
    pub interrupted: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Engine {
    pub engine: EngineKind,
    pub provider: String,
    pub model: String,
    pub runtime: Option<String>,
    pub base_url: Option<String>,
}

impl Engine {
    pub fn local(runtime: &str, model: &str) -> Self {
        Self { engine: EngineKind::Local, provider: "local".into(), model: model.into(), runtime: Some(runtime.into()), base_url: None }
    }

    pub fn server(provider: &str, base: &str, model: &str) -> Self {
        Self { engine: EngineKind::Server, provider: provider.into(), model: model.into(), runtime: None, base_url: Some(base.into()) }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    pub engine: EngineKind,
    pub local: LocalSettings,
    pub server: ServerSettings,
    pub cleanup: CleanupSettings,
    pub language: String,
    pub vocabulary: String,
    pub microphone: String,
}

impl From<&Settings> for Config {
    fn from(s: &Settings) -> Self {
        Self { engine: s.engine, local: s.local.clone(), server: s.server.clone(), cleanup: s.cleanup.clone(),
            language: s.language.clone(), vocabulary: s.vocabulary.clone(), microphone: s.microphone.clone() }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attempt {
    pub engine: Engine,
    pub ms: u64,
    pub error: Option<String>,
    pub upload: Option<crate::engine::UploadStats>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum CleanupStatus { NotRun, Disabled, Empty, Success, Skipped, Error }

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Result {
    pub id: u64,
    pub at: u64,
    pub recording_id: Option<u64>,
    pub source_test_id: Option<u64>,
    pub can_replay: bool,
    pub config: Config,
    pub microphone: Option<String>,
    pub microphone_fallback: bool,
    pub interrupted: bool,
    pub audio_ms: u64,
    pub prepared_audio_ms: u64,
    pub recognition_ms: Option<u64>,
    pub cleanup_ms: Option<u64>,
    pub attempts: Vec<Attempt>,
    pub actual_engine: Option<Engine>,
    pub cleanup_engine: Option<Engine>,
    pub fallback: bool,
    pub cleanup_status: CleanupStatus,
    pub cleanup_error: Option<String>,
    pub remote_audio: bool,
    pub remote_text: bool,
    pub raw: String,
    pub text: String,
    pub error: Option<String>,
    /// A live-session capability, never reconstructed from persisted recording IDs.
    #[serde(skip)]
    replay_id: Option<u64>,
}

impl Result {
    pub fn new(id: u64, at: u64, settings: &Settings) -> Self {
        Self { id, at, recording_id: None, source_test_id: None, can_replay: false,
            config: Config::from(settings), microphone: None, microphone_fallback: false,
            interrupted: false, audio_ms: 0, prepared_audio_ms: 0, recognition_ms: None, cleanup_ms: None,
            attempts: Vec::new(), actual_engine: None, cleanup_engine: None, fallback: false,
            cleanup_status: CleanupStatus::NotRun, cleanup_error: None, remote_audio: false, remote_text: false,
            raw: String::new(), text: String::new(), error: None, replay_id: None }
    }

    pub fn attempted(&mut self, engine: Engine, ms: u64, error: Option<String>) {
        self.remote_audio |= engine.engine == EngineKind::Server;
        if error.is_none() {
            self.fallback = self.config.engine == EngineKind::Server && engine.engine == EngineKind::Local;
            self.actual_engine = Some(engine.clone());
        }
        self.attempts.push(Attempt { engine, ms, error, upload: None });
    }

    pub fn cleaned(&mut self, text: String, error: Option<String>, remote: bool, ms: u64, empty: bool) {
        self.cleanup_ms = Some(ms);
        self.cleanup_status = if !self.config.cleanup.smart { CleanupStatus::Disabled }
            else if empty { CleanupStatus::Empty }
            else if error.is_none() { CleanupStatus::Success }
            else if self.cleanup_engine.is_some() { CleanupStatus::Error }
            else { CleanupStatus::Skipped };
        self.cleanup_error = error;
        self.remote_text = remote;
        self.text = text;
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Table {
    pub id: u64,
    pub name: String,
    pub saved_at: u64,
    pub results: Vec<Result>,
    // Recognizes an unchanged archive on retry, without trusting its persisted audio IDs.
    #[serde(skip)]
    fingerprint: [u8; 32],
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tables {
    version: u32,
    last_id: u64,
    tables: Vec<Table>,
}

impl Default for Tables {
    fn default() -> Self { Self { version: 1, last_id: 0, tables: Vec::new() } }
}

/// Bounded serialization runs on the storage worker, never under the session lock.
fn bounded_json(value: &impl Serialize) -> std::result::Result<Vec<u8>, String> {
    struct Bounded(Vec<u8>);
    impl Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > TABLE_BYTES_LIMIT.saturating_sub(self.0.len()) {
                return Err(std::io::Error::other("Saved comparisons exceed the 8 MiB safety limit"));
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
    }
    let mut out = Bounded(Vec::new());
    serde_json::to_writer(&mut out, value).map_err(|_| "Saved comparisons exceed the 8 MiB safety limit".to_string())?;
    Ok(out.0)
}

impl Tables {
    pub fn from_bytes(bytes: &[u8]) -> std::result::Result<Self, String> {
        if bytes.len() > TABLE_BYTES_LIMIT { return Err("Saved comparisons exceed the 8 MiB safety limit".into()); }
        let mut stored: Self = serde_json::from_slice(bytes)
            .map_err(|_| "Saved comparisons are corrupt (invalid data); existing storage was not changed".to_string())?;
        if stored.version != 1 { return Err("Unsupported saved comparison schema; existing storage was not changed".into()); }
        let mut ids = HashSet::new();
        let mut row_ids = HashSet::new();
        if stored.tables.len() > TABLE_LIMIT || stored.last_id > MAX_ID || stored.tables.iter().any(|table| {
            table.id == 0 || table.id > stored.last_id || !ids.insert(table.id)
                || table.name.trim().is_empty() || table.name.chars().count() > TABLE_NAME_LIMIT
                || table.results.is_empty() || table.results.len() > RESULT_LIMIT
                || table.results.iter().any(|row| row.id == 0 || row.id > MAX_ID || !row_ids.insert(row.id)
                    || row.recording_id.is_some_and(|id| id > MAX_ID) || row.source_test_id.is_some_and(|id| id > MAX_ID))
        }) {
            return Err("Saved comparisons have invalid table limits or IDs; existing storage was not changed".into());
        }
        for table in &mut stored.tables {
            for row in &mut table.results { row.can_replay = false; }
            table.fingerprint = Sha256::digest(bounded_json(table)?).into();
        }
        Ok(stored)
    }

    fn to_bytes(&mut self) -> std::result::Result<Vec<u8>, String> {
        let mut disk = self.clone();
        for (live, stored) in self.tables.iter_mut().zip(&mut disk.tables) {
            for row in &mut stored.results { row.can_replay = false; }
            live.fingerprint = Sha256::digest(bounded_json(stored)?).into();
        }
        bounded_json(&disk)
    }
}

pub enum TableChange { Save { name: String, at: u64 }, Delete(u64), Reload }

/// A proposed table change. Only a successful encrypted write/read may commit this value.
pub struct TableTransaction {
    tables: Tables,
    clear_current: bool,
    next_test_id: u64,
}

impl TableTransaction {
    pub fn persist(mut self, write: impl FnOnce(&[u8]) -> std::result::Result<(), String>) -> std::result::Result<Self, String> {
        write(&self.tables.to_bytes()?)?;
        Ok(self)
    }

    pub fn restore(mut self, mut stored: Tables) -> std::result::Result<Self, String> {
        // Keep live capabilities only for byte-identical tables already saved in this process.
        // Imported row IDs are rebound even after a failed startup load and later recordings.
        let mut rebound = HashMap::new();
        // Seed every unchanged table before importing any row: a newer table can refer to
        // an older table later in the file, or to one of its already-rebound orphan IDs.
        for table in &stored.tables {
            if let Some(live) = self.tables.tables.iter().find(|live| live.id == table.id && live.fingerprint == table.fingerprint) {
                for (disk, runtime) in table.results.iter().zip(&live.results) {
                    for (old, new) in [(Some(disk.id), Some(runtime.id)), (disk.recording_id, runtime.recording_id),
                        (disk.source_test_id, runtime.source_test_id)] {
                        if let (Some(old), Some(new)) = (old, new) {
                            if rebound.insert(old, new).is_some_and(|existing| existing != new) {
                                return Err("Saved comparison lineage is inconsistent; existing storage was not changed".into());
                            }
                        }
                    }
                }
            }
        }
        for table in &mut stored.tables {
            if let Some(live) = self.tables.tables.iter().find(|live| live.id == table.id && live.fingerprint == table.fingerprint) {
                *table = live.clone();
                continue;
            }
            for row in &mut table.results {
                // Include orphaned source/recording references, so their displayed lineage
                // cannot accidentally name an unrelated test from this process either.
                for old in [Some(row.id), row.recording_id, row.source_test_id].into_iter().flatten() {
                    if let std::collections::hash_map::Entry::Vacant(entry) = rebound.entry(old) {
                        self.next_test_id = next_id(self.next_test_id)?;
                        entry.insert(self.next_test_id);
                    }
                }
                row.id = rebound[&row.id];
                row.recording_id = row.recording_id.map(|id| rebound[&id]);
                row.can_replay = false;
                row.replay_id = None;
            }
        }
        for table in &mut stored.tables {
            if self.tables.tables.iter().any(|live| live.id == table.id && live.fingerprint == table.fingerprint) { continue; }
            for row in &mut table.results {
                row.source_test_id = row.source_test_id.map(|id| rebound.get(&id).copied().unwrap_or(id));
            }
        }
        stored.last_id = stored.last_id.max(self.tables.last_id);
        self.tables = stored;
        Ok(self)
    }
}

fn next_id(id: u64) -> std::result::Result<u64, String> {
    if id >= MAX_ID { return Err("Comparison IDs are exhausted; no data was changed".into()); }
    Ok(id + 1)
}

#[derive(Clone, Copy, Debug, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Phase { #[default] Idle, Recording, Recognizing, Cleaning, Cancelling }

#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub revision: u64,
    pub phase: Phase,
    pub active: Option<Result>,
    pub results: Vec<Result>,
    pub saved_tables: Vec<Table>,
    pub table_busy: bool,
    pub table_storage_error: Option<String>,
    pub audio_bytes: usize,
    pub recording_count: usize,
    #[serde(skip)]
    recordings: VecDeque<(u64, Arc<Recording>)>,
    #[serde(skip)]
    next_test_id: u64,
    #[serde(skip)]
    last_table_id: u64,
    #[serde(skip)]
    tables_loaded: bool,
}

impl Session {
    /// UI snapshots do not retain even shared audio buffers.
    pub fn snapshot(&self) -> Self {
        Self { revision: self.revision, phase: self.phase, active: self.active.clone(), results: self.results.clone(),
            saved_tables: self.saved_tables.clone(), table_busy: self.table_busy, table_storage_error: self.table_storage_error.clone(),
            audio_bytes: self.audio_bytes, recording_count: self.recording_count, next_test_id: self.next_test_id,
            last_table_id: self.last_table_id, tables_loaded: self.tables_loaded, recordings: VecDeque::new() }
    }

    pub fn ensure_idle(&self) -> std::result::Result<(), String> {
        if self.table_busy { return Err("A saved comparison operation is in progress".into()); }
        if self.phase != Phase::Idle || self.active.is_some() { return Err("Finish or cancel the active test first".into()); }
        Ok(())
    }

    pub fn allocate_test_id(&mut self) -> std::result::Result<u64, String> {
        self.ensure_idle()?;
        self.next_test_id = next_id(self.next_test_id)?;
        Ok(self.next_test_id)
    }

    pub fn begin(&mut self, result: Result) -> std::result::Result<(), String> {
        self.ensure_idle()?;
        self.next_test_id = self.next_test_id.max(result.id);
        self.active = Some(result);
        self.phase = Phase::Recording;
        self.revision += 1;
        Ok(())
    }

    pub fn update(&mut self, id: u64, phase: Phase) -> bool {
        if self.active.as_ref().is_none_or(|r| r.id != id) || self.phase == Phase::Cancelling { return false; }
        self.phase = phase;
        self.revision += 1;
        true
    }

    pub fn complete(&mut self, mut result: Result, recording: Option<Arc<Recording>>) -> bool {
        if !self.update(result.id, Phase::Idle) { return false; }
        self.active = None;
        if let Some(recording) = recording {
            let id = *result.recording_id.get_or_insert(result.id);
            result.replay_id = Some(id);
            self.recordings.retain(|(stored_id, _)| *stored_id != id);
            self.recordings.push_back((id, recording));
        }
        self.results.insert(0, result);
        self.results.truncate(RESULT_LIMIT);
        self.prune_audio(AUDIO_BYTES_LIMIT);
        true
    }

    pub fn recording(&self, test_id: u64) -> Option<(u64, Arc<Recording>)> {
        let id = self.results.iter().chain(self.saved_tables.iter().flat_map(|t| &t.results))
            .find(|r| r.id == test_id)?.replay_id?;
        self.recordings.iter().find(|(stored_id, _)| *stored_id == id).map(|(_, audio)| (id, audio.clone()))
    }

    fn prune_audio(&mut self, limit: usize) {
        self.recordings.retain(|(id, _)| self.results.iter().chain(self.saved_tables.iter().flat_map(|t| &t.results))
            .any(|r| r.replay_id == Some(*id)));
        self.audio_bytes = self.recordings.iter().map(|(_, audio)| audio.wav.len()).sum();
        while self.audio_bytes > limit {
            if let Some((_, audio)) = self.recordings.pop_front() { self.audio_bytes -= audio.wav.len(); }
        }
        self.recording_count = self.recordings.len();
        for result in self.results.iter_mut().chain(self.saved_tables.iter_mut().flat_map(|t| &mut t.results)) {
            result.can_replay = self.recordings.iter().any(|(id, _)| Some(*id) == result.replay_id);
        }
    }

    pub fn cancel(&mut self) {
        if self.active.is_some() {
            self.active = None;
            self.phase = Phase::Cancelling;
            self.revision += 1;
        }
    }

    pub fn settled(&mut self) {
        if self.phase == Phase::Cancelling {
            self.phase = Phase::Idle;
            self.revision += 1;
        }
    }

    pub fn clear(&mut self, id: Option<u64>) -> std::result::Result<(), String> {
        self.ensure_idle()?;
        self.results.retain(|r| id.is_some_and(|id| r.id != id));
        self.prune_audio(AUDIO_BYTES_LIMIT);
        self.revision += 1;
        Ok(())
    }

    pub fn prepare_tables(&mut self, change: TableChange) -> std::result::Result<TableTransaction, String> {
        self.ensure_idle()?;
        if !matches!(change, TableChange::Reload) {
            if let Some(error) = &self.table_storage_error { return Err(format!("Reload saved comparisons before changing them: {error}")); }
            if !self.tables_loaded { return Err("Reload saved comparisons before changing them".into()); }
        }
        let mut tables = Tables { version: 1, last_id: self.last_table_id, tables: self.saved_tables.clone() };
        let clear_current = matches!(change, TableChange::Save { .. });
        match change {
            TableChange::Save { name, at } => {
                if self.results.is_empty() { return Err("There are no current comparisons to save".into()); }
                if tables.tables.len() >= TABLE_LIMIT { return Err("Saved comparison limit reached (10 tables); delete a saved table first".into()); }
                let id = next_id(tables.last_id)?;
                let name = name.trim();
                if name.chars().count() > TABLE_NAME_LIMIT { return Err("Comparison names must be at most 80 characters".into()); }
                let name = if name.is_empty() { format!("Comparison {id}") } else { name.to_string() };
                tables.tables.insert(0, Table { id, name, saved_at: at, results: self.results.clone(), fingerprint: [0; 32] });
                tables.last_id = id;
            }
            TableChange::Delete(id) => {
                if !tables.tables.iter().any(|t| t.id == id) { return Err("This saved comparison table no longer exists".into()); }
                tables.tables.retain(|t| t.id != id);
            }
            TableChange::Reload => (),
        }
        self.table_busy = true;
        self.revision += 1;
        Ok(TableTransaction { tables, clear_current, next_test_id: self.next_test_id })
    }

    pub fn finish_tables(&mut self, outcome: std::result::Result<TableTransaction, String>) -> std::result::Result<(), String> {
        self.table_busy = false;
        self.revision += 1;
        match outcome {
            Ok(transaction) => {
                self.saved_tables = transaction.tables.tables;
                self.last_table_id = transaction.tables.last_id;
                self.next_test_id = transaction.next_test_id;
                if transaction.clear_current { self.results.clear(); }
                self.tables_loaded = true;
                self.table_storage_error = None;
                self.prune_audio(AUDIO_BYTES_LIMIT);
                Ok(())
            }
            Err(error) => {
                self.table_storage_error = Some(error.clone());
                Err(error)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshots_and_actual_fallback_are_not_current_settings() {
        let mut s = Settings::default();
        s.engine = EngineKind::Server;
        let mut result = Result::new(1, 123, &s);
        s.server.model = "new-model".into();
        s.cleanup.smart = !s.cleanup.smart;
        result.attempted(Engine::server("groq", "https://api.groq.com", "old-model"), 120, Some("Unavailable".into()));
        result.attempted(Engine::local("cpu", "actual-model"), 400, None);
        assert!(result.fallback && result.remote_audio);
        assert_eq!(result.actual_engine.unwrap().model, "actual-model");
        assert_ne!(result.config.server.model, s.server.model);
        assert_ne!(result.config.cleanup.smart, s.cleanup.smart);
        assert_eq!(result.attempts.len(), 2);
    }

    #[test]
    fn cancelled_and_old_workers_cannot_publish_or_clear_a_new_run() {
        let s = Settings::default();
        let mut session = Session::default();
        session.begin(Result::new(1, 0, &s)).unwrap();
        session.cancel();
        assert!(!session.complete(Result::new(1, 0, &s), None));
        session.settled();
        session.begin(Result::new(2, 0, &s)).unwrap();
        assert!(!session.update(1, Phase::Cleaning));
        assert!(!session.complete(Result::new(1, 0, &s), None));
        assert!(session.complete(Result::new(2, 0, &s), None));
        assert_eq!(session.results.len(), 1);
    }

    #[test]
    fn bounded_newest_first_and_explicit_clear() {
        let mut session = Session::default();
        for id in 0..25 {
            let result = Result::new(id, 0, &Settings::default());
            session.begin(result.clone()).unwrap();
            assert!(session.complete(result, None));
        }
        assert_eq!(session.results.len(), RESULT_LIMIT);
        assert_eq!(session.results[0].id, 24);
        session.clear(Some(24)).unwrap();
        assert_eq!(session.results[0].id, 23);
        session.clear(None).unwrap();
        assert!(session.results.is_empty());
    }

    #[test]
    fn cleanup_failure_keeps_rules_and_does_not_hide_remote_exposure() {
        let mut s = Settings::default();
        s.cleanup.smart = true;
        let mut result = Result::new(1, 0, &s);
        result.recognition_ms = Some(450);
        result.cleanup_engine = Some(Engine::server("custom", "https://cleanup.example/v1", "cleanup-model"));
        result.cleaned("Rule text".into(), Some("Server failed".into()), true, 900, false);
        assert_eq!(result.cleanup_status, CleanupStatus::Error);
        assert!(result.remote_text);
        assert_eq!(result.text, "Rule text");
        assert_eq!((result.recognition_ms, result.cleanup_ms), (Some(450), Some(900)));
        result.cleanup_engine = None;
        result.cleaned("Rule text".into(), Some("Model not installed".into()), false, 2, false);
        assert_eq!(result.cleanup_status, CleanupStatus::Skipped);
        assert!(!result.remote_text);
        result.cleaned(String::new(), None, false, 0, true);
        assert_eq!(result.cleanup_status, CleanupStatus::Empty);
        result.config.cleanup.smart = false;
        result.cleaned("Rule text".into(), None, false, 0, false);
        assert_eq!(result.cleanup_status, CleanupStatus::Disabled);
    }

    #[test]
    fn result_contract_has_no_delivery_or_persistence_settings() {
        let mut s = Settings::default();
        s.insert.paste_on_completion = true;
        s.terminal.run_commands = true;
        s.log_text = true;
        s.history = true;
        let result = serde_json::to_value(Result::new(1, 0, &s)).unwrap();
        for forbidden in ["insert", "terminal", "logText", "history", "phone", "profiles", "snippets"] {
            assert!(result["config"].get(forbidden).is_none());
        }
        assert!(result["recognitionMs"].is_null());
        assert_eq!(result["cleanupStatus"], "notRun");
    }

    fn audio(bytes: usize) -> Arc<Recording> {
        Arc::new(Recording { wav: vec![1; bytes], audio_ms: 5000, prepared_audio_ms: 4800,
            microphone: Some("Original microphone".into()), microphone_fallback: false, interrupted: false })
    }

    #[test]
    fn replay_shares_original_bytes_but_captures_new_settings() {
        let mut settings = Settings::default();
        let mut session = Session::default();
        let original = audio(128);
        let first = Result::new(1, 100, &settings);
        session.begin(first.clone()).unwrap();
        session.complete(first, Some(original.clone()));
        let (recording_id, reused) = session.recording(1).unwrap();
        assert!(Arc::ptr_eq(&original, &reused));
        settings.engine = EngineKind::Server;
        settings.server.model = "different-model".into();
        settings.server.upload_format = crate::settings::UploadFormat::Opus;
        let mut replay = Result::new(2, 200, &settings);
        replay.recording_id = Some(recording_id);
        replay.source_test_id = Some(1);
        session.begin(replay.clone()).unwrap();
        session.complete(replay, Some(reused));
        assert_eq!((session.audio_bytes, session.recording_count), (128, 1));
        assert_eq!(session.results[0].recording_id, Some(1));
        assert_eq!(session.results[0].source_test_id, Some(1));
        assert_ne!(session.results[0].config.server.model, session.results[1].config.server.model);
        assert_eq!(session.results[0].config.server.upload_format, crate::settings::UploadFormat::Opus);
        assert_eq!(session.results[1].config.server.upload_format, crate::settings::UploadFormat::Wav);
        assert_eq!(session.recording(2).unwrap().1.wav, original.wav);
        session.clear(Some(1)).unwrap();
        assert!(session.recording(1).is_none());
        assert!(Arc::ptr_eq(&session.recording(2).unwrap().1, &original));
        session.clear(None).unwrap();
        assert_eq!((session.audio_bytes, session.recording_count), (0, 0));
        assert_eq!(Arc::strong_count(&original), 1);
    }

    #[test]
    fn audio_budget_evicts_oldest_payload_without_losing_comparisons() {
        let mut session = Session::default();
        for id in 1..=3 {
            let result = Result::new(id, 0, &Settings::default());
            session.begin(result.clone()).unwrap();
            session.complete(result, Some(audio(4)));
        }
        session.prune_audio(8);
        assert_eq!(session.audio_bytes, 8);
        assert_eq!(session.results.len(), 3);
        assert_eq!(session.recording_count, 2);
        assert!(session.recording(1).is_none());
        assert!(!session.results[2].can_replay);
        assert!(session.results[0].can_replay);
        let (id, shared) = session.recording(2).unwrap();
        let mut replay = Result::new(4, 0, &Settings::default());
        replay.recording_id = Some(id);
        replay.source_test_id = Some(2);
        session.begin(replay.clone()).unwrap();
        session.complete(replay, Some(shared));
        session.prune_audio(4);
        assert!(session.recording(3).is_none());
        assert!(session.recording(2).is_some() && session.recording(4).is_some());
        assert_eq!(session.audio_bytes, 4);
    }

    #[test]
    fn failed_requests_can_replay_but_cancelled_runs_do_not_retain_new_audio() {
        let mut session = Session::default();
        let mut failed = Result::new(1, 0, &Settings::default());
        failed.error = Some("Server unavailable".into());
        session.begin(failed.clone()).unwrap();
        session.complete(failed, Some(audio(64)));
        assert!(session.recording(1).is_some());
        let cancelled = Result::new(2, 0, &Settings::default());
        session.begin(cancelled.clone()).unwrap();
        session.cancel();
        assert!(!session.complete(cancelled, Some(audio(128))));
        assert_eq!(session.audio_bytes, 64);
        assert!(session.recording(2).is_none());
    }

    #[test]
    fn result_eviction_releases_audio_and_serialization_never_exposes_it() {
        let mut session = Session::default();
        for id in 1..=25 {
            let result = Result::new(id, 0, &Settings::default());
            session.begin(result.clone()).unwrap();
            session.complete(result, Some(audio(4)));
        }
        assert_eq!((session.results.len(), session.recording_count), (20, 20));
        assert_eq!(session.audio_bytes, 80);
        assert!(session.recording(5).is_none());
        let json = serde_json::to_value(&session).unwrap();
        assert!(json.get("recordings").is_none());
        assert!(!json.to_string().contains("\"wav\":"));
        assert!(json["results"][0]["canReplay"].as_bool().unwrap());
    }

    fn loaded() -> Session {
        let mut session = Session::default();
        reload(&mut session, None);
        session
    }

    fn reload(session: &mut Session, bytes: Option<&[u8]>) {
        let transaction = session.prepare_tables(TableChange::Reload).unwrap();
        let tables = bytes.map(|bytes| Tables::from_bytes(bytes).unwrap()).unwrap_or_default();
        session.finish_tables(transaction.restore(tables)).unwrap();
    }

    fn add(session: &mut Session, recording: Option<Arc<Recording>>) -> u64 {
        let id = session.allocate_test_id().unwrap();
        let result = Result::new(id, 123, &Settings::default());
        session.begin(result.clone()).unwrap();
        assert!(session.complete(result, recording));
        id
    }

    fn save(session: &mut Session, name: &str) -> Vec<u8> {
        let transaction = session.prepare_tables(TableChange::Save { name: name.into(), at: 456 }).unwrap();
        let mut bytes = Vec::new();
        session.finish_tables(transaction.persist(|plain| { bytes = plain.to_vec(); Ok(()) })).unwrap();
        bytes
    }

    fn delete(session: &mut Session, id: u64) -> Vec<u8> {
        let transaction = session.prepare_tables(TableChange::Delete(id)).unwrap();
        let mut bytes = Vec::new();
        session.finish_tables(transaction.persist(|plain| { bytes = plain.to_vec(); Ok(()) })).unwrap();
        bytes
    }

    #[test]
    fn saves_and_deletes_commit_only_after_success_and_clear_is_current_only() {
        let mut session = loaded();
        add(&mut session, Some(audio(4)));
        let first = save(&mut session, "  First  ");
        assert!(session.results.is_empty());
        assert_eq!((session.saved_tables[0].id, session.saved_tables[0].saved_at), (1, 456));
        assert_eq!(session.saved_tables[0].name, "First");
        let id = add(&mut session, Some(audio(8)));
        let transaction = session.prepare_tables(TableChange::Save { name: String::new(), at: 789 }).unwrap();
        assert!(session.table_busy);
        assert!(session.begin(Result::new(99, 0, &Settings::default())).is_err());
        assert!(session.clear(None).is_err());
        assert!(session.prepare_tables(TableChange::Reload).is_err());
        assert_eq!(session.saved_tables.len(), 1);
        let failed = transaction.persist(|_| Err("Wallet locked".into()));
        assert!(session.finish_tables(failed).is_err());
        assert!(!session.table_busy);
        assert_eq!(session.results[0].id, id);
        assert_eq!(session.saved_tables.len(), 1);
        assert_eq!(session.audio_bytes, 12);
        assert!(session.prepare_tables(TableChange::Delete(1)).is_err());
        reload(&mut session, Some(&first));
        let second = save(&mut session, " ");
        assert_eq!(session.saved_tables[0].name, "Comparison 2");
        let transaction = session.prepare_tables(TableChange::Delete(1)).unwrap();
        assert!(session.finish_tables(transaction.persist(|_| Err("Disk full".into()))).is_err());
        assert_eq!(session.saved_tables.len(), 2);
        reload(&mut session, Some(&second));
        session.clear(None).unwrap();
        session.clear(Some(id)).unwrap();
        assert_eq!(session.saved_tables.len(), 2);
        assert_eq!(session.audio_bytes, 12);
        delete(&mut session, 1);
        assert_eq!(session.audio_bytes, 8);
        assert!(session.prepare_tables(TableChange::Delete(1)).is_err());
    }

    #[test]
    fn active_and_cancelling_runs_cannot_cross_table_boundaries() {
        let mut session = loaded();
        add(&mut session, Some(audio(4)));
        save(&mut session, "Original");
        let id = session.allocate_test_id().unwrap();
        let result = Result::new(id, 0, &Settings::default());
        session.begin(result.clone()).unwrap();
        for phase in [Phase::Recording, Phase::Recognizing, Phase::Cleaning, Phase::Cancelling] {
            if phase == Phase::Cancelling { session.cancel(); } else { assert!(session.update(id, phase)); }
            assert!(session.clear(None).is_err());
            assert!(session.clear(Some(1)).is_err());
            assert!(session.prepare_tables(TableChange::Save { name: "Late".into(), at: 0 }).is_err());
            assert!(session.prepare_tables(TableChange::Delete(1)).is_err());
            assert!(session.prepare_tables(TableChange::Reload).is_err());
        }
        assert!(!session.complete(result.clone(), Some(audio(16))));
        session.settled();
        session.clear(None).unwrap();
        add(&mut session, None);
        save(&mut session, "Next");
        assert!(!session.complete(result, Some(audio(16))));
        assert!(session.results.is_empty());
        assert_eq!((session.saved_tables.len(), session.audio_bytes), (2, 4));
    }

    #[test]
    fn table_limits_names_and_monotonic_ids_survive_deletion_and_restart() {
        let mut session = loaded();
        assert!(session.prepare_tables(TableChange::Save { name: String::new(), at: 0 }).is_err());
        add(&mut session, None);
        assert!(session.prepare_tables(TableChange::Save { name: "x".repeat(81), at: 0 }).is_err());
        save(&mut session, &"x".repeat(80));
        for _ in 1..TABLE_LIMIT { add(&mut session, None); save(&mut session, ""); }
        add(&mut session, None);
        assert!(session.prepare_tables(TableChange::Save { name: "Overflow".into(), at: 0 }).is_err());
        assert_eq!(session.saved_tables.len(), 10);
        assert_eq!(session.results.len(), 1);
        for id in 1..10 { delete(&mut session, id); }
        let bytes = delete(&mut session, 10);
        let mut restarted = Session::default();
        reload(&mut restarted, Some(&bytes));
        add(&mut restarted, None);
        save(&mut restarted, "");
        assert_eq!(restarted.saved_tables[0].id, 11);
        assert_eq!(restarted.saved_tables[0].name, "Comparison 11");
    }

    #[test]
    fn persistence_preserves_complete_comparison_snapshots_but_never_audio() {
        let mut session = loaded();
        let id = add(&mut session, Some(audio(4)));
        let row = &mut session.results[0];
        row.raw = "Uncleaned transcript".into();
        row.text = "Cleaned transcript.".into();
        row.error = Some("Recognition failed".into());
        row.recognition_ms = Some(234);
        row.cleanup_ms = Some(56);
        row.cleanup_status = CleanupStatus::Error;
        row.cleanup_error = Some("Cleanup failed".into());
        row.attempted(Engine::server("custom", "https://example.com/v1", "pinned-model"), 123, None);
        row.attempts[0].upload = Some(crate::engine::UploadStats { format: "opus".into(), input_bytes: 800,
            upload_bytes: 100, encoding_ms: 4, note: Some("Compressed".into()) });
        let mut expected = serde_json::to_value(&*row).unwrap();
        expected["canReplay"] = false.into();
        let bytes = save(&mut session, "Snapshot");
        let stored = Tables::from_bytes(&bytes).unwrap();
        assert_eq!(serde_json::to_value(&stored.tables[0].results[0]).unwrap(), expected);
        let json = String::from_utf8(bytes.clone()).unwrap();
        for forbidden in ["\"wav\":", "\"recordings\":", "\"replayId\":", "\"fingerprint\":", "\"apiKey\":"] {
            assert!(!json.contains(forbidden));
        }
        let mut restarted = Session::default();
        reload(&mut restarted, Some(&bytes));
        assert!(restarted.recording(id).is_none());
        assert_eq!((restarted.audio_bytes, restarted.recording_count), (0, 0));
        assert!(!restarted.saved_tables[0].results[0].can_replay);
        assert!(session.recording(id).is_some());
    }

    #[test]
    fn replay_and_clear_share_audio_across_tables_until_the_last_reference_is_removed() {
        let mut session = loaded();
        let original = audio(4);
        let first = add(&mut session, Some(original.clone()));
        save(&mut session, "First");
        let (recording_id, shared) = session.recording(first).unwrap();
        let id = session.allocate_test_id().unwrap();
        let mut replay = Result::new(id, 999, &Settings::default());
        replay.recording_id = Some(recording_id);
        replay.source_test_id = Some(first);
        session.begin(replay.clone()).unwrap();
        session.complete(replay, Some(shared));
        assert_eq!(session.saved_tables[0].results.len(), 1);
        assert_eq!(session.results[0].id, id);
        save(&mut session, "Second");
        assert_eq!((session.audio_bytes, session.recording_count), (4, 1));
        for _ in 0..25 { add(&mut session, None); }
        session.clear(None).unwrap();
        delete(&mut session, 1);
        assert!(session.recording(first).is_none());
        assert!(Arc::ptr_eq(&session.recording(id).unwrap().1, &original));
        let snapshot = session.snapshot();
        assert!(snapshot.recordings.is_empty());
        assert_eq!(Arc::strong_count(&original), 2, "snapshots never retain shared audio");
        delete(&mut session, 2);
        assert_eq!((session.audio_bytes, session.recording_count), (0, 0));
        assert_eq!(Arc::strong_count(&original), 1);
    }

    #[test]
    fn late_reload_cannot_bind_persisted_ids_to_new_session_audio() {
        let mut previous = loaded();
        add(&mut previous, Some(audio(4)));
        let bytes = save(&mut previous, "Previous session");
        let mut session = Session::default();
        let _transaction = session.prepare_tables(TableChange::Reload).unwrap();
        assert!(session.finish_tables(Err("Wallet locked".into())).is_err());
        let live = add(&mut session, Some(audio(8)));
        assert_eq!(live, 1, "deliberately collide with the old persisted IDs");
        reload(&mut session, Some(&bytes));
        let restored = session.saved_tables[0].results[0].id;
        assert_ne!(live, restored);
        assert_eq!(session.saved_tables[0].results[0].recording_id, Some(restored));
        assert!(!session.saved_tables[0].results[0].can_replay);
        assert!(session.recording(restored).is_none());
        assert_eq!(session.recording(live).unwrap().1.wav.len(), 8);
        session.clear(None).unwrap();
        assert_eq!(session.audio_bytes, 0, "persisted metadata is not an audio reference");
        let new = add(&mut session, Some(audio(16)));
        assert_ne!(new, restored);
        assert!(session.recording(restored).is_none());
    }

    #[test]
    fn restored_orphan_lineage_ids_cannot_name_unrelated_live_tests() {
        let mut previous = loaded();
        add(&mut previous, None);
        previous.results[0].recording_id = Some(42);
        previous.results[0].source_test_id = Some(42);
        let bytes = save(&mut previous, "Orphaned source");
        let mut session = Session::default();
        for _ in 0..42 { add(&mut session, None); }
        reload(&mut session, Some(&bytes));
        let row = &session.saved_tables[0].results[0];
        assert!(row.id > 42);
        assert!(row.recording_id.unwrap() > 42);
        assert_eq!(row.recording_id, row.source_test_id);
        assert!(!row.can_replay);
        let id = row.id;
        let orphan = row.recording_id.unwrap();
        reload(&mut session, Some(&bytes));
        assert_eq!(session.saved_tables[0].results[0].id, id, "unchanged reload keeps runtime IDs stable");
        assert!(add(&mut session, Some(audio(4))) > orphan);
        assert!(session.recording(id).is_none());
    }

    #[test]
    fn unchanged_reload_keeps_live_audio_but_replaced_tables_cannot_claim_it() {
        let mut session = loaded();
        let id = add(&mut session, Some(audio(4)));
        let bytes = save(&mut session, "Live");
        reload(&mut session, Some(&bytes));
        assert!(session.recording(id).is_some());
        let mut changed = Tables::from_bytes(&bytes).unwrap();
        changed.tables[0].results[0].text = "Different stored comparison".into();
        changed.tables[0].results[0].can_replay = true;
        reload(&mut session, Some(&serde_json::to_vec(&changed).unwrap()));
        assert_ne!(session.saved_tables[0].results[0].id, id);
        assert!(!session.saved_tables[0].results[0].can_replay);
        assert!(session.recording(id).is_none());
        assert_eq!(session.audio_bytes, 0);
    }

    #[test]
    fn reload_of_unconfirmed_replay_table_preserves_lineage_but_not_audio_authority() {
        let mut session = loaded();
        let first = add(&mut session, Some(audio(4)));
        save(&mut session, "A");
        let (recording_id, shared) = session.recording(first).unwrap();
        let current = session.allocate_test_id().unwrap();
        let mut replay = Result::new(current, 999, &Settings::default());
        replay.recording_id = Some(recording_id);
        replay.source_test_id = Some(first);
        session.begin(replay.clone()).unwrap();
        assert!(session.complete(replay, Some(shared)));
        let transaction = session.prepare_tables(TableChange::Save { name: "B".into(), at: 1000 }).unwrap();
        let mut committed = Vec::new();
        let outcome = transaction.persist(|plain| {
            committed = plain.to_vec();
            Err("Write durability could not be confirmed".into())
        });
        assert!(session.finish_tables(outcome).is_err());
        assert_eq!(session.results[0].id, current);
        assert_eq!(session.saved_tables.len(), 1);
        reload(&mut session, Some(&committed));
        // B precedes A on disk: mappings must be seeded before importing the first table.
        assert_eq!(session.saved_tables[0].name, "B");
        let imported = session.saved_tables[0].results[0].id;
        assert_ne!(imported, current);
        assert_eq!(session.saved_tables[0].results[0].recording_id, Some(recording_id));
        assert_eq!(session.saved_tables[0].results[0].source_test_id, Some(first));
        assert_eq!(session.saved_tables[1].results[0].id, first);
        assert!(session.recording(first).is_some());
        assert!(session.recording(current).is_some());
        assert!(!session.saved_tables[0].results[0].can_replay);
        assert!(session.recording(imported).is_none());
        session.saved_tables[0].results[0].can_replay = true;
        assert!(session.recording(imported).is_none(), "a UI flag is not a replay capability");
        session.saved_tables[0].results[0].can_replay = false;
        let confirmed = save(&mut session, "Confirmed current");
        let stored = Tables::from_bytes(&confirmed).unwrap();
        let b = &stored.tables.iter().find(|table| table.name == "B").unwrap().results[0];
        assert_eq!((b.recording_id, b.source_test_id), (Some(recording_id), Some(first)));
        reload(&mut session, Some(&confirmed));
        assert!(session.recording(imported).is_none());
        delete(&mut session, 3);
        delete(&mut session, 1);
        assert_eq!(session.audio_bytes, 0, "imported lineage must not retain live audio");
    }

    #[test]
    fn reload_seeds_rebound_row_and_orphan_reference_ids_from_unchanged_tables() {
        let mut previous = loaded();
        add(&mut previous, None);
        previous.results[0].recording_id = Some(42);
        previous.results[0].source_test_id = Some(43);
        let first = save(&mut previous, "A");
        let mut session = loaded();
        let current = add(&mut session, Some(audio(4)));
        reload(&mut session, Some(&first));
        let a = session.saved_tables[0].results[0].clone();
        assert_ne!(a.id, 1);
        assert_ne!(a.recording_id, Some(42));
        assert_ne!(a.source_test_id, Some(43));
        let mut changed = Tables::from_bytes(&first).unwrap();
        let mut references_row = Result::new(2, 123, &Settings::default());
        references_row.recording_id = Some(42);
        references_row.source_test_id = Some(1);
        let mut references_orphan = Result::new(3, 124, &Settings::default());
        references_orphan.recording_id = Some(42);
        references_orphan.source_test_id = Some(43);
        changed.last_id = 2;
        changed.tables.insert(0, Table { id: 2, name: "B".into(), saved_at: 456,
            results: vec![references_row, references_orphan], fingerprint: [0; 32] });
        reload(&mut session, Some(&changed.to_bytes().unwrap()));
        let imported = &session.saved_tables[0].results;
        assert_eq!(imported[0].source_test_id, Some(a.id));
        assert_eq!(imported[1].source_test_id, a.source_test_id);
        for row in imported {
            assert_eq!(row.recording_id, a.recording_id);
            assert!(session.recording(row.id).is_none());
        }
        assert_eq!(session.saved_tables[1].results[0].id, a.id);
        assert!(session.recording(current).is_some());
    }

    #[test]
    fn archive_audio_obeys_the_same_budget_as_current_audio() {
        let mut session = loaded();
        let first = add(&mut session, Some(audio(4)));
        save(&mut session, "First");
        let second = add(&mut session, Some(audio(4)));
        save(&mut session, "Second");
        let current = add(&mut session, Some(audio(4)));
        session.prune_audio(8);
        assert!(session.recording(first).is_none());
        assert!(!session.saved_tables[1].results[0].can_replay);
        assert!(session.recording(second).is_some());
        assert!(session.recording(current).is_some());
        assert_eq!((session.audio_bytes, session.recording_count), (8, 2));
    }

    #[test]
    fn invalid_or_oversized_archives_fail_closed_without_invoking_a_write() {
        let mut session = loaded();
        add(&mut session, None);
        let bytes = save(&mut session, "Valid");
        assert!(Tables::from_bytes(b"private corrupt transcript").is_err());
        assert!(Tables::from_bytes(&vec![b' '; TABLE_BYTES_LIMIT + 1]).is_err());
        let mut invalid = Tables::from_bytes(&bytes).unwrap();
        invalid.version = 2;
        assert!(Tables::from_bytes(&serde_json::to_vec(&invalid).unwrap()).is_err());
        invalid.version = 1;
        invalid.tables.push(invalid.tables[0].clone());
        assert!(Tables::from_bytes(&serde_json::to_vec(&invalid).unwrap()).is_err());
        invalid.tables.truncate(1);
        invalid.tables[0].results = vec![invalid.tables[0].results[0].clone(); RESULT_LIMIT + 1];
        assert!(Tables::from_bytes(&serde_json::to_vec(&invalid).unwrap()).is_err());
        add(&mut session, None);
        session.results[0].text = "x".repeat(TABLE_BYTES_LIMIT);
        let transaction = session.prepare_tables(TableChange::Save { name: "Too big".into(), at: 0 }).unwrap();
        let outcome = transaction.persist(|_| panic!("oversized data must never reach the encrypted store"));
        assert!(session.finish_tables(outcome).is_err());
        assert_eq!(session.results.len(), 1);
        assert_eq!(session.saved_tables.len(), 1);
    }
}
