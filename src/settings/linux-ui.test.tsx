/// <reference types="node" />

import assert from "node:assert/strict";
import test from "node:test";
import React, { createElement, type ComponentProps, type ComponentType } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import type { ChatProvider, LocalInfo, LocalItem, Overview, Settings, TestResult } from "../shared/types";
import type { PageProps } from "./App";
import { DownloadItem, SpeechModelActions } from "./downloads";
import { AppPage } from "./pages/AppPage";
import { CleanupPage } from "./pages/Cleanup";
import { CredentialLink, CredentialsPage } from "./pages/Credentials";
import { EnginePage, visibleSpeechModels } from "./pages/Engine";
import { HistoryPage, HistoryTimings } from "./pages/History";
import { accelerator, DEFAULT_PASTE_SHORTCUT, HotkeyInput, HotkeysPage, pretty } from "./pages/Hotkeys";
import { InsertPage } from "./pages/Insert";
import { MicrophonePage } from "./pages/Microphone";
import { Recorder } from "./pages/Recorder";
import { TestingPage, TestResultDetails } from "./pages/Testing";
import { phaseLabel, Row, Switch } from "./ui";
import { availableConnections, chooseConnection, eligibleModels } from "./connections";
import { ConnectionsPage } from "./pages/Connections";
import { BASH_EXAMPLE, instructionIssue, instructionKeyword, InstructionsPage } from "./pages/Instructions";
import "./history.test";
import { progress, ProgressRing } from "../bubble/fire";

test("progress refinements preserve continuity through recognition, cleanup and overruns", () => {
  const ring = new ProgressRing();
  assert.equal(ring.at(0), null);
  ring.expect(1000, 0);
  assert.equal(ring.at(500), 0.5);
  ring.expect(10_000, 500);
  assert.equal(ring.at(500), 0.5, "a slower actual route cannot move backwards");
  const beforeCleanup = ring.at(1500)!;
  assert.ok(beforeCleanup > 0.5 && beforeCleanup < 0.9);
  ring.expect(300, 1500);
  assert.equal(ring.at(1500), beforeCleanup, "actual text size does not reset or jump");
  const later = ring.at(1600)!;
  assert.ok(later > beforeCleanup);
  ring.expect(0, 1600);
  assert.equal(ring.at(1600), later, "zero remaining work still needs a terminal state");
  assert.equal(ring.at(1e20), 0.98);
  assert.equal(ring.at(0), 0.98, "clock anomalies cannot regress progress");
  ring.expect(Number.NaN, 1e20);
  assert.equal(ring.at(1e20), 0.98);
  ring.reset();
  assert.equal(ring.at(1e20), null);
  ring.expect(500, 100);
  assert.equal(ring.at(100), 0);
  assert.equal(progress(Number.NaN, 100), 0);
  assert.equal(progress(100, Number.POSITIVE_INFINITY), 0);
});

test("history explains independent indefinite aggregate learning even when history is off", () => {
  const html = render(HistoryPage, props("linux", { history: false, statistics: false }));
  assert.match(html, /kept locally indefinitely/);
  assert.match(html, /no audio, transcripts or individual timing records/);
  assert.match(html, /after deleting history/);
});

test("everyday timings distinguish legacy, retry, live and skipped cleanup", () => {
  const base = { at: 100, text: "Everyday text", program: "editor", engine: "local", inserted: false };
  const html = renderToStaticMarkup(<HistoryTimings entries={[
    base,
    { ...base, at: 200, timings: { recordingMs: null, transcriptionMs: 123, cleanupMs: 0, processingMs: 130, live: false, cleanupSkipped: true } },
    { ...base, at: 300, timings: { recordingMs: 5000, transcriptionMs: 50, cleanupMs: 10, processingMs: 65, live: true, cleanupSkipped: false } },
  ]} />);
  assert.match(html, /<table/);
  assert.match(html, /Recording/);
  assert.match(html, /Transcription after stop/);
  assert.match(html, /Processing total/);
  assert.equal((html.match(/Not measured/g) ?? []).length, 4);
  assert.match(html, /Retry: no new recording/);
  assert.match(html, /Live chunking: remaining work only/);
  assert.match(html, /0<small>Smart cleanup skipped or failed; fallback text kept/);
  assert.match(html, /Everyday text/);
});

test("everyday timing table follows history opt-in and filters", (t) => {
  const entries = [
    { at: 100, text: "Visible dictation", program: "editor", engine: "local", inserted: false },
    { at: 200, text: "Filtered dictation", program: "browser", engine: "local", inserted: true },
  ];
  const original = React.useState;
  let calls = 0;
  t.mock.method(React, "useState", ((initial: unknown) => {
    const call = calls++;
    return original(call === 0 ? entries : call === 3
      ? { query: "", program: "editor", date: "all", status: "all" } : initial);
  }) as typeof React.useState);
  const enabled = render(HistoryPage, props("linux", { history: true }));
  assert.match(enabled, /Everyday dictation timings/);
  assert.match(enabled, /Visible dictation/);
  assert.doesNotMatch(enabled, /Filtered dictation/);
  calls = 0;
  const disabled = render(HistoryPage, props("linux", { history: false }));
  assert.doesNotMatch(disabled, /Everyday dictation timings|Visible dictation|Filtered dictation/);
});

const settings: Settings = {
  engine: "server", local: { runtime: "cpu", model: "tiny", threads: 0, keepLoaded: false, chunking: false },
  server: { connectionId: null, provider: "custom", baseUrl: "https://speech.example/v1", model: "whisper", uploadFormat: "wav", localFallback: false },
  connections: [], instructions: [],
  language: "en", vocabulary: "", microphone: "", hotkey: { dictate: "Super+KeyD", mode: "toggle", toggleBubble: "" },
  bubble: { visibility: "hidden", edge: "left", y: 0.5, monitor: "", size: 64, opacity: 1, hiddenIn: [], hideFullscreen: false },
  insert: { method: "paste", pasteOnCompletion: false, pasteShortcut: "Ctrl+KeyV", pasteIntoAnyWindow: true, pasteIntoDifferentWindow: false, restoreClipboard: false, trailingSpace: false }, sounds: false,
  autostart: false, autoUpdate: true, logText: false, onboarded: true,
  phone: { enabled: false, port: 8990, cleanup: false, tunnel: false, publicHost: "", keys: [] },
  profiles: [], history: false, historyDays: 7, statistics: false, snippets: [],
  discord: { muteWhileDictating: true },
  terminal: { enabled: true, automatic: true, safeMode: true, runCommands: false, hotkey: "Ctrl+KeyT", prefixes: [] },
  cleanup: {
    connectionId: null,
    smart: true, engine: "local", fillers: true, spokenPunctuation: false, germanCommas: false,
    localRuntime: "cpu", localModel: "cleanup", provider: "custom", baseUrl: "", model: "cleanup",
  },
};

