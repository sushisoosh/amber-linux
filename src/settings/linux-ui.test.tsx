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
import { HistoryPage } from "./pages/History";
import { HotkeysPage, pretty } from "./pages/Hotkeys";
import { InsertPage } from "./pages/Insert";
import { MicrophonePage } from "./pages/Microphone";
import { Recorder } from "./pages/Recorder";
import { TestingPage, TestResultDetails } from "./pages/Testing";
import { phaseLabel, Row, Switch } from "./ui";
import { availableConnections, chooseConnection, eligibleModels } from "./connections";
import { ConnectionsPage } from "./pages/Connections";
import { BASH_EXAMPLE, instructionIssue, instructionKeyword, InstructionsPage } from "./pages/Instructions";
import "./history.test";

const settings: Settings = {
  engine: "server", local: { runtime: "cpu", model: "tiny", threads: 0, keepLoaded: false, chunking: false },
  server: { connectionId: null, provider: "custom", baseUrl: "https://speech.example/v1", model: "whisper", uploadFormat: "wav", localFallback: false },
  connections: [], instructions: [],
  language: "en", vocabulary: "", microphone: "", hotkey: { dictate: "Super+KeyD", mode: "toggle", toggleBubble: "" },
  bubble: { visibility: "hidden", edge: "left", y: 0.5, monitor: "", size: 64, opacity: 1, hiddenIn: [], hideFullscreen: false },
  insert: { method: "paste", pasteOnCompletion: false, pasteIntoAnyWindow: true, pasteIntoDifferentWindow: false, restoreClipboard: false, trailingSpace: false }, sounds: false,
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
  assert.match(linux, /Copy and paste completed dictation \(Ctrl\+V\)/);
  assert.match(linux, /Off by default/);
  assert.match(linux, /Manual paste on this desktop/);
  assert.match(linux, /Wayland and unknown sessions/);
  assert.match(linux, /Linux does not restore/);
  assert.match(linux, /completed text goes to the clipboard even when automatic paste is unavailable/);
  assert.match(linux, /Testing recordings never change the clipboard/);
  assert.doesNotMatch(linux, /Restore the clipboard|Try it|As keystrokes|Paste into any window|Allow pasting into a different window/);
  const unknown = props();
  unknown.desktop.status = null;
  assert.match(render(InsertPage, unknown), /Manual paste on this desktop/);
  assert.doesNotMatch(render(InsertPage, unknown), /Paste into any window|Allow pasting into a different window/);
  const win = props("windows");
  win.desktop.status!.session = "X11";
  const windows = render(InsertPage, win);
  assert.match(windows, /Paste \(Ctrl\+V\)/);
  assert.match(windows, /Restore the clipboard/);
  assert.match(windows, /Try it/);
  assert.doesNotMatch(windows, /Manual paste on this desktop|Off by default|Paste into any window|Allow pasting into a different window/);
  assert.match(render(Recorder, props()), /Configure delivery/);
  assert.match(render(HotkeysPage, props()), /does not open or focus Amber/);
});

test("X11 target switches default to any-window on and different-window off without enabling completion", () => {
  const p = props();
  p.desktop.status!.session = "x11";
  const html = render(InsertPage, p);
  assert.match(html, /role="switch" aria-checked="false" aria-label="Copy and paste completed dictation \(Ctrl\+V\)"/);
  assert.match(html, /role="switch" aria-checked="true" aria-label="Paste into any window"/);
  assert.match(html, /role="switch" aria-checked="false" aria-label="Allow pasting into a different window"/);
  assert.doesNotMatch(html, /disabled=|Restricted X11 paste/);
  for (const expected of [/On by default/, /browsers, chat apps, dialogs and other window types/, /--insert-last/, /does not enable completion paste/,
    /Turn off to restrict targets to known native editors/, /currently focused window once, when delivery begins, not when recording stops/,
    /Focus changes during paste still block it/, /single-line text without control characters/, /observable, outside Amber/,
    /stable window and input focus/, /never switches focus or presses Enter/, /password fields/, /paste acceptance cannot be verified/,
    /Terminals may need a different paste shortcut/, /Clipboard managers may retain/, /Keep Amber running/]) {
    assert.match(html, expected);
  }
  p.s.insert.pasteIntoAnyWindow = false;
  p.s.insert.pasteIntoDifferentWindow = true;
  const changed = render(InsertPage, p);
  assert.match(changed, /role="switch" aria-checked="false" aria-label="Paste into any window"/);
  assert.match(changed, /role="switch" aria-checked="true" aria-label="Allow pasting into a different window"/);
  assert.match(changed, /role="switch" aria-checked="false" aria-label="Copy and paste completed dictation \(Ctrl\+V\)"/);
});

test("X11 target switches save independently while completion is off and retain their values when it changes", async () => {
  const p = props("linux", { insert: { ...settings.insert, method: "type" } });
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
    ["Copy and paste completed dictation (Ctrl+V)", { pasteOnCompletion: true, method: "paste" }],
    ["Copy and paste completed dictation (Ctrl+V)", { pasteOnCompletion: false }],
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
  assert.match(changed, /focus must stay stable while sending Ctrl\+V/);
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
  assert.match(html, /Ctrl\+V only, never Enter or a focus switch/);
  assert.match(html, /Editable fields and paste acceptance cannot be verified/);
  assert.match(html, /Terminals may need another shortcut; Wayland requires Copy and manual paste/);
  assert.doesNotMatch(html, /For supported native editors|Wayland, terminals, multiline text/);
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
