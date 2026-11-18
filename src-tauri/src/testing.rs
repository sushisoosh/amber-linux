//! Isolated test recordings and explicitly saved, encrypted comparison tables.
//! This module has no text delivery or ordinary history path.

use std::path::PathBuf;
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use amber_core::engine::CancellationToken;
use amber_core::settings::{EngineKind, Settings};
use amber_core::testing::{Phase, Recording, Result as TestResult, Session, TableChange, TableTransaction, Tables};
use tauri::{AppHandle, Emitter, Manager};

use crate::capture::{self, Recorder};
use crate::state::Core;

#[derive(Default)]
struct Inner {
    session: Session,
    cancel: CancellationToken,
    stop: Option<mpsc::Sender<()>>,
}

pub struct Testing(Mutex<Inner>, PathBuf);

impl Testing {
    pub fn new(path: PathBuf) -> Self {
        crate::secrets::register(&path);
        Self(Mutex::new(Inner::default()), path)
    }

    pub fn snapshot(&self) -> Session { self.0.lock().unwrap().session.snapshot() }

    fn prepare(&self, change: TableChange) -> Result<(TableTransaction, Session), String> {
        let mut inner = self.0.lock().unwrap();
        let transaction = inner.session.prepare_tables(change)?;
        Ok((transaction, inner.session.snapshot()))
    }

    fn finish(&self, outcome: Result<TableTransaction, String>) -> (Result<(), String>, Session) {
        let mut inner = self.0.lock().unwrap();
        let result = inner.session.finish_tables(outcome);
        (result, inner.session.snapshot())
    }
}

fn emit(app: &AppHandle, session: Session) {
    let _ = app.emit_to(crate::bubble::SETTINGS, "testing-state", session);
}

trait TableStore {
    fn read(&self) -> Result<Option<Vec<u8>>, String>;
    fn write(&self, plain: &[u8]) -> Result<(), String>;
}

struct EncryptedTables(PathBuf);

impl TableStore for EncryptedTables {
    fn read(&self) -> Result<Option<Vec<u8>>, String> {
        crate::secrets::read_encrypted_bounded(&self.0, crate::secrets::TESTING_TABLES)
    }
    fn write(&self, plain: &[u8]) -> Result<(), String> {
        crate::secrets::write_encrypted(&self.0, plain, crate::secrets::TESTING_TABLES)
    }
}

fn store_transaction(transaction: TableTransaction, reload: bool, store: &impl TableStore) -> Result<TableTransaction, String> {
    if reload {
        let tables = match store.read()? {
            Some(plain) => {
                #[cfg(target_os = "linux")]
                let plain = zeroize::Zeroizing::new(plain);
                Tables::from_bytes(&plain)?
            }
            None => Tables::default(),
        };
        transaction.restore(tables)
    } else {
        transaction.persist(|plain| store.write(plain))
    }
}

pub async fn change_tables(app: AppHandle, change: TableChange) -> Result<(), String> {
    let reload = matches!(change, TableChange::Reload);
    let (transaction, snapshot) = app.state::<Core>().testing.prepare(change)?;
    emit(&app, snapshot);
    store_tables(app, transaction, reload).await
}

pub fn load_at_start(app: &AppHandle) -> Result<(), String> {
    // Reserve the initial load before any window can start a test, not when the task runs.
    let (transaction, snapshot) = app.state::<Core>().testing.prepare(TableChange::Reload)?;
    emit(app, snapshot);
    let app = app.clone();
    tauri::async_runtime::spawn(async move { let _ = store_tables(app, transaction, true).await; });
    Ok(())
}

async fn store_tables(app: AppHandle, transaction: TableTransaction, reload: bool) -> Result<(), String> {
    let core = app.state::<Core>();
    let store = EncryptedTables(core.testing.1.clone());
    let outcome = tauri::async_runtime::spawn_blocking(move || store_transaction(transaction, reload, &store))
        .await.map_err(|_| "Saved comparison storage worker failed; reload before retrying".to_string()).and_then(|r| r);
    let (result, snapshot) = core.testing.finish(outcome);
    emit(&app, snapshot);
    result
}

pub fn start(app: &AppHandle) -> Result<(), String> {
    begin(app, None)
}

pub fn replay(app: &AppHandle, source_test_id: u64) -> Result<(), String> {
    begin(app, Some(source_test_id))
}