const unexpectedAction = () => assert.fail("SSR must not perform an application action");

function props(platform: Overview["platform"] = "linux", change: Partial<Settings> = {}): PageProps & { quit: () => Promise<void>; compact: boolean } {
  const s = { ...structuredClone(settings), ...change };
  return {
    s,
    ov: {
      platform, version: "test", settings: s, phase: "idle",
      providers: [{ id: s.server.provider, name: "Test provider", baseUrl: s.server.baseUrl, models: [], hasKey: true, keyUrl: "", needsKey: s.server.provider !== "custom", supportsSpeech: true, supportsCleanup: true }],
    },
    phase: "idle", phaseMessage: null, settingsSaving: false, compact: false,
    save: unexpectedAction, refresh: unexpectedAction, quit: unexpectedAction, toast: unexpectedAction, go: unexpectedAction,
    desktop: {
      status: { session: "Wayland", shortcuts: "Denied", insertion: "Manual copy only", clipboard: "Available", secureStorage: "Locked", lifecycle: "Not available" },
      storage: { credentials: "Session only", history: "Unavailable" }, loading: false, error: null, refresh: unexpectedAction,
    },
  };
}

function render<P extends object>(component: ComponentType<P>, properties: P) {
  return renderToStaticMarkup(createElement(component, properties));
}

function keyEvent(key: string, code = key, extra: Partial<KeyboardEvent> = {}): KeyboardEvent {
  return { key, code, keyCode: 0, ctrlKey: false, altKey: false, shiftKey: false, metaKey: false, ...extra } as KeyboardEvent;
}

test("Linux settings hide updates and provide Quit while Windows keeps its controls", () => {
  const linux = render(AppPage, props());
  assert.match(linux, /Start at desktop login/);
  assert.match(linux, />Quit Amber</);
  assert.doesNotMatch(linux, /Update automatically|Check now|EinFabo|Start with Windows/);
  const windows = render(AppPage, props("windows"));
  assert.match(windows, /Start with Windows/);
  assert.match(windows, /Update automatically/);
  assert.match(windows, /Check now/);
  assert.doesNotMatch(windows, />Quit Amber</);
  assert.match(render(AppPage, { ...props(), phase: "recording" }), /disabled="">Quit Amber</);
});

test("Linux microphone and shortcuts hide unsupported Windows integrations", () => {
  assert.doesNotMatch(render(MicrophonePage, props()), /Discord|Windows Settings/);
  assert.match(render(MicrophonePage, props("windows")), /Mute me in Discord/);
  const linux = render(HotkeysPage, props());
  assert.match(linux, /amber-dictation --toggle/);
  assert.match(linux, /amber-dictation --cancel/);
  assert.match(linux, /Super \+ D/);
  assert.doesNotMatch(linux, /Terminal commands|Show \/ hide the bubble|Win \+ D/);
  const windows = render(HotkeysPage, props("windows"));
  assert.match(windows, /Terminal commands/);
  assert.match(windows, /Show \/ hide the bubble/);
  assert.equal(pretty("Super+KeyD"), "Win + D");
});

test("Delivery keeps Wayland manual-only and hides X11 target options on other desktops", () => {
  const linux = render(InsertPage, props());
  assert.match(linux, /Copy and paste completed dictation/);
  assert.match(linux, /Off by default/);
  assert.match(linux, /Manual paste on this desktop/);
  assert.match(linux, /Wayland and unknown sessions/);
  assert.match(linux, /Linux does not restore/);
  assert.match(linux, /completed text goes to the clipboard even when automatic paste is unavailable/);
  assert.match(linux, /Testing recordings never change the clipboard/);
  assert.doesNotMatch(linux, /Restore the clipboard|Try it|As keystrokes|Paste into any window|Allow pasting into a different window|Paste shortcut preset|Custom paste shortcut/);
  const unknown = props();
  unknown.desktop.status = null;
  assert.match(render(InsertPage, unknown), /Manual paste on this desktop/);
  assert.doesNotMatch(render(InsertPage, unknown), /Paste into any window|Allow pasting into a different window|Paste shortcut preset|Custom paste shortcut/);
  const win = props("windows");
  win.desktop.status!.session = "X11";
  const windows = render(InsertPage, win);
  assert.match(windows, /Paste \(Ctrl\+V\)/);
  assert.match(windows, /Restore the clipboard/);
  assert.match(windows, /Try it/);
  assert.doesNotMatch(windows, /Manual paste on this desktop|Off by default|Paste into any window|Allow pasting into a different window|Paste shortcut preset|Custom paste shortcut/);
  assert.match(render(Recorder, props()), /Configure delivery/);
  assert.match(render(HotkeysPage, props()), /does not open or focus Amber/);
});

test("X11 target switches default to any-window on and different-window off without enabling completion", () => {
  const p = props();
  p.desktop.status!.session = "x11";
  const html = render(InsertPage, p);
  assert.match(html, /role="switch" aria-checked="false" aria-label="Copy and paste completed dictation"/);
  assert.match(html, /role="switch" aria-checked="true" aria-label="Paste into any window"/);
  assert.match(html, /role="switch" aria-checked="false" aria-label="Allow pasting into a different window"/);
  assert.doesNotMatch(html, /<button[^>]*disabled=|Restricted X11 paste/);
  for (const expected of [/On by default/, /browsers, chat apps, dialogs and other window types/, /--insert-last/, /does not enable completion paste/,
    /Turn off to restrict targets to known native editors/, /currently focused window once, when delivery begins, not when recording stops/,
    /Focus changes during paste still block it/, /single-line text without control characters/, /observable, outside Amber/,
    /stable window and input focus/, /never switches focus or presses Enter/, /password fields/, /paste acceptance cannot be verified/,
    /Changing the shortcut does not relax these safeguards/, /Clipboard managers may retain/, /Keep Amber running/]) {
    assert.match(html, expected);
  }
  p.s.insert.pasteIntoAnyWindow = false;
  p.s.insert.pasteIntoDifferentWindow = true;
  const changed = render(InsertPage, p);
  assert.match(changed, /role="switch" aria-checked="false" aria-label="Paste into any window"/);
  assert.match(changed, /role="switch" aria-checked="true" aria-label="Allow pasting into a different window"/);
  assert.match(changed, /role="switch" aria-checked="false" aria-label="Copy and paste completed dictation"/);
});

