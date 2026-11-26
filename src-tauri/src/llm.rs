//! The smart cleanup's local model: llama.cpp's `llama-server` with the chosen model on
//! a free port on 127.0.0.1, managed like the whisper server (see `process`). And the
//! choice between it and a server with an API key.

use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use amber_core::cleanup::{Ask, ChatCleaner};
use amber_core::engine::CancellationToken;
use amber_core::profiles::{self, CleanupChoice, Profile};
use amber_core::local;
use amber_core::settings::{EngineKind, Settings};
use tauri::{AppHandle, Emitter, Manager};

use crate::process::{self, Output, SupervisedChild};
use crate::state::Core;
use crate::whisper::Status;

#[derive(Clone, PartialEq, Eq, Debug)]
struct Key {
    runtime: String,
    model: String,
}

struct Running {
    child: SupervisedChild,
    port: u16,
    key: Key,
    /// Its LLAMA_API_KEY: without one, any web page could use the model by scanning loopback.
    api_key: String,
}

impl Running {
    fn endpoint(&self) -> Endpoint {
        Endpoint { url: format!("http://127.0.0.1:{}", self.port), key: self.api_key.clone(), runtime: self.key.runtime.clone(), model: self.key.model.clone() }
    }
}

/// Where the running model answers, and the key it wants.
pub struct Endpoint {
    pub url: String,
    pub key: String,
    pub runtime: String,
    pub model: String,
}

#[derive(Default)]
pub struct Llm {
    running: Mutex<Option<Running>>,
    output: Output,
    status: Mutex<Option<Status>>,
    /// A background load is under way.
    loading: AtomicBool,
    /// When the last start failed - a model that does not fit is not retried at every
    /// dictation.
    failed_at: Mutex<Option<Instant>>,
}

/// How long a failed start keeps background loads from trying again.
const RETRY_AFTER: Duration = Duration::from_secs(180);

/// Loading 2.4 GB from a slow disk onto a GPU, or into RAM for the CPU.
const START_TIMEOUT: Duration = Duration::from_secs(180);

/// Room for a dictation plus the prompt; one request at a time keeps memory low.
const CONTEXT: &str = "4096";

impl Llm {
    pub fn status(&self) -> Status {
        self.status.lock().unwrap().clone().unwrap_or(Status::of("stopped", None))
    }

    fn status_if(&self, own: bool, app: &AppHandle, status: Status) {
        if own {
            self.set_status(app, status);
        }
    }

    fn set_status(&self, app: &AppHandle, status: Status) {
        *self.status.lock().unwrap() = Some(status.clone());
        let _ = app.emit("llm-status", status);
    }

    /// The running server, if it is the configured one and done loading - never waits. If
    /// not, a load starts in the background (unless the last one failed a moment ago).
    fn ready_for(&self, app: &AppHandle, s: &Settings) -> Option<Endpoint> {
        if let Some(endpoint) = self.ready_snapshot(s) { return Some(endpoint); }
        self.warm(app, false);
        None
    }

    /// Non-blocking prediction must neither load a model nor assume it will become ready.
    fn ready_snapshot(&self, s: &Settings) -> Option<Endpoint> {
        let c = &s.cleanup;
        if let Ok(mut running) = self.running.try_lock() {
            if let Some(r) = running.as_mut() {
                if matches!(r.child.try_wait(), Ok(None)) && r.key.runtime == c.local_runtime && r.key.model == c.local_model {
                    return Some(r.endpoint());
                }
            }
        }
        None
    }

    /// Loads in the background. [force]: also right after a failure - the user asked.
    pub fn warm(&self, app: &AppHandle, force: bool) {
        let recently_failed = self.failed_at.lock().unwrap().is_some_and(|t| t.elapsed() < RETRY_AFTER);
        if (recently_failed && !force) || self.loading.swap(true, Ordering::SeqCst) {
            return;
        }
        let app = app.clone();
        let log = crate::diag::enabled();
        std::thread::spawn(move || {
            let _quiet = (!log).then(crate::diag::silence);
            let llm = &app.state::<Core>().llm;
            let _ = llm.ensure(&app);
            llm.loading.store(false, Ordering::SeqCst);
        });
    }

    /// A llama-server with the configured model, started first if needed - or stopped, if
    /// the settings no longer want it. Blocks while the model loads. Like the whisper server:
    /// settings are read under the lock, and a running server is only replaced once its
    /// successor is fully downloaded.
    pub fn ensure(&self, app: &AppHandle) -> Result<Endpoint, String> {
        self.ensure_model(app, None)
    }

    /// Like [ensure], with [model] instead of the configured one - what the phone chose, also
    /// while the PC itself cleans up in the cloud.
    pub fn ensure_model(&self, app: &AppHandle, model: Option<&str>) -> Result<Endpoint, String> {
        self.ensure_with(app, model, None)
    }

    fn ensure_with(&self, app: &AppHandle, model: Option<&str>, snapshot: Option<&Settings>) -> Result<Endpoint, String> {
        let mut running = self.running.lock().unwrap();
        self.ensure_locked(app, model, snapshot, &mut running)
    }

