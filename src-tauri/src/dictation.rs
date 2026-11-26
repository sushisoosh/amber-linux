//! One dictation at a time: Idle -> Recording -> Transcribing -> Inserting -> Done -> Idle,
//! or -> Error -> Idle. Every step is announced to the bubble and the settings window.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use amber_core::audio::{self, Rejected};
use amber_core::engine::{CancellationToken, ErrorKind, provider_transcriber, TranscribeOptions, Transcriber};
use amber_core::chunking::{self, ChunkPlanner, FrameVad, Parts};
use amber_core::{estimate, local};
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
    /// Expected remaining work (recognition plus cleanup); refinements never reset the ring.
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
    /// Consent at recording start, not a setting enabled while recognition was running.
    paste_on_completion: bool,
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
    /// Generated shell output must not reach either automatic or shortcut-driven paste.
    result_copy_only: bool,
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
    testing: AtomicBool,
}

impl Default for Dictation {
    fn default() -> Self {
        Dictation {
            inner: Mutex::new(Inner {
                phase: Phase::Idle,
                session: 0,
                recorder: None,
                target: None,
                paste_on_completion: false,
                program: None,
                title: String::new(),
                command: false,
                started: None,
                stop_requested: false,
                chunks: None,
                cancel: CancellationToken::default(),
                result: None,
                result_copy_only: false,
                pending_paste: None,
            }),
            processing: AtomicBool::new(false),
            testing: AtomicBool::new(false),
        }
    }
}

impl Dictation {
    pub fn is_recording(&self) -> bool {
        self.inner.lock().unwrap().phase == Phase::Recording
    }

    /// Recording or working on the result.
    pub fn is_active(&self) -> bool {
        self.processing.load(Ordering::Acquire) || matches!(self.inner.lock().unwrap().phase, Phase::Recording | Phase::Transcribing | Phase::Inserting)
    }

    /// Keep a new desktop recording from racing a user-requested model unload.
    pub fn when_idle<T>(&self, action: impl FnOnce() -> T) -> Result<T, String> {
        let inner = self.inner.lock().unwrap();
        if self.processing.load(Ordering::Acquire) || matches!(inner.phase, Phase::Recording | Phase::Transcribing | Phase::Inserting) {
            return Err("Stop the recording or wait for recognition to finish before unloading the model.".into());
        }
        Ok(action())
    }

    /// Share the ordinary worker gate, but never acquire a target or delivery consent.
    fn reserve_test(&self) -> Result<bool, String> {
        let mut inner = self.inner.lock().unwrap();
        if matches!(inner.phase, Phase::Recording | Phase::Transcribing | Phase::Inserting)
            || self.processing.swap(true, Ordering::AcqRel) {
            return Err("A recording is already running or finishing. Stop or cancel it first.".into());
        }
        inner.cancel_pending_paste();
        inner.session += 1;
        // The old settle timer belongs to the previous session and will no longer run.
        let settled = matches!(inner.phase, Phase::Done | Phase::Error);
        if settled { inner.phase = Phase::Idle; }
        self.testing.store(true, Ordering::Release);
        Ok(settled)
    }

    pub(crate) fn release_test(&self) {
        self.testing.store(false, Ordering::Release);
        self.processing.store(false, Ordering::Release);
    }

    pub fn phase(&self) -> Phase {
        self.inner.lock().unwrap().phase
    }

    pub fn result(&self) -> Option<DictationResult> {
        self.inner.lock().unwrap().result.clone()
    }
}