test("X11 target switches save independently while completion is off and retain their values when it changes", async () => {
  const p = props("linux", { insert: { ...settings.insert, method: "type", pasteShortcut: "Shift+Insert" } });
  p.desktop.status!.session = "X11";
  const saved: Settings[] = [];
  p.save = async (change) => {
    const next = structuredClone(p.s);
    change(next);
    saved.push(next);
    p.s = next;
    p.ov.settings = next;
    return true;
  };
  const switches = new Map<string, ComponentProps<typeof Switch>>();
  // Capture the actual page callbacks during SSR without mocking its save handlers or hooks.
  function Probe() {
    const page = InsertPage(p);
    const visit = (node: React.ReactNode) => {
      if (!React.isValidElement<{ label?: string; children?: React.ReactNode }>(node)) return;
      if (node.type === Row && typeof node.props.label === "string" &&
        React.isValidElement<ComponentProps<typeof Switch>>(node.props.children) && node.props.children.type === Switch) {
        switches.set(node.props.label, node.props.children.props);
      }
      React.Children.forEach(node.props.children, visit);
    };
    visit(page);
    return page;
  }
  const expected = structuredClone(p.s);
  const changes: [string, Partial<Settings["insert"]>][] = [
    ["Paste into any window", { pasteIntoAnyWindow: false }],
    ["Allow pasting into a different window", { pasteIntoDifferentWindow: true }],
    ["Paste into any window", { pasteIntoAnyWindow: true }],
    ["Allow pasting into a different window", { pasteIntoDifferentWindow: false }],
    ["Allow pasting into a different window", { pasteIntoDifferentWindow: true }],
    ["Copy and paste completed dictation", { pasteOnCompletion: true, method: "paste" }],
    ["Copy and paste completed dictation", { pasteOnCompletion: false }],
  ];
  for (const [index, [label, change]] of changes.entries()) {
    switches.clear();
    render(Probe, {});
    assert.equal(saved.length, index, "Rendering must not save settings");
    const control = switches.get(label);
    assert.ok(control, `${label} must remain configurable`);
    await control.onChange(!control.on);
    Object.assign(expected.insert, change);
    assert.equal(saved.length, index + 1, "Each toggle must save exactly once");
    assert.deepEqual(saved[index], expected, "Only the requested fields should change");
    assert.deepEqual(p.s, expected);
  }
});

test("X11 paste shortcut defaults and presets remain available with completion off", () => {
  const p = props();
  p.desktop.status!.session = "X11";
  const html = render(InsertPage, p);
  assert.equal(p.s.insert.pasteShortcut, "Ctrl+KeyV");
  assert.equal(DEFAULT_PASTE_SHORTCUT, "Ctrl+KeyV");
  assert.equal(p.s.insert.pasteOnCompletion, false);
  assert.match(html, /value="Ctrl\+KeyV" selected="">Ctrl\+V \(default\)/);
  assert.match(html, /value="Ctrl\+Shift\+KeyV">Ctrl\+Shift\+V \(terminals\)/);
  assert.match(html, /value="Shift\+Insert">Shift\+Insert/);
  assert.match(html, /aria-label="Record custom paste shortcut" title="Ctrl\+KeyV"/);
  assert.match(html, />Reset to Ctrl\+V</);
  assert.doesNotMatch(html, />Clear<|<button[^>]*disabled=/);
  for (const expected of [/clipboard-paste binding your target application actually uses/, /same binding applies to all targets/,
    /both completion and/, /Configuring it does not enable completion paste/, /not registered globally/,
    /Escape cancels/, /Enter and unmodified typing are not allowed/, /Avoid your recording hotkey/,
    /primary selection instead of the clipboard/, /leaves that selection unchanged/]) assert.match(html, expected);
  const other = props("other");
  other.desktop.status!.session = "X11";
  assert.doesNotMatch(render(InsertPage, other), /Paste shortcut preset|Custom paste shortcut/);
});

test("Paste presets, custom bindings and reset save only the shortcut", async () => {
  const p = props("linux", { insert: { ...settings.insert, method: "type", pasteIntoAnyWindow: false, pasteIntoDifferentWindow: true, trailingSpace: true } });
  p.desktop.status!.session = "X11";
  const saved: Settings[] = [];
  p.save = async (change) => {
    const next = structuredClone(p.s);
    change(next);
    saved.push(next);
    p.s = next;
    p.ov.settings = next;
    return true;
  };
  let preset: ComponentProps<"select">;
  let custom: ComponentProps<typeof HotkeyInput>;
  let reset: ComponentProps<"button">;
  function Probe() {
    const page = InsertPage(p);
    const visit = (node: React.ReactNode) => {
      if (!React.isValidElement<{ children?: React.ReactNode; "aria-label"?: string }>(node)) return;
      if (node.type === "select" && node.props["aria-label"] === "Paste shortcut preset") preset = node.props;
      if (node.type === HotkeyInput) custom = node.props as ComponentProps<typeof HotkeyInput>;
      React.Children.forEach(node.props.children, visit);
    };
    visit(page);
    return page;
  }
  function ResetProbe() {
    const input = HotkeyInput(custom);
    React.Children.forEach(input.props.children, (node) => {
      if (React.isValidElement<ComponentProps<"button">>(node) && node.props.children === "Reset to Ctrl+V") reset = node.props;
    });
    return input;
  }
  const before = structuredClone(p.s);
  const choices: [string, () => void][] = [
    ["Ctrl+Shift+KeyV", () => preset.onChange!({ target: { value: "Ctrl+Shift+KeyV" } } as React.ChangeEvent<HTMLSelectElement>)],
    ["Shift+Insert", () => preset.onChange!({ target: { value: "Shift+Insert" } } as React.ChangeEvent<HTMLSelectElement>)],
    ["Ctrl+KeyV", () => preset.onChange!({ target: { value: "Ctrl+KeyV" } } as React.ChangeEvent<HTMLSelectElement>)],
    ["Alt+KeyP", () => custom.onChange(accelerator(keyEvent("p", "KeyR", { altKey: true }), true)!)],
    ["Ctrl+KeyV", () => { render(ResetProbe, {}); reset.onClick!({} as React.MouseEvent<HTMLButtonElement>); }],
  ];
  for (const [index, [value, change]] of choices.entries()) {
    render(Probe, {});
    assert.equal(saved.length, index, "Rendering must not save or normalize a binding");
    assert.equal(custom!.paste, true);
    assert.equal(custom!.value, p.s.insert.pasteShortcut);
    change();
    assert.equal(saved.length, index + 1);
    assert.deepEqual(saved[index], { ...before, insert: { ...before.insert, pasteShortcut: value } });
  }
});

test("Imported paste bindings are displayed without silently selecting or saving a default", () => {
  for (const value of ["", "Ctrl+Enter", "Ctrl+Unknown", "Control+V", "Meta+Insert", " nonsense "]) {
    const p = props("linux", { insert: { ...settings.insert, pasteShortcut: value } });
    p.desktop.status!.session = "X11";
    const before = structuredClone(p.s);
    const html = render(InsertPage, p);
    assert.match(html, /value="" disabled="" selected="">Custom \/ imported binding/);
    assert.ok(html.includes(`title="${value}"`));
    assert.doesNotMatch(html, /value="Ctrl\+KeyV" selected=""/);
    assert.deepEqual(p.s, before);
  }
});

