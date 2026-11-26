//! The last dictations, kept on this PC so a dictation into the wrong window or one that could
//! not be inserted is not lost: text, program, time, engine. Kept for a number of days, at
//! most [MAX_ENTRIES]. The app stores it encrypted for the Windows account (DPAPI); this
//! module only knows the entries and the rules.

use serde::{Deserialize, Serialize};
use crate::estimate::{Estimates, Route, Source, Summary};

pub const MAX_ENTRIES: usize = 500;
pub const DAY_MS: u64 = 24 * 60 * 60 * 1000;

/// Wall times for one completed conversion, stored only with its history entry.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Timings {
    /// Original microphone duration is unavailable when retrying prepared audio.
    pub recording_ms: Option<u64>,
    pub transcription_ms: u64,
    pub cleanup_ms: u64,
    /// Stop/retry to cleaned text, excluding insertion and recording.
    pub processing_ms: u64,
    pub live: bool,
    /// Smart cleanup was unavailable or failed; fallback text was retained.
    pub cleanup_skipped: bool,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum CleanupMethod { None, Rules, Snippet, Smart, Instruction }

impl CleanupMethod {
    fn label(self) -> &'static str {
        match self {
            Self::None => "No cleanup", Self::Rules => "Rules only", Self::Snippet => "Snippet",
            Self::Smart => "Smart cleanup", Self::Instruction => "Keyword instruction",
        }
    }

    pub fn uses_model(self) -> bool { matches!(self, Self::Smart | Self::Instruction) }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum CleanupOutcome { Success, Skipped, Failed }

/// What actually ran, not a reconstruction from the user's current settings.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Cleanup {
    pub method: CleanupMethod,
    pub outcome: CleanupOutcome,
    pub engine: Option<String>,
}

impl Default for Cleanup {
    fn default() -> Self {
        Self { method: CleanupMethod::None, outcome: CleanupOutcome::Skipped, engine: None }
    }
}

/// Successful execution context is retained only as long as its encrypted history row.
/// None/rules/snippets have no model cleanup route. This is stripped from history IPC.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Execution {
    pub speech: Route,
    pub cleanup: Option<Route>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    /// Milliseconds since 1970 (UTC); also the id.
    pub at: u64,
    pub text: String,
    /// "discord.exe", "phone", or empty when unknown.
    pub program: String,
    pub engine: String,
    /// Inserted, or left on the clipboard / only kept here.
    pub inserted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timings: Option<Timings>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cleanup: Option<Cleanup>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<Execution>,
}

#[derive(Serialize, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Comparisons {
    pub reference_audio_ms: u64,
    pub reference_chars: u64,
    pub rows: Vec<Comparison>,
}

#[derive(Serialize, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Comparison {
    pub id: String,
    pub speech: String,
    pub cleanup: String,
    pub dictations: u64,
    pub recognition_ms: u64,
    pub cleanup_ms: u64,
    pub total_ms: u64,
    pub recognition_samples: u64,
    pub cleanup_samples: u64,
    pub recognition_source: Source,
    pub cleanup_source: Source,
}

/// Only combinations actually recorded in the retained, optionally filtered history.
/// These are model-request budgets, not normalized wall times or a Cartesian product.
pub fn comparisons(entries: &[Entry], estimates: &Estimates, ats: Option<&[u64]>) -> Comparisons {
    let mut result = Comparisons { reference_audio_ms: 10_000, reference_chars: 100, rows: Vec::new() };
    let selected = ats.map(|ids| ids.iter().copied().collect::<std::collections::HashSet<_>>());
    let mut rows = std::collections::BTreeMap::<String, Comparison>::new();
    for entry in entries {
        if selected.as_ref().is_some_and(|ids| !ids.contains(&entry.at)) { continue; }
        let (Some(execution), Some(cleanup)) = (&entry.execution, &entry.cleanup) else { continue };
        if cleanup.outcome != CleanupOutcome::Success
            || cleanup.method.uses_model() != execution.cleanup.is_some() { continue; }
        let id = format!("{}:{:?}:{}", execution.speech.key(), cleanup.method, execution.cleanup.as_ref().map_or("-", Route::key));
        if let Some(row) = rows.get_mut(&id) { row.dictations += 1; continue; }
        let recognition = estimates.summary(&execution.speech, result.reference_audio_ms);
        let clean = execution.cleanup.as_ref().map(|r| estimates.summary(r, result.reference_chars))
            .unwrap_or(Summary { ms: 0, samples: 0, source: Source::NotNeeded });
        let cleanup_label = match &execution.cleanup {
            Some(route) => format!("{}: {}", cleanup.method.label(), route.label()),
            None => cleanup.method.label().to_string(),
        };
        rows.insert(id.clone(), Comparison {
            id, speech: execution.speech.label(), cleanup: cleanup_label, dictations: 1,
            recognition_ms: recognition.ms, cleanup_ms: clean.ms, total_ms: recognition.ms.saturating_add(clean.ms),
            recognition_samples: recognition.samples, cleanup_samples: clean.samples,
            recognition_source: recognition.source, cleanup_source: clean.source,
        });
    }
    result.rows = rows.into_values().collect();
    result
}

