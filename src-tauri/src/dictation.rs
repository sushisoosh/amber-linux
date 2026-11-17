//! One dictation at a time: Idle -> Recording -> Transcribing -> Inserting -> Done -> Idle,
//! or -> Error -> Idle. Every step is announced to the bubble and the settings window.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use amber_core::audio::{self, Rejected};
use amber_core::engine::{CancellationToken, ErrorKind, OpenAiTranscriber, TranscribeOptions, Transcriber};
#[cfg(not(target_os = "linux"))]
use amber_core::engine::WhisperServerTranscriber;
use amber_core::chunking::{self, ChunkPlanner, FrameVad, Parts};
use amber_core::{estimate, local};
use amber_core::providers;
use amber_core::settings::{EngineKind, Settings};
use amber_core::text;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use crate::capture::{self, Recorder};
use crate::insert::{self, Outcome, Target};
use crate::state::Core;

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Phase {
    Idle,
    Recording,
    Transcribing,
    Ready,
    Inserting,
    Done,
    Error,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct StateEvent {
    pub state: Phase,
    pub message: Option<String>,
    /// While recognizing: how long it is expected to take, for the bubble's progress ring.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_ms: Option<u64>,
    /// A terminal request, not a dictation: the bubble shows ">_".
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub command: bool,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct DictationResult {
    pub text: String,
    pub message: String,
    pub engine: String,
    pub exposure: String,
}

struct Inner {
    phase: Phase,
    /// Bumped by every new dictation, so a late timer from an old one changes nothing.
    session: u64,
    recorder: Option<Recorder>,
    target: Option<Target>,
    /// The target's program ("discord.exe"), for its profile.
    program: Option<String>,
    /// The target window's title, for terminal mode.
    title: String,
    /// Started with the command hotkey: a terminal request wherever it goes.
    command: bool,
    started: Option<Instant>,
    /// Stop arrived while the microphone was still opening (a very short hold).
    stop_requested: bool,
    /// Recognizes finished parts while the user is still speaking.
    chunks: Option<ChunkJob>,
    cancel: CancellationToken,
    result: Option<DictationResult>,
    pending_paste: Option<CancellationToken>,
}

impl Inner {
    fn cancel_pending_paste(&mut self) {
        if let Some(cancel) = self.pending_paste.take() { cancel.cancel(); }
    }
}

pub struct Dictation {
    inner: Mutex<Inner>,
    processing: AtomicBool,
}

impl Default for Dictation {
    fn default() -> Self {
        Dictation {
            inner: Mutex::new(Inner {
                phase: Phase::Idle,
                session: 0,
                recorder: None,
                target: None,
                program: None,
                title: String::new(),
                command: false,
                started: None,
                stop_requested: false,
                chunks: None,
                cancel: CancellationToken::default(),
                result: None,
                pending_paste: None,
            }),
            processing: AtomicBool::new(false),
        }
    }
}

impl Dictation {
    pub fn is_recording(&self) -> bool {
        self.inner.lock().unwrap().phase == Phase::Recording
    }

    /// Recording or working on the result.
    pub fn is_active(&self) -> bool {
        matches!(self.inner.lock().unwrap().phase, Phase::Recording | Phase::Transcribing | Phase::Inserting)
    }

    pub fn phase(&self) -> Phase {
        self.inner.lock().unwrap().phase
    }

    pub fn result(&self) -> Option<DictationResult> {
        self.inner.lock().unwrap().result.clone()
    }
}

pub fn clear_result(app: &AppHandle) {
    let core = app.state::<Core>();
    let mut inner = core.dictation.inner.lock().unwrap();
    if inner.phase == Phase::Inserting { return; }
    inner.cancel_pending_paste();
    inner.result = None;
    if inner.phase == Phase::Ready {
        inner.phase = Phase::Idle;
    }
    let phase = inner.phase;
    let _ = app.emit("dictation-result", Option::<DictationResult>::None);
    drop(inner);
    announce(app, phase, None);
}

fn keep_result(app: &AppHandle, session: u64, result: DictationResult) -> bool {
    let core = app.state::<Core>();
    let mut inner = core.dictation.inner.lock().unwrap();
    if inner.session != session || inner.cancel.is_cancelled() {
        return false;
    }
    inner.cancel_pending_paste();
    inner.result = Some(result.clone());
    inner.phase = Phase::Ready;
    let _ = app.emit("dictation-result", &result);
    drop(inner);
    announce(app, Phase::Ready, Some(result.message));
    true
}

#[cfg(target_os = "linux")]
pub fn insert_last_result(app: &AppHandle) {
    let core = app.state::<Core>();
    let Some(target) = insert::remember() else {
        crate::notify(app, "Amber", "This desktop cannot identify the destination. Use Copy in the recorder window.");
        return;
    };
    let (result, session, cancel) = {
        let mut inner = core.dictation.inner.lock().unwrap();
        if matches!(inner.phase, Phase::Recording | Phase::Transcribing | Phase::Inserting) || inner.pending_paste.is_some() {
            return;
        }
        let Some(result) = inner.result.clone() else {
            crate::notify(app, "Amber", "There is no dictation result to insert.");
            return;
        };
        let cancel = CancellationToken::default();
        inner.pending_paste = Some(cancel.clone());
        (result, inner.session, cancel)
    };
    let app = app.clone();
    std::thread::spawn(move || {
        // Let a desktop shortcut's modifiers release, then revalidate the target once.
        std::thread::sleep(Duration::from_millis(200));
        let core = app.state::<Core>();
        {
            let mut inner = core.dictation.inner.lock().unwrap();
            if inner.session != session || cancel.is_cancelled() || crate::linux_desktop::session_is_blocked() { return; }
            inner.pending_paste = None;
            inner.phase = Phase::Inserting;
        }
        announce(&app, Phase::Inserting, None);
        let message = match insert::paste_explicit(&result.text, target) {
            Ok(_) => "Paste submitted. Check the destination; insertion cannot be verified automatically.".into(),
            Err(e) => format!("Paste was not confirmed: {e} The result is still in the recorder."),
        };
        core.dictation.inner.lock().unwrap().phase = Phase::Ready;
        announce(&app, Phase::Ready, Some(message.clone()));
        crate::notify(&app, "Amber", &message);
    });
}

fn announce(app: &AppHandle, phase: Phase, message: Option<String>) {
    let core = app.state::<Core>();
    // Only what is still true: an Idle from a dictation that just ended must not arrive after
    // the next one's Recording - the bubble would show idle, Discord would be unmuted.
    let program = {
        let inner = core.dictation.inner.lock().unwrap();
        if inner.phase != phase { return; }
        let _ = app.emit("dictation-state", StateEvent { state: phase, message, expected_ms: None, command: false });
        inner.program.clone()
    };
    // Muted in Discord exactly while Amber listens - stop, cancel and a failed microphone alike.
    crate::mute::follow(&core.settings().discord, program.as_deref(), || core.dictation.phase() == Phase::Recording);
    crate::bubble::sync_visibility(app, !matches!(core.dictation.phase(), Phase::Idle));
}

fn progress(app: &AppHandle, session: u64, expected_ms: Option<u64>, command: bool) -> bool {
    let core = app.state::<Core>();
    let inner = core.dictation.inner.lock().unwrap();
    if inner.session != session || inner.cancel.is_cancelled() || inner.phase != Phase::Transcribing {
        return false;
    }
    let _ = app.emit("dictation-state", StateEvent { state: Phase::Transcribing, message: None, expected_ms, command });
    true
}

/// Leaves Done/Error for Idle after a moment, unless a new dictation started meanwhile.
fn settle(app: &AppHandle, session: u64, after: Duration) {
    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(after);
        let core = app.state::<Core>();
        let mut inner = core.dictation.inner.lock().unwrap();
        if inner.session == session && matches!(inner.phase, Phase::Done | Phase::Error) {
            inner.phase = Phase::Idle;
            drop(inner);
            announce(&app, Phase::Idle, None);
        }
    });
}

