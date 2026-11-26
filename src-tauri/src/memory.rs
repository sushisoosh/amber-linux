//! What Amber remembers about dictations: encrypted history (DPAPI or Secret Service),
//! statistics (counts and timings only), and the last
//! recording whose recognition failed, for "Recognize again".

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use amber_core::history::{Entry, History};
use amber_core::settings::Settings;
use amber_core::stats::{self, Ledger, Stat};

/// How long a failed recording waits for "Recognize again".
const KEEP_FAILED: Duration = Duration::from_secs(10 * 60);

/// A recording whose recognition failed - only in memory, never on disk.
pub struct Failed {
    pub wav: Vec<u8>,
    pub spoken_ms: u64,
    pub program: Option<String>,
    /// A later local retry must not erase the original remote-attempt disclosure.
    pub remote_speech: bool,
    at: Instant,
}

impl Drop for Failed {
    fn drop(&mut self) {
        #[cfg(target_os = "linux")]
        {
            use zeroize::Zeroize;
            self.wav.zeroize();
        }
    }
}

#[derive(Default)]
struct FailedCache {
    recording: Option<Failed>,
    expiring: bool,
    changed: Arc<tokio::sync::Notify>,
}

impl Drop for FailedCache {
    fn drop(&mut self) {
        self.changed.notify_one();
    }
}

impl FailedCache {
    fn claim_expiry_worker(&mut self) -> bool {
        if self.expiring || self.recording.is_none() {
            return false;
        }
        self.expiring = true;
        true
    }

    fn next_deadline(&mut self, now: Instant) -> Option<Instant> {
        if let Some(failed) = &self.recording {
            let deadline = failed.at + KEEP_FAILED;
            if now < deadline {
                return Some(deadline);
            }
        }
        self.recording = None;
        self.expiring = false;
        None
    }
}

fn ensure_failed_expiry(slot: &Arc<Mutex<FailedCache>>) {
    let (start, changed) = {
        let mut cache = slot.lock().unwrap();
        (cache.claim_expiry_worker(), Arc::clone(&cache.changed))
    };
    // A returned recording can have an earlier deadline than the one currently awaited.
    changed.notify_one();
    if start {
        let _expiry = tauri::async_runtime::spawn(expire_failed(Arc::downgrade(slot)));
    }
}