test("Paste capture uses logical letters across layouts and accepts presets and supported custom keys", () => {
  for (const [key, code] of [["v", "KeyB"], ["V", "Period"], ["v", "KeyW"]]) {
    assert.equal(accelerator(keyEvent(key, code, { ctrlKey: true, keyCode: 66 }), true), "Ctrl+KeyV");
    assert.equal(accelerator(keyEvent(key, code, { ctrlKey: true, shiftKey: true, keyCode: 66 }), true), "Ctrl+Shift+KeyV");
  }
  assert.equal(accelerator(keyEvent("Insert", "Insert", { shiftKey: true }), true), "Shift+Insert");
  assert.equal(accelerator(keyEvent("P", "KeyR", { altKey: true, shiftKey: true }), true), "Alt+Shift+KeyP");
  assert.equal(accelerator(keyEvent("v", "KeyB", { metaKey: true }), true), "Super+KeyV");
  assert.equal(accelerator(keyEvent("V", "KeyB", { ctrlKey: true, altKey: true, shiftKey: true, metaKey: true }), true), "Ctrl+Alt+Shift+Super+KeyV");
  assert.equal(accelerator(keyEvent("V", "KeyB", { shiftKey: true }), true), "Shift+KeyV");
  assert.equal(accelerator(keyEvent("7", "Digit1", { ctrlKey: true }), true), "Ctrl+Digit7");
  assert.equal(accelerator(keyEvent(" ", "Space", { ctrlKey: true }), true), "Ctrl+Space");
  for (const key of ["Insert", "Home", "End", "PageUp", "PageDown", "ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown", "Tab", "Backspace", "Delete"]) {
    assert.equal(accelerator(keyEvent(key, key, { ctrlKey: true }), true), `Ctrl+${key}`);
  }
  for (const [key, name] of [[";", "Semicolon"], ["=", "Equal"], [",", "Comma"], ["-", "Minus"], [".", "Period"], ["/", "Slash"],
    ["`", "Backquote"], ["[", "BracketLeft"], ["\\", "Backslash"], ["]", "BracketRight"], ["'", "Quote"]]) {
    assert.equal(accelerator(keyEvent(key, "Unidentified", { altKey: true }), true), `Alt+${name}`);
  }
  for (let i = 1; i <= 24; i++) assert.equal(accelerator(keyEvent(`F${i}`), true), `F${i}`);
});

test("Paste capture rejects Enter aliases, unmodified typing, modifier-only and unsupported keys", () => {
  for (const name of ["Enter", "Return", "NumpadEnter"]) {
    for (const event of [keyEvent(name, name), keyEvent(name, "KeyV", { ctrlKey: true }), keyEvent("v", name, { ctrlKey: true })]) {
      assert.equal(accelerator(event, true), null);
    }
  }
  for (const key of ["v", "7", " ", "Insert", "Tab", "Backspace", "Delete", ";", "ArrowLeft", "Escape", "F0", "F25"]) {
    assert.equal(accelerator(keyEvent(key), true), null);
  }
  for (const key of ["Control", "Shift", "Alt", "Meta", "Dead", "Unidentified", "AudioVolumeUp", "F25"]) {
    assert.equal(accelerator(keyEvent(key, key, { ctrlKey: true }), true), null);
  }
  assert.equal(accelerator(keyEvent("v", "KeyV", { ctrlKey: true, isComposing: true }), true), null);
  assert.equal(accelerator(keyEvent("v", "KeyV", { ctrlKey: true, altKey: true, getModifierState: (name) => name === "AltGraph" }), true), null);
  for (const [key, code] of [[":", "Period"], ["/", "Digit7"], ["7", "Digit7"], ["!", "Digit1"], ["+", "Equal"]]) {
    assert.equal(accelerator(keyEvent(key, code, { ctrlKey: true, shiftKey: true }), true), null, "Do not infer a logical base from shifted symbols or physical codes");
  }
});

test("Global accelerator and clear behavior stay unchanged by paste mode", () => {
  assert.equal(accelerator(keyEvent("v", "KeyB", { ctrlKey: true, keyCode: 66 })), "Ctrl+KeyB");
  assert.equal(accelerator(keyEvent("Insert", "Insert", { shiftKey: true })), null);
  assert.equal(accelerator(keyEvent("Enter", "Enter", { ctrlKey: true })), "Ctrl+Enter");
  assert.equal(accelerator(keyEvent("F8")), "F8");
  assert.equal(accelerator(keyEvent("v", "KeyV")), null);
  let cleared: string | null = null;
  let clear: ComponentProps<"button">;
  function Probe() {
    const input = HotkeyInput({ value: "Ctrl+KeyV", onChange: (value) => { cleared = value; } });
    React.Children.forEach(input.props.children, (node) => {
      if (React.isValidElement<ComponentProps<"button">>(node) && node.props.children === "Clear") clear = node.props;
    });
    return input;
  }
  const html = render(Probe, {});
  assert.match(html, />Clear</);
  assert.doesNotMatch(html, /Reset to Ctrl\+V|Record custom paste shortcut/);
  clear!.onClick!({} as React.MouseEvent<HTMLButtonElement>);
  assert.equal(cleared, "");
});

test("Paste recorder captures keys locally, rejects Enter, and cancels on Escape or blur", (t) => {
  const target = new EventTarget();
  const previous = Object.getOwnPropertyDescriptor(globalThis, "window");
  // Node's EventTarget does not match a boolean capture flag on removal like browsers do.
  Object.defineProperty(globalThis, "window", { configurable: true, value: {
    addEventListener: (type: string, listener: EventListener, capture: boolean) => target.addEventListener(type, listener, { capture }),
    removeEventListener: (type: string, listener: EventListener, capture: boolean) => target.removeEventListener(type, listener, { capture }),
  } });
  let recording = false;
  let effect: React.EffectCallback = () => {};
  let cleanup: ReturnType<React.EffectCallback>;
  let button: ComponentProps<"button">;
  const saved: string[] = [];
  t.mock.method(React, "useState", (() => [recording, (next: boolean) => { recording = next; }]) as typeof React.useState);
  t.mock.method(React, "useEffect", ((callback) => { effect = callback; }) as typeof React.useEffect);
  t.after(() => {
    if (typeof cleanup === "function") cleanup();
    if (previous) Object.defineProperty(globalThis, "window", previous);
    else Reflect.deleteProperty(globalThis, "window");
  });
  function Probe() {
    const input = HotkeyInput({ value: "Ctrl+KeyV", onChange: (value) => saved.push(value), linux: true, paste: true });
    button = input.props.children[0].props;
    return input;
  }
  const rerender = () => {
    if (typeof cleanup === "function") cleanup();
    const html = render(Probe, {});
    cleanup = effect();
    return html;
  };
  const press = (event: KeyboardEvent) => {
    const keydown = Object.assign(new Event("keydown", { cancelable: true }), event);
    target.dispatchEvent(keydown);
    assert.equal(keydown.defaultPrevented, true, "Recorded keys must not reach focused controls");
  };
  rerender();
  button!.onClick!({} as React.MouseEvent<HTMLButtonElement>);
  assert.match(rerender(), /Press the keys/);
  for (const event of [keyEvent("Enter", "Enter", { ctrlKey: true }), keyEvent("Enter", "NumpadEnter", { shiftKey: true }),
    keyEvent("Return", "Return", { altKey: true }), keyEvent("v", "KeyB"), keyEvent("Control", "ControlLeft", { ctrlKey: true })]) press(event);
  assert.equal(recording, true);
  assert.deepEqual(saved, []);
  press(keyEvent("V", "KeyB", { ctrlKey: true, shiftKey: true }));
  assert.equal(recording, false);
  assert.deepEqual(saved, ["Ctrl+Shift+KeyV"]);
  rerender();
  button!.onClick!({} as React.MouseEvent<HTMLButtonElement>);
  rerender();
  press(keyEvent("Insert", "Insert", { shiftKey: true }));
  assert.deepEqual(saved, ["Ctrl+Shift+KeyV", "Shift+Insert"]);
  rerender();
  button!.onClick!({} as React.MouseEvent<HTMLButtonElement>);
  rerender();
  press(keyEvent("Escape", "Escape"));
  assert.equal(recording, false);
  assert.equal(saved.length, 2);
  rerender();
  button!.onClick!({} as React.MouseEvent<HTMLButtonElement>);
  rerender();
  button!.onBlur!({} as React.FocusEvent<HTMLButtonElement>);
  assert.equal(recording, false);
  rerender();
  target.dispatchEvent(Object.assign(new Event("keydown"), keyEvent("v", "KeyV", { ctrlKey: true })));
  assert.equal(saved.length, 2, "Blur must remove the capture listener without saving");
});