fn fail(app: &AppHandle, session: u64, message: String, notify: bool) {
    fail_with(app, session, message, notify, || {});
}

fn fail_with(app: &AppHandle, session: u64, message: String, notify: bool, retain: impl FnOnce()) {
    let core = app.state::<Core>();
    {
        let mut inner = core.dictation.inner.lock().unwrap();
        // A dictation cancelled or replaced meanwhile fails silently.
        if inner.session != session || !matches!(inner.phase, Phase::Recording | Phase::Transcribing | Phase::Inserting) {
            return;
        }
        retain();
        inner.phase = Phase::Error;
    }
    crate::diag::warn(format!("dictation failed: {message}"));
    announce(app, Phase::Error, Some(message.clone()));
    if notify {
        crate::notify(app, "Amber", &message);
    }
    #[cfg(not(target_os = "linux"))]
    settle(app, session, Duration::from_millis(2500));
}

pub fn start(app: &AppHandle) {
    start_as(app, false);
}

#[cfg(target_os = "linux")]
pub fn start_after_desktop_ready(app: &AppHandle) {
    let session = app.state::<Core>().dictation.inner.lock().unwrap().session;
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let ready = tokio::time::timeout(Duration::from_secs(10), async {
            while crate::linux_desktop::initialization_pending() {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        }).await;
        if ready.is_err() {
            crate::notify(&app, "Amber", "Desktop checks are still pending. Use Record once the Recorder diagnostics are ready.");
            return;
        }
        // Do not resurrect a launch-time request after cancellation or another dictation.
        start_for_session(&app, false, Some(session));
    });
}

/// A dictation - with [command] (the command hotkey) a terminal request wherever it goes.
pub fn start_as(app: &AppHandle, command: bool) {
    start_for_session(app, command, None);
}