    /// Caller can retain the model lock through a test request, not just model setup.
    fn ensure_locked(&self, app: &AppHandle, model: Option<&str>, snapshot: Option<&Settings>, running: &mut Option<Running>) -> Result<Endpoint, String> {
        // The Cleanup page and the retry pause are about the PC's own model, not a phone's.
        let own = model.is_none();
        let core = app.state::<Core>();
        let layout = &core.paths.layout;
        let settings = snapshot.cloned().unwrap_or_else(|| core.settings());
        if model.is_none() && !wants_local(&settings) {
            // Switched off (or to a server) while this waited for the lock.
            if let Some(mut r) = running.take() {
                r.child.stop();
                crate::diag::info("cleanup model stopped");
            }
            self.status_if(own, app, Status::of("stopped", None));
            return Err("The smart cleanup on this PC is off.".into());
        }
        let c = settings.cleanup;
        let key = Key { runtime: c.local_runtime.clone(), model: model.unwrap_or(&c.local_model).to_string() };
        let rt = local::llm_runtime(&key.runtime).ok_or("Unknown runtime.")?;
        let model = local::llm_model(&key.model).ok_or("Unknown model.")?;
        let missing = if !layout.llm_runtime_installed(rt) {
            Some(format!("The cleanup engine ({}) is not installed yet - download it under Cleanup.", rt.label))
        } else if !layout.llm_model_installed(model) {
            Some(format!("The model {} is not downloaded yet - get it under Cleanup.", model.label))
        } else {
            None
        };

        if let Some(r) = running.as_mut() {
            let alive = matches!(r.child.try_wait(), Ok(None));
            if alive && (r.key == key || missing.is_some()) {
                return Ok(r.endpoint());
            }
            if !alive {
                crate::diag::warn(format!("cleanup model had exited: {}", process::last_lines(&self.output, 6)));
            }
            r.child.stop();
            *running = None;
        }
        if let Some(msg) = missing {
            self.status_if(own, app, Status::of("failed", Some(msg.clone())));
            return Err(msg);
        }

        self.status_if(own, app, Status::of("starting", Some(format!("Loading {}…", model.label))));
        let started = Instant::now();
        match self.spawn(layout, rt, model, &key) {
            Ok(r) => {
                let endpoint = r.endpoint();
                crate::diag::info(format!("cleanup model ready: {} on {} in {} ms", model.label, rt.label, started.elapsed().as_millis()));
                *running = Some(r);
                if own {
                    *self.failed_at.lock().unwrap() = None;
                }
                self.status_if(own, app, Status::of("ready", Some(format!("{} on {}", model.label, rt.label))));
                Ok(endpoint)
            }
            Err(e) => {
                crate::diag::error(format!("cleanup model failed to start: {e}"));
                if own {
                    *self.failed_at.lock().unwrap() = Some(Instant::now());
                }
                self.status_if(own, app, Status::of("failed", Some(e.clone())));
                Err(e)
            }
        }
    }