test("Recorder distinguishes target timing and eligibility from completion opt-in", () => {
  const p = props();
  p.desktop.status!.session = "X11";
  const defaults = render(Recorder, p);
  assert.match(defaults, /Review and copy/);
  assert.match(defaults, /destination captured at recording start to be focused when delivery begins/);
  assert.match(defaults, /Starting in Amber does not select another app/);
  assert.match(defaults, /Any-window targeting is enabled; it does not turn on completion paste/);
  assert.doesNotMatch(defaults, /restricted to supported X11 editors|Paste on completion is enabled/);
  p.s.insert.pasteIntoAnyWindow = false;
  p.s.insert.pasteIntoDifferentWindow = true;
  p.s.insert.pasteOnCompletion = true;
  const changed = render(Recorder, p);
  assert.match(changed, /Paste on completion is enabled/);
  assert.match(changed, /focused window chosen once when delivery begins, not when recording stops/);
  assert.match(changed, /Targets are restricted to known native editors/);
  assert.match(changed, /focus must stay stable while sending the configured paste shortcut/);
  assert.match(changed, /same binding applies to all targets and explicit --insert-last/);
  assert.match(changed, /never presses Enter or switches focus/);
  assert.doesNotMatch(changed, /Ctrl\+V/);
  assert.doesNotMatch(changed, /Starting in Amber does not select another app|destination captured at recording start to be focused when delivery begins/);
  p.desktop.status!.session = "Wayland";
  const wayland = render(Recorder, p);
  assert.match(wayland, /Copy on completion is enabled/);
  assert.match(wayland, /Wayland and unknown sessions use manual paste/);
  assert.match(wayland, /focus Amber to copy/);
  assert.doesNotMatch(wayland, /Paste on completion is enabled|focused window chosen once|Targets are restricted/);
});

test("Explicit X11 paste help describes current targeting independently of completion", () => {
  const html = render(HotkeysPage, props());
  assert.match(html, /amber-dictation --insert-last/);
  assert.match(html, /captures the currently focused destination and respects Paste into any window, even with completion off/);
  assert.match(html, /single-line, control-free text; focus must stay stable/);
  assert.match(html, /paste shortcut configured in Delivery, never Enter or a focus switch/);
  assert.match(html, /same binding applies to all targets and completion paste/);
  assert.match(html, /Editable fields and paste acceptance cannot be verified/);
  assert.match(html, /Wayland requires Copy and manual paste/);
  assert.doesNotMatch(html, /For supported native editors|Wayland, terminals, multiline text|Ctrl\+V only/);
});

test("Linux keys and history report storage status rather than Windows encryption", () => {
  const credentials = render(CredentialsPage, props("linux", { server: { ...settings.server, provider: "openai" } }));
  assert.match(credentials, /available for this session/);
  assert.match(credentials, /Session only/);
  assert.doesNotMatch(credentials, /Windows account/);
  assert.match(render(CredentialsPage, props("windows")), /encrypted for your Windows account/);
  const custom = render(CredentialsPage, props());
  assert.match(custom, /Keys are scoped to the server/);
  assert.doesNotMatch(custom, /A key is available for this session/);
  const engine = render(EnginePage, props());
  assert.match(engine, /Manage keys/);
  assert.doesNotMatch(engine, /type="password"/);
  const history = render(HistoryPage, props());
  assert.match(history, /Persistent history is opt-in/);
  assert.match(history, /Unavailable/);
  assert.doesNotMatch(history, /Windows account/);
  assert.match(render(HistoryPage, props("windows")), /Windows account/);
});

test("Linux cleanup describes CPU execution while Windows keeps GPU wording", () => {
  const linux = render(CleanupPage, props());
  assert.match(linux, /llama.cpp on your CPU/);
  assert.doesNotMatch(linux, /on your GPU/);
  assert.match(render(CleanupPage, props("windows")), /on your GPU/);
});

test("Custom cleanup directs both platforms to address-scoped Credentials", (t) => {
  const providers: ChatProvider[] = [{ id: "custom", name: "Custom", models: [], hasKey: false }];
  const useState = React.useState;
  let seeded = 0;
  // SSR skips the provider-loading effect. Seed its data, not its markup, and keep React's real hook.
  t.mock.method(React, "useState", ((initial: unknown) => {
    if (Array.isArray(initial) && initial.length === 0) {
      seeded++;
      return useState(providers);
    }
    return useState(initial);
  }) as typeof React.useState);
  const cleanup: Settings["cleanup"] = { ...settings.cleanup, engine: "server", baseUrl: "https://cleanup.example/v1" };
  const linux = render(CleanupPage, props("linux", { cleanup }));
  assert.match(linux, /Manage address-scoped keys in Credentials/);
  assert.match(linux, /scheme, host or port/);
  assert.doesNotMatch(linux, /type="password"/);
  const windows = render(CleanupPage, props("windows", { cleanup }));
  assert.doesNotMatch(windows, /type="password"/);
  assert.match(windows, />Manage keys</);
  assert.equal(seeded, 2, "Seed only the provider list in each platform render");
});