async fn expire_failed(slot: Weak<Mutex<FailedCache>>) {
    use std::future::Future;
    use std::task::Poll;

    loop {
        // Hold neither the cache nor its buffer across the timer. A late wake checks
        // the current recording's deadline, not the recording that started the wait.
        let (deadline, changed) = {
            let Some(slot) = slot.upgrade() else { return };
            let mut current = slot.lock().unwrap();
            let Some(deadline) = current.next_deadline(Instant::now()) else { return };
            (deadline, Arc::clone(&current.changed))
        };
        let mut timer = std::pin::pin!(tokio::time::sleep_until(deadline.into()));
        let mut notification = std::pin::pin!(changed.notified());
        std::future::poll_fn(|cx| {
            if timer.as_mut().poll(cx).is_ready() || notification.as_mut().poll(cx).is_ready() {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        })
        .await;
    }
}

pub struct Memory {
    history_path: PathBuf,
    history: Mutex<StoredHistory>,
    /// The history switch as it is now, not as a dictation's settings snapshot had it.
    enabled: std::sync::atomic::AtomicBool,
    history_days: std::sync::atomic::AtomicU32,
    pub ledger: Ledger,
    failed: Arc<Mutex<FailedCache>>,
}

#[derive(Default)]
struct StoredHistory {
    history: History,
    error: Option<String>,
}

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

impl Memory {
    pub fn load(data_dir: &Path, settings: &Settings) -> Memory {
        let history_path = data_dir.join("history.bin");
        crate::secrets::register(&history_path);
        let memory = Memory {
            history_path,
            history: Mutex::new(StoredHistory::default()),
            enabled: std::sync::atomic::AtomicBool::new(settings.history),
            history_days: std::sync::atomic::AtomicU32::new(settings.history_days),
            ledger: Ledger::new(&data_dir.join("stats.jsonl")),
            failed: Arc::new(Mutex::new(FailedCache::default())),
        };
        if settings.history {
            if let Err(e) = memory.reload_history() {
                crate::diag::warn(format!("history unavailable: {e}"));
            }
        }
        memory
    }

    fn save(&self, stored: &mut StoredHistory) -> Result<(), String> {
        if let Some(error) = &stored.error {
            return Err(error.clone());
        }
        let result = crate::secrets::write_encrypted(&self.history_path, &stored.history.to_bytes(), crate::secrets::HISTORY);
        if let Err(error) = &result {
            // A locked/missing wallet must not quietly turn persistent history into a
            // transcript cache. The result/retry flow is independent from this history.
            stored.history.clear();
            stored.error = Some(error.clone());
            crate::diag::warn(format!("history not saved: {error}"));
        }
        result
    }

    pub fn history_error(&self) -> Option<String> {
        match self.history.try_lock() {
            Ok(stored) => stored.error.clone(),
            Err(_) => Some("History storage is busy; a wallet operation is in progress".into()),
        }
    }

    pub fn storage_status(&self) -> String {
        if let Some(error) = self.history_error() {
            return error;
        }
        if !self.enabled.load(std::sync::atomic::Ordering::SeqCst) {
            return "Persistent history is off".into();
        }
        "Encrypted history is enabled; a locked or unavailable wallet stops persistence".into()
    }

    /// Explicit retry after unlocking/restoring the wallet. Failed data stays in place.
    pub fn reload_history(&self) -> Result<(), String> {
        let mut stored = self.history.lock().unwrap();
        if !self.enabled.load(std::sync::atomic::Ordering::SeqCst) {
            stored.history.clear();
            return Ok(());
        }
        let result: Result<History, String> = (|| match crate::secrets::read_encrypted(&self.history_path, crate::secrets::HISTORY)? {
            Some(plain) => {
                #[cfg(target_os = "linux")]
                let plain = zeroize::Zeroizing::new(plain);
                let mut history = serde_json::from_slice::<History>(&plain).map_err(|_| {
                    let error = "History is corrupt (invalid data); existing storage was not changed".to_string();
                    crate::secrets::block(&self.history_path, &error);
                    error
                })?;
                let count = history.entries().len();
                history.prune(now_ms(), self.history_days.load(std::sync::atomic::Ordering::SeqCst));
                if history.entries().len() != count {
                    crate::secrets::write_encrypted(&self.history_path, &history.to_bytes(), crate::secrets::HISTORY)?;
                }
                Ok(history)
            }
            None => {
                crate::secrets::check_available()?;
                Ok(History::default())
            }
        })();
        match result {
            Ok(history) => {
                stored.history = history;
                stored.error = None;
                Ok(())
            }
            Err(error) => {
                stored.history.clear();
                stored.error = Some(error.clone());
                Err(error)
            }
        }
    }

    /// Keeps a finished dictation in the history and counts it in the statistics, as the
    /// settings allow.
    pub fn remember(&self, s: &Settings, text: &str, program: &str, engine: &str, inserted: bool, audio_ms: u64, wait_ms: u64, timings: Option<amber_core::history::Timings>) {
        let at = now_ms();
        if s.history {
            // Under the lock, so switching the history off (which clears under it too) wins
            // over a dictation that was still being recognized.
            let mut stored = self.history.lock().unwrap();
            if self.enabled.load(std::sync::atomic::Ordering::SeqCst) && stored.error.is_none() {
                stored.history.add(
                    Entry { at, text: text.to_string(), program: program.to_string(), engine: engine.to_string(), inserted, timings },
                    at,
                    s.history_days,
                );
                let _ = self.save(&mut stored);
            }
        }
        if s.statistics {
            self.ledger.record(&Stat {
                at,
                day: stats::day_of(at, utc_offset_minutes()),
                engine: engine.to_string(),
                words: amber_core::text::words(text) as u32,
                audio_ms,
                wait_ms,
                program: program.to_string(),
            });
        }
    }

    pub fn history(&self) -> Vec<Entry> {
        self.history.lock().unwrap().history.entries().to_vec()
    }

    pub fn latest(&self) -> Option<Entry> {
        self.history.lock().unwrap().history.latest().cloned()
    }

    pub fn forget(&self, at: u64) -> Result<(), String> {
        let mut stored = self.history.lock().unwrap();
        if let Some(error) = &stored.error {
            return Err(error.clone());
        }
        if !self.enabled.load(std::sync::atomic::Ordering::SeqCst) {
            return Ok(());
        }
        stored.history.remove(at);
        self.save(&mut stored)
    }

    /// Follows the history switch: off clears what is kept and refuses what is still coming.
    pub fn set_enabled(&self, on: bool, keep_days: u32) {
        let changed = {
            let mut stored = self.history.lock().unwrap();
            self.history_days.store(keep_days, std::sync::atomic::Ordering::SeqCst);
            let before = self.enabled.swap(on, std::sync::atomic::Ordering::SeqCst);
            if !on {
                stored.history.clear();
            }
            before != on
        };
        if on && changed {
            if let Err(e) = self.reload_history() {
                crate::diag::warn(format!("history unavailable: {e}"));
            }
        }
    }

    /// Explicit deletion works without a wallet key and also removes unreadable history.
    /// It is not a secure erase of backups, snapshots, or the underlying storage device.
    pub fn clear_history(&self) -> Result<(), String> {
        let mut stored = self.history.lock().unwrap();
        stored.history.clear();
        let result = crate::secrets::remove_encrypted(&self.history_path);
        stored.error = result.as_ref().err().cloned();
        result
    }

    /// After the settings changed how long dictations are kept.
    pub fn prune(&self, days: u32) -> Result<(), String> {
        let mut stored = self.history.lock().unwrap();
        self.history_days.store(days, std::sync::atomic::Ordering::SeqCst);
        if !self.enabled.load(std::sync::atomic::Ordering::SeqCst) {
            return Ok(());
        }
        if let Some(error) = &stored.error {
            return Err(error.clone());
        }
        stored.history.prune(now_ms(), days);
        self.save(&mut stored)
    }

    pub fn keep_failed(&self, wav: Vec<u8>, spoken_ms: u64, program: Option<String>, remote_speech: bool) {
        let at = Instant::now();
        self.failed.lock().unwrap().recording = Some(Failed { wav, spoken_ms, program, remote_speech, at });
        ensure_failed_expiry(&self.failed);
    }

    /// A taken recording back after another failed try - only into an empty slot, and with
    /// its own age, so it still goes after ten minutes.
    pub fn return_failed(&self, failed: Failed) {
        {
            let mut slot = self.failed.lock().unwrap();
            if slot.recording.is_none() && failed.at.elapsed() < KEEP_FAILED {
                slot.recording = Some(failed);
            }
        }
        ensure_failed_expiry(&self.failed);
    }

    /// The failed recording, if it is recent enough - taken, so it is recognized once.
    pub fn take_failed(&self) -> Option<Failed> {
        let mut slot = self.failed.lock().unwrap();
        let failed = slot.recording.take().filter(|f| f.at.elapsed() < KEEP_FAILED);
        slot.changed.notify_one();
        failed
    }
}

/// The local time zone's offset from UTC in minutes, daylight saving included - for the
/// statistics' calendar days.
#[cfg(windows)]
pub fn utc_offset_minutes() -> i32 {
    use windows::Win32::System::Time::{GetTimeZoneInformation, TIME_ZONE_INFORMATION};
    let mut tz = TIME_ZONE_INFORMATION::default();
    // 2 = TIME_ZONE_ID_DAYLIGHT. Bias is "UTC = local + bias".
    let id = unsafe { GetTimeZoneInformation(&mut tz) };
    let bias = tz.Bias + if id == 2 { tz.DaylightBias } else { tz.StandardBias };
    -bias
}

#[cfg(target_os = "linux")]
pub fn utc_offset_minutes() -> i32 {
    let now = unsafe { libc::time(std::ptr::null_mut()) };
    let mut local = std::mem::MaybeUninit::<libc::tm>::uninit();
    if unsafe { libc::localtime_r(&now, local.as_mut_ptr()) }.is_null() {
        return 0;
    }
    (unsafe { local.assume_init() }.tm_gmtoff / 60) as i32
}

#[cfg(not(any(windows, target_os = "linux")))]
pub fn utc_offset_minutes() -> i32 {
    0
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    fn temp_dir() -> PathBuf {
        let mut random = [0u8; 8];
        getrandom::getrandom(&mut random).unwrap();
        std::env::temp_dir().join(format!("amber-history-test-{}-{}", std::process::id(), u64::from_le_bytes(random)))
    }

    #[test]
    fn unreadable_history_is_preserved_and_not_replaced_by_new_transcripts() {
        let dir = temp_dir();
        let path = dir.join("history.bin");
        crate::secrets::write_private(&path, b"old unsupported history").unwrap();
        let settings = Settings { history: true, statistics: false, ..Settings::default() };
        let memory = Memory::load(&dir, &settings);
        assert!(memory.history_error().unwrap().contains("not imported"));
        assert!(memory.history().is_empty());
        memory.remember(&settings, "new private transcript", "editor", "local", false, 100, 100, None);
        assert!(memory.history().is_empty());
        assert!(memory.forget(1).is_err());
        assert!(memory.prune(1).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"old unsupported history");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "no backups or plaintext temporary files");
        memory.clear_history().unwrap();
        assert!(!path.exists());
        assert!(memory.history_error().is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn disabled_history_does_not_read_create_or_keep_transcripts() {
        let dir = temp_dir();
        let settings = Settings { history: false, statistics: false, ..Settings::default() };
        let memory = Memory::load(&dir, &settings);
        memory.remember(&settings, "not retained", "editor", "local", false, 100, 100, Some(amber_core::history::Timings {
            recording_ms: Some(100), transcription_ms: 50, cleanup_ms: 10, processing_ms: 100,
            live: false, cleanup_skipped: false,
        }));
        memory.prune(1).unwrap();
        memory.forget(1).unwrap();
        assert!(memory.history().is_empty());
        assert!(memory.history_error().is_none());
        assert!(!dir.exists());
    }

    #[test]
    fn clear_failure_is_reported_instead_of_claiming_deletion() {
        let dir = temp_dir();
        let path = dir.join("history.bin");
        std::fs::create_dir_all(&path).unwrap();
        let settings = Settings { history: false, ..Settings::default() };
        let memory = Memory::load(&dir, &settings);
        assert!(memory.clear_history().is_err());
        assert!(memory.history_error().unwrap().contains("Cannot delete"));
        assert!(path.exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn failed_recording_keeps_remote_exposure_and_original_expiry() {
        let settings = Settings { history: false, statistics: false, ..Settings::default() };
        let memory = Memory::load(&temp_dir(), &settings);
        memory.keep_failed(vec![1, 2, 3], 100, None, true);
        let failed = memory.take_failed().unwrap();
        assert!(failed.remote_speech);
        let first_at = failed.at;
        memory.return_failed(failed);
        let mut failed = memory.take_failed().unwrap();
        assert!(failed.remote_speech);
        assert_eq!(failed.at, first_at);
        failed.at = Instant::now() - KEEP_FAILED;
        memory.return_failed(failed);
        assert!(memory.take_failed().is_none());
    }

    #[test]
    fn expiry_purges_without_retry_and_releases_worker_claim() {
        let old = Instant::now() - KEEP_FAILED;
        let slot = Arc::new(Mutex::new(FailedCache {
            recording: Some(Failed { wav: vec![1, 2, 3], spoken_ms: 100, program: None, remote_speech: true, at: old }),
            expiring: true,
            changed: Arc::default(),
        }));
        tauri::async_runtime::block_on(expire_failed(Arc::downgrade(&slot)));
        let cache = slot.lock().unwrap();
        assert!(cache.recording.is_none(), "expiry does not depend on take_failed being called");
        assert!(!cache.expiring);
    }

    #[test]
    fn late_expiry_rechecks_new_generation_without_spawning_more_workers() {
        let old = Instant::now();
        let mut cache = FailedCache {
            recording: Some(Failed { wav: vec![1], spoken_ms: 100, program: None, remote_speech: false, at: old }),
            expiring: false,
            changed: Arc::default(),
        };
        assert!(cache.claim_expiry_worker());
        assert_eq!(cache.next_deadline(old), Some(old + KEEP_FAILED));
        let new = old + Duration::from_secs(30);
        for _ in 0..100 {
            cache.recording = Some(Failed { wav: vec![2], spoken_ms: 100, program: None, remote_speech: true, at: new });
            assert!(!cache.claim_expiry_worker(), "replacing a failure must not create another timer task");
        }
        assert_eq!(cache.next_deadline(old + KEEP_FAILED), Some(new + KEEP_FAILED));
        assert_eq!(cache.recording.as_ref().unwrap().at, new);
        assert!(cache.recording.as_ref().unwrap().remote_speech);
        assert_eq!(cache.next_deadline(new + KEEP_FAILED), None);
        assert!(cache.recording.is_none());
        assert!(!cache.expiring);
        assert!(!cache.claim_expiry_worker(), "empty cache needs no worker");
        cache.recording = Some(Failed { wav: vec![3], spoken_ms: 100, program: None, remote_speech: false, at: new });
        assert!(cache.claim_expiry_worker(), "a completed worker releases its claim for the next failure");
    }

    #[test]
    fn returned_recording_restarts_expiry_with_its_original_deadline() {
        let settings = Settings { history: false, statistics: false, ..Settings::default() };
        let memory = Memory::load(&temp_dir(), &settings);
        let original = Instant::now() - Duration::from_secs(30);
        memory.return_failed(Failed { wav: vec![1], spoken_ms: 100, program: None, remote_speech: true, at: original });
        let cache = memory.failed.lock().unwrap();
        assert!(cache.expiring, "return_failed must start a worker if the earlier worker has exited");
        assert_eq!(cache.recording.as_ref().unwrap().at + KEEP_FAILED, original + KEEP_FAILED);
    }

    #[test]
    fn expiry_does_not_keep_memory_or_its_recording_alive() {
        use std::future::Future;
        use std::task::{Context, Waker};

        let settings = Settings { history: false, statistics: false, ..Settings::default() };
        let memory = Memory::load(&temp_dir(), &settings);
        let weak = Arc::downgrade(&memory.failed);
        memory.failed.lock().unwrap().recording =
            Some(Failed { wav: vec![1, 2, 3], spoken_ms: 100, program: None, remote_speech: false, at: Instant::now() });
        tauri::async_runtime::block_on(async {
            let mut expiry = std::pin::pin!(expire_failed(weak.clone()));
            assert!(expiry.as_mut().poll(&mut Context::from_waker(Waker::noop())).is_pending());
            drop(memory);
            assert!(weak.upgrade().is_none());
            assert!(expiry.as_mut().poll(&mut Context::from_waker(Waker::noop())).is_ready());
        });
    }

    #[test]
    fn earlier_returned_deadline_wakes_the_existing_worker() {
        use std::future::Future;
        use std::task::{Context, Waker};

        let slot = Arc::new(Mutex::new(FailedCache {
            recording: Some(Failed { wav: vec![1], spoken_ms: 100, program: None, remote_speech: false, at: Instant::now() }),
            expiring: true,
            changed: Arc::default(),
        }));
        tauri::async_runtime::block_on(async {
            let mut expiry = std::pin::pin!(expire_failed(Arc::downgrade(&slot)));
            assert!(expiry.as_mut().poll(&mut Context::from_waker(Waker::noop())).is_pending());
            {
                let mut cache = slot.lock().unwrap();
                cache.recording.as_mut().unwrap().at = Instant::now() - KEEP_FAILED;
                cache.changed.notify_one();
            }
            assert!(expiry.as_mut().poll(&mut Context::from_waker(Waker::noop())).is_ready());
        });
        assert!(slot.lock().unwrap().recording.is_none());
        assert!(!slot.lock().unwrap().expiring);
    }
}