fn start_for_session(app: &AppHandle, command: bool, expected_session: Option<u64>) {
    #[cfg(target_os = "linux")]
    if crate::linux_desktop::session_is_blocked() { return; }
    let core = app.state::<Core>();
    if core.dictation.processing.load(Ordering::Acquire) {
        crate::notify(app, "Amber", "The previous dictation is still finishing. Wait for it to finish before recording again.");
        return;
    }
    let settings = core.settings();
    let session = {
        let mut inner = core.dictation.inner.lock().unwrap();
        if expected_session.is_some_and(|session| session != inner.session) { return; }
        if core.dictation.processing.load(Ordering::Acquire) || matches!(inner.phase, Phase::Recording | Phase::Transcribing | Phase::Inserting) {
            return;
        }
        #[cfg(target_os = "linux")]
        if crate::linux_desktop::session_is_blocked() { return; }
        inner.cancel_pending_paste();
        inner.session += 1;
        inner.cancel = CancellationToken::default();
        inner.phase = Phase::Recording;
        inner.target = insert::remember();
        inner.program = inner.target.and_then(|t| t.program());
        inner.title = inner.target.map(|t| t.title()).unwrap_or_default();
        inner.command = command;
        inner.started = Some(Instant::now());
        inner.stop_requested = false;
        inner.session
    };
    // The bubble answers at once; the microphone opens in the background.
    announce(app, Phase::Recording, None);
    #[cfg(target_os = "linux")]
    crate::bubble::open_settings(app, Some("recorder"));

    let app = app.clone();
    std::thread::spawn(move || {
        let core = app.state::<Core>();
        // The microphone meter in the settings would hold the device open twice.
        drop(core.mic_test.lock().unwrap().take());

        let level_app = app.clone();
        let opened = Recorder::start(&settings.microphone, move |level| {
            let _ = level_app.emit_to(crate::bubble::BUBBLE, "level", serde_json::json!({ "level": level }));
        });
        let recorder = match opened {
            Ok(r) => r,
            Err(e) => return fail(&app, session, e, true),
        };
        if recorder.fell_back {
            crate::diag::warn(format!("microphone \"{}\" not found, using \"{}\"", settings.microphone, recorder.device));
        }
        let stop_now = {
            let mut inner = core.dictation.inner.lock().unwrap();
            if inner.session != session || inner.phase != Phase::Recording {
                // Cancelled while the microphone was opening.
                return;
            }
            if settings.engine == EngineKind::Local && settings.local.chunking {
                inner.chunks = ChunkJob::start(&app, &settings, recorder.shared_samples(), recorder.rate, inner.cancel.clone());
            }
            inner.recorder = Some(recorder);
            std::mem::take(&mut inner.stop_requested)
        };
        if stop_now {
            stop(&app);
            return;
        }

        // The local model loads while the user speaks, not after - on its own thread, so the
        // time limit below counts from the start of the recording.
        if settings.engine == EngineKind::Local && !core.whisper.is_ready(&settings.local.model) {
            let app = app.clone();
            std::thread::spawn(move || {
                let core = app.state::<Core>();
                // Not if the engine was switched to a server meanwhile.
                if core.settings().engine == EngineKind::Local {
                    let _ = core.whisper.ensure(&app);
                }
            });
        }

        // Hard limit: a forgotten toggle does not record forever.
        std::thread::sleep(Duration::from_secs(capture::MAX_SECONDS as u64));
        let same = {
            let inner = core.dictation.inner.lock().unwrap();
            inner.session == session && inner.phase == Phase::Recording
        };
        if same {
            crate::diag::warn("dictation reached the time limit");
            stop(&app);
        }
    });
}

/// Ends the recording and sends it off.
pub fn stop(app: &AppHandle) {
    let core = app.state::<Core>();
    let (recorder, target, program, session, started, chunks, asked, cancel) = {
        let mut inner = core.dictation.inner.lock().unwrap();
        if inner.phase != Phase::Recording {
            return;
        }
        let Some(recorder) = inner.recorder.take() else {
            inner.stop_requested = true;
            return;
        };
        inner.phase = Phase::Transcribing;
        core.dictation.processing.store(true, Ordering::Release);
        let asked = Asked { title: std::mem::take(&mut inner.title), command: inner.command };
        (recorder, inner.target.take(), inner.program.take(), inner.session, inner.started.take(), inner.chunks.take(), asked, inner.cancel.clone())
    };
    announce(app, Phase::Transcribing, None);
    let app = app.clone();
    std::thread::spawn(move || {
        struct Working<'a>(&'a AtomicBool);
        impl Drop for Working<'_> {
            fn drop(&mut self) { self.0.store(false, Ordering::Release); }
        }
        let core = app.state::<Core>();
        let _working = Working(&core.dictation.processing);
        // What the user waits from here on counts as the wait - also for a part still being
        // recognized.
        let stopped = Instant::now();
        let held_ms = started.map(|s| s.elapsed().as_millis() as u64).unwrap_or(0);
        let recording = recorder.finish();
        // Waits for a part still being recognized; what is left after it is the tail.
        let parts = chunks.map(ChunkJob::finish);
        run(&app, session, recording, parts, target, program, asked, held_ms, stopped, &cancel);
    });
}

/// Throws the recording away (the press turned into a drag).
pub fn cancel(app: &AppHandle) {
    let core = app.state::<Core>();
    if let Some(meter) = core.mic_test.lock().unwrap().take() {
        std::thread::spawn(move || drop(meter));
    }
    let recorder = {
        let mut inner = core.dictation.inner.lock().unwrap();
        inner.cancel_pending_paste();
        if inner.phase == Phase::Inserting { return; }
        if !matches!(inner.phase, Phase::Recording | Phase::Transcribing) {
            #[cfg(target_os = "linux")]
            {
                inner.cancel.cancel();
                inner.session += 1;
            }
            return;
        }
        inner.cancel.cancel();
        inner.session += 1;
        inner.phase = Phase::Idle;
        inner.target = None;
        if let Some(job) = inner.chunks.take() {
            job.abandon();
        }
        inner.recorder.take()
    };
    // Some audio drivers block while closing; cancellation must not freeze the GUI thread.
    std::thread::spawn(move || drop(recorder));
    announce(app, Phase::Idle, None);
}

/// Start if idle, stop if recording - for the tray menu.
pub fn toggle(app: &AppHandle) {
    if app.state::<Core>().dictation.is_recording() {
        stop(app);
    } else {
        start(app);
    }
}

/// What a dictation was started into: its window's title, and whether by the command hotkey.
struct Asked {
    title: String,
    command: bool,
}