test("Credentials manages Tinfoil with a local engine without saving any settings", () => {
  for (const platform of ["linux", "windows"] as const) {
    const p = props(platform, { engine: "local", cleanup: { ...settings.cleanup, engine: "server", provider: "tinfoil" } });
    const provider = {
      id: "tinfoil", name: "Tinfoil.sh", baseUrl: "https://inference.tinfoil.sh/v1", hasKey: false,
      keyUrl: "https://dash.tinfoil.sh", needsKey: true, models: [], supportsSpeech: true, supportsCleanup: true,
    };
    p.ov.providers.push(provider);
    const before = structuredClone(p.s);
    const html = render(CredentialsPage, p);
    assert.match(html, /Manage API keys independently/);
    assert.match(html, /Tinfoil.sh API key/);
    assert.match(html, /Saved keys are never displayed/);
    assert.match(html, />Save key</);
    assert.match(html, />Get a key</);
    assert.match(html, /does not change your Engine or Cleanup addresses/);
    assert.match(html, /Use speech address/);
    assert.match(html, /Use cleanup address/);
    let destination = "";
    const link = CredentialLink({ provider, go: (page) => { destination = page; } });
    const button = link.props.children as React.ReactElement<{ onClick: () => void }>;
    button.props.onClick();
    assert.equal(destination, "credentials");
    assert.deepEqual(p.s, before);
  }
});

test("Built-in runtime has no download or delete action while ordinary models retain both", () => {
  const item: ComponentProps<typeof DownloadItem> = {
    local: { info: null, progress: {}, errors: {}, reload: unexpectedAction, download: unexpectedAction },
    kind: "runtime", item: { id: "cpu", label: "CPU", note: "Embedded", sizeMb: 0, installed: true, wantsGpu: false, replaced: false, imported: false },
    selected: true, onSelect: unexpectedAction, toast: unexpectedAction,
  };
  const builtIn = render(DownloadItem, item);
  assert.match(builtIn, /Built into Amber; no download needed/);
  assert.doesNotMatch(builtIn, />Delete<|>Download<|0 MB|Interrupted downloads/);
  const model: ComponentProps<typeof DownloadItem> = { ...item, kind: "model", item: { ...item.item, sizeMb: 75 } };
  assert.match(render(DownloadItem, model), />Delete</);
  assert.match(render(DownloadItem, { ...model, item: { ...model.item, installed: false } }), />Download &amp; use</);
  assert.match(render(DownloadItem, { ...model, selectOnDownload: false, item: { ...model.item, installed: false } }), />Download</);
});

test("Download rows explain durable progress and cancellation keeps it for later", () => {
  const props: ComponentProps<typeof DownloadItem> = {
    local: { info: null, progress: {}, errors: {}, reload: unexpectedAction, download: unexpectedAction },
    kind: "model", item: { id: "test", label: "Test model", note: "", sizeMb: 75, installed: false, wantsGpu: false, replaced: false, imported: false },
    selected: false, onSelect: unexpectedAction, toast: unexpectedAction,
  };
  assert.match(render(DownloadItem, props), /Download again to resume, even after restarting Amber/);
  props.local.progress["model:test"] = { key: "model:test", done: 25, total: 100, state: "running" };
  const running = render(DownloadItem, props);
  assert.match(running, /Stop downloading and keep progress for later/);
  assert.match(running, />Cancel</);
  assert.match(running, /25%/);
  assert.doesNotMatch(render(DownloadItem, { ...props, item: { ...props.item, imported: true } }), /Interrupted downloads/);
});

test("Speech model browsing preserves the selected model and separates backend families", () => {
  const model = (id: string, family: LocalItem["family"], installed = false): LocalItem => ({
    id, family, label: id, note: "", sizeMb: 1, installed, wantsGpu: false, replaced: false, imported: false,
  });
  const models = [model("futo-multilingual-74", "whisper"), model("tiny-q5_1", "whisper", true),
    model("parakeet-v3-int8", "parakeet"), model("moonshine-tiny-int8", "moonshine")];
  const ids = (view: Parameters<typeof visibleSpeechModels>[1], selected = "tiny-q5_1") => visibleSpeechModels(models, view, selected).map((m) => m.id);
  assert.deepEqual(ids("recommended"), ["futo-multilingual-74", "tiny-q5_1"]);
  assert.deepEqual(ids("whisper"), ["futo-multilingual-74", "tiny-q5_1"]);
  assert.deepEqual(ids("parakeet"), ["tiny-q5_1", "parakeet-v3-int8"]);
  assert.deepEqual(ids("moonshine"), ["tiny-q5_1", "moonshine-tiny-int8"]);
  assert.deepEqual(ids("installed"), ["tiny-q5_1"]);
});

test("Speech controls distinguish unloading memory from loading and refuse busy UI actions", () => {
  const p = { busy: false, toast: unexpectedAction };
  assert.match(render(SpeechModelActions, { ...p, status: { state: "stopped" } }), />Load now</);
  assert.match(render(SpeechModelActions, { ...p, status: { state: "ready" } }), />Unload model</);
  assert.match(render(SpeechModelActions, { ...p, busy: true, status: { state: "ready" } }), /disabled=""/);
  assert.match(render(SpeechModelActions, { ...p, status: { state: "starting" } }), /disabled=""/);
  const failed = render(SpeechModelActions, { ...p, status: { state: "failed", message: "Failed to switch models" } });
  assert.match(failed, />Load now</);
  assert.match(failed, />Unload model</, "a failed switch can leave the previous model resident");
});

test("An installed ONNX model exposes runtime repair and does not offer irrelevant Whisper tuning", (t) => {
  const info: LocalInfo = {
    runtimes: [], models: [{ id: "parakeet-v3-int8", label: "Parakeet v3", family: "parakeet", note: "25 languages", installed: true, sizeMb: 671, wantsGpu: false, replaced: false, imported: false }],
    hasNvidia: false, recommended: ["cpu", "futo-multilingual-74"], measured: {}, status: { state: "ready", message: "Parakeet v3 on CPU" },
    downloads: [], folder: "/models", llmRuntimes: [], llmModels: [], llmStatus: { state: "stopped" }, onnxRuntimeInstalled: false, importsError: null,
  };
  const useState = React.useState;
  let suppliedInfo = false;
  t.mock.method(React, "useState", ((initial: unknown) => {
    if (initial === null && !suppliedInfo) { suppliedInfo = true; return useState(info); }
    return useState(initial);
  }) as typeof React.useState);
  const p = props("linux", { engine: "local", local: { ...settings.local, model: "parakeet-v3-int8" } });
  const html = render(EnginePage, p);
  assert.match(html, /Model library/);
  assert.match(html, />Install runtime</);
  assert.match(html, /119 MiB extra download/);
  assert.match(html, />Unload model</);
  assert.match(html, /The Whisper runtime choice does not apply/);
  assert.match(html, /Use downloads when finished/);
  assert.doesNotMatch(html, /CPU threads|Whisper runs on/);
});