#[derive(Serialize, Deserialize, Default, Debug, PartialEq)]
pub struct History {
    entries: Vec<Entry>,
}

impl History {
    pub fn from_bytes(bytes: &[u8]) -> History {
        serde_json::from_slice(bytes).unwrap_or_default()
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("history serializes")
    }

    /// Newest first.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn latest(&self) -> Option<&Entry> {
        self.entries.first()
    }

    pub fn add(&mut self, entry: Entry, now: u64, keep_days: u32) {
        if entry.text.trim().is_empty() {
            return;
        }
        self.entries.insert(0, entry);
        self.prune(now, keep_days);
    }

    /// Drops what is older than [keep_days] and anything beyond [MAX_ENTRIES].
    pub fn prune(&mut self, now: u64, keep_days: u32) {
        let oldest = now.saturating_sub(keep_days as u64 * DAY_MS);
        self.entries.retain(|e| e.at >= oldest);
        self.entries.truncate(MAX_ENTRIES);
    }

    pub fn remove(&mut self, at: u64) {
        self.entries.retain(|e| e.at != at);
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(at: u64, text: &str) -> Entry {
        Entry { at, text: text.into(), program: "notepad.exe".into(), engine: "local".into(), inserted: true, timings: None, cleanup: None, execution: None }
    }

    #[test]
    fn newest_first_and_kept_for_the_days_asked() {
        let mut h = History::default();
        let now = 100 * DAY_MS;
        h.add(entry(now - 8 * DAY_MS, "old"), now - 8 * DAY_MS, 7);
        h.add(entry(now - DAY_MS, "yesterday"), now - DAY_MS, 7);
        h.add(entry(now, "now"), now, 7);
        let texts: Vec<&str> = h.entries().iter().map(|e| e.text.as_str()).collect();
        assert_eq!(texts, ["now", "yesterday"], "the week-old one is gone");
        assert_eq!(h.latest().unwrap().text, "now");
        h.add(entry(now + 1, "   "), now + 1, 7);
        assert_eq!(h.entries().len(), 2, "nothing said, nothing kept");
    }

    #[test]
    fn bounded_and_editable() {
        let mut h = History::default();
        for i in 0..(MAX_ENTRIES as u64 + 20) {
            h.add(entry(1_000 + i, "x"), 1_000 + i, 7);
        }
        assert_eq!(h.entries().len(), MAX_ENTRIES);
        h.remove(1_000 + MAX_ENTRIES as u64 + 19);
        assert_eq!(h.entries().len(), MAX_ENTRIES - 1);
        h.clear();
        assert!(h.entries().is_empty());
    }

    #[test]
    fn survives_a_round_trip_and_garbage() {
        let mut h = History::default();
        h.add(entry(5, "Hallo"), 5, 7);
        assert_eq!(History::from_bytes(&h.to_bytes()), h);
        assert_eq!(History::from_bytes(b"not json"), History::default());
    }

    #[test]
    fn legacy_entries_have_no_invented_timings() {
        let h = History::from_bytes(br#"{"entries":[{"at":5,"text":"old","program":"editor","engine":"local","inserted":true}]}"#);
        assert_eq!(h.entries().len(), 1);
        assert_eq!(h.latest().unwrap().timings, None);
        assert_eq!(h.latest().unwrap().cleanup, None);
        assert_eq!(h.latest().unwrap().execution, None);
    }

    #[test]
    fn timings_round_trip_and_follow_history_retention() {
        let mut h = History::default();
        let mut e = entry(DAY_MS, "measured");
        e.timings = Some(Timings {
            recording_ms: None, transcription_ms: 123, cleanup_ms: 0, processing_ms: 130,
            live: false, cleanup_skipped: true,
        });
        h.add(e, DAY_MS, 7);
        assert_eq!(History::from_bytes(&h.to_bytes()), h);
        let json: serde_json::Value = serde_json::from_slice(&h.to_bytes()).unwrap();
        assert!(json["entries"][0]["timings"]["recordingMs"].is_null());
        assert_eq!(json["entries"][0]["timings"]["cleanupMs"], 0);
        h.prune(10 * DAY_MS, 7);
        assert!(h.entries().is_empty());
    }

    fn recorded(at: u64, speech: Route, cleanup: Option<Route>, method: CleanupMethod) -> Entry {
        let mut entry = entry(at, "retained text");
        entry.cleanup = Some(Cleanup { method, outcome: CleanupOutcome::Success, engine: cleanup.as_ref().map(Route::label) });
        entry.execution = Some(Execution { speech, cleanup });
        entry
    }

    #[test]
    fn cleanup_and_execution_roundtrip_without_inventing_legacy_metadata() {
        let settings = crate::settings::Settings::default();
        let speech = crate::estimate::local_speech(&settings, "cpu", "speech", 4);
        let route = crate::estimate::cleanup(&settings, None, None, Some(("cpu", "chat", 4))).unwrap();
        for method in [CleanupMethod::None, CleanupMethod::Rules, CleanupMethod::Snippet, CleanupMethod::Smart, CleanupMethod::Instruction] {
            for outcome in [CleanupOutcome::Success, CleanupOutcome::Skipped, CleanupOutcome::Failed] {
                let mut entry = recorded(1, speech.clone(), method.uses_model().then(|| route.clone()), method);
                entry.cleanup.as_mut().unwrap().outcome = outcome;
                let mut history = History::default();
                history.add(entry, 1, 7);
                assert_eq!(History::from_bytes(&history.to_bytes()), history);
                let json: serde_json::Value = serde_json::from_slice(&history.to_bytes()).unwrap();
                assert_eq!(json["entries"][0]["cleanup"]["outcome"], match outcome {
                    CleanupOutcome::Success => "success", CleanupOutcome::Skipped => "skipped", CleanupOutcome::Failed => "failed",
                });
                assert_eq!(json["entries"][0]["cleanup"]["engine"].is_string(), method.uses_model());
            }
        }
    }

    #[test]
    fn comparisons_normalize_real_pairs_with_exact_estimator_counts_and_retained_filtering() {
        let settings = crate::settings::Settings::default();
        let a = crate::estimate::local_speech(&settings, "cpu", "speech-a", 4);
        let b = crate::estimate::local_speech(&settings, "cpu", "speech-b", 8);
        let x = crate::estimate::cleanup(&settings, None, None, Some(("cpu", "chat-x", 4))).unwrap();
        let y = crate::estimate::cleanup(&settings, None, None, Some(("cpu", "chat-y", 4))).unwrap();
        let estimates = Estimates::in_memory();
        for size in [1_000, 2_000, 3_000, 4_000] { estimates.observe(&a, size, 500 + size / 2, true); }
        estimates.observe(&x, 50, 250, true);
        let mut history = History::default();
        history.add(recorded(1, a.clone(), Some(x.clone()), CleanupMethod::Smart), 1, 7);
        let mut live = recorded(2, a.clone(), Some(x.clone()), CleanupMethod::Smart);
        live.timings = Some(Timings { recording_ms: Some(90_000), transcription_ms: 999, cleanup_ms: 888, processing_ms: 4_000, live: true, cleanup_skipped: false });
        history.add(live, 2, 7);
        history.add(recorded(3, b.clone(), Some(y.clone()), CleanupMethod::Smart), 3, 7);
        let comparison = comparisons(history.entries(), &estimates, None);
        assert_eq!((comparison.reference_audio_ms, comparison.reference_chars), (10_000, 100));
        assert_eq!(comparison.rows.len(), 2, "never synthesize A/Y or B/X");
        let ax = comparison.rows.iter().find(|row| row.speech == a.label()).unwrap();
        assert_eq!(ax.dictations, 2, "live and ordinary history can share an actual complete route");
        assert_eq!(ax.recognition_ms, estimates.expected_ms(&a, 10_000));
        assert_eq!(ax.recognition_ms, 5_500, "normalize model work, never divide stored wall times");
        assert_eq!(ax.cleanup_ms, estimates.expected_ms(&x, 100));
        assert_eq!(ax.total_ms, ax.recognition_ms + ax.cleanup_ms);
        assert_eq!((ax.recognition_samples, ax.cleanup_samples), (4, 1));
        assert_eq!((ax.recognition_source, ax.cleanup_source), (Source::Learned, Source::Learned));
        let by = comparison.rows.iter().find(|row| row.speech == b.label()).unwrap();
        assert_eq!((by.recognition_samples, by.cleanup_samples), (0, 0));
        assert_eq!((by.recognition_source, by.cleanup_source), (Source::Default, Source::Default));
        assert!(comparisons(history.entries(), &estimates, Some(&[])).rows.is_empty());
        let selected = comparisons(history.entries(), &estimates, Some(&[1, 1, 500]));
        assert_eq!(selected.rows.len(), 1);
        assert_eq!(selected.rows[0].dictations, 1, "duplicate/stale UI IDs cannot manufacture samples or rows");
        history.remove(1);
        assert!(comparisons(history.entries(), &estimates, Some(&[1])).rows.is_empty());
        history.clear();
        assert!(comparisons(history.entries(), &estimates, None).rows.is_empty());
        assert_eq!(estimates.summary(&a, 10_000).samples, 4, "history deletion must not erase aggregate learning");
    }

    #[test]
    fn comparisons_exclude_unknown_failed_skipped_and_incomplete_routes() {
        let settings = crate::settings::Settings::default();
        let speech = crate::estimate::local_speech(&settings, "cpu", "speech", 4);
        let route = crate::estimate::cleanup(&settings, None, None, Some(("cpu", "chat", 4))).unwrap();
        let mut entries = vec![entry(0, "old, phone or terminal")];
        for outcome in [CleanupOutcome::Skipped, CleanupOutcome::Failed] {
            let mut entry = recorded(1, speech.clone(), Some(route.clone()), CleanupMethod::Smart);
            entry.cleanup.as_mut().unwrap().outcome = outcome;
            entries.push(entry);
        }
        entries.push(recorded(2, speech.clone(), None, CleanupMethod::Smart));
        entries.push(recorded(3, speech.clone(), Some(route), CleanupMethod::Rules));
        let mut no_speech_route = recorded(4, speech.clone(), None, CleanupMethod::Rules);
        no_speech_route.execution = None;
        entries.push(no_speech_route);
        let estimates = Estimates::in_memory();
        assert!(comparisons(&entries, &estimates, None).rows.is_empty());
        for method in [CleanupMethod::None, CleanupMethod::Rules, CleanupMethod::Snippet] {
            entries.push(recorded(5, speech.clone(), None, method));
        }
        let comparison = comparisons(&entries, &estimates, None);
        assert_eq!(comparison.rows.len(), 3, "distinct real non-model cleanup methods");
        for row in comparison.rows {
            assert_eq!((row.cleanup_ms, row.cleanup_samples, row.cleanup_source), (0, 0, Source::NotNeeded));
            assert_eq!(row.total_ms, row.recognition_ms);
        }
    }

    #[test]
    fn comparisons_keep_legacy_priors_distinct_from_defaults_without_writing_learning() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("estimates.json");
        let prior = br#"{"local:cpu:speech":{"per_ms":0.25,"samples":9}}"#;
        std::fs::write(&path, prior).unwrap();
        let estimates = Estimates::load(&path);
        let speech = crate::estimate::local_speech(&crate::settings::Settings::default(), "cpu", "speech", 4);
        let comparison = comparisons(&[recorded(1, speech.clone(), None, CleanupMethod::Rules)], &estimates, None);
        let row = &comparison.rows[0];
        assert_eq!((row.recognition_ms, row.recognition_samples, row.recognition_source), (2_700, 0, Source::Legacy));
        assert_eq!(row.recognition_ms, estimates.expected_ms(&speech, 10_000));
        assert_eq!(Some(row.recognition_ms), estimates.measured_ms(&speech, 10_000));
        let json = serde_json::to_value(comparison).unwrap();
        assert_eq!(json["referenceAudioMs"], 10_000);
        assert_eq!(json["referenceChars"], 100);
        assert_eq!(json["rows"][0]["recognitionSource"], "legacy");
        assert_eq!(json["rows"][0]["cleanupSource"], "notNeeded");
        assert_eq!(std::fs::read(path).unwrap(), prior, "querying never rewrites or creates learning samples");
    }
}