fn begin(app: &AppHandle, source_test_id: Option<u64>) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    if crate::linux_desktop::session_is_blocked() { return Err("Unlock the desktop before recording a test.".into()); }
    let core = app.state::<Core>();
    let mut inner = core.testing.0.lock().unwrap();
    inner.session.ensure_idle()?;
    let source = source_test_id.map(|id| inner.session.recording(id)
        .ok_or_else(|| "This recording is no longer retained. Record a new comparison.".to_string())).transpose()?;
    let settings = core.settings();
    if settings.engine == EngineKind::Server && settings.server.model.trim().is_empty() {
        return Err("Choose a specific recognition model in Engine before comparing test recordings.".into());
    }
    let id = inner.session.allocate_test_id()?;
    crate::dictation::reserve_test(app)?;
    #[cfg(target_os = "linux")]
    if crate::linux_desktop::session_is_blocked() {
        core.dictation.release_test();
        return Err("Unlock the desktop before recording a test.".into());
    }
    let mut result = TestResult::new(id, crate::memory::now_ms(), &settings);
    result.source_test_id = source_test_id;
    result.recording_id = source.as_ref().map(|(id, _)| *id);
    inner.cancel = CancellationToken::default();
    let cancel = inner.cancel.clone();
    let (stop, receiver) = mpsc::channel();
    inner.stop = source.is_none().then_some(stop);
    if let Err(error) = inner.session.begin(result.clone()) {
        core.dictation.release_test();
        return Err(error);
    }
    if source.is_some() { inner.session.update(result.id, Phase::Recognizing); }
    let snapshot = inner.session.snapshot();
    drop(inner);
    emit(app, snapshot);

    let worker_app = app.clone();
    let spawned = std::thread::Builder::new().name("amber-testing".into()).spawn(move || {
        let app = worker_app;
        let _quiet = crate::diag::silence();
        let core = app.state::<Core>();
        let mut result = result;
        let mut audio = source.map(|(_, recording)| recording);
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if audio.is_none() {
                // Close an existing meter outside its mutex; replay never opens a microphone.
                let meter = core.mic_test.lock().unwrap().take();
                drop(meter);
                audio = Some(record(&app, &settings, &mut result, receiver, &cancel)?);
            }
            recognize(&app, &settings, &mut result, audio.as_ref().unwrap(), &cancel)
        })).unwrap_or_else(|_| Err("The test worker failed unexpectedly.".into()));
        let mut inner = core.testing.0.lock().unwrap();
        if !cancel.is_cancelled() {
            result.error = outcome.err();
            inner.session.complete(result, audio);
        }
        inner.stop = None;
        inner.session.settled();
        // Keep the reservation until capture and native inference have actually returned.
        core.dictation.release_test();
        let snapshot = inner.session.snapshot();
        drop(inner);
        emit(&app, snapshot);
    });
    if let Err(e) = spawned {
        let mut inner = core.testing.0.lock().unwrap();
        inner.session.cancel();
        inner.session.settled();
        inner.stop = None;
        core.dictation.release_test();
        let snapshot = inner.session.snapshot();
        drop(inner);
        emit(app, snapshot);
        return Err(format!("Could not start the test worker: {e}"));
    }
    Ok(())
}

fn record(app: &AppHandle, settings: &Settings, result: &mut TestResult, stop: mpsc::Receiver<()>, cancel: &CancellationToken) -> Result<Arc<Recording>, String> {
    cancel.check().map_err(|e| e.message)?;
    let recorder = Recorder::start(&settings.microphone, |_| {})?;
    result.microphone = Some(recorder.device.clone());
    result.microphone_fallback = recorder.fell_back;
    // Stop/cancel during microphone opening is queued. A forgotten test has the same hard cap.
    let _ = stop.recv_timeout(Duration::from_secs(capture::MAX_SECONDS as u64));
    cancel.check().map_err(|e| e.message)?;
    let recording = recorder.finish();
    result.interrupted = recording.interrupted;
    result.audio_ms = recording.samples.len() as u64 * 1000 / recording.rate.max(1) as u64;
    if !advance(app, result.id, Phase::Recognizing) { return Err("Test cancelled.".into()); }
    if recording.interrupted && recording.samples.is_empty() {
        return Err("The microphone stopped delivering sound.".into());
    }
    let prepared = amber_core::audio::prepare(&recording.samples, recording.rate)
        .map_err(|_| "Nothing heard - is the right microphone selected?".to_string())?;
    Ok(Arc::new(Recording { wav: prepared.wav, audio_ms: result.audio_ms, prepared_audio_ms: prepared.spoken_ms,
        microphone: result.microphone.clone(), microphone_fallback: result.microphone_fallback, interrupted: result.interrupted }))
}