pub(crate) fn reserve_test(app: &AppHandle) -> Result<(), String> {
    if app.state::<Core>().dictation.reserve_test()? {
        announce(app, Phase::Idle, None);
    }
    Ok(())
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

fn keep_result(app: &AppHandle, session: u64, result: DictationResult, copy_only: bool) -> bool {
    let core = app.state::<Core>();
    let mut inner = core.dictation.inner.lock().unwrap();
    if inner.session != session || inner.cancel.is_cancelled() {
        return false;
    }
    inner.cancel_pending_paste();
    inner.result = Some(result.clone());
    inner.result_copy_only = copy_only;
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
        if matches!(inner.phase, Phase::Recording | Phase::Transcribing | Phase::Inserting) || inner.pending_paste.is_some() || core.dictation.processing.load(Ordering::Acquire) {
            return;
        }
        let Some(result) = inner.result.clone() else {
            crate::notify(app, "Amber", "There is no dictation result to insert.");
            return;
        };
        if inner.result_copy_only {
            crate::notify(app, "Amber", "Shell instructions are review/copy-only. Review the result and use Copy; Amber will not paste or execute it.");
            return;
        }
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
        let message = match insert::paste_explicit(&result.text, target, &core.settings().insert) {
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
    crate::tray::sync_state(app);
    // Muted in Discord exactly while Amber listens - stop, cancel and a failed microphone alike.
    crate::mute::follow(&core.settings().discord, program.as_deref(), || core.dictation.phase() == Phase::Recording);
    crate::bubble::sync_visibility(app);
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
pub fn start_after_desktop_ready(app: &AppHandle, target: Option<Target>) {
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
        start_for_session(&app, false, Some(session), target);
    });
}

/// A dictation - with [command] (the command hotkey) a terminal request wherever it goes.
pub fn start_as(app: &AppHandle, command: bool) {
    start_for_session(app, command, None, insert::remember());
}

fn start_for_session(app: &AppHandle, command: bool, expected_session: Option<u64>, target: Option<Target>) {
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
        // Keep the original target; only the explicit different-window setting may retarget delivery.
        inner.target = target;
        inner.paste_on_completion = amber_core::insertion::paste_on_completion(&settings.insert, true, false);
        inner.program = inner.target.and_then(|t| t.program());
        inner.title = inner.target.map(|t| t.title()).unwrap_or_default();
        inner.command = command;
        inner.started = Some(Instant::now());
        inner.stop_requested = false;
        inner.session
    };
    // The bubble answers at once; the microphone opens in the background.
    announce(app, Phase::Recording, None);

    let app = app.clone();
    std::thread::spawn(move || {
        let core = app.state::<Core>();
        // The microphone meter in the settings would hold the device open twice.
        let meter = core.mic_test.lock().unwrap().take();
        drop(meter);

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
            if settings.engine == EngineKind::Local && settings.local.chunking && !settings.instructions.iter().any(|r| r.enabled) {
                // An instruction belongs to the whole utterance, not the first audio chunk.
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
        let asked = Asked {
            title: std::mem::take(&mut inner.title), command: inner.command,
            paste_on_completion: inner.paste_on_completion,
        };
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
        let settings = core.settings();
        let audio_ms = audio::duration_ms(recording.samples.len(), recording.rate);
        let profile = program.as_deref().and_then(|p| amber_core::profiles::find(&settings.profiles, p));
        let expected = expected_speech_ms(&core, &settings, audio_ms)
            .saturating_add(crate::llm::expected_ms(&app, &settings, profile, None, estimate::predicted_chars(audio_ms)));
        if !progress(&app, session, Some(expected), false) { return; }
        // Waits for a part still being recognized; what is left after it is the tail.
        let parts = chunks.map(ChunkJob::finish);
        run(&app, session, recording, parts, target, program, asked, held_ms, stopped, &cancel);
    });
}

/// Throws the recording away (the press turned into a drag).
pub fn cancel(app: &AppHandle) {
    let core = app.state::<Core>();
    if core.dictation.testing.load(Ordering::Acquire) {
        crate::testing::cancel(app);
    }
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
    paste_on_completion: bool,
}

/// Terminal mode: [spoken] as a request for a shell command - by the command hotkey, a spoken
/// prefix ("Befehl: ..."), or a terminal program - typed at the prompt without Enter; a risky
/// one commented out. True when it was handled (or failed) as a request.
#[allow(clippy::too_many_arguments)]
fn terminal(app: &AppHandle, settings: &Settings, session: u64, spoken: &str, program: Option<&str>, asked: &Asked, target: Option<Target>, spoken_ms: u64) -> bool {
    use amber_core::terminal::{self as term, Answer, Shell};
    // Saved keyword transformations take precedence over automatic/prefixed terminal mode,
    // even when smart cleanup is off or the request is empty. Never run their output.
    if !settings.terminal.enabled || amber_core::snippets::find(&settings.snippets, spoken).is_some()
        || amber_core::instructions::match_start(&settings.instructions, spoken).is_some() {
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
            core.memory.remember(&core.settings(), &request, program.unwrap_or(""), "terminal", false, spoken_ms, 0, None, None, None);
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
    let how = amber_core::settings::InsertSettings {
        method: amber_core::settings::InsertMethod::Type, trailing_space: false,
        ..amber_core::settings::InsertSettings::default()
    };
    let outcome = insert::insert(&typed, &how, target);
    let inserted = matches!(outcome, Ok(Outcome::Pasted | Outcome::Typed));
    core.memory.remember(&core.settings(), &format!("{request} → {typed}"), program.unwrap_or(""), "terminal", inserted, spoken_ms, 0, None, None, None);
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
    let speech_tail = matches!(&tail, Some(Ok(_)));

    let profile = program.as_deref().and_then(|p| amber_core::profiles::find(&settings.profiles, p));
    let cleanup_budget = crate::llm::expected_ms(app, &settings, profile, None, estimate::predicted_chars(prepared.spoken_ms));
    let left_ms = match &tail {
        Some(Ok(t)) => t.spoken_ms,
        Some(Err(_)) => 0,
        None => prepared.spoken_ms,
    };
    let expected = expected_speech_ms(&core, &settings, left_ms).saturating_add(cleanup_budget);
    if !progress(app, session, Some(expected), false) { return; }
    // Once ready, the actual route (including a previous model or a fallback) owns both the
    // prediction and observation. Setup/retries may extend the wait, but never train it.
    let refine = |route: &estimate::Route| {
        progress(app, session, Some(core.estimates.expected_ms(route, left_ms).saturating_add(cleanup_budget)), false);
    };

    let transcription_started = Instant::now();
    let recognized = match (&parts, tail) {
        (Some(p), Some(Ok(t))) => {
            // The tail in the parts' language, with the text so far as its prompt.
            let language = p.language.clone().unwrap_or_else(|| settings.language.clone());
            let options = TranscribeOptions::new(&language, &chunking::prompt(&settings.vocabulary, &p.texts));
            recognize(app, &settings, &t.wav, &options, t.spoken_ms, true, cancel, Some(&refine)).map(|(raw, engine, ms, timing)| (p.with_tail(Some(&raw)), engine, ms, timing))
        }
        // The tail held no speech: the parts are the whole dictation.
        (Some(p), Some(Err(_))) => Ok((p.with_tail(None), p.engine.clone(), 0, None)),
        _ => {
            let options = TranscribeOptions::new(&settings.language, &settings.vocabulary);
            recognize(app, &settings, &prepared.wav, &options, prepared.spoken_ms, true, cancel, Some(&refine))
        }
    };
    if let Some(p) = &parts {
        crate::diag::info(format!("recognized while speaking: {} parts, then the last {} ms", p.texts.len(), left_ms));
    }
    if cancel.is_cancelled() { return; }
    let (raw, engine, recognize_ms, recognition_timing) = match recognized {
        Ok(r) => r,
        Err(message) => {
            // Kept in memory for a few minutes: one click recognizes it again.
            return fail_with(app, session, format!("{message} The recording is kept for ten minutes; use Recognize again."), true, || {
                core.memory.keep_failed(prepared.wav, prepared.spoken_ms, program, settings.engine == EngineKind::Server);
            });
        }
    };
    let transcription_ms = transcription_started.elapsed().as_millis() as u64;
    let recognized = text::clean(&raw);
    let copy_only = instruction_copy_only(&settings, &recognized);
    if terminal(app, &settings, session, &recognized, program.as_deref(), &asked, target, prepared.spoken_ms) {
        if core.dictation.phase() == Phase::Done {
            if let Some(timing) = recognition_timing { timing.learn(&core.estimates, cancel); }
        }
        return;
    }
    let cleanup_budget = crate::llm::expected_ms(app, &settings, profile, Some(&recognized), recognized.chars().count() as u64);
    if !progress(app, session, Some(cleanup_budget), false) { return; }
    let cleanup_started = Instant::now();
    let crate::llm::Tidied { text: cleaned, skipped, remote: remote_cleanup, cleanup, route } = crate::llm::tidy_cancellable(app, &settings, &recognized, profile, true, cancel);
    let speech_route = match &parts {
        Some(parts) if !speech_tail => parts.execution_route(None),
        Some(parts) => recognition_timing.as_ref().and_then(|timing| parts.execution_route(Some(&timing.route))),
        None => recognition_timing.as_ref().map(|timing| timing.route.clone()),
    };
    let execution = speech_route.map(|speech| amber_core::history::Execution { speech, cleanup: route });
    let timings = amber_core::history::Timings {
        recording_ms: Some(audio::duration_ms(recording.samples.len(), recording.rate)),
        transcription_ms,
        cleanup_ms: cleanup_started.elapsed().as_millis() as u64,
        processing_ms: stopped.elapsed().as_millis() as u64,
        live: parts.is_some(),
        cleanup_skipped: skipped.is_some(),
    };
    if cancel.is_cancelled() { return; }
    if cleaned.is_empty() {
        crate::diag::info(format!("{} ms of audio, nothing recognized", prepared.spoken_ms));
        return fail(app, session, "Nothing recognized.".into(), false);
    }
    if let Some(timing) = recognition_timing { timing.learn(&core.estimates, cancel); }

    #[cfg(target_os = "linux")]
    {
        let exposure = exposure_label(settings.engine == EngineKind::Server, remote_cleanup);
        let mut result = DictationResult {
            text: text::compose(&cleaned, settings.insert.trailing_space && !copy_only),
            message: if copy_only {
                "Shell instruction ready for review and copy only. Amber will not paste or execute it. The clipboard has not been changed."
            } else if interrupted {
                "The microphone stopped unexpectedly, so this result may be incomplete. Review and copy; the clipboard has not been changed."
            } else {
                "Ready to review and copy. The clipboard has not been changed."
            }.into(),
            engine: engine.clone(),
            exposure: exposure.into(),
        };
        if let Some(why) = skipped.filter(|_| amber_core::instructions::match_start(&settings.instructions, &recognized).is_some()) {
            result.message = format!("{why} {}", result.message);
        }
        if !copy_only && amber_core::insertion::paste_on_completion(&core.settings().insert, asked.paste_on_completion, interrupted) {
            {
                let mut inner = core.dictation.inner.lock().unwrap();
                if inner.session != session || cancel.is_cancelled() { return; }
                inner.phase = Phase::Inserting;
            }
            announce(app, Phase::Inserting, None);
            result.message = match insert::deliver(&result.text, &core.settings().insert, target, cancel) {
                Ok(Outcome::OnClipboard(message)) => message,
                Ok(_) => "Paste shortcut submitted to the selected destination. Check the destination before pasting again; insertion cannot be verified. The clipboard was replaced.".into(),
                Err(e) => format!("Paste was not confirmed: {e} The result is still available here. Check the destination before copying or trying again."),
            };
        }
        let message = result.message.clone();
        if keep_result(app, session, result, copy_only) {
            core.memory.remember(&core.settings(), &cleaned, program.as_deref().unwrap_or(""), &engine, false,
                prepared.spoken_ms, stopped.elapsed().as_millis() as u64, Some(timings), Some(cleanup), execution);
            crate::notify(app, "Amber", &message);
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
    if copy_only {
        // Copy only: even a safe-looking single line may be destructive, and multi-line
        // shell text can execute on paste without a separate Enter keypress.
        if let Err(e) = insert::to_clipboard(&cleaned) {
            return fail(app, session, format!("The shell instruction could not be copied: {e}"), true);
        }
        core.memory.remember(&core.settings(), &cleaned, program.as_deref().unwrap_or(""), &engine, false,
            prepared.spoken_ms, stopped.elapsed().as_millis() as u64, Some(timings), Some(cleanup), execution);
        let message = format!("{}Shell instruction copied for review. Amber did not paste it or press Enter.",
            skipped.map(|why| format!("{why} ")).unwrap_or_default());
        core.dictation.inner.lock().unwrap().phase = Phase::Done;
        announce(app, Phase::Done, Some(message.clone()));
        crate::notify(app, "Amber", &message);
        settle(app, session, Duration::from_millis(2500));
        return;
    }
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
        Some(timings),
        Some(cleanup),
        execution,
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
    let profile = failed.program.as_deref().and_then(|p| amber_core::profiles::find(&settings.profiles, p));
    let cleanup_budget = crate::llm::expected_ms(app, &settings, profile, None, estimate::predicted_chars(failed.spoken_ms));
    progress(app, session, Some(expected_speech_ms(&core, &settings, failed.spoken_ms).saturating_add(cleanup_budget)), false);
    let refine = |route: &estimate::Route| {
        progress(app, session, Some(core.estimates.expected_ms(route, failed.spoken_ms).saturating_add(cleanup_budget)), false);
    };
    let transcription_started = Instant::now();
    let (raw, engine, _, recognition_timing) = match recognize(app, &settings, &failed.wav, &options, failed.spoken_ms, true, &cancel, Some(&refine)) {
        Ok(r) => r,
        Err(e) => {
            // Still there for another try (another engine, the key fixed) - unless a newer
            // dictation failed meanwhile; that one is the one to keep.
            if !cancel.is_cancelled() { core.memory.return_failed(failed); }
            fail(app, session, e.clone(), false);
            return Err(e);
        }
    };
    let transcription_ms = transcription_started.elapsed().as_millis() as u64;
    cancel.check().map_err(|e| e.message)?;
    let recognized = text::clean(&raw);
    let copy_only = instruction_copy_only(&settings, &recognized);
    progress(app, session, Some(crate::llm::expected_ms(app, &settings, profile, Some(&recognized), recognized.chars().count() as u64)), false);
    let cleanup_started = Instant::now();
    let crate::llm::Tidied { text, skipped, remote: remote_cleanup, cleanup, route } = crate::llm::tidy_cancellable(app, &settings, &recognized, profile, true, &cancel);
    let execution = recognition_timing.as_ref().map(|timing| amber_core::history::Execution { speech: timing.route.clone(), cleanup: route });
    let timings = amber_core::history::Timings {
        recording_ms: None,
        transcription_ms,
        cleanup_ms: cleanup_started.elapsed().as_millis() as u64,
        processing_ms: started.elapsed().as_millis() as u64,
        live: false,
        cleanup_skipped: skipped.is_some(),
    };
    cancel.check().map_err(|e| e.message)?;
    if text.is_empty() {
        fail(app, session, "Nothing recognized.".into(), false);
        return Err("Nothing recognized.".into());
    }
    if let Some(timing) = recognition_timing { timing.learn(&core.estimates, &cancel); }
    #[cfg(target_os = "linux")]
    if !keep_result(app, session, DictationResult {
        text: text.clone(),
        message: if copy_only {
            "Shell instruction ready for review and copy only. Amber will not paste or execute it."
        } else { "Ready to review and copy. The clipboard has not been changed." }.into(),
        engine: engine.clone(),
        exposure: exposure_label(failed.remote_speech, remote_cleanup).into(),
    }, copy_only) {
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
        let _ = (remote_cleanup, copy_only);
    }
    core.memory.remember(&core.settings(), &text, failed.program.as_deref().unwrap_or(""), &engine, false, failed.spoken_ms, started.elapsed().as_millis() as u64, Some(timings), Some(cleanup), execution);
    crate::diag::info(format!("recognized again: {} words", text::words(&text)));
    Ok(text)
}

fn instruction_copy_only(settings: &Settings, recognized: &str) -> bool {
    amber_core::instructions::match_start(&settings.instructions, recognized)
        .is_some_and(|selected| selected.instruction.output == amber_core::instructions::InstructionOutput::Shell)
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
                    // Match final recognition's language identity without ever including the
                    // dynamic text prompt in a persisted route or changing chunk learning.
                    let mut actual = settings.clone();
                    actual.language = options.language.clone().unwrap_or_else(|| "auto".into());
                    let result = transcriber(&app, &actual).and_then(|p| {
                        parts.engine = p.label.clone();
                        let r = if detect {
                            p.transcriber.transcribe_detecting_cancellable(&part.wav, &options, &cancel)
                        } else {
                            p.transcriber.transcribe_cancellable(&part.wav, &options, &cancel).map(|t| (t, None))
                        };
                        r.map(|(text, detected)| (text, detected, p.estimate_route)).map_err(|e| e.message)
                    });
                    match result {
                        Ok((text, detected, route)) => {
                            parts.record_route(route);
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
        self.handle.join().unwrap_or_else(|_| {
            let mut parts = Parts::default();
            parts.failed = true;
            parts
        })
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
    estimate_route: estimate::Route,
    metadata: amber_core::testing::Engine,
}

/// One in-flight measurement, discarded on cancellation. It is merged into moments only
/// after cleanup returns; no observation record is ever persisted.
struct RecognitionTiming {
    route: estimate::Route,
    audio_ms: u64,
    took_ms: u64,
}

impl RecognitionTiming {
    fn learn(self, estimates: &estimate::Estimates, cancel: &CancellationToken) {
        estimates.observe(&self.route, self.audio_ms, self.took_ms, !cancel.is_cancelled());
    }
}

type Recognition = (String, String, u128, Option<RecognitionTiming>);

/// A non-blocking initial guess. Readiness refines it using the actual Served metadata.
fn expected_speech_ms(core: &Core, settings: &Settings, audio_ms: u64) -> u64 {
    let route = match settings.engine {
        EngineKind::Server => estimate::server_speech(settings),
        EngineKind::Local => {
            let onnx = amber_core::imports::resolve(&core.paths.layout, &settings.local.model).is_ok_and(|m| m.is_onnx());
            Some(local_estimate(settings, &settings.local.model, onnx))
        }
    };
    route.map(|r| core.estimates.expected_ms(&r, audio_ms)).unwrap_or(0)
}

pub(crate) fn local_estimate(settings: &Settings, model: &str, onnx: bool) -> estimate::Route {
    let runtime = if onnx { "onnx-cpu" } else if cfg!(target_os = "linux") { "cpu" } else { &settings.local.runtime };
    let mut threads = if onnx { 0 } else if settings.local.threads == 0 { local::default_threads() } else { settings.local.threads };
    if !onnx && cfg!(target_os = "linux") {
        threads = threads.min(std::thread::available_parallelism().map(|n| n.get() as u32).unwrap_or(1)).max(1);
    }
    estimate::local_speech(settings, runtime, model, threads)
}

/// Recognizes [wav] with the engine the settings ask for. A local server that went away
/// mid-request gets one more try; a server that cannot be reached hands the recording to a
/// downloaded local model (if allowed) - never the other way round. Returns the successful
/// attempt's timing for learning after the cancellable pipeline finishes.
#[allow(clippy::too_many_arguments)]
fn recognize(
    app: &AppHandle,
    settings: &Settings,
    wav: &[u8],
    options: &TranscribeOptions,
    spoken_ms: u64,
    learn: bool,
    cancel: &CancellationToken,
    refine: Option<&dyn Fn(&estimate::Route)>,
) -> Result<Recognition, String> {
    recognize_with_report(app, settings, wav, options, spoken_ms, learn, cancel, None, refine)
}

pub(crate) fn recognize_for_test(app: &AppHandle, settings: &Settings, wav: &[u8], result: &mut amber_core::testing::Result, cancel: &CancellationToken) -> Result<String, String> {
    let started = Instant::now();
    let options = TranscribeOptions::new(&settings.language, &settings.vocabulary);
    let recognized = recognize_with_report(app, settings, wav, &options, result.prepared_audio_ms, false, cancel, Some(result), None);
    result.recognition_ms = Some(started.elapsed().as_millis() as u64);
    recognized.map(|(raw, _, _, _)| raw)
}

#[allow(clippy::too_many_arguments)]
fn recognize_with_report(
    app: &AppHandle, settings: &Settings, wav: &[u8], options: &TranscribeOptions,
    spoken_ms: u64, learn: bool, cancel: &CancellationToken, mut report: Option<&mut amber_core::testing::Result>,
    refine: Option<&dyn Fn(&estimate::Route)>,
) -> Result<Recognition, String> {
    let _quiet = report.is_some().then(crate::diag::silence);
    cancel.check().map_err(|e| e.message)?;
    // A live tail can use the language detected by earlier parts. Its dynamic prompt may
    // contain dictated text: never use that prompt in a persistent identity.
    let mut actual_settings = settings.clone();
    actual_settings.language = options.language.clone().unwrap_or_else(|| "auto".into());
    let settings = &actual_settings;
    let core = app.state::<Core>();
    let pick = || if report.is_some() && settings.engine == EngineKind::Local {
        local_snapshot(app, settings)
    } else { transcriber(app, settings) };
    let mut picked = pick()?;
    if let Some(refine) = refine { refine(&picked.estimate_route); }
    cancel.check().map_err(|e| e.message)?;
    let t0 = Instant::now();
    let (mut result, upload) = picked.transcriber.transcribe_with_upload(wav, options, cancel);
    let mut took = t0.elapsed().as_millis();
    if let Some(report) = report.as_deref_mut() {
        report.attempted(picked.metadata.clone(), took as u64, result.as_ref().err().map(|e| e.message.clone()));
        if let Some(attempt) = report.attempts.last_mut() { attempt.upload = upload; }
    }
    cancel.check().map_err(|e| e.message)?;

    if let Err(e) = &result {
        // Not when the engine was switched to a server meanwhile - that stopped the local one.
        if settings.engine == EngineKind::Local && e.kind == ErrorKind::Unreachable && (report.is_some() || core.settings().engine == EngineKind::Local) {
            // Restarted for another model, or crashed.
            crate::diag::warn("local engine went away during the request, trying again");
            picked = if report.is_some() { local_snapshot(app, settings)? } else { transcriber(app, settings)? };
            if let Some(refine) = refine { refine(&picked.estimate_route); }
            let t0 = Instant::now();
            result = picked.transcriber.transcribe_cancellable(wav, options, cancel);
            took = t0.elapsed().as_millis();
            if let Some(report) = report.as_deref_mut() {
                report.attempted(picked.metadata.clone(), took as u64, result.as_ref().err().map(|e| e.message.clone()));
            }
        } else if settings.engine == EngineKind::Server && e.kind.service_unavailable() && settings.server.local_fallback && local_ready(&core, settings) {
            crate::diag::warn(format!("server unavailable ({}), recognizing on this PC", e.detail));
            let server_error = e.message.clone();
            let local_result = (if report.is_some() { local_snapshot(app, settings) } else { local(app, settings) }).and_then(|p| {
                if let Some(refine) = refine { refine(&p.estimate_route); }
                let t = Instant::now();
                let r = p.transcriber.transcribe_cancellable(wav, options, cancel).map_err(|e| e.message);
                let took = t.elapsed().as_millis();
                if let Some(report) = report.as_deref_mut() {
                    report.attempted(p.metadata.clone(), took as u64, r.as_ref().err().cloned());
                }
                Ok((p, took, r))
            });
            match local_result {
                Ok((p, t, Ok(raw))) => {
                    if report.is_none() { crate::notify(app, "Amber", "The server could not be reached - recognized on this PC instead."); }
                    unload_after_fallback(app);
                    picked = p;
                    took = t;
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
    finish_recognition(picked, result, spoken_ms, took, learn && report.is_none(), cancel)
}

fn finish_recognition(picked: Picked, result: Result<String, amber_core::engine::SttError>, spoken_ms: u64, took: u128, learn: bool, cancel: &CancellationToken) -> Result<Recognition, String> {
    cancel.check().map_err(|e| e.message)?;
    match result {
        Ok(raw) => {
            let timing = (learn && !text::clean(&raw).is_empty()).then_some(RecognitionTiming {
                route: picked.estimate_route, audio_ms: spoken_ms, took_ms: took.min(u64::MAX as u128) as u64,
            });
            Ok((raw, picked.label, took, timing))
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
    let log = crate::diag::enabled();
    std::thread::spawn(move || {
        let _quiet = (!log).then(crate::diag::silence);
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
    if amber_core::imports::is_import_id(&s.local.model) {
        return amber_core::imports::resolve(layout, &s.local.model).is_ok_and(|m| {
            amber_core::imports::installed(layout, &m) && if m.is_onnx() { amber_core::onnx::runtime_installed(layout) } else { local::runtime(&s.local.runtime).is_some_and(|rt| layout.runtime_installed(rt)) }
        });
    }
    if let Some(model) = amber_core::onnx::model(&s.local.model) {
        return amber_core::onnx::runtime_installed(layout) && amber_core::onnx::installed(layout, model);
    }
    local::runtime(&s.local.runtime).is_some_and(|rt| layout.runtime_installed(rt))
        && local::model(&s.local.model).is_some_and(|m| layout.model_installed(m))
}

/// The local whisper server - with a short encoder context when the model it runs (which may
/// still be the previous one while the chosen one downloads) was tuned for it.
fn local(app: &AppHandle, settings: &Settings) -> Result<Picked, String> {
    local_model(app, None, settings)
}

/// The local engine with [model] instead of the configured one (the phone's choice).
fn local_model(app: &AppHandle, model: Option<&str>, settings: &Settings) -> Result<Picked, String> {
    let served = app.state::<Core>().whisper.ensure_model(app, model)?;
    Ok(picked_local(served, settings))
}

fn local_snapshot(app: &AppHandle, settings: &Settings) -> Result<Picked, String> {
    Ok(picked_local(app.state::<Core>().whisper.ensure_snapshot(app, &settings.local)?, settings))
}

fn picked_local(served: crate::whisper::Served, settings: &Settings) -> Picked {
    Picked {
        transcriber: served.transcriber,
        label: format!("local {}/{}", served.runtime, served.model),
        estimate_route: estimate::local_speech(settings, &served.runtime, &served.model, served.threads),
        metadata: amber_core::testing::Engine::local(&served.runtime, &served.model),
    }
}

/// The engine for a phone request naming [model]: a downloaded local model, the cloud model
/// set in the app, or else the PC's own. One local model at a time.
pub fn transcriber_for(app: &AppHandle, settings: &Settings, model: Option<&str>) -> Result<Picked, String> {
    let layout = &app.state::<Core>().paths.layout;
    if let Some(id) = model.filter(|id| id.starts_with("import-")) {
        let imported = amber_core::imports::resolve(layout, id).map_err(|e| e.to_string())?;
        if !amber_core::imports::installed(layout, &imported) { return Err("The requested imported model is missing or incomplete. Import it again.".into()); }
        return local_model(app, Some(id), settings);
    }
    if let Some(m) = model.and_then(amber_core::onnx::model).filter(|m| amber_core::onnx::installed(layout, m)) {
        return local_model(app, Some(m.id), settings);
    }
    if let Some(m) = model.and_then(local::model).filter(|m| layout.model_installed(m)) {
        if !(settings.engine == EngineKind::Local && settings.local.model == m.id) {
            return local_model(app, Some(m.id), settings);
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
        EngineKind::Local => local(app, settings),
        EngineKind::Server => {
            let target = amber_core::connections::resolve_speech(settings)?;
            let key = crate::connections::key_for(&core, &target)?;
            let (provider, base) = (target.provider, target.base_url);
            let transcriber = provider_transcriber(provider, &base, &key, &settings.server.model, settings.server.upload_format)
                .map_err(|e| e.message)?;
            Ok(Picked {
                transcriber: Box::new(transcriber),
                label: format!("{} {}", provider.id, settings.server.model),
                estimate_route: estimate::server_speech(settings).ok_or("Invalid recognition route.")?,
                metadata: amber_core::testing::Engine::server(provider.id, &base, &settings.server.model),
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

// Compile and exercise the helper-backed snapshot lock on Linux without starting a helper,
// touching a desktop, or changing the real platform backend selected by lib.rs.
#[cfg(all(test, target_os = "linux"))]
#[allow(dead_code)]
#[path = "whisper.rs"]
mod windows_whisper_tests;

#[cfg(test)]
mod testing_tests {
    use super::*;

    fn picked(settings: &Settings, runtime: &str, model: &str, threads: u32) -> Picked {
        picked_local(crate::whisper::Served {
            transcriber: Box::new(amber_core::engine::WhisperServerTranscriber::new("http://127.0.0.1:9", None)),
            runtime: runtime.into(), model: model.into(), threads,
        }, settings)
    }

    #[test]
    fn actual_onnx_and_effective_threads_share_prediction_and_observation_keys() {
        let mut settings = Settings::default();
        settings.local.runtime = "cuda".into();
        settings.local.threads = 32;
        let estimates = estimate::Estimates::in_memory();
        let cancel = CancellationToken::default();
        let route = local_estimate(&settings, "onnx-model", true);
        assert!(estimates.measured_ms(&route, 10_000).is_none());
        finish_recognition(picked(&settings, "onnx-cpu", "onnx-model", 0), Ok("Hello".into()), 10_000, 1_500, true, &cancel).unwrap().3.unwrap().learn(&estimates, &cancel);
        assert!(estimates.measured_ms(&route, 10_000).is_some());
        // The current selection must not replace the engine that actually served the request.
        let fallback = estimate::local_speech(&settings, "cpu", "previous-model", 2);
        finish_recognition(picked(&settings, "cpu", "previous-model", 2), Ok("Hello".into()), 10_000, 3_000, true, &cancel).unwrap().3.unwrap().learn(&estimates, &cancel);
        assert!(estimates.measured_ms(&fallback, 10_000).is_some());
        assert!(estimates.measured_ms(&local_estimate(&settings, "different-model", false), 10_000).is_none());
        assert!(estimates.measured_ms(&estimate::server_speech(&settings).unwrap(), 10_000).is_none(), "no failed server timing is attributed to the fallback");
    }

    #[test]
    fn testing_failed_cancelled_and_empty_recognition_never_learn() {
        let settings = Settings::default();
        let estimates = estimate::Estimates::in_memory();
        let route = estimate::local_speech(&settings, "cpu", "model", 4);
        for (learn, cancelled, result) in [
            (false, false, Ok("Testing text".into())),
            (true, true, Ok("Cancelled text".into())),
            (true, false, Err(amber_core::engine::SttError::new(ErrorKind::Server, "failed", "test failure"))),
            (true, false, Ok(String::new())),
        ] {
            let cancel = CancellationToken::default();
            if cancelled { cancel.cancel(); }
            if let Ok((_, _, _, Some(timing))) = finish_recognition(picked(&settings, "cpu", "model", 4), result, 10_000, 500, learn, &cancel) {
                timing.learn(&estimates, &cancel);
            }
            assert!(estimates.measured_ms(&route, 10_000).is_none());
        }
        let cancel = CancellationToken::default();
        let pending = finish_recognition(picked(&settings, "cpu", "model", 4), Ok("Recognized before cleanup cancellation".into()), 10_000, 500, true, &cancel).unwrap().3.unwrap();
        cancel.cancel();
        pending.learn(&estimates, &cancel);
        assert!(estimates.measured_ms(&route, 10_000).is_none());
    }

    #[test]
    fn shell_instructions_are_copy_only_even_when_terminal_auto_run_and_paste_are_enabled() {
        let mut settings = Settings::default();
        settings.terminal.enabled = true;
        settings.terminal.automatic = true;
        settings.terminal.run_commands = true;
        settings.insert.paste_on_completion = true;
        settings.instructions.push(amber_core::instructions::Instruction {
            id: "bash".into(), name: "Bash".into(), keyword: "bash".into(),
            prompt: amber_core::instructions::BASH_PROMPT.into(), enabled: true,
            output: amber_core::instructions::InstructionOutput::Shell,
        });
        for smart in [false, true] {
            settings.cleanup.smart = smart;
            assert!(instruction_copy_only(&settings, "BASH: list files"));
            assert!(instruction_copy_only(&settings, "bash:"), "an empty request cannot fall into terminal mode");
        }
        assert!(!instruction_copy_only(&settings, "please say bash"));
        settings.instructions[0].output = amber_core::instructions::InstructionOutput::Text;
        assert!(!instruction_copy_only(&settings, "bash list files"));
        assert!(amber_core::instructions::match_start(&settings.instructions, "bash list files").is_some(),
            "text transformations also bypass terminal auto mode");
    }

    #[test]
    fn unload_requires_an_idle_worker_and_excludes_new_recordings() {
        let dictation = Dictation::default();
        assert_eq!(dictation.when_idle(|| {
            assert!(dictation.inner.try_lock().is_err(), "new recordings must wait for unload");
            "unloaded"
        }).unwrap(), "unloaded");
        for phase in [Phase::Recording, Phase::Transcribing, Phase::Inserting] {
            dictation.inner.lock().unwrap().phase = phase;
            assert!(dictation.when_idle(|| panic!("must not unload while busy")).is_err());
        }
        dictation.inner.lock().unwrap().phase = Phase::Idle;
        dictation.reserve_test().unwrap();
        assert!(dictation.when_idle(|| panic!("must not unload during Testing")).is_err());
        dictation.release_test();
        assert!(dictation.when_idle(|| ()).is_ok());
    }

    #[test]
    fn testing_reserves_worker_without_target_or_delivery_and_cancels_pending_paste() {
        let dictation = Dictation::default();
        let pending = CancellationToken::default();
        dictation.inner.lock().unwrap().pending_paste = Some(pending.clone());
        assert!(dictation.reserve_test().is_ok());
        assert!(dictation.is_active());
        assert!(pending.is_cancelled());
        assert!(dictation.reserve_test().is_err());
        let inner = dictation.inner.lock().unwrap();
        assert!(inner.target.is_none());
        assert!(!inner.paste_on_completion);
        assert!(inner.result.is_none());
        assert_eq!(inner.phase, Phase::Idle);
        drop(inner);
        dictation.release_test();
        assert!(!dictation.is_active());
        assert!(dictation.reserve_test().is_ok());
    }

    #[test]
    fn testing_cannot_overlap_an_ordinary_recording_inference_or_insertion() {
        for phase in [Phase::Recording, Phase::Transcribing, Phase::Inserting] {
            let dictation = Dictation::default();
            dictation.inner.lock().unwrap().phase = phase;
            assert!(dictation.reserve_test().is_err());
            assert!(!dictation.processing.load(Ordering::Acquire));
            assert!(!dictation.testing.load(Ordering::Acquire));
        }
        let dictation = Dictation::default();
        dictation.processing.store(true, Ordering::Release);
        assert!(dictation.reserve_test().is_err());
        assert!(dictation.processing.load(Ordering::Acquire));
    }

    #[test]
    fn testing_settles_transient_phases_but_preserves_a_ready_result() {
        for phase in [Phase::Done, Phase::Error, Phase::Ready, Phase::Idle] {
            let dictation = Dictation::default();
            {
                let mut inner = dictation.inner.lock().unwrap();
                inner.phase = phase;
                inner.session = 7;
                inner.result = Some(DictationResult { text: "Retained ordinary result".into(), message: String::new(), engine: "local".into(), exposure: String::new() });
            }
            let transient = matches!(phase, Phase::Done | Phase::Error);
            assert_eq!(dictation.reserve_test().unwrap(), transient, "announce only a real transition");
            assert_eq!(dictation.phase(), if transient { Phase::Idle } else { phase });
            assert_eq!(dictation.inner.lock().unwrap().session, 8);
            assert_eq!(dictation.result().unwrap().text, "Retained ordinary result");
            dictation.release_test();
            assert_eq!(dictation.phase(), if transient { Phase::Idle } else { phase });
        }
    }
}
