//! The last dictations, kept on this PC so a dictation into the wrong window or one that could
//! not be inserted is not lost: text, program, time, engine. Kept for a number of days, at
//! most [MAX_ENTRIES]. The app stores it encrypted for the Windows account (DPAPI); this
//! module only knows the entries and the rules.

use serde::{Deserialize, Serialize};

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
        Entry { at, text: text.into(), program: "notepad.exe".into(), engine: "local".into(), inserted: true, timings: None }
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
}