fn recognize(app: &AppHandle, settings: &Settings, result: &mut TestResult, audio: &Recording, cancel: &CancellationToken) -> Result<(), String> {
    cancel.check().map_err(|e| e.message)?;
    result.microphone = audio.microphone.clone();
    result.microphone_fallback = audio.microphone_fallback;
    result.interrupted = audio.interrupted;
    result.audio_ms = audio.audio_ms;
    result.prepared_audio_ms = audio.prepared_audio_ms;
    if !advance(app, result.id, Phase::Recognizing) { return Err("Test cancelled.".into()); }
    result.raw = crate::dictation::recognize_for_test(app, settings, &audio.wav, result, cancel)?;
    cancel.check().map_err(|e| e.message)?;
    if !advance(app, result.id, Phase::Cleaning) { return Err("Test cancelled.".into()); }
    crate::llm::tidy_for_test(app, settings, result, cancel);
    cancel.check().map_err(|e| e.message)?;
    if result.text.is_empty() { return Err("Nothing recognized.".into()); }
    Ok(())
}

fn advance(app: &AppHandle, id: u64, phase: Phase) -> bool {
    let core = app.state::<Core>();
    let mut inner = core.testing.0.lock().unwrap();
    let updated = inner.session.update(id, phase);
    let snapshot = updated.then(|| inner.session.snapshot());
    drop(inner);
    if let Some(snapshot) = snapshot { emit(app, snapshot); }
    updated
}

pub fn stop(app: &AppHandle, id: u64) -> Result<(), String> {
    let core = app.state::<Core>();
    let mut inner = core.testing.0.lock().unwrap();
    if inner.session.phase != Phase::Recording || inner.session.active.as_ref().is_none_or(|r| r.id != id) {
        return Err("This test is no longer recording.".into());
    }
    if let Some(stop) = inner.stop.take() { let _ = stop.send(()); }
    inner.session.update(id, Phase::Recognizing);
    let snapshot = inner.session.snapshot();
    drop(inner);
    emit(app, snapshot);
    Ok(())
}

pub fn cancel(app: &AppHandle) {
    cancel_session(app, None);
}

pub fn cancel_session(app: &AppHandle, id: Option<u64>) {
    let core = app.state::<Core>();
    let mut inner = core.testing.0.lock().unwrap();
    if id.is_some_and(|id| inner.session.active.as_ref().is_none_or(|r| r.id != id)) { return; }
    inner.cancel.cancel();
    inner.session.cancel();
    if let Some(stop) = inner.stop.take() { let _ = stop.send(()); }
    let snapshot = inner.session.snapshot();
    drop(inner);
    emit(app, snapshot);
}