test("Connections keep account keys independent and filter operations conservatively", () => {
  const p = props();
  p.s.connections = [{ id: "conn_speech", name: "My speech account", provider: "custom", baseUrl: "https://named.example/v1" },
    { id: "conn_chat", name: "My text account", provider: "anthropic", baseUrl: "https://api.anthropic.com/v1" }];
  p.ov.providers.push({ id: "anthropic", name: "Anthropic", baseUrl: "https://api.anthropic.com/v1", models: [], hasKey: false, needsKey: true, keyUrl: "", supportsSpeech: false, supportsCleanup: true });
  assert.deepEqual(availableConnections(p.s.connections, p.ov.providers, "speech").map((c) => c.id), ["conn_speech"]);
  assert.equal(availableConnections(p.s.connections, p.ov.providers, "cleanup").length, 2);
  const models = [{ id: "speech", label: "Speech", capabilities: ["speech" as const], note: "" },
    { id: "chat", label: "Chat", capabilities: ["cleanup" as const], note: "" }, { id: "unknown", label: "Unknown", capabilities: [], note: "" }];
  assert.deepEqual(eligibleModels(models, "speech", false).map((m) => m.id), ["speech"]);
  assert.deepEqual(eligibleModels(models, "cleanup", true).map((m) => m.id), ["chat", "unknown"]);
  const html = render(ConnectionsPage, p);
  assert.match(html, /independent key|own encrypted key/);
  assert.match(html, /never copied automatically/);
  assert.match(html, /My speech account/);
  assert.match(html, /Discover models/);
  p.s.server.connectionId = "conn_speech";
  const engine = render(EnginePage, p);
  assert.match(engine, /named.example/);
  assert.match(engine, /Discover models/);
  assert.doesNotMatch(engine, />Manage keys</);
  p.s.server.connectionId = "conn_missing";
  const missing = render(EnginePage, p);
  assert.match(missing, /Connection unavailable/);
  assert.doesNotMatch(missing, /Speech model ID/);
});

test("Instructions explain explicit opt-in, whole-word triggers and shell review-only delivery", () => {
  const p = props();
  const rule = { id: "bash-rule", ...BASH_EXAMPLE };
  assert.equal(rule.enabled, false);
  assert.equal(rule.output, "shell");
  assert.equal(instructionIssue(rule, []), null);
  assert.match(instructionIssue({ ...rule, id: "duplicate", keyword: "BASH" }, [rule])!, /already used/);
  for (const keyword of ["bash command", "bash-", "", "x".repeat(41)]) assert.ok(instructionIssue({ ...rule, keyword }, []));
  assert.equal(instructionKeyword("  BASH: list files"), "bash");
  assert.equal(instructionKeyword("bashful list files"), "bashful");
  assert.equal(instructionKeyword("bash-file list files"), null);
  assert.equal(instructionKeyword("\"bash\" list files"), null);
  p.s.instructions = [rule];
  const html = render(InstructionsPage, p);
  assert.match(html, /Instructions/);
  assert.match(html, /First word only/);
  assert.match(html, /review\/copy-only/);
  assert.match(html, /never automatically paste or execute/);
  assert.match(html, /New instructions start disabled/);
  assert.match(html, /These desktop rules are not applied to phone API requests/);
  p.s.cleanup.smart = false;
  assert.match(render(InstructionsPage, p), /Smart cleanup is off/);
});

test("Returning from a saved connection restores the default provider's model family", () => {
  const s = structuredClone(settings);
  s.server.provider = "groq";
  s.cleanup.provider = "groq";
  s.connections = [{ id: "conn_openai", name: "OpenAI", provider: "openai", baseUrl: "https://api.openai.com/v1" },
    { id: "conn_claude", name: "Claude", provider: "anthropic", baseUrl: "https://api.anthropic.com/v1" }];
  const providers = props().ov.providers.map((p) => ({ ...p, id: "groq", models: [{ id: "whisper-large-v3", label: "Whisper", note: "" }] }));
  providers.push({ ...providers[0], id: "openai", models: [{ id: "whisper-1", label: "Whisper", note: "" }] });
  const chats = [{ id: "groq", name: "Groq", hasKey: false, models: [{ id: "llama-3.3-70b-versatile", label: "Llama", note: "" }] },
    { id: "anthropic", name: "Claude", hasKey: false, models: [{ id: "claude-haiku-4-5", label: "Claude", note: "" }] }];
  chooseConnection(s, "speech", "conn_openai", providers, chats);
  assert.equal(s.server.model, "whisper-1");
  chooseConnection(s, "speech", null, providers, chats);
  assert.equal(s.server.model, "whisper-large-v3");
  chooseConnection(s, "cleanup", "conn_claude", providers, chats);
  assert.equal(s.cleanup.model, "claude-haiku-4-5");
  chooseConnection(s, "cleanup", null, providers, chats);
  assert.equal(s.cleanup.model, "llama-3.3-70b-versatile");
});

test("Missing imported files can be removed but cannot be downloaded as a catalog model", () => {
  const p: ComponentProps<typeof DownloadItem> = {
    local: { info: null, progress: {}, errors: {}, reload: unexpectedAction, download: unexpectedAction }, kind: "model",
    item: { id: `import-${"a".repeat(32)}`, label: "My model", note: "Imported", sizeMb: 30, installed: false, imported: true, wantsGpu: false, replaced: false, family: "moonshine" },
    selected: false, onSelect: unexpectedAction, toast: unexpectedAction,
  };
  const html = render(DownloadItem, p);
  assert.match(html, /files missing/);
  assert.match(html, />Delete</);
  assert.doesNotMatch(html, />Download/);
  assert.match(html, /disabled=""/);
  assert.deepEqual(visibleSpeechModels([p.item], "imported", "none"), [p.item]);
});

test("Cleanup-only providers have credentials but never appear as speech engines", () => {
  const p = props();
  p.ov.providers.push({ id: "anthropic", name: "Anthropic", baseUrl: "https://api.anthropic.com/v1", models: [],
    hasKey: false, keyUrl: "", needsKey: true, supportsSpeech: false, supportsCleanup: true });
  const speech = render(EnginePage, p);
  assert.doesNotMatch(speech, /Anthropic/);
  assert.match(speech, /Speech model ID/);
  const credentials = render(CredentialsPage, p);
  assert.match(credentials, /Anthropic API key/);
  assert.match(credentials, /Smart cleanup only/);
  assert.match(credentials, /Speech recognition and smart cleanup/);
});

test("Recorder provides stop/cancel, safe processing controls, failed retry and the ready label", () => {
  const recording = render(Recorder, { ...props(), phase: "recording" });
  assert.match(recording, />Stop recording</);
  assert.match(recording, />Cancel</);
  const processing = render(Recorder, { ...props(), phase: "transcribing" });
  assert.match(processing, /disabled="">Record</);
  assert.match(processing, />Cancel</);
  const failed = render(Recorder, { ...props(), phase: "error", phaseMessage: "Network unavailable" });
  assert.match(failed, /Network unavailable/);
  assert.match(failed, />Retry recognition</);
  assert.match(render(Recorder, { ...props(), phase: "ready" }), /Result ready/);
  assert.equal(phaseLabel("ready"), "Result ready");
});

