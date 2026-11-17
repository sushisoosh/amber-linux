//! Native Linux desktop integration. Portal permission is not inferred from a desktop name;
//! XWayland is never used to bypass Wayland input restrictions.

use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use amber_core::gesture::{Gesture, Mode};
use amber_core::settings::{HotkeySettings, Settings};
use ashpd::desktop::global_shortcuts::{Activated, Deactivated, GlobalShortcuts, NewShortcut, ShortcutsChanged};
use ashpd::zbus::{self, zvariant::OwnedValue};
use futures_util::StreamExt;
use gtk::prelude::*;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_global_shortcut::{Modifiers, Shortcut, ShortcutState};
use tokio::sync::watch;

use crate::state::Core;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopStatus {
    pub session: String,
    pub shortcuts: String,
    pub insertion: String,
    pub clipboard: String,
    pub lifecycle: String,
    pub shortcuts_ready: bool,
    pub release_observed: bool,
    pub bindings: Vec<Binding>,
}

#[derive(Clone, Serialize)]
pub struct Binding {
    pub id: String,
    pub trigger: String,
}

struct Desktop {
    app: AppHandle,
    status: Mutex<DesktopStatus>,
    shortcuts: watch::Sender<Option<HotkeySettings>>,
    lifecycle: Mutex<Lifecycle>,
    shutdown: watch::Sender<bool>,
}

#[derive(Default)]
struct Lifecycle {
    checking: bool,
    locked: bool,
    sleeping: bool,
}

static DESKTOP: OnceLock<Desktop> = OnceLock::new();
const FALLBACK: &str = "Use the recorder controls or configure a desktop shortcut to amber-dictation --toggle (toggle only).";

/// Also usable before GTK initialization to decide whether to load the X11-only plugin.
pub fn use_x11_shortcuts() -> bool {
    if let Some(desktop) = DESKTOP.get() {
        return desktop.status.lock().unwrap().session == "x11";
    }
    std::env::var_os("WAYLAND_DISPLAY").is_none() && std::env::var("XDG_SESSION_TYPE").as_deref() != Ok("wayland") && std::env::var_os("DISPLAY").is_some()
}

/// Call on the GUI thread, after Core is managed and before registering shortcuts.
pub fn init(app: &AppHandle) {
    if DESKTOP.get().is_some() {
        return;
    }
    let display = gtk::gdk::Display::default();
    let kind = display.as_ref().map(|d| d.type_().name()).unwrap_or("");
    let session = if kind.contains("Wayland") || std::env::var_os("WAYLAND_DISPLAY").is_some() || std::env::var("XDG_SESSION_TYPE").as_deref() == Ok("wayland")
    {
        "wayland"
    } else if kind.contains("X11") {
        "x11"
    } else {
        "unknown"
    };
    let (tx, rx) = watch::channel(None);
    let (shutdown, shutdown_rx) = watch::channel(false);
    let status = DesktopStatus {
        session: session.into(),
        shortcuts: "Not registered.".into(),
        insertion: if session == "x11" {
            "Automatic insertion is disabled: editable-field identity is unavailable. Explicit XTest paste is restricted to supported native text editors; all other targets require Copy/manual paste. Submission is not confirmation."
        } else {
            "Copy/manual paste only. Wayland target identity and permission-based RemoteDesktop/Clipboard insertion are not implemented; no automatic paste is attempted."
        }.into(),
        lifecycle: "Checking current-session lock and suspend monitoring; recording is temporarily blocked.".into(),
        clipboard: if display.is_some() {
            "Explicit Copy replaces the clipboard. Previous text, images and files are not restored; clipboard managers may retain copied text. Keep Amber running until paste completes."
        } else {
            "Unavailable: no GTK desktop display."
        }.into(),
        shortcuts_ready: false,
        release_observed: false,
        bindings: Vec::new(),
    };
    if DESKTOP
        .set(Desktop {
            app: app.clone(),
            status: Mutex::new(status),
            shortcuts: tx,
            lifecycle: Mutex::new(Lifecycle { checking: true, ..Lifecycle::default() }),
            shutdown,
        })
        .is_ok()
    {
        tauri::async_runtime::spawn(shortcut_worker(app.clone(), rx));
        tauri::async_runtime::spawn(lifecycle_worker(app.clone(), shutdown_rx));
    }
}

pub fn status() -> DesktopStatus {
    DESKTOP.get().map(|d| d.status.lock().unwrap().clone()).unwrap_or(DesktopStatus {
        session: "unknown".into(),
        shortcuts: format!("Desktop integration is not initialized. {FALLBACK}"),
        insertion: "Copy/manual paste only; automatic insertion unavailable.".into(),
        clipboard: "Desktop integration is not initialized.".into(),
        lifecycle: "Unavailable: desktop lifecycle monitoring is not initialized.".into(),
        shortcuts_ready: false,
        release_observed: false,
        bindings: Vec::new(),
    })
}

fn update_status(app: &AppHandle, update: impl FnOnce(&mut DesktopStatus)) {
    if let Some(desktop) = DESKTOP.get() {
        let snapshot = {
            let mut status = desktop.status.lock().unwrap();
            update(&mut status);
            status.clone()
        };
        let _ = app.emit("linux-desktop-status", snapshot);
    }
}

pub fn note_x11_shortcuts(app: &AppHandle, settings: &Settings, problems: &[String]) {
    update_status(app, |s| {
        s.shortcuts_ready = problems.is_empty() && !settings.hotkey.dictate.trim().is_empty();
        s.shortcuts = if !problems.is_empty() {
            format!("X11 registration failed: {}. {FALLBACK}", problems.join("; "))
        } else if s.shortcuts_ready {
            "X11 shortcuts registered; press and release events use the selected gesture mode.".into()
        } else {
            format!("No dictation shortcut configured. {FALLBACK}")
        };
    });
}

pub fn register_shortcuts(app: &AppHandle, settings: &Settings) -> Result<(), String> {
    let desktop = DESKTOP.get().ok_or("Linux desktop integration is not initialized")?;
    if desktop.status.lock().unwrap().session != "wayland" {
        let message = format!("No supported desktop session. {FALLBACK}");
        update_status(app, |s| s.shortcuts = message.clone());
        return Err(message);
    }
    if settings.hotkey.dictate.trim().is_empty() && settings.hotkey.toggle_bubble.trim().is_empty() {
        desktop.shortcuts.send_replace(None);
        update_status(app, |s| {
            s.shortcuts_ready = false;
            s.release_observed = false;
            s.bindings.clear();
            s.shortcuts = format!("Portal shortcuts disabled. {FALLBACK}");
        });
        return Ok(());
    }
    for key in [&settings.hotkey.dictate, &settings.hotkey.toggle_bubble] {
        if !key.trim().is_empty() && key.parse::<Shortcut>().is_err() {
            desktop.shortcuts.send_replace(None);
            let message = format!("{key} is not a valid hotkey. {FALLBACK}");
            update_status(app, |s| {
                s.shortcuts_ready = false;
                s.shortcuts = message.clone();
            });
            return Err(message);
        }
    }
    // Unrelated settings saves must not reopen a permission dialog or tear down a pending one.
    if desktop.shortcuts.borrow().as_ref() == Some(&settings.hotkey) {
        return Ok(());
    }
    update_status(app, |s| {
        s.shortcuts_ready = false;
        s.release_observed = false;
        s.bindings.clear();
        s.shortcuts = format!("Requesting GlobalShortcuts permission. The desktop controls the actual bindings. {FALLBACK}");
    });
    desktop.shortcuts.send_replace(Some(settings.hotkey.clone()));
    // The permission dialog is asynchronous. Read status() or linux-desktop-status, not this
    // return value, to determine whether shortcuts are actually usable.
    Ok(())
}

pub fn shutdown() {
    if let Some(desktop) = DESKTOP.get() {
        desktop.shortcuts.send_replace(None);
        desktop.shutdown.send_replace(true);
    }
}