/// Terminal mode: [spoken] as a request for a shell command - by the command hotkey, a spoken
/// prefix ("Befehl: ..."), or a terminal program - typed at the prompt without Enter; a risky
/// one commented out. True when it was handled (or failed) as a request.
#[allow(clippy::too_many_arguments)]
fn terminal(app: &AppHandle, settings: &Settings, session: u64, spoken: &str, program: Option<&str>, asked: &Asked, target: Option<Target>, spoken_ms: u64) -> bool {
    use amber_core::terminal::{self as term, Answer, Shell};
    // A snippet's phrase stays a snippet, also in a terminal.
    if !settings.terminal.enabled || amber_core::snippets::find(&settings.snippets, spoken).is_some() {
        return false;
    }
    let prefixed = term::command_prefix(spoken, &settings.terminal.prefixes);
    let profile = program.and_then(|p| amber_core::profiles::find(&settings.profiles, p));
    let explicit = asked.command || prefixed.is_some();
    let Some(choice) = term::wanted(&settings.terminal, profile, program.unwrap_or(""), &asked.title, explicit) else {
        return false;
    };
    let Some(shell) = term::shell_for(program.unwrap_or(""), &asked.title, choice, Shell::Bash) else {
        if explicit {
            fail(app, session, "Terminal commands are for PowerShell and bash, not cmd.".into(), false);
            return true;
        }
        // A cmd tab of Windows Terminal: an ordinary dictation.
        return false;
    };
    let request = prefixed.unwrap_or_else(|| spoken.to_string());
    if request.trim().is_empty() {
        fail(app, session, "Nothing recognized.".into(), false);
        return true;
    }
    let core = app.state::<Core>();
    let emit = |command: bool| {
        progress(app, session, None, command);
    };
    // An ordinary dictation after all - the ">_" goes.
    let dictate = |why: &str| {
        crate::diag::info(format!("terminal: {why} - typed as a dictation"));
        emit(false);
        false
    };
    emit(true);
    let command = match crate::llm::command(app, settings, &request, shell, &asked.title) {
        Ok(Answer::Command(c)) => c,
        // Said into a terminal (or starting with a start word, "Command-line tools are...") but
        // meant as text.
        Ok(Answer::Text) if !asked.command => return dictate("no command asked"),
        Ok(Answer::Text) => {
            fail(app, session, "That sounded like no command - nothing typed.".into(), false);
            return true;
        }
        // No model, or it failed: a dictation into a terminal is not lost for it.
        Err(e) if !explicit => return dictate(&format!("no command ({e})")),
        Err(e) => {
            core.memory.remember(&core.settings(), &request, program.unwrap_or(""), "terminal", false, spoken_ms, 0);
            let kept = if settings.history { " What you said is in the history." } else { "" };
            fail(app, session, format!("{e}{kept}"), true);
            return true;
        }
    };
    let (typed, risk) = term::prepare(&settings.terminal, &command, shell);
    // Never anything that could press Enter (command_from refuses it already).
    if typed.chars().any(char::is_control) {
        fail(app, session, "The command held control characters - nothing typed.".into(), true);
        return true;
    }
    {
        let mut inner = core.dictation.inner.lock().unwrap();
        if inner.session != session {
            return true;
        }
        inner.phase = Phase::Inserting;
    }
    announce(app, Phase::Inserting, None);
    // Typed, not pasted: no clipboard, no trailing space - and never a line break or Enter.
    let how = amber_core::settings::InsertSettings { method: amber_core::settings::InsertMethod::Type, restore_clipboard: true, trailing_space: false };
    let outcome = insert::insert(&typed, &how, target);
    let inserted = matches!(outcome, Ok(Outcome::Pasted | Outcome::Typed));
    core.memory.remember(&core.settings(), &format!("{request} → {typed}"), program.unwrap_or(""), "terminal", inserted, spoken_ms, 0);
    let what = if settings.log_text { format!("\"{typed}\"") } else { format!("{} characters", typed.chars().count()) };
    crate::diag::info(format!("terminal: {} command {what} into {}", shell.name(), program.unwrap_or("?")));
    match risk {
        Some(why) if settings.terminal.safe_mode => crate::notify(app, "Amber", &format!("Typed as a comment - it {why}. Remove the # to run it.")),
        Some(why) => crate::notify(app, "Amber", &format!("Careful - this command {why}. Read it before you press Enter.")),
        None => {}
    }
    match outcome {
        // An elevated window, or the focus moved: on the clipboard instead.
        Ok(Outcome::OnClipboard(message)) => {
            fail(app, session, message, true);
            return true;
        }
        Err(e) => {
            let kept = if settings.history { " It is in the history." } else { "" };
            fail(app, session, format!("The command could not be typed ({e}).{kept}"), true);
            return true;
        }
        Ok(_) => {}
    }
    // Switched on behind a warning: a harmless command runs at once.
    if term::runs(&settings.terminal, &typed, shell) {
        match insert::press_enter(target) {
            Ok(()) => crate::diag::info("terminal: command run (Enter pressed)"),
            Err(e) => crate::diag::warn(format!("terminal: command not run - {e}")),
        }
    }
    {
        let mut inner = core.dictation.inner.lock().unwrap();
        if inner.session != session {
            return true;
        }
        inner.phase = Phase::Done;
    }
    announce(app, Phase::Done, Some(typed));
    settle(app, session, Duration::from_millis(900));
    true
}