pub fn clear(app: &AppHandle, id: Option<u64>) -> Result<(), String> {
    let core = app.state::<Core>();
    let mut inner = core.testing.0.lock().unwrap();
    inner.session.clear(id)?;
    let snapshot = inner.session.snapshot();
    drop(inner);
    emit(app, snapshot);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    // This boundary deliberately never constructs Core or accesses Secret Service/DPAPI.
    #[derive(Default)]
    struct Store {
        data: Mutex<Option<Vec<u8>>>,
        fail_read: AtomicBool,
        fail_write: AtomicBool,
        fail_after_write: AtomicBool,
        writes: AtomicUsize,
    }

    impl TableStore for Store {
        fn read(&self) -> Result<Option<Vec<u8>>, String> {
            if self.fail_read.load(Ordering::SeqCst) { return Err("Mock wallet locked".into()); }
            Ok(self.data.lock().unwrap().clone())
        }
        fn write(&self, plain: &[u8]) -> Result<(), String> {
            if self.fail_write.load(Ordering::SeqCst) { return Err("Mock encrypted write failed".into()); }
            *self.data.lock().unwrap() = Some(plain.to_vec());
            self.writes.fetch_add(1, Ordering::SeqCst);
            if self.fail_after_write.load(Ordering::SeqCst) { return Err("Mock write durability could not be confirmed".into()); }
            Ok(())
        }
    }

    fn testing() -> Testing { Testing(Mutex::new(Inner::default()), PathBuf::new()) }

    fn change(testing: &Testing, store: &impl TableStore, change: TableChange) -> Result<(), String> {
        let reload = matches!(change, TableChange::Reload);
        let (transaction, _) = testing.prepare(change)?;
        testing.finish(store_transaction(transaction, reload, store)).0
    }

    fn add(testing: &Testing) -> u64 {
        let mut inner = testing.0.lock().unwrap();
        let id = inner.session.allocate_test_id().unwrap();
        let mut result = TestResult::new(id, 100, &Settings::default());
        result.raw = "Mock uncleaned transcript".into();
        result.text = "Mock cleaned transcript.".into();
        inner.session.begin(result.clone()).unwrap();
        inner.session.complete(result, Some(Arc::new(Recording { wav: vec![7; 16], audio_ms: 1,
            prepared_audio_ms: 1, microphone: None, microphone_fallback: false, interrupted: false })));
        id
    }

    fn save(name: &str) -> TableChange { TableChange::Save { name: name.into(), at: 200 } }

    #[test]
    fn explicit_save_is_the_only_recording_persistence_and_restart_has_no_audio() {
        let store = Store::default();
        let live = testing();
        change(&live, &store, TableChange::Reload).unwrap();
        add(&live);
        assert!(store.data.lock().unwrap().is_none());
        assert_eq!(store.writes.load(Ordering::SeqCst), 0);
        change(&live, &store, save("Comparison")).unwrap();
        assert_eq!(store.writes.load(Ordering::SeqCst), 1);
        assert!(live.snapshot().results.is_empty());
        assert!(live.snapshot().saved_tables[0].results[0].can_replay);
        let restarted = testing();
        change(&restarted, &store, TableChange::Reload).unwrap();
        let snapshot = restarted.snapshot();
        assert_eq!(snapshot.saved_tables[0].results[0].text, "Mock cleaned transcript.");
        assert!(!snapshot.saved_tables[0].results[0].can_replay);
        assert_eq!((snapshot.audio_bytes, snapshot.recording_count), (0, 0));
        assert_eq!(store.writes.load(Ordering::SeqCst), 1, "loading must not rewrite an archive");
    }

    #[test]
    fn unreadable_archives_block_writes_and_late_retry_does_not_rebind_audio() {
        let store = Store::default();
        let previous = testing();
        change(&previous, &store, TableChange::Reload).unwrap();
        add(&previous);
        change(&previous, &store, save("Old table")).unwrap();
        let original = store.data.lock().unwrap().clone();
        store.fail_read.store(true, Ordering::SeqCst);
        let live = testing();
        assert!(change(&live, &store, TableChange::Reload).is_err());
        let current = add(&live);
        assert_eq!(current, 1);
        assert!(change(&live, &store, save("Must not overwrite")).is_err());
        assert!(change(&live, &store, TableChange::Delete(1)).is_err());
        assert_eq!(*store.data.lock().unwrap(), original);
        assert_eq!(store.writes.load(Ordering::SeqCst), 1);
        assert!(live.snapshot().table_storage_error.is_some());
        store.fail_read.store(false, Ordering::SeqCst);
        change(&live, &store, TableChange::Reload).unwrap();
        let snapshot = live.snapshot();
        assert!(snapshot.table_storage_error.is_none());
        assert_eq!(snapshot.results[0].id, current);
        assert_ne!(snapshot.saved_tables[0].results[0].id, current);
        assert!(!snapshot.saved_tables[0].results[0].can_replay);
        assert!(snapshot.results[0].can_replay);
    }

    #[test]
    fn save_delete_and_parse_failures_preserve_existing_state_and_file() {
        let store = Store::default();
        let live = testing();
        change(&live, &store, TableChange::Reload).unwrap();
        add(&live);
        change(&live, &store, save("Saved")).unwrap();
        let original = store.data.lock().unwrap().clone();
        let id = add(&live);
        store.fail_write.store(true, Ordering::SeqCst);
        assert!(change(&live, &store, save("Failed save")).is_err());
        assert_eq!(live.snapshot().results[0].id, id);
        assert_eq!(live.snapshot().saved_tables.len(), 1);
        assert_eq!(*store.data.lock().unwrap(), original);
        assert!(!live.snapshot().table_busy);
        change(&live, &store, TableChange::Reload).unwrap();
        assert!(change(&live, &store, TableChange::Delete(1)).is_err());
        assert_eq!(live.snapshot().saved_tables.len(), 1);
        assert_eq!(*store.data.lock().unwrap(), original);
        *store.data.lock().unwrap() = Some(b"not a valid archive: private mock transcript".to_vec());
        let error = change(&live, &store, TableChange::Reload).unwrap_err();
        assert!(!error.contains("private mock transcript"));
        store.fail_write.store(false, Ordering::SeqCst);
        assert!(change(&live, &store, save("Blocked")).is_err());
        assert!(change(&live, &store, TableChange::Delete(1)).is_err());
        assert_eq!(store.writes.load(Ordering::SeqCst), 1);
        assert_eq!(live.snapshot().saved_tables.len(), 1);
        *store.data.lock().unwrap() = original;
        change(&live, &store, TableChange::Reload).unwrap();
        change(&live, &store, TableChange::Delete(1)).unwrap();
        assert!(live.snapshot().saved_tables.is_empty());
        let restarted = testing();
        change(&restarted, &store, TableChange::Reload).unwrap();
        assert!(restarted.snapshot().saved_tables.is_empty());
        add(&restarted);
        change(&restarted, &store, save("Next")).unwrap();
        assert_eq!(restarted.snapshot().saved_tables[0].id, 2);
    }

    #[test]
    fn slow_storage_never_holds_the_testing_mutex_and_busy_blocks_competing_changes() {
        struct SlowStore {
            store: Store,
            started: mpsc::Sender<()>,
            resume: Mutex<mpsc::Receiver<()>>,
        }
        impl TableStore for SlowStore {
            fn read(&self) -> Result<Option<Vec<u8>>, String> { self.store.read() }
            fn write(&self, plain: &[u8]) -> Result<(), String> {
                self.started.send(()).unwrap();
                self.resume.lock().unwrap().recv_timeout(Duration::from_secs(5)).unwrap();
                self.store.write(plain)
            }
        }
        let (started, worker_started) = mpsc::channel();
        let (resume, worker_resume) = mpsc::channel();
        let store = SlowStore { store: Store::default(), started, resume: Mutex::new(worker_resume) };
        let live = testing();
        change(&live, &store, TableChange::Reload).unwrap();
        add(&live);
        let (transaction, before) = live.prepare(save("Slow save")).unwrap();
        assert!(before.table_busy);
        std::thread::scope(|scope| {
            let worker = scope.spawn(|| live.finish(store_transaction(transaction, false, &store)));
            worker_started.recv_timeout(Duration::from_secs(5)).unwrap();
            {
                let mut inner = live.0.try_lock().expect("wallet I/O must not hold the Testing mutex");
                assert!(inner.session.clear(None).is_err());
                assert!(inner.session.begin(TestResult::new(99, 0, &Settings::default())).is_err());
            }
            assert!(live.snapshot().table_busy);
            assert_eq!(live.snapshot().results.len(), 1);
            assert!(live.prepare(TableChange::Reload).is_err());
            assert!(live.prepare(TableChange::Delete(1)).is_err());
            resume.send(()).unwrap();
            let (result, after) = worker.join().unwrap();
            result.unwrap();
            assert!(!after.table_busy);
            assert!(after.results.is_empty());
            assert_eq!(after.saved_tables.len(), 1);
            assert!(after.revision > before.revision);
        });
    }

    #[test]
    fn unconfirmed_write_preserves_current_and_requires_reload_before_any_further_write() {
        let store = Store::default();
        let live = testing();
        change(&live, &store, TableChange::Reload).unwrap();
        let current = add(&live);
        store.fail_after_write.store(true, Ordering::SeqCst);
        assert!(change(&live, &store, save("Unconfirmed")).is_err());
        assert_eq!(live.snapshot().results[0].id, current);
        assert!(live.snapshot().saved_tables.is_empty());
        assert!(change(&live, &store, save("Blocked")).is_err());
        assert_eq!(store.writes.load(Ordering::SeqCst), 1);
        store.fail_after_write.store(false, Ordering::SeqCst);
        change(&live, &store, TableChange::Reload).unwrap();
        let snapshot = live.snapshot();
        assert_eq!(snapshot.saved_tables.len(), 1);
        assert_ne!(snapshot.saved_tables[0].results[0].id, current);
        assert!(!snapshot.saved_tables[0].results[0].can_replay);
        assert!(snapshot.results[0].can_replay);
        change(&live, &store, save("Confirmed")).unwrap();
        assert_eq!(live.snapshot().saved_tables[0].id, 2);
        assert!(live.snapshot().saved_tables[0].results[0].can_replay);
        let restarted = testing();
        change(&restarted, &store, TableChange::Reload).unwrap();
        assert_eq!(restarted.snapshot().saved_tables.len(), 2);
    }
}