/// Unknown/unavailable monitoring is reported in status, not presented as a confirmed unlock.
/// A known lock/suspend remains blocking if its watcher fails. Initial discovery is bounded.
pub fn session_is_blocked() -> bool {
    DESKTOP.get().is_some_and(|desktop| {
        let lifecycle = desktop.lifecycle.lock().unwrap();
        lifecycle.checking || lifecycle.locked || lifecycle.sleeping
    })
}

fn lifecycle_status(app: &AppHandle, lock: &str, sleep: &str) {
    let Some(desktop) = DESKTOP.get() else { return };
    let state = {
        let lifecycle = desktop.lifecycle.lock().unwrap();
        if lifecycle.checking {
            "Checking; recording blocked."
        } else if lifecycle.locked || lifecycle.sleeping {
            "Blocked by session lock or suspend."
        } else {
            "No lock or suspend is currently known."
        }
    };
    update_status(app, |s| s.lifecycle = format!("Lock: {lock} Suspend: {sleep} {state}"));
}

async fn lifecycle_call<T>(future: impl std::future::Future<Output = zbus::Result<T>>) -> Result<T, String> {
    tokio::time::timeout(Duration::from_secs(5), future).await.map_err(|_| "D-Bus request timed out".to_string())?.map_err(|e| e.to_string())
}

async fn lifecycle_worker(app: AppHandle, mut shutdown: watch::Receiver<bool>) {
    let result = tokio::select! {
        biased;
        _ = shutdown.wait_for(|stop| *stop) => return,
        result = monitor_lifecycle(&app) => result,
    };
    if let Err(error) = result {
        if let Some(desktop) = DESKTOP.get() {
            desktop.lifecycle.lock().unwrap().checking = false;
        }
        // Loss of monitoring revokes in-flight work too, but never invents an unlock.
        crate::dictation::cancel(&app);
        lifecycle_status(&app, &format!("Unavailable ({error})."), "Unavailable; restart Amber after the service is restored.");
    }
}

const LOGIN1: &str = "org.freedesktop.login1";
const LOGIN_MANAGER: &str = "/org/freedesktop/login1";
const LOGIN_SESSION: &str = "org.freedesktop.login1.Session";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";

async fn locked_hint(properties: &zbus::Proxy<'_>) -> Result<(bool, zbus::message::Sequence), String> {
    let reply = lifecycle_call(properties.call_method("Get", &(LOGIN_SESSION, "LockedHint"))).await?;
    let value: OwnedValue = reply.body().deserialize().map_err(|e| e.to_string())?;
    Ok((bool::try_from(value).map_err(|e| format!("Invalid LockedHint: {e}"))?, reply.recv_position()))
}

fn session_signal_rule(owner: &str, path: &str) -> Result<zbus::MatchRule<'static>, String> {
    Ok(zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender(owner)
        .map_err(|e| e.to_string())?
        .path(path)
        .map_err(|e| e.to_string())?
        .build()
        .into_owned())
}

#[derive(Debug, PartialEq, Eq)]
enum LockSignal {
    Locked,
    Hint(bool),
    Refresh,
    Ignore,
}

/// Decode only the current session's stream. Unlock is a request, not proof that the lock
/// screen has gone away; the authoritative LockedHint must be read before unblocking.
fn lock_signal(message: &zbus::Message) -> Result<LockSignal, String> {
    let header = message.header();
    match (header.interface().map(|s| s.as_str()), header.member().map(|s| s.as_str())) {
        (Some(LOGIN_SESSION), Some("Lock")) => Ok(LockSignal::Locked),
        (Some(LOGIN_SESSION), Some("Unlock")) => Ok(LockSignal::Refresh),
        (Some(PROPERTIES), Some("PropertiesChanged")) => {
            let (interface, changed, invalidated): (String, std::collections::HashMap<String, OwnedValue>, Vec<String>) =
                message.body().deserialize().map_err(|e| format!("Invalid session property signal: {e}"))?;
            if interface != LOGIN_SESSION {
                return Ok(LockSignal::Ignore);
            }
            if let Some(value) = changed.get("LockedHint") {
                return bool::try_from(value).map(LockSignal::Hint).map_err(|e| format!("Invalid LockedHint signal: {e}"));
            }
            Ok(if invalidated.iter().any(|name| name == "LockedHint") { LockSignal::Refresh } else { LockSignal::Ignore })
        }
        _ => Ok(LockSignal::Ignore),
    }
}

async fn monitor_lifecycle(app: &AppHandle) -> Result<(), String> {
    let connection = lifecycle_call(zbus::Connection::system()).await?;
    let dbus = lifecycle_call(zbus::Proxy::new(&connection, "org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus")).await?;
    let mut owners = lifecycle_call(dbus.receive_signal_with_args("NameOwnerChanged", &[(0, LOGIN1)])).await?;
    // Pin the unique owner. GetNameOwner does not activate services or ask for permission.
    let owner: String = lifecycle_call(dbus.call("GetNameOwner", &(LOGIN1,))).await?;
    let manager = lifecycle_call(zbus::Proxy::new(&connection, owner.as_str(), LOGIN_MANAGER, "org.freedesktop.login1.Manager")).await?;
    let manager_properties = lifecycle_call(zbus::Proxy::new(&connection, owner.as_str(), LOGIN_MANAGER, PROPERTIES)).await?;
    let mut sleep_events = lifecycle_call(manager.receive_signal("PrepareForSleep")).await?;

    // Never enumerate users/sessions or trust an environment-selected session. If PID lookup
    // fails (for example, a detached user service), keep sleep monitoring but report no lock watch.
    let session_path: Result<zbus::zvariant::OwnedObjectPath, String> = lifecycle_call(manager.call("GetSessionByPID", &(std::process::id(),))).await;
    let mut lock_report;
    let mut session_events = None;
    let mut session_properties = None;
    let mut lock_snapshot = None;
    match session_path {
        Ok(path) => {
            let setup = async {
                let rule = session_signal_rule(&owner, path.as_str())?;
                let events = lifecycle_call(zbus::MessageStream::for_match_rule(rule, &connection, Some(64))).await?;
                let properties = lifecycle_call(zbus::Proxy::new(&connection, owner.clone(), path, PROPERTIES)).await?;
                Ok::<_, String>((events, properties))
            }
            .await;
            match setup {
                Ok((events, properties)) => {
                    // Subscribe before reading: a lock occurring during initialization is queued.
                    match locked_hint(&properties).await {
                        Ok((locked, position)) => {
                            lock_snapshot = Some(position);
                            DESKTOP.get().unwrap().lifecycle.lock().unwrap().locked = locked;
                            if locked {
                                crate::dictation::cancel(app);
                            }
                            lock_report = "Current login1 session monitored (LockedHint and Lock).".to_string();
                        }
                        Err(e) => lock_report = format!("Current session signals subscribed, initial state unavailable ({e})."),
                    }
                    session_events = Some(events);
                    session_properties = Some(properties);
                }
                Err(e) => lock_report = format!("Unavailable for the current session ({e})."),
            }
        }
        Err(e) => lock_report = format!("Unavailable: login1 could not identify this process's session ({e})."),
    }
    let initial_sleep = lifecycle_call(manager_properties.call_method("Get", &("org.freedesktop.login1.Manager", "PreparingForSleep"))).await;
    let mut sleep_snapshot = None;
    let mut sleep_report = match initial_sleep.and_then(|reply| {
        let value: OwnedValue = reply.body().deserialize().map_err(|e| e.to_string())?;
        Ok((bool::try_from(value).map_err(|e| e.to_string())?, reply.recv_position()))
    }) {
        Ok((sleeping, position)) => {
            sleep_snapshot = Some(position);
            DESKTOP.get().unwrap().lifecycle.lock().unwrap().sleeping = sleeping;
            if sleeping {
                crate::dictation::cancel(app);
            }
            "login1 PrepareForSleep monitored.".to_string()
        }
        Err(e) => format!("PrepareForSleep subscribed, initial state unavailable ({e})."),
    };
    DESKTOP.get().unwrap().lifecycle.lock().unwrap().checking = false;
    lifecycle_status(app, &lock_report, &sleep_report);

    loop {
        tokio::select! {
            event = owners.next() => {
                let event = event.ok_or("The system bus owner stream ended")?;
                let (name, _, new): (String, String, String) = event.body().deserialize().map_err(|e| e.to_string())?;
                if name == LOGIN1 && new != owner { return Err("The login1 service stopped or restarted".into()); }
            }
            event = sleep_events.next() => {
                let event = event.ok_or("The suspend signal stream ended")?;
                let (sleeping,): (bool,) = event.body().deserialize().map_err(|e| format!("Invalid PrepareForSleep signal: {e}"))?;
                if sleep_snapshot.is_some_and(|position| event.recv_position() <= position) {
                    // A fresh property reply is newer than queued pre-startup signals. Honor
                    // the cancellation, but never let stale state override that snapshot.
                    if sleeping { crate::dictation::cancel(app); }
                    continue;
                }
                if sleeping {
                    DESKTOP.get().unwrap().lifecycle.lock().unwrap().sleeping = true;
                    crate::dictation::cancel(app);
                } else {
                    // Do not reopen the recording gate on resume before refreshing this
                    // session's lock state. Keep a previous known lock if the refresh fails.
                    if let Some(properties) = &session_properties {
                        match locked_hint(properties).await {
                            Ok((locked, position)) => {
                                lock_snapshot = Some(position);
                                DESKTOP.get().unwrap().lifecycle.lock().unwrap().locked = locked;
                                if locked { crate::dictation::cancel(app); }
                            }
                            Err(e) => lock_report = format!("Lock state refresh after resume failed ({e})."),
                        }
                    }
                    DESKTOP.get().unwrap().lifecycle.lock().unwrap().sleeping = false;
                }
                sleep_report = "login1 PrepareForSleep monitored.".into();
                lifecycle_status(app, &lock_report, &sleep_report);
            }
            event = async {
                match session_events.as_mut() {
                    Some(events) => events.next().await,
                    None => std::future::pending().await,
                }
            } => {
                let event = event.ok_or("The current-session signal stream ended")?.map_err(|e| e.to_string())?;
                let signal = lock_signal(&event)?;
                if lock_snapshot.is_some_and(|position| event.recv_position() <= position) {
                    if matches!(signal, LockSignal::Locked | LockSignal::Hint(true)) { crate::dictation::cancel(app); }
                    continue;
                }
                let locked = match signal {
                    LockSignal::Locked => true,
                    LockSignal::Hint(locked) => locked,
                    LockSignal::Refresh => {
                        let (locked, position) = locked_hint(session_properties.as_ref().ok_or("The current session is unavailable")?).await?;
                        lock_snapshot = Some(position);
                        locked
                    }
                    LockSignal::Ignore => continue,
                };
                DESKTOP.get().unwrap().lifecycle.lock().unwrap().locked = locked;
                if locked { crate::dictation::cancel(app); }
                lock_report = "Current login1 session monitored (LockedHint and Lock).".into();
                lifecycle_status(app, &lock_report, &sleep_report);
            }
        }
    }
}