#[allow(clippy::too_many_arguments)]
fn run(
    app: &AppHandle,
    session: u64,
    recording: capture::Recording,
    parts: Option<Parts>,
    target: Option<Target>,
    program: Option<String>,
    asked: Asked,
    held_ms: u64,
    stopped: Instant,
    cancel: &CancellationToken,
) {
    if cancel.is_cancelled() { return; }
    #[cfg(target_os = "linux")]
    let interrupted = recording.interrupted;
    let core = app.state::<Core>();
    let settings = core.settings();
    if recording.interrupted && recording.samples.is_empty() {
        return fail(app, session, "The microphone stopped delivering sound.".into(), true);
    }
    let prepared = match audio::prepare(&recording.samples, recording.rate) {
        Ok(p) => p,
        Err(Rejected::Silence) => {
            crate::diag::info(format!("nothing heard in {held_ms} ms"));
            // Not worth a notification: the red bubble says enough.
            return fail(app, session, "Nothing heard - is the right microphone selected?".into(), false);
        }
    };

    // Recognized while speaking: only the tail after the last cut is left.
    let parts = parts.filter(|p| p.usable(recording.samples.len()));
    let tail = parts.as_ref().map(|p| audio::prepare(&recording.samples[p.cut_at..], recording.rate));

    // The bubble's ring fills against what recognition is expected to take here.
    let (estimate_key, typical) = match settings.engine {
        EngineKind::Local => (estimate::key("local", &settings.local.runtime, &settings.local.model), 0.1),
        EngineKind::Server => (estimate::key(&settings.server.provider, "", &settings.server.model), 0.05),
    };
    let left_ms = match &tail {
        Some(Ok(t)) => t.spoken_ms,
        Some(Err(_)) => 0,
        None => prepared.spoken_ms,
    };
    let expected = core.estimates.expected_ms(&estimate_key, left_ms, typical, OVERHEAD_MS);
    if !progress(app, session, Some(expected), false) { return; }

    let recognized = match (&parts, tail) {
        (Some(p), Some(Ok(t))) => {
            // The tail in the parts' language, with the text so far as its prompt.
            let language = p.language.clone().unwrap_or_else(|| settings.language.clone());
            let options = TranscribeOptions::new(&language, &chunking::prompt(&settings.vocabulary, &p.texts));
            // A short tail costs the context floor, not its length: not worth learning from.
            let learn = t.spoken_ms >= 5_000;
            recognize(app, &settings, &t.wav, &options, t.spoken_ms, learn, cancel).map(|(raw, engine, ms)| (p.with_tail(Some(&raw)), engine, ms))
        }
        // The tail held no speech: the parts are the whole dictation.
        (Some(p), Some(Err(_))) => Ok((p.with_tail(None), p.engine.clone(), 0)),
        _ => {
            let options = TranscribeOptions::new(&settings.language, &settings.vocabulary);
            recognize(app, &settings, &prepared.wav, &options, prepared.spoken_ms, true, cancel)
        }
    };
    if let Some(p) = &parts {
        crate::diag::info(format!("recognized while speaking: {} parts, then the last {} ms", p.texts.len(), left_ms));
    }
    if cancel.is_cancelled() { return; }
    let (raw, engine, recognize_ms) = match recognized {
        Ok(r) => r,
        Err(message) => {
            // Kept in memory for a few minutes: one click recognizes it again.
            return fail_with(app, session, format!("{message} The recording is kept for ten minutes; use Recognize again."), true, || {
                core.memory.keep_failed(prepared.wav, prepared.spoken_ms, program, settings.engine == EngineKind::Server);
            });
        }
    };
    if terminal(app, &settings, session, &text::clean(&raw), program.as_deref(), &asked, target, prepared.spoken_ms) {
        return;
    }
    let profile = program.as_deref().and_then(|p| amber_core::profiles::find(&settings.profiles, p));
    let (cleaned, _skipped, remote_cleanup) = crate::llm::tidy_cancellable(app, &settings, &text::clean(&raw), profile, true, cancel);
    if cancel.is_cancelled() { return; }
    if cleaned.is_empty() {
        crate::diag::info(format!("{} ms of audio, nothing recognized", prepared.spoken_ms));
        return fail(app, session, "Nothing recognized.".into(), false);
    }

    #[cfg(target_os = "linux")]
    {
        let exposure = exposure_label(settings.engine == EngineKind::Server, remote_cleanup);
        let result = DictationResult {
            text: text::compose(&cleaned, settings.insert.trailing_space),
            message: if interrupted {
                "The microphone stopped unexpectedly, so this result may be incomplete. Review and copy; the clipboard has not been changed."
            } else {
                "Ready to review and copy. The clipboard has not been changed."
            }.into(),
            engine: engine.clone(),
            exposure: exposure.into(),
        };
        if keep_result(app, session, result) {
            core.memory.remember(&core.settings(), &cleaned, program.as_deref().unwrap_or(""), &engine, false,
                prepared.spoken_ms, stopped.elapsed().as_millis() as u64);
        }
        let _ = (recognize_ms, target);
    }

    #[cfg(not(target_os = "linux"))]
    {
    let _ = remote_cleanup;
    {
        let mut inner = core.dictation.inner.lock().unwrap();
        if inner.session != session {
            return;
        }
        inner.phase = Phase::Inserting;
    }
    announce(app, Phase::Inserting, None);
    let final_text = text::compose(&cleaned, settings.insert.trailing_space);
    let outcome = insert::insert(&final_text, &settings.insert, target);

    let what = if settings.log_text { format!("\"{cleaned}\"") } else { format!("{} words", text::words(&cleaned)) };
    let inserted = matches!(outcome, Ok(Outcome::Pasted | Outcome::Typed));
    core.memory.remember(
        &core.settings(),
        &cleaned,
        program.as_deref().unwrap_or(""),
        &engine,
        inserted,
        prepared.spoken_ms,
        stopped.elapsed().as_millis() as u64,
    );
    match outcome {
        Ok(Outcome::OnClipboard(message)) => {
            crate::diag::warn(format!("{what} left on the clipboard: {message}"));
            fail(app, session, message, true);
        }
        Ok(how) => {
            crate::diag::info(format!(
                "{what} · {} ms audio · {engine} {recognize_ms} ms · {how:?} into {}",
                prepared.spoken_ms,
                program.as_deref().unwrap_or("?")
            ));
            {
                let mut inner = core.dictation.inner.lock().unwrap();
                if inner.session != session {
                    return;
                }
                inner.phase = Phase::Done;
            }
            announce(app, Phase::Done, Some(cleaned));
            settle(app, session, Duration::from_millis(900));
        }
        Err(e) => {
            // Never lost: on the clipboard if it can go there, and in the history if it is on.
            let where_ = match (insert::to_clipboard(&final_text).is_ok(), settings.history) {
                (true, true) => " It is on the clipboard and in the history.",
                (true, false) => " It is on the clipboard.",
                (false, true) => " It is in the history.",
                (false, false) => "",
            };
            fail(app, session, format!("The text could not be inserted ({e}).{where_}"), true)
        }
    }
    }
}