test("Compact recorder keeps stop and cancel available away from the recorder page", () => {
  const compact = render(Recorder, { ...props(), phase: "recording", compact: true });
  assert.match(compact, />Stop recording</);
  assert.match(compact, />Cancel</);
  assert.match(compact, />Open recorder</);
  assert.doesNotMatch(compact, /Latest result|Desktop capabilities/);
});

test("Empty recorder shows manual fallback and capability diagnostics without a Copy action", () => {
  const html = render(Recorder, props());
  assert.match(html, /No result yet/);
  assert.match(html, /amber-dictation --toggle/);
  assert.match(html, /amber-dictation --cancel/);
  assert.match(html, /Manual copy only/);
  assert.match(html, /Session only/);
  assert.match(html, /Lock and suspend/);
  assert.match(html, /Persistent history is off/);
  assert.doesNotMatch(html, />Copy</);
});

test("Tinfoil exposes speech and cleanup models without claiming key or enclave verification", (t) => {
  const providers: ChatProvider[] = [{
    id: "tinfoil", name: "Tinfoil.sh", hasKey: true,
    models: [{ id: "deepseek-v4-1-flash", label: "DeepSeek V4.1 Flash", note: "thinking off" }],
  }];
  const useState = React.useState;
  t.mock.method(React, "useState", ((initial: unknown) =>
    useState(Array.isArray(initial) && initial.length === 0 ? providers : initial)
  ) as typeof React.useState);
  for (const platform of ["linux", "windows"] as const) {
    const p = props(platform, {
      server: { ...settings.server, provider: "tinfoil", model: "voxtral-small-24b" },
      cleanup: { ...settings.cleanup, engine: "server", provider: "tinfoil", model: "deepseek-v4-1-flash" },
    });
    p.ov.providers = [{
      id: "tinfoil", name: "Tinfoil.sh", baseUrl: "https://inference.tinfoil.sh/v1", hasKey: true,
      keyUrl: "https://dash.tinfoil.sh", needsKey: true, supportsSpeech: true, supportsCleanup: true,
      models: [{ id: "voxtral-small-24b", label: "Voxtral Small 24B", note: "recommended" }],
    }];
    const engine = render(EnginePage, p);
    assert.match(engine, /value="voxtral-small-24b"/);
    assert.match(engine, />Voxtral Small 24B</);
    assert.match(engine, /Public catalogs, including Tinfoil&#x27;s, do not validate the API key/);
    assert.match(engine, /Audio upload/);
    assert.match(engine, /WAV \(lossless, default\)/);
    const compressed = render(EnginePage, { ...p, s: { ...p.s, server: { ...p.s.server, uploadFormat: "opus" } } });
    assert.match(compressed, /selected="">Ogg Opus \(lossy, ~32 kbps\)/);
    assert.match(compressed, /published Voxtral server source accepts Ogg Opus/);
    assert.match(compressed, /Live authenticated inference has not been verified/);
    assert.match(compressed, /local recognition, fallback and retained test recordings keep the original WAV/);
    const cleanup = render(CleanupPage, p);
    assert.match(cleanup, /value="deepseek-v4-1-flash"/);
    assert.match(cleanup, />DeepSeek V4.1 Flash</);
    assert.match(cleanup, /Uses the Tinfoil.sh key/);
    assert.match(cleanup, /thinking disabled/);
    for (const html of [engine, cleanup]) {
      assert.match(html, /https:\/\/inference.tinfoil.sh\/v1/);
      assert.match(html, /does not verify Tinfoil&#x27;s enclave attestation/);
      assert.doesNotMatch(html, />Address</);
    }
  }
});

test("Testing explains isolated delivery and explicit encrypted table saving", () => {
  for (const platform of ["linux", "windows"] as const) {
    const html = render(TestingPage, props(platform));
    assert.match(html, /Start test recording/);
    assert.match(html, /Stop and recognize/);
    assert.match(html, /Cancel test/);
    assert.match(html, /Tests never paste, type into other windows, or change the clipboard/);
    assert.match(html, /Up to 20 current results and 64 MiB of audio in memory/);
    assert.match(html, /Tests never enter ordinary history or statistics/);
    assert.match(html, /Only explicitly saved tables survive quitting/);
    assert.match(html, /Settings are captured when you start/);
    assert.match(html, /Wall times include model setup, encoding, retries and local fallback/);
    assert.match(html, /never a previously compressed copy/);
    assert.doesNotMatch(html, />Record</);
  }
});

test("A comparison shows its captured models, fallback, separate timings and both texts", () => {
  const result: TestResult = {
    id: 7, at: 1_800_000_000_000, recordingId: 7, sourceTestId: null, canReplay: true, config: structuredClone(settings),
    microphone: "Test microphone", microphoneFallback: false, interrupted: false,
    audioMs: 5000, preparedAudioMs: 4500, recognitionMs: 1200, cleanupMs: 350,
    attempts: [], actualEngine: { engine: "local", provider: "local", model: "tiny-captured", runtime: "cpu", baseUrl: null },
    cleanupEngine: { engine: "server", provider: "tinfoil", model: "deepseek-captured", runtime: null, baseUrl: "https://inference.tinfoil.sh/v1" },
    fallback: true, cleanupStatus: "success", cleanupError: null, remoteAudio: true, remoteText: true,
    raw: "hello um world", text: "Hello world.", error: null,
  };
  const controls = { busy: false, replayDisabled: false, remove: unexpectedAction, replay: unexpectedAction };
  const html = render(TestResultDetails, { result, ...controls });
  assert.match(html, /Test 7 \/ Local fallback/);
  assert.match(html, /Recognition engine: Local \/ cpu \/ tiny-captured/);
  assert.match(html, /Cleanup engine: tinfoil \/ deepseek-captured/);
  assert.match(html, /Recognition wall time: <strong>1.20 s \(1200 ms\)/);
  assert.match(html, /Cleanup wall time: <strong>0.35 s \(350 ms\)/);
  assert.match(html, /Raw recognized text/);
  assert.match(html, /hello um world/);
  assert.match(html, /Final cleaned text/);
  assert.match(html, /Hello world\./);
  assert.match(html, /including before local fallback/);
  const skipped = render(TestResultDetails, { result: { ...result, fallback: false, cleanupStatus: "error", cleanupError: "Mock cleanup failed", cleanupMs: null }, ...controls });
  assert.match(skipped, /Ready with cleanup skipped/);
  assert.match(skipped, /Mock cleanup failed/);
  assert.match(skipped, /Cleanup wall time: <strong>Not run/);
});