    fn spawn(&self, layout: &local::Layout, rt: &local::LlmRuntime, model: &local::LlmModel, key: &Key) -> Result<Running, String> {
        let port = process::free_port().map_err(|e| format!("No free port: {e}"))?;
        let api_key = amber_core::api::new_key();
        let cmd = llama_command(layout, rt, model, port, &api_key);
        let mut child = SupervisedChild::spawn(cmd, &self.output).map_err(|e| format!("The cleanup engine could not be started: {e}"))?;

        // llama-server opens its port at once and answers /health with 503 until the model
        // has loaded.
        let base = format!("http://127.0.0.1:{port}");
        let deadline = Instant::now() + START_TIMEOUT;
        loop {
            match child.try_wait() {
                Ok(Some(status)) => return Err(format!("The cleanup engine stopped while loading ({status}). {}", self.explain())),
                Err(e) => return Err(format!("The cleanup engine could not be monitored: {e}")),
                Ok(None) => {}
            }
            // A different listener or a loading/unauthorized response is not a ready model.
            if amber_core::engine::health_ok(&base) {
                return Ok(Running { child, port, key: key.clone(), api_key });
            }
            if Instant::now() > deadline {
                return Err("The cleanup engine took too long to load its model.".into());
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }

    fn explain(&self) -> String {
        let out = process::last_lines(&self.output, 6);
        let lower = out.to_lowercase();
        if lower.contains("out of memory") || lower.contains("failed to allocate") || lower.contains("cudamalloc") {
            if cfg!(target_os = "linux") {
                format!("Not enough memory - free some memory or leave smart cleanup off. ({out})")
            } else {
                format!("Not enough graphics memory - try the CPU, or a smaller speech model. ({out})")
            }
        } else {
            out
        }
    }

    pub fn stop(&self, app: &AppHandle) {
        if let Some(mut r) = self.running.lock().unwrap().take() {
            r.child.stop();
            crate::diag::info("cleanup model stopped");
        }
        self.set_status(app, Status::of("stopped", None));
    }

    /// Stops the cleanup model if it sits on the graphics card - so a speech model starting
    /// there finds the memory free. Whether it did; the caller warms it again afterwards.
    pub fn make_room_on_gpu(&self, app: &AppHandle) -> bool {
        let on_gpu = self.running.lock().unwrap().as_mut().is_some_and(|r| r.key.runtime != "cpu" && matches!(r.child.try_wait(), Ok(None)));
        if on_gpu {
            crate::diag::info("cleanup model makes room on the graphics card for the speech model");
            self.stop(app);
        }
        on_gpu
    }

    pub fn stop_runtime(&self, app: &AppHandle, runtime: &str) {
        let uses = self.running.lock().unwrap().as_ref().is_some_and(|r| r.key.runtime == runtime);
        if uses {
            self.stop(app);
        }
    }

    pub fn stop_model(&self, app: &AppHandle, model: &str) {
        let uses = self.running.lock().unwrap().as_ref().is_some_and(|r| r.key.model == model);
        if uses {
            self.stop(app);
        }
    }

}

fn llama_command(layout: &local::Layout, rt: &local::LlmRuntime, model: &local::LlmModel, port: u16, api_key: &str) -> Command {
    let mut cmd = Command::new(layout.llm_server_exe(rt));
    // Keep the Windows filename workaround. Linux's bundled libraries resolve via $ORIGIN;
    // no global library path changes are needed. Environment secrets remain visible to the
    // same OS user, but unlike argv they are not exposed to other users by default.
    let model_path = layout.llm_model_path(model);
    cmd.current_dir(model_path.parent().unwrap_or(&layout.root))
        .args(["--model", model.file, "--host", "127.0.0.1", "--port", &port.to_string()])
        .args(["--ctx-size", CONTEXT, "--parallel", "1"])
        .args(["--threads", &process::default_threads().to_string()])
        .env("LLAMA_API_KEY", api_key);
    if cfg!(target_os = "linux") || rt.id == "cpu" {
        cmd.args(["--n-gpu-layers", "0"]);
    }
    cmd
}

/// Whether the smart cleanup wants the local model loaded: switched on, or always on for
/// some program.
pub fn wants_local(s: &Settings) -> bool {
    let anywhere = s.cleanup.smart || s.profiles.iter().any(|p| p.cleanup == CleanupChoice::Always);
    anywhere && s.cleanup.engine == EngineKind::Local
}

/// The client for the smart cleanup the settings ask for - or why not. [wait]: for the local
/// model, wait while it loads (the settings page's preview) instead of skipping (a dictation).
pub fn cleaner(app: &AppHandle, s: &Settings, wait: bool) -> Result<ChatCleaner, String> {
    cleaner_measured(app, s, wait, None, None).map(|(cleaner, _)| cleaner)
}

fn cleaner_measured(app: &AppHandle, s: &Settings, wait: bool, profile: Option<&Profile>, recognized: Option<&str>) -> Result<(ChatCleaner, Option<amber_core::estimate::Route>), String> {
    let c = &s.cleanup;
    match c.engine {
        EngineKind::Local => {
            let llm = &app.state::<Core>().llm;
            let endpoint = if wait {
                llm.ensure(app)?
            } else {
                llm.ready_for(app, s).ok_or("the cleanup model is still loading")?
            };
            // llama-server serves the one model it loaded, whatever the request names.
            let route = amber_core::estimate::cleanup(s, profile, recognized, Some((&endpoint.runtime, &endpoint.model, process::default_threads())));
            Ok((ChatCleaner::new(&endpoint.url, &endpoint.key, &endpoint.model, None), route))
        }
        EngineKind::Server => {
            let target = amber_core::connections::resolve_cleanup(s)?;
            let key = crate::connections::key_for(&app.state::<Core>(), &target)?;
            if c.model.trim().is_empty() {
                return Err("No chat model chosen for the cleanup.".into());
            }
            target.cleaner(&key, &c.model).map(|cleaner| (cleaner, amber_core::estimate::cleanup(s, profile, recognized, None))).map_err(|e| e.message)
        }
    }
}

/// The cleaner for a chat request that names [model]: a downloaded local model, the cloud
/// model set in the app, or else the PC's own.
pub fn cleaner_for(app: &AppHandle, s: &Settings, model: Option<&str>) -> Result<ChatCleaner, String> {
    let layout = &app.state::<Core>().paths.layout;
    let c = &s.cleanup;
    // A downloaded local model - also the PC's own while its smart cleanup is off: the
    // phone asked for it, the list offered it.
    if let Some(m) = model.and_then(local::llm_model).filter(|m| layout.llm_model_installed(m)) {
        let own = c.engine == EngineKind::Local && c.local_model == m.id;
        let endpoint = app.state::<Core>().llm.ensure_model(app, (!own || !wants_local(s)).then_some(m.id))?;
        return Ok(ChatCleaner::new(&endpoint.url, &endpoint.key, m.id, None));
    }
    if model.is_some_and(|id| id == c.model) && c.engine != EngineKind::Server {
        let mut cloud = s.clone();
        cloud.cleanup.engine = EngineKind::Server;
        return cleaner(app, &cloud, true);
    }
    cleaner(app, s, true)
}

/// A terminal request turned into one shell command by the cleanup's model - the local one or
/// the provider's set under Cleanup, also while the smart cleanup itself is off.
pub fn command(app: &AppHandle, s: &Settings, spoken: &str, shell: amber_core::terminal::Shell, title: &str) -> Result<amber_core::terminal::Answer, String> {
    let c = &s.cleanup;
    let model = match c.engine {
        EngineKind::Local => c.local_model.as_str(),
        EngineKind::Server => c.model.as_str(),
    };
    let layout = &app.state::<Core>().paths.layout;
    if c.engine == EngineKind::Local && !local::llm_model(model).is_some_and(|m| layout.llm_model_installed(m)) {
        return Err("Terminal commands need a language model - download one under Cleanup, or choose a provider there.".into());
    }
    let cleaner = cleaner_for(app, s, Some(model)).map_err(|e| format!("Terminal commands need the cleanup's model: {e}"))?;
    let (system, user) = amber_core::terminal::request(spoken, shell, title);
    let request = serde_json::json!({
        "messages": [{ "role": "system", "content": system }, { "role": "user", "content": user }],
        "temperature": 0,
        // One line; a provider's model may reason first. Short for the local one: a small model
        // that repeats itself stops soon.
        "max_tokens": if c.engine == EngineKind::Local { 200 } else { 1024 },
    });
    let answer = cleaner.forward(&request).map_err(|e| format!("The cleanup's model did not answer: {}", e.message))?;
    let reply = answer.pointer("/choices/0/message/content").and_then(|v| v.as_str()).unwrap_or("");
    amber_core::terminal::command_from(reply, shell)
}

/// General cleanup for phone requests, without desktop keyword instructions. Snippets, when
/// permitted, win over the ordinary rules and meaning-preserving model cleanup.
pub fn tidy(app: &AppHandle, s: &Settings, recognized: &str, profile: Option<&Profile>, snippets: bool) -> (String, Option<String>) {
    // The phone has no desktop copy-only delivery guard. Its existing general cleanup must
    // not expose saved prompts or turn a remotely supplied keyword into executable output.
    let mut phone = s.clone();
    phone.instructions.clear();
    let result = tidy_cancellable(app, &phone, recognized, profile, snippets, &CancellationToken::default());
    (result.text, result.skipped)
}

pub struct Tidied {
    pub text: String,
    pub skipped: Option<String>,
    pub remote: bool,
    pub cleanup: amber_core::history::Cleanup,
    pub route: Option<amber_core::estimate::Route>,
}

/// The remote flag conservatively records a configured remote cleanup attempt, even on
/// failure/cancellation. It must not be inferred from whether the returned text changed.
pub fn tidy_cancellable(app: &AppHandle, s: &Settings, recognized: &str, profile: Option<&Profile>, snippets: bool, cancel: &CancellationToken) -> Tidied {
    tidy_learning(&app.state::<Core>().estimates, s, recognized, profile, snippets, cancel,
        || cleaner_measured(app, s, false, profile, Some(recognized)))
}

#[allow(clippy::too_many_arguments)]
fn tidy_learning(estimates: &amber_core::estimate::Estimates, s: &Settings, recognized: &str, profile: Option<&Profile>, snippets: bool, cancel: &CancellationToken,
    make_cleaner: impl FnOnce() -> Result<(ChatCleaner, Option<amber_core::estimate::Route>), String>) -> Tidied {
    let mut observation = None;
    let mut cleanup = amber_core::history::Cleanup::default();
    let result = tidy_recorded(s, recognized, profile, snippets, cancel, true, &mut cleanup, || {
        let (cleaner, route) = make_cleaner()?;
        observation = route.map(|route| (route, Instant::now()));
        Ok((cleaner, ()))
    });
    if let Some((route, started)) = &observation {
        estimates.observe(route, recognized.chars().count() as u64, started.elapsed().as_millis() as u64,
            result.1.is_none() && !result.0.is_empty() && !cancel.is_cancelled());
        if cleanup.outcome != amber_core::history::CleanupOutcome::Skipped {
            cleanup.engine = Some(route.label());
        }
    }
    let route = observation.filter(|_| cleanup.outcome == amber_core::history::CleanupOutcome::Success && !cancel.is_cancelled()).map(|(route, _)| route);
    Tidied { text: result.0, skipped: result.1, remote: result.2, cleanup, route }
}

/// Only budget model work that would be attempted now. A cold local model is skipped by
/// ordinary dictation; a later readiness change gets its own refinement before cleanup.
pub(crate) fn expected_ms(app: &AppHandle, s: &Settings, profile: Option<&Profile>, recognized: Option<&str>, chars: u64) -> u64 {
    if !amber_core::estimate::cleanup_wanted(s, profile, recognized, true) { return 0; }
    let core = app.state::<Core>();
    let endpoint = if s.cleanup.engine == EngineKind::Local { core.llm.ready_snapshot(s) } else { None };
    let local = endpoint.as_ref().map(|e| (e.runtime.as_str(), e.model.as_str(), process::default_threads()));
    amber_core::estimate::cleanup(s, profile, recognized, local).map(|route| core.estimates.expected_ms(&route, chars)).unwrap_or(0)
}

/// Tests use the same rules and smart cleanup, without app profiles or snippet expansion.
/// Unlike live dictation, wait for a cold local model and include its load in the timing.
pub fn tidy_for_test(app: &AppHandle, s: &Settings, result: &mut amber_core::testing::Result, cancel: &CancellationToken) {
    use amber_core::testing::Engine;
    let started = Instant::now();
    let recognized = amber_core::text::clean(&result.raw);
    let empty = by_rule(s, &recognized).is_empty();
    let core = app.state::<Core>();
    let (text, error, remote) = tidy_with_cleaner(s, &recognized, None, false, cancel, false, || {
        let c = &s.cleanup;
        let (cleaner, engine, lease) = match c.engine {
            EngineKind::Local => {
                let mut running = core.llm.running.lock().unwrap();
                cancel.check().map_err(|e| e.message)?;
                let endpoint = core.llm.ensure_locked(app, Some(&c.local_model), Some(s), &mut running)?;
                let engine = Engine::local(&endpoint.runtime, &endpoint.model);
                (ChatCleaner::new(&endpoint.url, &endpoint.key, &endpoint.model, None), engine, Some(running))
            }
            EngineKind::Server => {
                let cleaner = cleaner(app, s, true)?;
                let target = amber_core::connections::resolve_cleanup(s)?;
                (cleaner, Engine::server(target.provider.id, &target.base_url, &c.model), None)
            }
        };
        result.cleanup_engine = Some(engine);
        Ok((cleaner, lease))
    });
    result.cleaned(text, error, remote, started.elapsed().as_millis() as u64, empty);
}

#[allow(clippy::too_many_arguments)]
fn tidy_with_cleaner<L>(s: &Settings, recognized: &str, profile: Option<&Profile>, snippets: bool, cancel: &CancellationToken, log: bool, make_cleaner: impl FnOnce() -> Result<(ChatCleaner, L), String>) -> (String, Option<String>, bool) {
    tidy_recorded(s, recognized, profile, snippets, cancel, log, &mut amber_core::history::Cleanup::default(), make_cleaner)
}

#[allow(clippy::too_many_arguments)]
fn tidy_recorded<L>(s: &Settings, recognized: &str, profile: Option<&Profile>, snippets: bool, cancel: &CancellationToken, log: bool,
    cleanup: &mut amber_core::history::Cleanup, make_cleaner: impl FnOnce() -> Result<(ChatCleaner, L), String>) -> (String, Option<String>, bool) {
    use amber_core::history::{CleanupMethod, CleanupOutcome};
    let _quiet = (!log).then(crate::diag::silence);
    if let Err(e) = cancel.check() {
        return (recognized.to_string(), Some(e.message), false);
    }
    let instruction = amber_core::instructions::match_start(&s.instructions, recognized);
    // Explicit leading instructions win; otherwise a snippet stays exactly as saved.
    if let Some(snippet) = amber_core::snippets::find(&s.snippets, recognized).filter(|_| snippets && instruction.is_none()) {
        cleanup.method = CleanupMethod::Snippet;
        cleanup.outcome = CleanupOutcome::Success;
        // The trigger is what was just said: logged only where dictated text is.
        crate::diag::info(if s.log_text && !cfg!(target_os = "linux") { format!("snippet \"{}\"", snippet.trigger) } else { "snippet inserted".to_string() });
        return (snippet.text.clone(), None, false);
    }
    if let Some(selected) = &instruction {
        cleanup.method = CleanupMethod::Instruction;
        let error = if !s.cleanup.smart || !profiles::smart_cleanup(s.cleanup.smart, profile) {
            Some("Keyword instructions need smart cleanup enabled globally and for this program; kept the original.")
        } else if selected.body.is_empty() {
            Some("Say a request after the instruction keyword; kept the original.")
        } else { None };
        if let Some(error) = error { return (recognized.into(), Some(error.into()), false); }
    }
    // Do not turn a later keyword into the first word by dropping fillers, and do not apply
    // spoken punctuation to a request intended for an explicit transformation.
    let text = if instruction.is_some() { recognized.into() } else { by_rule(s, recognized) };
    if !profiles::smart_cleanup(s.cleanup.smart, profile) || text.is_empty() {
        cleanup.method = if s.cleanup.fillers || s.cleanup.spoken_punctuation || s.cleanup.german_commas { CleanupMethod::Rules } else { CleanupMethod::None };
        cleanup.outcome = CleanupOutcome::Success;
        return (text, None, false);
    }
    cleanup.method = if instruction.is_some() { CleanupMethod::Instruction } else { CleanupMethod::Smart };
    let started = Instant::now();
    // One language chosen for recognition: the cleanup is told it; several: it tells by itself.
    let languages = amber_core::engine::TranscribeOptions::new(&s.language, "");
    let ask = Ask {
        style: profile.map(|p| p.style).unwrap_or_default(),
        instruction: profile.map(|p| p.instruction.as_str()).unwrap_or(""),
        vocabulary: &s.vocabulary,
        language: languages.fixed(),
    };
    let mut remote_attempted = false;
    // Ordinary dictation skips loading models; tests can wait and retain a model lease.
    // Mark remote exposure before the call,
    // not after success: a failed or cancelled HTTP request may already have sent the text.
    let result = cancel.check().map_err(|e| e.message).and_then(|_| make_cleaner()).and_then(|(c, _lease)| {
        // A local test owns its model through the request and cancellation checks. Ordinary
        // cleanup passes (), preserving its existing non-pinning behavior.
        cancel.check().map_err(|e| e.message)?;
        remote_attempted = s.cleanup.engine == EngineKind::Server;
        cleanup.outcome = CleanupOutcome::Failed;
        let cleaned = match &instruction {
            Some(selected) => c.transform_cancellable(selected.body, selected.instruction, cancel),
            None => c.clean_cancellable(&text, &ask, cancel),
        }.map_err(|e| e.message)?;
        cancel.check().map_err(|e| e.message)?;
        Ok(cleaned)
    });
    match result {
        Ok(cleaned) => {
            cleanup.outcome = CleanupOutcome::Success;
            crate::diag::info(format!("cleanup: {} -> {} chars in {} ms", text.chars().count(), cleaned.chars().count(), started.elapsed().as_millis()));
            (cleaned, None, remote_attempted)
        }
        Err(e) => {
            crate::diag::warn(format!("cleanup skipped: {e}"));
            (text, Some(e), remote_attempted)
        }
    }
}

/// The steps that are rules - see `text::by_rule` for the order and why.
pub fn by_rule(s: &Settings, recognized: &str) -> String {
    amber_core::text::by_rule(
        recognized,
        amber_core::text::Rules {
            fillers: s.cleanup.fillers,
            spoken_punctuation: s.cleanup.spoken_punctuation,
            german_commas: s.cleanup.german_commas,
            language: (s.language != "auto").then_some(s.language.as_str()),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;

    #[test]
    fn helper_key_is_not_in_argv_and_cpu_is_explicit() {
        let layout = local::Layout { root: std::path::PathBuf::from("/test/amber") };
        let rt = local::llm_runtime("cpu").unwrap();
        let model = &local::LLM_MODELS[0];
        let cmd = llama_command(&layout, rt, model, 54321, "private-test-key");
        let args: Vec<_> = cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert!(!args.iter().any(|a| a.contains("private-test-key") || a == "--api-key"));
        for pair in [["--host", "127.0.0.1"], ["--port", "54321"], ["--parallel", "1"], ["--n-gpu-layers", "0"]] {
            assert!(args.windows(2).any(|w| w == pair));
        }
        assert!(cmd.get_envs().any(|(k, v)| k == "LLAMA_API_KEY" && v == Some(std::ffi::OsStr::new("private-test-key"))));
        assert!(!cmd.get_envs().any(|(k, _)| k == "LD_LIBRARY_PATH"));
        assert_eq!(cmd.get_program(), layout.llm_server_exe(rt).as_os_str());
        #[cfg(target_os = "linux")]
        assert_eq!(std::path::Path::new(cmd.get_program()).file_name().unwrap(), "llama-server");
    }

    fn settings(engine: EngineKind) -> Settings {
        let mut s = Settings::default();
        s.cleanup.engine = engine;
        s.cleanup.smart = true;
        s
    }

    fn instruction_settings() -> Settings {
        let mut s = settings(EngineKind::Server);
        s.instructions.push(amber_core::instructions::Instruction {
            id: "bash".into(), name: "Bash".into(), keyword: "bash".into(),
            prompt: amber_core::instructions::BASH_PROMPT.into(), enabled: true,
            output: amber_core::instructions::InstructionOutput::Shell,
        });
        s
    }

    #[test]
    fn keyword_instructions_require_global_and_program_consent_and_nonempty_payload() {
        let mut s = instruction_settings();
        let cancel = CancellationToken::default();
        let never = Profile { cleanup: CleanupChoice::Never, ..Profile::default() };
        let always = Profile { cleanup: CleanupChoice::Always, ..Profile::default() };
        for (smart, profile, text) in [(false, None, "Bash: list files"), (false, Some(&always), "Bash: list files"),
            (true, Some(&never), "Bash: list files"), (true, None, "Bash:  ")] {
            s.cleanup.smart = smart;
            let mut cleanup = amber_core::history::Cleanup::default();
            let (out, why, remote) = tidy_recorded::<()>(&s, text, profile, false, &cancel, false, &mut cleanup,
                || panic!("an inactive or empty instruction must not construct a model"));
            assert_eq!(out, text);
            assert!(why.is_some() && !remote);
            assert_eq!(cleanup, amber_core::history::Cleanup { method: amber_core::history::CleanupMethod::Instruction,
                outcome: amber_core::history::CleanupOutcome::Skipped, engine: None });
        }
    }

    #[test]
    fn one_instruction_transforms_the_whole_utterance_and_keeps_internal_newlines() {
        let mut s = instruction_settings();
        let spoken = "Bash: list all files then print the working directory";
        s.snippets.push(amber_core::snippets::Snippet { trigger: spoken.into(), text: "An overlapping snippet".into() });
        let body = serde_json::json!({"choices": [{"message": {"content": "```bash\nls -la\npwd\n```\n"}, "finish_reason": "stop"}]}).to_string();
        let (cleaner, server) = reply(200, body, None);
        let result = tidy_with_cleaner(&s, spoken, None, true, &CancellationToken::default(), false, || Ok((cleaner, ())));
        server.join().unwrap();
        assert_eq!(result, ("ls -la\npwd".into(), None, true));
    }

    #[test]
    fn an_instruction_failure_or_cancellation_keeps_the_trigger_and_original_request() {
        let s = instruction_settings();
        let spoken = "Bash: uh list all files comma then print the working directory";
        for cancelled in [false, true] {
            let cancel = CancellationToken::default();
            let (cleaner, server) = reply(503, r#"{"error":"private spoken request"}"#.into(), cancelled.then(|| cancel.clone()));
            let (out, why, remote) = tidy_with_cleaner(&s, spoken, None, false, &cancel, false, || Ok((cleaner, ())));
            server.join().unwrap();
            assert_eq!(out, spoken, "no filler/punctuation changes and no lost keyword on failure");
            assert!(why.is_some() && remote);
            assert!(!why.unwrap().contains("private spoken request"));
        }
        let cancel = CancellationToken::default();
        cancel.cancel();
        let (out, why, remote) = tidy_with_cleaner::<()>(&s, spoken, None, false, &cancel, false, || panic!("pre-cancelled"));
        assert_eq!(out, spoken);
        assert!(why.is_some() && !remote);
        let missing = tidy_with_cleaner::<()>(&s, spoken, None, false, &CancellationToken::default(), false,
            || Err("The selected cleanup model is not ready.".into()));
        assert_eq!(missing, (spoken.into(), Some("The selected cleanup model is not ready.".into()), false));
    }

    #[test]
    fn filler_removal_cannot_reveal_a_trigger_and_nonmatches_keep_cleanup_checks() {
        let s = instruction_settings();
        for spoken in ["uh bash list all files", "please use bash to list all files", "bashful people list files"] {
            let body = serde_json::json!({"choices": [{"message": {"content": "ls -la"}, "finish_reason": "stop"}]}).to_string();
            let (cleaner, server) = reply(200, body, None);
            let (out, why, remote) = tidy_with_cleaner(&s, spoken, None, false, &CancellationToken::default(), false, || Ok((cleaner, ())));
            server.join().unwrap();
            assert_eq!(out, by_rule(&s, spoken));
            assert!(why.is_some() && remote, "ordinary cleanup must reject the command rewrite");
        }
    }

    #[test]
    fn skipped_cleanup_and_precancellation_do_not_expose_text() {
        let mut s = settings(EngineKind::Server);
        let cancel = CancellationToken::default();
        let no_client = || -> Result<(ChatCleaner, ()), String> { panic!("must not construct a cleaner") };
        s.cleanup.smart = false;
        let (_, why, remote) = tidy_with_cleaner(&s, "Hello.", None, true, &cancel, true, no_client);
        assert!(why.is_none() && !remote);
        s.cleanup.smart = true;
        let (_, why, remote) = tidy_with_cleaner(&s, "", None, true, &cancel, true, no_client);
        assert!(why.is_none() && !remote);
        s.snippets.push(amber_core::snippets::Snippet { trigger: "signature".into(), text: "Saved text".into() });
        let result = tidy_with_cleaner(&s, "Signature.", None, true, &cancel, true, no_client);
        assert_eq!(result, ("Saved text".into(), None, false));
        cancel.cancel();
        let (text, why, remote) = tidy_with_cleaner(&s, "Signature.", None, true, &cancel, true, no_client);
        assert_eq!(text, "Signature.");
        assert!(why.is_some() && !remote);
    }

    #[test]
    fn missing_configuration_keeps_rules_without_remote_exposure() {
        let s = settings(EngineKind::Server);
        let text = "Please send the report tomorrow.";
        let result = tidy_with_cleaner::<()>(&s, text, None, false, &CancellationToken::default(), true, || Err("No chat model chosen.".into()));
        assert_eq!(result, (by_rule(&s, text), Some("No chat model chosen.".into()), false));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_never_logs_snippet_text_even_with_a_stale_log_text_setting() {
        let mut s = settings(EngineKind::Server);
        s.log_text = true;
        let trigger = "private snippet trigger test";
        let body = "private snippet body test";
        s.snippets.push(amber_core::snippets::Snippet { trigger: trigger.into(), text: body.into() });
        let result = tidy_with_cleaner::<()>(&s, trigger, None, true, &CancellationToken::default(), true, || panic!("snippet must not use a model"));
        assert_eq!(result, (body.into(), None, false));
        assert!(!crate::diag::entries().iter().any(|entry| entry.message.contains(trigger) || entry.message.contains(body)));
    }

    /// A single loopback HTTP response, never a paid provider or a model download.
    fn reply(status: u16, body: String, cancel: Option<CancellationToken>) -> (ChatCleaner, std::thread::JoinHandle<()>) {
        reply_with(status, body, move || { if let Some(cancel) = cancel { cancel.cancel(); } })
    }

    fn reply_with(status: u16, body: String, before_reply: impl FnOnce() + Send + 'static) -> (ChatCleaner, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let until = Instant::now() + Duration::from_secs(5);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < until => std::thread::sleep(Duration::from_millis(5)),
                    Err(e) => panic!("cleanup request did not arrive: {e}"),
                }
            };
            stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut reader = BufReader::new(&mut stream);
            let mut length = 0;
            loop {
                let mut line = String::new();
                assert!(reader.read_line(&mut line).unwrap() > 0);
                if line == "\r\n" { break; }
                if let Some(n) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = n.trim().parse::<usize>().unwrap();
                }
            }
            assert!(length > 0 && length < 100_000);
            reader.read_exact(&mut vec![0; length]).unwrap();
            drop(reader);
            before_reply();
            let _ = write!(stream, "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
        });
        (ChatCleaner::new(&base, "test-key", "test-model", None), server)
    }

    #[test]
    fn failed_remote_cleanup_still_records_exposure_and_keeps_text() {
        let s = settings(EngineKind::Server);
        let text = "Please send the report tomorrow.";
        let (cleaner, server) = reply(503, r#"{"error":{"message":"private echoed transcript"}}"#.into(), None);
        let (out, why, remote) = tidy_with_cleaner(&s, text, None, false, &CancellationToken::default(), true, || Ok((cleaner, ())));
        server.join().unwrap();
        assert_eq!(out, by_rule(&s, text));
        assert!(why.is_some() && remote);
        assert!(!why.unwrap().contains("private echoed transcript"));
    }

    #[test]
    fn completed_cleanup_exposure_depends_on_configuration_not_text_changes() {
        for engine in [EngineKind::Local, EngineKind::Server] {
            let s = settings(engine);
            let text = "Please send the report tomorrow.";
            let body = serde_json::json!({"choices": [{"message": {"content": text}, "finish_reason": "stop"}]}).to_string();
            let (cleaner, server) = reply(200, body, None);
            let result = tidy_with_cleaner(&s, text, None, false, &CancellationToken::default(), true, || Ok((cleaner, ())));
            server.join().unwrap();
            assert_eq!(result, (text.into(), None, engine == EngineKind::Server));
        }
    }

    #[test]
    fn cleanup_learning_only_counts_successful_actual_model_work() {
        use amber_core::estimate::{self, Estimates};
        use amber_core::history::{CleanupMethod, CleanupOutcome};
        let s = settings(EngineKind::Server);
        let spoken = "Please send the report tomorrow.";
        let route = estimate::cleanup(&s, None, Some(spoken), None).unwrap();
        for (status, cancelled) in [(503, false), (200, true), (200, false)] {
            let estimates = Estimates::in_memory();
            let cancel = CancellationToken::default();
            let body = serde_json::json!({"choices": [{"message": {"content": spoken}, "finish_reason": "stop"}]}).to_string();
            let flag = cancel.clone();
            let (cleaner, server) = reply_with(status, body, move || {
                std::thread::sleep(Duration::from_millis(2));
                if cancelled { flag.cancel(); }
            });
            let result = tidy_learning(&estimates, &s, spoken, None, true, &cancel, || Ok((cleaner, Some(route.clone()))));
            server.join().unwrap();
            assert_eq!(estimates.measured_ms(&route, 100).is_some(), status == 200 && !cancelled);
            assert_eq!(result.cleanup.method, CleanupMethod::Smart);
            assert_eq!(result.cleanup.engine, Some(route.label()));
            assert_eq!(result.cleanup.outcome, if status == 200 && !cancelled { CleanupOutcome::Success } else { CleanupOutcome::Failed });
            assert_eq!(result.route.is_some(), status == 200 && !cancelled);
        }
        let estimates = Estimates::in_memory();
        let mut disabled = s.clone();
        disabled.cleanup.smart = false;
        let result = tidy_learning(&estimates, &disabled, spoken, None, true, &CancellationToken::default(), || panic!("rules must not learn"));
        assert_eq!(result.cleanup.method, CleanupMethod::Rules);
        assert_eq!(result.cleanup.outcome, CleanupOutcome::Success);
        assert!(result.cleanup.engine.is_none() && result.route.is_none());
        disabled.cleanup.fillers = false;
        disabled.cleanup.spoken_punctuation = false;
        disabled.cleanup.german_commas = false;
        let result = tidy_learning(&estimates, &disabled, spoken, None, true, &CancellationToken::default(), || panic!("disabled cleanup must not learn"));
        assert_eq!(result.cleanup.method, CleanupMethod::None);
        assert_eq!(result.cleanup.outcome, CleanupOutcome::Success);
        let never = Profile { cleanup: CleanupChoice::Never, ..Profile::default() };
        let result = tidy_learning(&estimates, &s, spoken, Some(&never), true, &CancellationToken::default(), || panic!("profile disabled model cleanup"));
        assert_eq!(result.cleanup.method, CleanupMethod::Rules);
        assert!(result.cleanup.engine.is_none());
        let _ = tidy_learning(&estimates, &s, "", None, true, &CancellationToken::default(), || panic!("empty text must not learn"));
        let result = tidy_learning(&estimates, &s, spoken, None, true, &CancellationToken::default(), || Err("model still loading".into()));
        assert_eq!(result.cleanup.method, CleanupMethod::Smart);
        assert_eq!(result.cleanup.outcome, CleanupOutcome::Skipped);
        assert!(result.cleanup.engine.is_none() && result.route.is_none());
        let mut snippet = s.clone();
        snippet.snippets.push(amber_core::snippets::Snippet { trigger: spoken.into(), text: "Saved text".into() });
        let result = tidy_learning(&estimates, &snippet, spoken, None, true, &CancellationToken::default(), || panic!("snippets must not learn"));
        assert_eq!(result.cleanup.method, CleanupMethod::Snippet);
        assert_eq!(result.cleanup.outcome, CleanupOutcome::Success);
        assert!(result.cleanup.engine.is_none() && result.route.is_none());
        let cancel = CancellationToken::default();
        cancel.cancel();
        let _ = tidy_learning(&estimates, &s, spoken, None, true, &cancel, || panic!("cancelled requests must not learn"));
        assert!(estimates.measured_ms(&route, 100).is_none());
    }

    #[test]
    fn cleanup_metadata_uses_actual_model_route_for_profiles_and_instructions() {
        use amber_core::history::{CleanupMethod, CleanupOutcome};
        let estimates = amber_core::estimate::Estimates::in_memory();
        let mut s = instruction_settings();
        s.cleanup.engine = EngineKind::Local;
        let profile = Profile { cleanup: CleanupChoice::Always, instruction: "private profile".into(), ..Profile::default() };
        for instruction in [false, true] {
            s.cleanup.smart = instruction;
            let spoken = if instruction { "Bash: list files" } else { "Please send the report tomorrow." };
            let answer = if instruction { "ls" } else { spoken };
            // The factory's served identity wins over the configured selection.
            let route = amber_core::estimate::cleanup(&s, Some(&profile), Some(spoken), Some(("cpu", "actual-loaded-model", 3))).unwrap();
            let body = serde_json::json!({"choices": [{"message": {"content": answer}, "finish_reason": "stop"}]}).to_string();
            let (cleaner, server) = reply(200, body, None);
            let result = tidy_learning(&estimates, &s, spoken, Some(&profile), true, &CancellationToken::default(), || Ok((cleaner, Some(route.clone()))));
            server.join().unwrap();
            assert_eq!(result.text, answer);
            assert_eq!(result.cleanup.method, if instruction { CleanupMethod::Instruction } else { CleanupMethod::Smart });
            assert_eq!(result.cleanup.outcome, CleanupOutcome::Success);
            assert_eq!(result.cleanup.engine, Some(route.label()));
            assert_eq!(result.route, Some(route));
            assert!(result.cleanup.engine.unwrap().contains("actual-loaded-model"));
        }
    }

    #[test]
    fn cancelled_remote_cleanup_keeps_text_and_exposure() {
        let s = settings(EngineKind::Server);
        let text = "Please send the report tomorrow.";
        let cancel = CancellationToken::default();
        let (cleaner, server) = reply(200, "{}".into(), Some(cancel.clone()));
        let (out, why, remote) = tidy_with_cleaner(&s, text, None, false, &cancel, true, || Ok((cleaner, ())));
        server.join().unwrap();
        assert_eq!(out, by_rule(&s, text));
        assert!(why.is_some() && remote && cancel.is_cancelled());
    }

    #[test]
    fn local_test_lease_pins_model_until_request_success_failure_or_cancellation() {
        for (status, cancelled) in [(200, false), (503, false), (200, true)] {
            let llm = std::sync::Arc::new(Llm::default());
            let during_request = llm.clone();
            let (send, recv) = std::sync::mpsc::channel();
            let cancel = CancellationToken::default();
            let flag = cancel.clone();
            let text = "Please send the report tomorrow.";
            let body = serde_json::json!({"choices": [{"message": {"content": text}, "finish_reason": "stop"}]}).to_string();
            let (cleaner, server) = reply_with(status, body, move || {
                // Settings replacement and stop both need this exact model lock.
                send.send(during_request.running.try_lock().is_err()).unwrap();
                if cancelled { flag.cancel(); }
            });
            let (out, why, remote) = tidy_with_cleaner(&settings(EngineKind::Local), text, None, false, &cancel, false, || {
                let lease = llm.running.lock().unwrap();
                Ok((cleaner, lease))
            });
            server.join().unwrap();
            assert!(recv.recv().unwrap(), "model must stay pinned while HTTP is in flight");
            assert!(llm.running.try_lock().is_ok(), "every exit must release the model lease");
            assert_eq!(out, text);
            assert_eq!(why.is_some(), status != 200 || cancelled);
            assert!(!remote);
        }
    }

    #[test]
    fn test_cleanup_suppresses_counts_failures_and_factory_logs_but_production_still_logs() {
        let mut s = settings(EngineKind::Server);
        s.log_text = true;
        let text = format!("{}.", "a".repeat(731));
        let body = serde_json::json!({"choices": [{"message": {"content": text}, "finish_reason": "stop"}]}).to_string();
        let (cleaner, server) = reply(200, body, None);
        let result = tidy_with_cleaner(&s, &text, None, false, &CancellationToken::default(), false, || {
            crate::diag::info("private-test-cleaner-factory");
            Ok((cleaner, ()))
        });
        server.join().unwrap();
        assert!(result.1.is_none());
        let reason = "private-test-cleanup-failure";
        let result = tidy_with_cleaner::<()>(&s, "Hello.", None, false, &CancellationToken::default(), false, || Err(reason.into()));
        assert_eq!(result.1.as_deref(), Some(reason));
        let entries = crate::diag::entries();
        assert!(!entries.iter().any(|e| e.message.contains("private-test-cleaner-factory") || e.message.contains(reason) || e.message.contains("cleanup: 732 ->")));
        assert!(!entries.iter().any(|e| e.message.contains(&text)));
        let production = "ordinary-cleanup-failure-log-test";
        let _ = tidy_with_cleaner::<()>(&s, "Hello.", None, false, &CancellationToken::default(), true, || Err(production.into()));
        assert!(crate::diag::entries().iter().any(|e| e.message.contains(production)));
    }
}