/// Retries with the current engine. Linux retains a result for review; Windows copies it.
pub fn recognize_again(app: &AppHandle) -> Result<String, String> {
    #[cfg(target_os = "linux")]
    if crate::linux_desktop::session_is_blocked() { return Err("Unlock the desktop before retrying a dictation.".into()); }
    let core = app.state::<Core>();
    let (mut failed, session, cancel) = {
        let mut inner = core.dictation.inner.lock().unwrap();
        if matches!(inner.phase, Phase::Recording | Phase::Transcribing | Phase::Inserting) || core.dictation.processing.load(Ordering::Acquire) {
            return Err("A dictation is already running.".into());
        }
        #[cfg(target_os = "linux")]
        if crate::linux_desktop::session_is_blocked() { return Err("Unlock the desktop before retrying a dictation.".into()); }
        let failed = core.memory.take_failed().ok_or("No failed recording from the last ten minutes.")?;
        core.dictation.processing.store(true, Ordering::Release);
        inner.cancel_pending_paste();
        inner.session += 1;
        inner.cancel = CancellationToken::default();
        inner.phase = Phase::Transcribing;
        (failed, inner.session, inner.cancel.clone())
    };
    struct Working<'a>(&'a AtomicBool);
    impl Drop for Working<'_> {
        fn drop(&mut self) { self.0.store(false, Ordering::Release); }
    }
    let _working = Working(&core.dictation.processing);
    announce(app, Phase::Transcribing, None);
    let settings = core.settings();
    failed.remote_speech |= settings.engine == EngineKind::Server;
    let started = Instant::now();
    let options = TranscribeOptions::new(&settings.language, &settings.vocabulary);
    let (raw, engine, _) = match recognize(app, &settings, &failed.wav, &options, failed.spoken_ms, true, &cancel) {
        Ok(r) => r,
        Err(e) => {
            // Still there for another try (another engine, the key fixed) - unless a newer
            // dictation failed meanwhile; that one is the one to keep.
            if !cancel.is_cancelled() { core.memory.return_failed(failed); }
            fail(app, session, e.clone(), false);
            return Err(e);
        }
    };
    let profile = failed.program.as_deref().and_then(|p| amber_core::profiles::find(&settings.profiles, p));
    cancel.check().map_err(|e| e.message)?;
    let (text, _, remote_cleanup) = crate::llm::tidy_cancellable(app, &settings, &text::clean(&raw), profile, true, &cancel);
    cancel.check().map_err(|e| e.message)?;
    if text.is_empty() {
        fail(app, session, "Nothing recognized.".into(), false);
        return Err("Nothing recognized.".into());
    }
    #[cfg(target_os = "linux")]
    if !keep_result(app, session, DictationResult {
        text: text.clone(),
        message: "Ready to review and copy. The clipboard has not been changed.".into(),
        engine: engine.clone(),
        exposure: exposure_label(failed.remote_speech, remote_cleanup).into(),
    }) {
        return Err("Dictation was cancelled.".into());
    }
    #[cfg(not(target_os = "linux"))]
    {
        {
            let mut inner = core.dictation.inner.lock().unwrap();
            if inner.session != session || cancel.is_cancelled() { return Err("Dictation was cancelled.".into()); }
            inner.phase = Phase::Inserting;
        }
        announce(app, Phase::Inserting, None);
        if let Err(e) = insert::to_clipboard(&text) {
            fail(app, session, e.clone(), false);
            return Err(e);
        }
        core.dictation.inner.lock().unwrap().phase = Phase::Done;
        announce(app, Phase::Done, Some("The result is on the clipboard.".into()));
        settle(app, session, Duration::from_millis(900));
        let _ = remote_cleanup;
    }
    core.memory.remember(&settings, &text, failed.program.as_deref().unwrap_or(""), &engine, false, failed.spoken_ms, started.elapsed().as_millis() as u64);
    crate::diag::info(format!("recognized again: {} words", text::words(&text)));
    Ok(text)
}

/// Watches the growing recording, cuts it at real pauses (`chunking::ChunkPlanner`) and
/// recognizes each finished part in order, with the text so far as its prompt and the first
/// part's language kept for the rest.
struct ChunkJob {
    stop: Arc<AtomicBool>,
    handle: std::thread::JoinHandle<Parts>,
}