async fn shortcut_worker(app: AppHandle, mut rx: watch::Receiver<Option<HotkeySettings>>) {
    loop {
        let settings = rx.borrow_and_update().clone();
        let changed = if let Some(settings) = settings {
            match portal_session(&app, &settings, &mut rx).await {
                Ok(changed) => changed,
                Err(e) => {
                    update_status(&app, |s| {
                        s.shortcuts_ready = false;
                        s.release_observed = false;
                        s.bindings.clear();
                        s.shortcuts = format!("GlobalShortcuts unavailable or permission denied: {e}. {FALLBACK}");
                    });
                    false
                }
            }
        } else {
            false
        };
        if !changed && rx.changed().await.is_err() {
            return;
        }
    }
}

async fn portal_session(app: &AppHandle, settings: &HotkeySettings, rx: &mut watch::Receiver<Option<HotkeySettings>>) -> Result<bool, String> {
    let portal = tokio::time::timeout(Duration::from_secs(10), GlobalShortcuts::new())
        .await
        .map_err(|_| "Portal discovery timed out".to_string())?
        .map_err(|e| e.to_string())?;
    let session = tokio::time::timeout(Duration::from_secs(10), portal.create_session())
        .await
        .map_err(|_| "Portal session creation timed out".to_string())?
        .map_err(|e| e.to_string())?;
    // Session's Serialize implementation is the D-Bus object path. Match it on every signal:
    // the connection may also receive signals for other applications/sessions.
    let path: String = serde_json::from_value(serde_json::to_value(&session).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    let result = tokio::select! {
        biased;
        _ = rx.changed() => Ok(true),
        result = async {
            // One ordered D-Bus stream: selecting separate activation/release streams can
            // reorder a quick tap and leave hold-to-talk recording after the key is up.
            let mut events = portal.receive_all_signals().await.map_err(|e| e.to_string())?;
            let mut closed = session.receive_closed().await.map_err(|e| e.to_string())?;
            let mut shortcuts = Vec::new();
            if !settings.dictate.trim().is_empty() {
                shortcuts.push(new_shortcut("dictate", "Start or stop dictation", &settings.dictate));
            }
            if !settings.toggle_bubble.trim().is_empty() && settings.toggle_bubble.parse::<Shortcut>().ok() != settings.dictate.parse::<Shortcut>().ok() {
                shortcuts.push(new_shortcut("toggle-bubble", "Show or hide the recorder", &settings.toggle_bubble));
            }
            // Always provide a recovery action if a compositor loses a release event.
            shortcuts.push(NewShortcut::new("cancel", "Cancel dictation"));
            let response = tokio::select! {
                _ = closed.next() => return Err("The shortcut session closed during permission setup".into()),
                response = tokio::time::timeout(Duration::from_secs(120), portal.bind_shortcuts(&session, &shortcuts, None)) => {
                    response.map_err(|_| "Shortcut permission request timed out".to_string())?
                        .map_err(|e| e.to_string())?.response().map_err(|e| e.to_string())?
                }
            };
            let mut bindings = response.shortcuts().iter().map(|s| Binding { id: s.id().into(), trigger: s.trigger_description().into() }).collect::<Vec<_>>();
            publish_bindings(app, &bindings, false);
            let mut release_observed = false;
            *app.state::<Core>().hotkey_gesture.lock().unwrap() = Gesture::new(Mode::Toggle);
            loop {
                tokio::select! {
                    _ = closed.next() => return Err("The shortcut session closed or permission was revoked".into()),
                    message = events.next() => {
                        let message = message.ok_or("The shortcut event stream ended")?;
                        match message.header().member().map(|m| m.as_str()) {
                            Some("Activated") => {
                                let event: Activated = message.body().deserialize().map_err(|e| e.to_string())?;
                                if event.session_handle().as_str() != path { continue; }
                                if !bindings.iter().any(|b| b.id == event.shortcut_id() && !b.trigger.is_empty()) { continue; }
                                match event.shortcut_id() {
                                    "dictate" => {
                                        if !release_observed {
                                            // Without observed releases, each activation is a toggle.
                                            *app.state::<Core>().hotkey_gesture.lock().unwrap() = Gesture::new(Mode::Toggle);
                                        }
                                        crate::hotkeys::on_dictate(app, ShortcutState::Pressed);
                                    }
                                    "cancel" => crate::dictation::cancel(app),
                                    "toggle-bubble" => crate::hotkeys::toggle_bubble(app),
                                    _ => {}
                                }
                            }
                            Some("Deactivated") => {
                                let event: Deactivated = message.body().deserialize().map_err(|e| e.to_string())?;
                                if event.session_handle().as_str() != path || event.shortcut_id() != "dictate" { continue; }
                                if !release_observed {
                                    release_observed = true;
                                    app.state::<Core>().hotkey_gesture.lock().unwrap().mode = settings.mode;
                                    publish_bindings(app, &bindings, true);
                                }
                                crate::hotkeys::on_dictate(app, ShortcutState::Released);
                            }
                            Some("ShortcutsChanged") => {
                                let event: ShortcutsChanged = message.body().deserialize().map_err(|e| e.to_string())?;
                                if event.session_handle().as_str() != path { continue; }
                                bindings = event.shortcuts().iter().map(|s| Binding { id: s.id().into(), trigger: s.trigger_description().into() }).collect();
                                publish_bindings(app, &bindings, release_observed);
                            }
                            _ => {}
                        }
                    }
                }
            }
        } => result,
    };
    // Close before replacing the binding set; no duplicate sessions or stuck hold recording.
    let _ = tokio::time::timeout(Duration::from_secs(3), session.close()).await;
    app.state::<Core>().hotkey_gesture.lock().unwrap().reset();
    if app.state::<Core>().dictation.is_recording() {
        crate::dictation::cancel(app);
    }
    result
}

fn publish_bindings(app: &AppHandle, bindings: &[Binding], release_observed: bool) {
    update_status(app, |s| {
        s.bindings = bindings.to_vec();
        s.release_observed = release_observed;
        let trigger = bindings.iter().find(|b| b.id == "dictate" && !b.trigger.is_empty()).map(|b| b.trigger.as_str());
        s.shortcuts_ready = trigger.is_some();
        s.shortcuts = if let Some(trigger) = trigger {
            let mode = if release_observed {
                "Release events observed; the selected hold/toggle mode is enabled."
            } else {
                "Toggle-only until the first release event is observed."
            };
            format!("GlobalShortcuts active: {trigger}. {mode}")
        } else {
            format!("The portal did not bind a dictation shortcut. {FALLBACK}")
        };
    });
}

fn new_shortcut(id: &str, description: &str, accelerator: &str) -> NewShortcut {
    let shortcut = NewShortcut::new(id, description);
    let Ok(key) = accelerator.parse::<Shortcut>() else {
        return shortcut;
    };
    let code = key.key.to_string();
    let name = match code.as_str() {
        "Space" => "space".into(),
        "Enter" => "Return".into(),
        "Backspace" => "BackSpace".into(),
        "PageUp" => "Page_Up".into(),
        "PageDown" => "Page_Down".into(),
        _ => code
            .strip_prefix("Key")
            .map(str::to_ascii_lowercase)
            .or_else(|| code.strip_prefix("Digit").map(str::to_string))
            .or_else(|| code.strip_prefix("Arrow").map(str::to_string))
            .unwrap_or(code),
    };
    let symbol = gtk::gdk::keys::Key::from_name(&name);
    if symbol == gtk::gdk::keys::constants::VoidSymbol {
        return shortcut;
    }
    let Some(name) = symbol.name() else {
        return shortcut;
    };
    let mut trigger = String::new();
    for (modifier, prefix) in [(Modifiers::CONTROL, "CTRL+"), (Modifiers::ALT, "ALT+"), (Modifiers::SHIFT, "SHIFT+"), (Modifiers::SUPER, "LOGO+")] {
        if key.mods.contains(modifier) {
            trigger.push_str(prefix);
        }
    }
    trigger.push_str(&name);
    shortcut.preferred_trigger(Some(trigger.as_str()))
}

/// GTK retains the selection data while Amber runs and transfers it on request, on both
/// native Wayland and X11. Do not snapshot/restore: that would destroy newer user copies.
pub fn to_clipboard(text: &str) -> Result<(), String> {
    let desktop = DESKTOP.get().ok_or("Linux clipboard is not initialized")?;
    let copy = |text: String| -> Result<(), String> {
        let display = gtk::gdk::Display::default().ok_or("No desktop clipboard display")?;
        if display.is_closed() {
            return Err("The desktop display has closed".into());
        }
        if !use_x11_shortcuts()
            && !gtk::Window::list_toplevels().iter().any(|widget| widget.downcast_ref::<gtk::Window>().is_some_and(|window| window.is_active()))
        {
            return Err("On Wayland, open the result window and use Copy while Amber is focused.".into());
        }
        let clipboard = gtk::Clipboard::default(&display).ok_or("No desktop clipboard")?;
        let targets = ["UTF8_STRING", "text/plain;charset=utf-8", "text/plain", "TEXT"].map(|name| gtk::TargetEntry::new(name, gtk::TargetFlags::empty(), 0));
        if !clipboard.set_with_data(&targets, move |_, selection, _| {
            selection.set_text(&text);
        }) {
            return Err("The desktop refused clipboard ownership. The result is still available for manual selection.".into());
        }
        display.flush();
        Ok(())
    };
    if gtk::is_initialized_main_thread() {
        return copy(text.to_owned());
    }
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let task_cancelled = cancelled.clone();
    let text = text.to_owned();
    desktop
        .app
        .run_on_main_thread(move || {
            if task_cancelled.load(std::sync::atomic::Ordering::SeqCst) {
                return;
            }
            let _ = tx.send(copy(text));
        })
        .map_err(|e| e.to_string())?;
    match rx.recv_timeout(Duration::from_secs(5)) {
        Ok(result) => result,
        Err(_) => {
            cancelled.store(true, std::sync::atomic::Ordering::SeqCst);
            Err("Clipboard response timed out; check the clipboard before retrying.".into())
        }
    }
}

pub use x11::{paste_explicit, remember_target, target_program, target_title, X11Target};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Submission {
    pub submitted: bool,
    pub confirmed: bool,
    pub message: String,
}

mod x11 {
    use super::*;
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{
        AtomEnum, ConnectionExt, CreateWindowAux, EventMask, MapState, PropMode, SelectionNotifyEvent, WindowClass, KEY_PRESS_EVENT, KEY_RELEASE_EVENT,
        SELECTION_NOTIFY_EVENT,
    };
    use x11rb::protocol::{xtest::ConnectionExt as _, Event};
    use x11rb::rust_connection::RustConnection;
    use x11rb::wrapper::ConnectionExt as _;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct X11Target {
        root: u32,
        window: u32,
        focus: u32,
        pid: u32,
    }

    fn connect() -> Result<(RustConnection, usize), String> {
        if !use_x11_shortcuts() {
            return Err("X11 input is disabled in Wayland or unknown sessions".into());
        }
        x11rb::connect(None).map_err(|e| e.to_string())
    }

    fn atom(conn: &RustConnection, name: &str) -> Result<u32, String> {
        Ok(conn.intern_atom(false, name.as_bytes()).map_err(|e| e.to_string())?.reply().map_err(|e| e.to_string())?.atom)
    }

    fn property(conn: &RustConnection, window: u32, name: &str, type_: AtomEnum) -> Result<x11rb::protocol::xproto::GetPropertyReply, String> {
        conn.get_property(false, window, atom(conn, name)?, type_, 0, 1024).map_err(|e| e.to_string())?.reply().map_err(|e| e.to_string())
    }

    fn snapshot(conn: &RustConnection, root: u32) -> Result<X11Target, String> {
        let window = property(conn, root, "_NET_ACTIVE_WINDOW", AtomEnum::WINDOW)?
            .value32()
            .and_then(|mut v| v.next())
            .filter(|&w| w > 1)
            .ok_or("No observable active X11 window")?;
        let focus = conn.get_input_focus().map_err(|e| e.to_string())?.reply().map_err(|e| e.to_string())?.focus;
        if focus <= 1 {
            return Err("No observable X11 input focus".into());
        }
        let mut ancestor = focus;
        for _ in 0..64 {
            if ancestor == window {
                break;
            }
            if ancestor == 0 || ancestor == root {
                return Err("Focused X11 window is not the active target".into());
            }
            ancestor = conn.query_tree(ancestor).map_err(|e| e.to_string())?.reply().map_err(|e| e.to_string())?.parent;
        }
        if ancestor != window {
            return Err("Cannot establish X11 target ancestry".into());
        }
        if conn.get_window_attributes(window).map_err(|e| e.to_string())?.reply().map_err(|e| e.to_string())?.map_state != MapState::VIEWABLE {
            return Err("The target window is not visible".into());
        }
        let pid = property(conn, window, "_NET_WM_PID", AtomEnum::CARDINAL)?
            .value32()
            .and_then(|mut v| v.next())
            .filter(|&p| p != 0 && p != std::process::id())
            .ok_or("The target process is unknown or belongs to Amber")?;
        Ok(X11Target { root, window, focus, pid })
    }

    pub fn remember_target() -> Option<X11Target> {
        let (conn, screen) = connect().ok()?;
        snapshot(&conn, conn.setup().roots[screen].root).ok()
    }

    pub fn target_title(target: X11Target) -> Option<String> {
        let (conn, _) = connect().ok()?;
        let value = property(&conn, target.window, "_NET_WM_NAME", AtomEnum::ANY).ok()?.value;
        String::from_utf8(value).ok()
    }

    pub fn target_program(target: X11Target) -> Option<String> {
        let (conn, _) = connect().ok()?;
        program(&conn, target).ok()
    }

    fn program(conn: &RustConnection, target: X11Target) -> Result<String, String> {
        let property = property(conn, target.window, "WM_CLASS", AtomEnum::STRING)?;
        if property.format != 8 || property.bytes_after != 0 {
            return Err("The target application class is unknown".into());
        }
        let mut parts = property.value.split(|&b| b == 0);
        let _instance = parts.next();
        let class = parts.next().filter(|s| !s.is_empty()).ok_or("The target application class is unknown")?;
        if parts.next() != Some(b"".as_slice()) || parts.next().is_some() {
            return Err("The target application class is malformed".into());
        }
        String::from_utf8(class.to_vec()).map_err(|e| e.to_string())
    }

    /// Explicitly requested delivery only. No foreground switching, automatic delivery,
    /// automatic retry, synthetic Enter, clipboard restoration, or root/input helper.
    pub fn paste_explicit(text: &str, target: X11Target) -> Result<Submission, String> {
        if session_is_blocked() {
            return Err("Paste blocked while the session is locked, suspended, or being checked.".into());
        }
        let (conn, _) = connect()?;
        if snapshot(&conn, target.root)? != target {
            return Err("The target window or input focus changed. Nothing was pasted.".into());
        }
        let class = program(&conn, target)?;
        if !amber_core::insertion::allows_explicit_x11_paste(&class, text) {
            return Err("This target or text requires review and manual paste. Explicit X11 paste only supports single-line text in known native editors, not terminals, browsers or unknown fields.".into());
        }
        let normal = atom(&conn, "_NET_WM_WINDOW_TYPE_NORMAL")?;
        let types = property(&conn, target.window, "_NET_WM_WINDOW_TYPE", AtomEnum::ATOM)?;
        if !types.value32().is_some_and(|mut t| t.next() == Some(normal) && t.next().is_none()) {
            return Err("Dialogs and unknown window types require Copy/manual paste.".into());
        }
        conn.xtest_get_version(2, 2).map_err(|e| e.to_string())?.reply().map_err(|e| format!("XTest unavailable: {e}"))?;
        let setup = conn.setup();
        let mapping = conn
            .get_keyboard_mapping(setup.min_keycode, setup.max_keycode - setup.min_keycode + 1)
            .map_err(|e| e.to_string())?
            .reply()
            .map_err(|e| e.to_string())?;
        let width = usize::from(mapping.keysyms_per_keycode);
        if width == 0 {
            return Err("The keyboard mapping is unavailable".into());
        }
        // Shift+Insert avoids assuming that the current keyboard layout maps a key to V.
        let keycode = |symbol| {
            mapping
                .keysyms
                .chunks(width)
                .position(|row| row.contains(&symbol) && row.iter().all(|&s| s == 0 || s == symbol))
                .map(|i| setup.min_keycode + i as u8)
        };
        let shift = keycode(0xffe1).ok_or("No unambiguous Shift key; use manual paste")?;
        let insert = keycode(0xff63).ok_or("No layout-independent Insert key; use manual paste")?;
        let modifiers = conn.get_modifier_mapping().map_err(|e| e.to_string())?.reply().map_err(|e| e.to_string())?;
        if !modifiers.keycodes[..modifiers.keycodes.len() / 8].contains(&shift) {
            return Err("Nonstandard Shift modifier mapping; use manual paste".into());
        }
        if conn.query_keymap().map_err(|e| e.to_string())?.reply().map_err(|e| e.to_string())?.keys.iter().any(|&k| k != 0) {
            return Err("Release all keys before explicit paste. Nothing was pasted.".into());
        }
        if u16::from(conn.query_pointer(target.root).map_err(|e| e.to_string())?.reply().map_err(|e| e.to_string())?.mask) & 0xff != 0 {
            return Err("Active or locked keyboard modifiers require manual paste. Nothing was pasted.".into());
        }
        let (owner, selection) = own_clipboard(text)?;
        // Freeze other clients only for the final target/ownership checks and four events.
        // No sleeps or clipboard transfer while grabbed. The server releases on disconnect.
        conn.grab_server().map_err(|e| e.to_string())?.check().map_err(|e| e.to_string())?;
        let result = (|| {
            if session_is_blocked() {
                return Err("The session became blocked. No paste was sent.".into());
            }
            if snapshot(&conn, target.root)? != target {
                return Err("The target changed. The result is on the clipboard, but no paste was sent.".into());
            }
            if program(&conn, target)? != class || property(&conn, target.window, "_NET_WM_WINDOW_TYPE", AtomEnum::ATOM)?.value != types.value {
                return Err("The target application or window type changed. No paste was sent.".into());
            }
            if conn.get_selection_owner(selection).map_err(|e| e.to_string())?.reply().map_err(|e| e.to_string())?.owner != owner {
                return Err("The clipboard changed. No paste was sent and the newer clipboard was not overwritten.".into());
            }
            if conn.query_keymap().map_err(|e| e.to_string())?.reply().map_err(|e| e.to_string())?.keys.iter().any(|&k| k != 0) {
                return Err("A key is held. The result is on the clipboard, but no paste was sent.".into());
            }
            if u16::from(conn.query_pointer(target.root).map_err(|e| e.to_string())?.reply().map_err(|e| e.to_string())?.mask) & 0xff != 0 {
                return Err("Keyboard modifiers changed. The result is on the clipboard, but no paste was sent.".into());
            }
            let mut failure = None;
            for (event, key) in [(KEY_PRESS_EVENT, shift), (KEY_PRESS_EVENT, insert), (KEY_RELEASE_EVENT, insert), (KEY_RELEASE_EVENT, shift)] {
                // Always attempt the releases, including after a partial submission failure.
                if failure.is_some() && event == KEY_PRESS_EVENT {
                    continue;
                }
                if let Err(e) =
                    conn.xtest_fake_input(event, key, 0, target.root, 0, 0, 0).map_err(|e| e.to_string()).and_then(|c| c.check().map_err(|e| e.to_string()))
                {
                    failure = Some(e);
                }
            }
            if let Some(e) = failure {
                return Err(format!("Paste submission was uncertain ({e}). Do not retry automatically; check the destination first."));
            }
            Ok(Submission {
                submitted: true,
                confirmed: false,
                message: "Paste shortcut submitted, not confirmed. The clipboard was replaced and is not restored. Check the destination before trying again."
                    .into(),
            })
        })();
        let _ = conn.ungrab_server().and_then(|_| conn.flush());
        result
    }

    /// Own a bounded UTF-8 selection until replaced. A dedicated X11 connection gives us an
    /// exact owner ID for the final race check; no clipboard manager or helper is required.
    fn own_clipboard(text: &str) -> Result<(u32, u32), String> {
        if text.len() > 64 * 1024 {
            return Err("Text is too large for explicit X11 paste. Use Copy/manual paste.".into());
        }
        let (conn, screen) = connect()?;
        let root = conn.setup().roots[screen].root;
        let window = conn.generate_id().map_err(|e| e.to_string())?;
        conn.create_window(0, window, root, 0, 0, 1, 1, 0, WindowClass::INPUT_ONLY, 0, &CreateWindowAux::new())
            .map_err(|e| e.to_string())?
            .check()
            .map_err(|e| e.to_string())?;
        let selection = atom(&conn, "CLIPBOARD")?;
        let targets = atom(&conn, "TARGETS")?;
        let utf8 = atom(&conn, "UTF8_STRING")?;
        let plain = atom(&conn, "text/plain;charset=utf-8")?;
        let text_atom = atom(&conn, "TEXT")?;
        conn.set_selection_owner(window, selection, x11rb::CURRENT_TIME).map_err(|e| e.to_string())?.check().map_err(|e| e.to_string())?;
        if conn.get_selection_owner(selection).map_err(|e| e.to_string())?.reply().map_err(|e| e.to_string())?.owner != window {
            return Err("The clipboard changed before ownership could be established; nothing was pasted.".into());
        }
        let text = text.as_bytes().to_vec();
        std::thread::Builder::new()
            .name("amber-x11-clipboard".into())
            .spawn(move || {
                while let Ok(event) = conn.wait_for_event() {
                    match event {
                        Event::SelectionClear(_) => break,
                        Event::SelectionRequest(request) if request.owner == window && request.selection == selection => {
                            let property = if request.property == 0 { request.target } else { request.property };
                            let accepted = if request.target == targets {
                                conn.change_property32(PropMode::REPLACE, request.requestor, property, AtomEnum::ATOM, &[targets, utf8, plain, text_atom])
                                    .map_err(|e| e.to_string())
                                    .and_then(|c| c.check().map_err(|e| e.to_string()))
                            } else if [utf8, plain, text_atom].contains(&request.target) {
                                // TEXT negotiates an encoding; a MIME conversion keeps its MIME type.
                                let type_ = if request.target == text_atom { utf8 } else { request.target };
                                conn.change_property8(PropMode::REPLACE, request.requestor, property, type_, &text)
                                    .map_err(|e| e.to_string())
                                    .and_then(|c| c.check().map_err(|e| e.to_string()))
                            } else {
                                let reply = SelectionNotifyEvent {
                                    response_type: SELECTION_NOTIFY_EVENT,
                                    sequence: 0,
                                    time: request.time,
                                    requestor: request.requestor,
                                    selection,
                                    target: request.target,
                                    property: 0,
                                };
                                let _ = conn.send_event(false, request.requestor, EventMask::NO_EVENT, reply);
                                let _ = conn.flush();
                                continue;
                            };
                            let reply = SelectionNotifyEvent {
                                response_type: SELECTION_NOTIFY_EVENT,
                                sequence: 0,
                                time: request.time,
                                requestor: request.requestor,
                                selection,
                                target: request.target,
                                property: if accepted.is_ok() { property } else { 0 },
                            };
                            let _ = conn.send_event(false, request.requestor, EventMask::NO_EVENT, reply);
                            let _ = conn.flush();
                        }
                        _ => {}
                    }
                }
            })
            .map_err(|e| format!("Could not serve the X11 clipboard: {e}"))?;
        Ok((window, selection))
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::MetadataExt;
        use std::time::Instant;
        use x11rb::protocol::xproto::{GetPropertyReply, InputFocus, KeyButMask};

        // Validate the harness before even connecting. An ignored test plus DISPLAY alone
        // must never be enough to send input or replace someone's desktop clipboard.
        fn isolated() -> (RustConnection, u32) {
            let root = std::path::PathBuf::from(std::env::var_os("AMBER_X11_TEST_ROOT").expect("Run python3 tools/test-x11.py"));
            assert!(root.file_name().unwrap().to_str().unwrap().starts_with("amber-x11-test-"));
            let metadata = std::fs::symlink_metadata(&root).unwrap();
            assert!(metadata.is_dir());
            assert_eq!(metadata.mode() & 0o777, 0o700);
            assert_eq!(metadata.uid(), unsafe { libc::geteuid() });
            let display = std::env::var("AMBER_X11_TEST_DISPLAY").unwrap();
            assert_eq!(std::env::var("DISPLAY").unwrap(), display);
            let number = display.strip_prefix(':').unwrap();
            assert!(!number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()));
            let pid: u32 = std::env::var("AMBER_X11_TEST_SERVER_PID").unwrap().parse().unwrap();
            // Xorg's -displayfd mode does not create the usual /tmp/.XN-lock file.
            // Ask the kernel which process owns the Unix socket instead.
            let socket = std::os::unix::net::UnixStream::connect(format!("/tmp/.X11-unix/X{number}")).unwrap();
            let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
            let mut size = std::mem::size_of_val(&credentials) as libc::socklen_t;
            assert_eq!(unsafe {
                libc::getsockopt(socket.as_raw_fd(), libc::SOL_SOCKET, libc::SO_PEERCRED, (&mut credentials as *mut libc::ucred).cast(), &mut size)
            }, 0);
            assert_eq!(credentials.pid as u32, pid);
            drop(socket);
            let executable = std::fs::read_link(format!("/proc/{pid}/exe")).unwrap();
            assert_eq!(executable.file_name().unwrap(), "Xvfb");
            let args = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap();
            let args: Vec<_> = args.split(|&b| b == 0).collect();
            let auth = root.join("Xauthority");
            assert_eq!(std::env::var_os("XAUTHORITY").unwrap(), auth.as_os_str());
            assert!(args.windows(2).any(|a| a[0] == b"-auth" && a[1] == auth.as_os_str().as_encoded_bytes()));
            assert!(args.windows(2).any(|a| a[0] == b"-nolisten" && a[1] == b"tcp"));
            assert!(args.contains(&b"-displayfd".as_slice()));
            let (conn, screen) = connect().unwrap();
            let root = conn.setup().roots[screen].root;
            let wm = property(&conn, root, "_NET_SUPPORTING_WM_CHECK", AtomEnum::WINDOW).unwrap().value32().unwrap().next().unwrap();
            assert_eq!(property(&conn, wm, "_NET_WM_NAME", AtomEnum::ANY).unwrap().value, b"i3");
            (conn, root)
        }

        #[track_caller]
        fn until(mut ready: impl FnMut() -> bool) {
            let deadline = Instant::now() + Duration::from_secs(3);
            while !ready() {
                assert!(Instant::now() < deadline, "Timed out waiting for isolated X11 fixture");
                std::thread::sleep(Duration::from_millis(5));
            }
        }

        fn create(conn: &RustConnection, parent: u32) -> u32 {
            let window = conn.generate_id().unwrap();
            conn.create_window(
                x11rb::COPY_DEPTH_FROM_PARENT, window, parent, 0, 0, 320, 200, 0,
                WindowClass::INPUT_OUTPUT, 0,
                &CreateWindowAux::new().event_mask(EventMask::KEY_PRESS | EventMask::KEY_RELEASE),
            ).unwrap().check().unwrap();
            window
        }

        fn class(conn: &RustConnection, window: u32, value: &[u8]) {
            conn.change_property8(PropMode::REPLACE, window, AtomEnum::WM_CLASS, AtomEnum::STRING, value).unwrap().check().unwrap();
        }

        fn editor(conn: &RustConnection, root: u32) -> X11Target {
            let window = create(conn, root);
            class(conn, window, b"test-instance\0Mousepad\0");
            conn.change_property32(PropMode::REPLACE, window, AtomEnum::WM_HINTS, AtomEnum::WM_HINTS, &[1, 1, 0, 0, 0, 0, 0, 0, 0]).unwrap().check().unwrap();
            // Deliberately distinct from the test process, just as an external client is.
            let pid: u32 = std::env::var("AMBER_X11_TEST_SERVER_PID").unwrap().parse().unwrap();
            conn.change_property32(PropMode::REPLACE, window, atom(conn, "_NET_WM_PID").unwrap(), AtomEnum::CARDINAL, &[pid]).unwrap().check().unwrap();
            conn.change_property32(PropMode::REPLACE, window, atom(conn, "_NET_WM_WINDOW_TYPE").unwrap(), AtomEnum::ATOM,
                &[atom(conn, "_NET_WM_WINDOW_TYPE_NORMAL").unwrap()]).unwrap().check().unwrap();
            conn.change_property8(PropMode::REPLACE, window, atom(conn, "_NET_WM_NAME").unwrap(), atom(conn, "UTF8_STRING").unwrap(), b"Amber isolated editor").unwrap().check().unwrap();
            conn.map_window(window).unwrap().check().unwrap();
            until(|| snapshot(conn, root).is_ok_and(|target| target.window == window));
            snapshot(conn, root).unwrap()
        }

        fn conversion(conn: &RustConnection, requestor: u32, target: u32, property: u32) -> Option<GetPropertyReply> {
            let clipboard = atom(conn, "CLIPBOARD").unwrap();
            conn.convert_selection(requestor, clipboard, target, property, x11rb::CURRENT_TIME).unwrap().check().unwrap();
            let mut notify = None;
            until(|| {
                while let Some(event) = conn.poll_for_event().unwrap() {
                    if let Event::SelectionNotify(event) = event {
                        assert_eq!((event.requestor, event.target, event.selection), (requestor, target, clipboard));
                        notify = Some(event);
                        return true;
                    }
                }
                false
            });
            let property = notify.unwrap().property;
            (property != 0).then(|| conn.get_property(true, requestor, property, AtomEnum::ANY, 0, 32 * 1024).unwrap().reply().unwrap())
        }

        fn key_events(conn: &RustConnection) -> Vec<(u8, u8, u32, u16)> {
            conn.get_input_focus().unwrap().reply().unwrap();
            let mut keys = Vec::new();
            while let Some(event) = conn.poll_for_event().unwrap() {
                match event {
                    Event::KeyPress(e) => keys.push((KEY_PRESS_EVENT, e.detail, e.event, e.state.into())),
                    Event::KeyRelease(e) => keys.push((KEY_RELEASE_EVENT, e.detail, e.event, e.state.into())),
                    _ => {}
                }
            }
            keys
        }

        fn keycode(conn: &RustConnection, symbol: u32) -> u8 {
            let setup = conn.setup();
            let mapping = conn.get_keyboard_mapping(setup.min_keycode, setup.max_keycode - setup.min_keycode + 1).unwrap().reply().unwrap();
            setup.min_keycode + mapping.keysyms.chunks(mapping.keysyms_per_keycode as usize).position(|row| row[0] == symbol).unwrap() as u8
        }

        fn key(conn: &RustConnection, root: u32, code: u8, down: bool) {
            conn.xtest_fake_input(if down { KEY_PRESS_EVENT } else { KEY_RELEASE_EVENT }, code, 0, root, 0, 0, 0).unwrap().check().unwrap();
        }

        #[test]
        #[ignore = "Requires the disposable authenticated Xvfb+i3 from tools/test-x11.py"]
        fn i3_focus_leaf_identity_and_stale_targets() {
            let (conn, root) = isolated();
            let target = editor(&conn, root);
            assert_eq!(remember_target(), Some(target));
            assert_eq!(target_program(target).as_deref(), Some("Mousepad"));
            assert_eq!(target_title(target).as_deref(), Some("Amber isolated editor"));
            // i3 reparents the client under a frame, while _NET_ACTIVE_WINDOW is the client.
            assert_ne!(conn.query_tree(target.window).unwrap().reply().unwrap().parent, root);
            let second = editor(&conn, root);
            assert_ne!(second.window, target.window);
            assert!(paste_explicit("stale window", target).is_err());
            let child = create(&conn, second.window);
            conn.map_window(child).unwrap().check().unwrap();
            conn.set_input_focus(InputFocus::PARENT, child, x11rb::CURRENT_TIME).unwrap().check().unwrap();
            let leaf = snapshot(&conn, root).unwrap();
            assert_eq!((leaf.window, leaf.focus), (second.window, child));
            assert!(paste_explicit("stale focus", second).unwrap_err().contains("changed"));
            conn.set_input_focus(InputFocus::PARENT, second.window, x11rb::CURRENT_TIME).unwrap().check().unwrap();
            assert!(paste_explicit("stale leaf", leaf).unwrap_err().contains("changed"));
            conn.set_input_focus(InputFocus::PARENT, target.window, x11rb::CURRENT_TIME).unwrap().check().unwrap();
            assert!(snapshot(&conn, root).is_err(), "Unrelated active window and focus must fail closed");
            conn.destroy_window(second.window).unwrap().check().unwrap();
            conn.destroy_window(target.window).unwrap().check().unwrap();
            assert!(paste_explicit("destroyed", leaf).is_err());
            assert!(key_events(&conn).is_empty());
        }

        #[test]
        #[ignore = "Requires the disposable authenticated Xvfb+i3 from tools/test-x11.py"]
        fn clipboard_targets_utf8_limits_and_replacement() {
            let (conn, root) = isolated();
            let requestor = create(&conn, root);
            conn.set_selection_owner(requestor, AtomEnum::PRIMARY.into(), x11rb::CURRENT_TIME).unwrap().check().unwrap();
            let text = "Gr\u{fc}\u{df}e \u{1f642}";
            let (owner, clipboard) = own_clipboard(text).unwrap();
            let destination = atom(&conn, "AMBER_TEST_SELECTION").unwrap();
            let targets = atom(&conn, "TARGETS").unwrap();
            let utf8 = atom(&conn, "UTF8_STRING").unwrap();
            let plain = atom(&conn, "text/plain;charset=utf-8").unwrap();
            let text_atom = atom(&conn, "TEXT").unwrap();
            let list = conversion(&conn, requestor, targets, destination).unwrap();
            assert_eq!(list.type_, u32::from(AtomEnum::ATOM));
            assert_eq!(list.value32().unwrap().collect::<Vec<_>>(), [targets, utf8, plain, text_atom]);
            for (target, expected_type) in [(utf8, utf8), (plain, plain), (text_atom, utf8)] {
                let value = conversion(&conn, requestor, target, destination).unwrap();
                assert_eq!(value.type_, expected_type, "Selection conversion returned the wrong property type");
                assert_eq!(value.format, 8);
                assert_eq!(value.value, text.as_bytes());
            }
            assert_eq!(conversion(&conn, requestor, utf8, 0).unwrap().value, text.as_bytes());
            assert!(conversion(&conn, requestor, atom(&conn, "image/png").unwrap(), destination).is_none());
            assert!(own_clipboard(&"x".repeat(64 * 1024 + 1)).is_err());
            assert_eq!(conn.get_selection_owner(clipboard).unwrap().reply().unwrap().owner, owner);
            let maximum = "z".repeat(64 * 1024);
            let (replacement, _) = own_clipboard(&maximum).unwrap();
            assert_eq!(conversion(&conn, requestor, utf8, destination).unwrap().value, maximum.as_bytes());
            until(|| conn.get_window_attributes(owner).unwrap().reply().is_err());
            conn.set_selection_owner(requestor, clipboard, x11rb::CURRENT_TIME).unwrap().check().unwrap();
            until(|| conn.get_window_attributes(replacement).unwrap().reply().is_err());
            assert_eq!(conn.get_selection_owner(clipboard).unwrap().reply().unwrap().owner, requestor);
            assert_eq!(conn.get_selection_owner(AtomEnum::PRIMARY.into()).unwrap().reply().unwrap().owner, requestor);
        }

        #[test]
        #[ignore = "Requires the disposable authenticated Xvfb+i3 from tools/test-x11.py"]
        fn explicit_paste_policy_modifiers_and_unverified_submission() {
            let (conn, root) = isolated();
            let target = editor(&conn, root);
            let clipboard = atom(&conn, "CLIPBOARD").unwrap();
            conn.set_selection_owner(target.window, clipboard, x11rb::CURRENT_TIME).unwrap().check().unwrap();
            for value in [b"Mousepad\0XTerm\0".as_slice(), b"test\0firefox\0", b"test\0unknown\0"] {
                class(&conn, target.window, value);
                assert!(paste_explicit("not a terminal command", target).is_err());
            }
            class(&conn, target.window, b"test\0Mousepad\0");
            for text in ["", "line\nline", "line\rline", "line\u{2028}line", "line\u{2029}line", "\u{1b}[31m"] {
                assert!(paste_explicit(text, target).is_err(), "Should reject {text:?}");
            }
            conn.change_property32(PropMode::REPLACE, target.window, atom(&conn, "_NET_WM_WINDOW_TYPE").unwrap(), AtomEnum::ATOM,
                &[atom(&conn, "_NET_WM_WINDOW_TYPE_DIALOG").unwrap()]).unwrap().check().unwrap();
            assert!(paste_explicit("dialog", target).is_err());
            conn.change_property32(PropMode::REPLACE, target.window, atom(&conn, "_NET_WM_WINDOW_TYPE").unwrap(), AtomEnum::ATOM,
                &[atom(&conn, "_NET_WM_WINDOW_TYPE_NORMAL").unwrap()]).unwrap().check().unwrap();
            assert_eq!(conn.get_selection_owner(clipboard).unwrap().reply().unwrap().owner, target.window);
            assert!(key_events(&conn).is_empty());
            let shift = keycode(&conn, 0xffe1);
            let caps = keycode(&conn, 0xffe5);
            key(&conn, root, shift, true);
            assert!(paste_explicit("held key", target).unwrap_err().contains("Release all keys"));
            assert_ne!(conn.query_keymap().unwrap().reply().unwrap().keys[shift as usize / 8] & (1 << (shift % 8)), 0, "Never release a user's held key");
            key(&conn, root, shift, false);
            key(&conn, root, caps, true);
            key(&conn, root, caps, false);
            assert!(paste_explicit("locked modifier", target).unwrap_err().contains("modifiers"));
            key(&conn, root, caps, true);
            key(&conn, root, caps, false);
            key_events(&conn);
            assert_eq!(conn.get_selection_owner(clipboard).unwrap().reply().unwrap().owner, target.window);
            let result = paste_explicit("Explicit native paste", target).unwrap();
            assert!(result.submitted && !result.confirmed);
            let insert = keycode(&conn, 0xff63);
            assert_eq!(key_events(&conn), [
                (KEY_PRESS_EVENT, shift, target.focus, 0),
                (KEY_PRESS_EVENT, insert, target.focus, KeyButMask::SHIFT.into()),
                (KEY_RELEASE_EVENT, insert, target.focus, KeyButMask::SHIFT.into()),
                (KEY_RELEASE_EVENT, shift, target.focus, KeyButMask::SHIFT.into()),
            ]);
            assert!(conn.query_keymap().unwrap().reply().unwrap().keys.iter().all(|&k| k == 0));
            let utf8 = atom(&conn, "UTF8_STRING").unwrap();
            assert_eq!(conversion(&conn, target.window, utf8, utf8).unwrap().value, b"Explicit native paste");
            conn.set_selection_owner(target.window, clipboard, x11rb::CURRENT_TIME).unwrap().check().unwrap();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gdk_names_are_serialized_as_xdg_shortcut_hints() {
        for (accelerator, expected) in [
            ("Ctrl+Alt+Space", "CTRL+ALT+space"),
            ("CmdOrCtrl+KeyA", "CTRL+a"),
            ("Super+Shift+Digit5", "SHIFT+LOGO+5"),
            ("Ctrl+Enter", "CTRL+Return"),
            ("Alt+ArrowUp", "ALT+Up"),
        ] {
            let value = serde_json::to_value(new_shortcut("dictate", "Dictate", accelerator)).unwrap();
            assert_eq!(value[0], "dictate");
            assert_eq!(value[1]["preferred_trigger"]["signature"], "s", "{accelerator}: {value}");
            assert_eq!(value[1]["preferred_trigger"]["value"], expected, "{accelerator}: {value}");
        }
    }

    #[test]
    fn unknown_key_name_is_left_to_the_portal() {
        let value = serde_json::to_value(new_shortcut("dictate", "Dictate", "IntlBackslash")).unwrap();
        assert!(value[1].get("preferred_trigger").is_none_or(serde_json::Value::is_null));
    }

    #[test]
    fn login1_rule_is_scoped_to_exact_session_and_service_owner() {
        let path = "/org/freedesktop/login1/session/c2";
        let rule = session_signal_rule(":1.42", path).unwrap();
        for (sender, candidate, expected) in [
            (":1.42", path, true),
            (":1.42", "/org/freedesktop/login1/session/c3", false),
            (":1.42", "/org/freedesktop/login1/session/c2/child", false),
            (":1.99", path, false),
        ] {
            let message = zbus::Message::signal(candidate, LOGIN_SESSION, "Lock").unwrap().sender(sender).unwrap().build(&()).unwrap();
            assert_eq!(rule.matches(&message).unwrap(), expected, "{sender} {candidate}");
        }
    }

    #[test]
    fn unlock_request_requires_authoritative_hint_refresh() {
        for (member, expected) in [("Lock", LockSignal::Locked), ("Unlock", LockSignal::Refresh), ("Unrelated", LockSignal::Ignore)] {
            let message = zbus::Message::signal("/org/freedesktop/login1/session/c2", LOGIN_SESSION, member).unwrap().build(&()).unwrap();
            assert_eq!(lock_signal(&message).unwrap(), expected);
        }
    }

    #[test]
    fn locked_hint_changes_and_invalidation_decode_without_a_bus() {
        let build = |interface: &str, changed: std::collections::HashMap<String, OwnedValue>, invalidated: Vec<String>| {
            zbus::Message::signal("/org/freedesktop/login1/session/c2", PROPERTIES, "PropertiesChanged")
                .unwrap()
                .build(&(interface, changed, invalidated))
                .unwrap()
        };
        for locked in [true, false] {
            let changed = std::collections::HashMap::from([("LockedHint".into(), OwnedValue::from(locked))]);
            assert_eq!(lock_signal(&build(LOGIN_SESSION, changed, vec![])).unwrap(), LockSignal::Hint(locked));
        }
        assert_eq!(lock_signal(&build(LOGIN_SESSION, Default::default(), vec!["LockedHint".into()])).unwrap(), LockSignal::Refresh);
        assert_eq!(lock_signal(&build("org.example.Other", Default::default(), vec!["LockedHint".into()])).unwrap(), LockSignal::Ignore);
        let wrong_type = std::collections::HashMap::from([("LockedHint".into(), OwnedValue::from(42u32))]);
        assert!(lock_signal(&build(LOGIN_SESSION, wrong_type, vec![])).is_err());
    }

    #[test]
    fn sleep_signal_has_one_boolean_argument() {
        for preparing in [true, false] {
            let message = zbus::Message::signal(LOGIN_MANAGER, "org.freedesktop.login1.Manager", "PrepareForSleep").unwrap().build(&(preparing,)).unwrap();
            assert_eq!(message.body().deserialize::<(bool,)>().unwrap(), (preparing,));
        }
    }
}