impl ChunkJob {
    /// Only for models tuned to a short encoder context - a stock model works through 30 s
    /// for any part, and parts would only make it slower. Judged by the model the server
    /// actually runs, which may still be the previous one while the chosen one downloads.
    fn start(app: &AppHandle, settings: &Settings, samples: Arc<Mutex<Vec<f32>>>, rate: u32, cancel: CancellationToken) -> Option<ChunkJob> {
        let model = app.state::<Core>().whisper.running_model().unwrap_or_else(|| settings.local.model.clone());
        if !local::model(&model).is_some_and(|m| m.short_context) {
            return None;
        }
        let stop = Arc::new(AtomicBool::new(false));
        let (app, settings, flag) = (app.clone(), settings.clone(), stop.clone());
        let handle = std::thread::spawn(move || {
            let mut planner = ChunkPlanner::new(rate, 4_000);
            let mut vad = FrameVad::default();
            let frame = planner.frame_samples();
            let mut parts = Parts::default();
            let mut pos = 0;
            loop {
                // The recording's buffer is emptied by the stop; reading past it means: done.
                let Some(fresh) = ({
                    let all = samples.lock().unwrap();
                    all.get(pos..).map(|rest| rest[..rest.len() / frame * frame].to_vec())
                }) else {
                    return parts;
                };
                for f in fresh.chunks_exact(frame) {
                    if flag.load(Ordering::SeqCst) || cancel.is_cancelled() {
                        // Stopped: whatever is not recognized yet belongs to the tail.
                        return parts;
                    }
                    pos += frame;
                    let rms = audio::rms(f);
                    let Some(cut) = planner.on_frame(pos, rms, vad.is_speech(rms)) else { continue };
                    let Some(slice) = samples.lock().unwrap().get(cut.start..cut.end).map(<[f32]>::to_vec) else { return parts };
                    let Ok(part) = audio::prepare(&slice, rate) else {
                        // A cough, a breath: nothing to recognize, but the tail starts after it.
                        parts.cut_at = cut.end;
                        continue;
                    };
                    let language = parts.language.clone().unwrap_or_else(|| settings.language.clone());
                    let options = TranscribeOptions::new(&language, &chunking::prompt(&settings.vocabulary, &parts.texts));
                    // Asked for its language only until it is known - with no language set, or
                    // several to choose among.
                    let detect = options.fixed().is_none() && parts.language.is_none();
                    let result = transcriber(&app, &settings).and_then(|p| {
                        parts.engine = p.label.clone();
                        let r = if detect {
                            p.transcriber.transcribe_detecting_cancellable(&part.wav, &options, &cancel)
                        } else {
                            p.transcriber.transcribe_cancellable(&part.wav, &options, &cancel).map(|t| (t, None))
                        };
                        r.map_err(|e| e.message)
                    });
                    match result {
                        Ok((text, detected)) => {
                            if detect {
                                parts.language = chunking::part_language(detected.as_deref(), &options.among());
                            }
                            // Cleaned right away: the next part's prompt must not carry
                            // Whisper's "[Musik]" or invented credits.
                            parts.texts.push(text::clean(&text));
                            parts.cut_at = cut.end;
                        }
                        Err(e) => {
                            crate::diag::warn(format!("a part failed ({e}), the whole recording will be recognized"));
                            parts.failed = true;
                            return parts;
                        }
                    }
                }
                if flag.load(Ordering::SeqCst) || cancel.is_cancelled() {
                    return parts;
                }
                std::thread::sleep(Duration::from_millis(150));
            }
        });
        Some(ChunkJob { stop, handle })
    }

    /// The parts recognized until now - waits for one still being recognized.
    fn finish(self) -> Parts {
        self.stop.store(true, Ordering::SeqCst);
        self.handle.join().unwrap_or(Parts { failed: true, ..Parts::default() })
    }

    /// The recording was thrown away: the job stops at the next frame, nobody waits for it.
    fn abandon(self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

/// An engine ready for one request, with the names it is logged and measured under.
pub struct Picked {
    pub transcriber: Box<dyn Transcriber>,
    label: String,
    estimate_key: String,
}

/// Fixed cost per request on top of the audio: a round trip, a context switch.
const OVERHEAD_MS: u64 = 200;

/// Recognizes [wav] with the engine the settings ask for. A local server that went away
/// mid-request gets one more try; a server that cannot be reached hands the recording to a
/// downloaded local model (if allowed) - never the other way round. Learns how long it took.
fn recognize(
    app: &AppHandle,
    settings: &Settings,
    wav: &[u8],
    options: &TranscribeOptions,
    spoken_ms: u64,
    learn: bool,
    cancel: &CancellationToken,
) -> Result<(String, String, u128), String> {
    cancel.check().map_err(|e| e.message)?;
    let core = app.state::<Core>();
    let mut picked = transcriber(app, settings)?;
    let mut t0 = Instant::now();
    let mut result = picked.transcriber.transcribe_cancellable(wav, options, cancel);
    cancel.check().map_err(|e| e.message)?;

    if let Err(e) = &result {
        // Not when the engine was switched to a server meanwhile - that stopped the local one.
        if settings.engine == EngineKind::Local && e.kind == ErrorKind::Unreachable && core.settings().engine == EngineKind::Local {
            // Restarted for another model, or crashed.
            crate::diag::warn("local engine went away during the request, trying again");
            picked = transcriber(app, settings)?;
            t0 = Instant::now();
            result = picked.transcriber.transcribe_cancellable(wav, options, cancel);
        } else if settings.engine == EngineKind::Server && e.kind.service_unavailable() && settings.server.local_fallback && local_ready(&core, settings) {
            crate::diag::warn(format!("server unavailable ({}), recognizing on this PC", e.detail));
            let server_error = e.message.clone();
            let local_result = local(app).and_then(|p| {
                let t = Instant::now();
                let r = p.transcriber.transcribe_cancellable(wav, options, cancel).map_err(|e| e.message);
                Ok((p, t, r))
            });
            match local_result {
                Ok((p, t, Ok(raw))) => {
                    crate::notify(app, "Amber", "The server could not be reached - recognized on this PC instead.");
                    unload_after_fallback(app);
                    picked = p;
                    t0 = t;
                    result = Ok(raw);
                }
                // The server's failure is the one the user needs to hear about.
                Ok((_, _, Err(local))) | Err(local) => {
                    crate::diag::warn(format!("local fallback failed too: {local}"));
                    return Err(server_error);
                }
            }
        }
    }
    match result {
        Ok(raw) => {
            let took = t0.elapsed().as_millis();
            if learn {
                core.estimates.learn(&picked.estimate_key, spoken_ms, took as u64, OVERHEAD_MS);
            }
            Ok((raw, picked.label, took))
        }
        Err(e) => {
            crate::diag::warn(format!("engine error: {}", e.detail));
            Err(e.message)
        }
    }
}

/// The model loaded for a fallback holds memory the server engine does not need: unloaded
/// after ten minutes without another fallback.
fn unload_after_fallback(app: &AppHandle) {
    use std::sync::atomic::{AtomicU64, Ordering};
    static LAST: AtomicU64 = AtomicU64::new(0);
    let this = LAST.fetch_add(1, Ordering::SeqCst) + 1;
    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(600));
        let core = app.state::<Core>();
        if LAST.load(Ordering::SeqCst) == this && core.settings().engine == EngineKind::Server && !core.dictation.is_active() {
            core.whisper.stop(&app);
        }
    });
}

/// Whether the local engine could take over: runtime and model downloaded.
fn local_ready(core: &Core, s: &Settings) -> bool {
    let layout = &core.paths.layout;
    local::runtime(&s.local.runtime).is_some_and(|rt| layout.runtime_installed(rt))
        && local::model(&s.local.model).is_some_and(|m| layout.model_installed(m))
}

/// The local whisper server - with a short encoder context when the model it runs (which may
/// still be the previous one while the chosen one downloads) was tuned for it.
fn local(app: &AppHandle) -> Result<Picked, String> {
    local_model(app, None)
}

/// The local engine with [model] instead of the configured one (the phone's choice).
fn local_model(app: &AppHandle, model: Option<&str>) -> Result<Picked, String> {
    let served = app.state::<Core>().whisper.ensure_model(app, model)?;
    #[cfg(not(target_os = "linux"))]
    let slack = local::model(&served.model).filter(|m| m.short_context).map(|m| m.context_slack);
    Ok(Picked {
        #[cfg(not(target_os = "linux"))]
        transcriber: Box::new(WhisperServerTranscriber::new(&served.url, slack)),
        #[cfg(target_os = "linux")]
        transcriber: served.transcriber,
        label: format!("local {}/{}", served.runtime, served.model),
        estimate_key: estimate::key("local", &served.runtime, &served.model),
    })
}

/// The engine for a phone request naming [model]: a downloaded local model, the cloud model
/// set in the app, or else the PC's own. One local model at a time.
pub fn transcriber_for(app: &AppHandle, settings: &Settings, model: Option<&str>) -> Result<Picked, String> {
    let layout = &app.state::<Core>().paths.layout;
    if let Some(m) = model.and_then(local::model).filter(|m| layout.model_installed(m)) {
        if !(settings.engine == EngineKind::Local && settings.local.model == m.id) {
            return local_model(app, Some(m.id));
        }
    }
    if model.is_some_and(|id| id == settings.server.model) && settings.engine != EngineKind::Server {
        let mut cloud = settings.clone();
        cloud.engine = EngineKind::Server;
        return transcriber(app, &cloud);
    }
    transcriber(app, settings)
}

/// The engine the settings ask for, ready to use - or why not.
pub fn transcriber(app: &AppHandle, settings: &Settings) -> Result<Picked, String> {
    let core = app.state::<Core>();
    match settings.engine {
        EngineKind::Local => local(app),
        EngineKind::Server => {
            let provider = providers::by_id(&settings.server.provider).ok_or("Unknown server provider.")?;
            let base = providers::effective_base_url(provider, &settings.server.base_url);
            providers::check_base_url(&base)?;
            let key = core.key_for(provider.id, &base).unwrap_or_default();
            if provider.needs_key && key.is_empty() {
                return Err(format!("No API key for {} yet - add it in the settings.", provider.name));
            }
            let transcriber = OpenAiTranscriber::new(&base, &key, &settings.server.model);
            let transcriber = if provider.id == "custom" { transcriber.with_catalog_discovery() } else { transcriber };
            Ok(Picked {
                transcriber: Box::new(transcriber),
                label: format!("{} {}", provider.id, settings.server.model),
                estimate_key: estimate::key(provider.id, "", &settings.server.model),
            })
        }
    }
}

fn exposure_label(remote_speech: bool, remote_cleanup: bool) -> &'static str {
    match (remote_speech, remote_cleanup) {
        (true, true) => "Audio and text may have been sent to your configured recognition and cleanup servers.",
        (true, false) => "Audio may have been sent to your configured recognition server, including before a local fallback.",
        (false, true) => "Recognition stayed local. Text may have been sent to your configured cleanup server, even if cleanup failed.",
        (false, false) => "Processed locally on this computer.",
    }
}
