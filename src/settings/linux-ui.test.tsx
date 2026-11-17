/// <reference types="node" />

import assert from "node:assert/strict";
import test from "node:test";
import React, { createElement, type ComponentProps, type ComponentType } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import type { ChatProvider, Overview, Settings } from "../shared/types";
import type { PageProps } from "./App";
import { DownloadItem } from "./downloads";
import { AppPage } from "./pages/AppPage";
import { CleanupPage } from "./pages/Cleanup";
import { EnginePage } from "./pages/Engine";
import { HistoryPage } from "./pages/History";
import { HotkeysPage, pretty } from "./pages/Hotkeys";
import { MicrophonePage } from "./pages/Microphone";
import { Recorder } from "./pages/Recorder";
import { phaseLabel } from "./ui";

const settings: Settings = {
  engine: "server", local: { runtime: "cpu", model: "tiny", threads: 0, keepLoaded: false, chunking: false },
  server: { provider: "custom", baseUrl: "https://speech.example/v1", model: "whisper", localFallback: false },
  language: "en", vocabulary: "", microphone: "", hotkey: { dictate: "Super+KeyD", mode: "toggle", toggleBubble: "" },
  bubble: { visibility: "hidden", edge: "left", y: 0.5, monitor: "", size: 64, opacity: 1, hiddenIn: [], hideFullscreen: false },
  insert: { method: "paste", restoreClipboard: false, trailingSpace: false }, sounds: false,
  autostart: false, autoUpdate: true, logText: false, onboarded: true,
  phone: { enabled: false, port: 8990, cleanup: false, tunnel: false, publicHost: "", keys: [] },
  profiles: [], history: false, historyDays: 7, statistics: false, snippets: [],
  discord: { muteWhileDictating: true },
  terminal: { enabled: true, automatic: true, safeMode: true, runCommands: false, hotkey: "Ctrl+KeyT", prefixes: [] },
  cleanup: {
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
      providers: [{ id: s.server.provider, name: "Test provider", baseUrl: s.server.baseUrl, models: [], hasKey: true, keyUrl: "", needsKey: false }],
    },
    phase: "idle", phaseMessage: null, compact: false,
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

test("Linux keys and history report storage status rather than Windows encryption", () => {
  const engine = render(EnginePage, props("linux", { server: { ...settings.server, provider: "openai" } }));
  assert.match(engine, /available for this session/);
  assert.match(engine, /Session only/);
  assert.doesNotMatch(engine, /Windows account|NVIDIA GPU/);
  assert.match(render(EnginePage, props("windows")), /encrypted for your Windows account/);
  const custom = render(EnginePage, props());
  assert.match(custom, /Keys are scoped to the server/);
  assert.doesNotMatch(custom, /A key is available for this session/);
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

test("Custom cleanup offers origin-scoped Linux credentials without changing Windows controls", (t) => {
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
  assert.match(linux, /API key for this server/);
  assert.match(linux, /Keys are scoped to scheme, host and port/);
  assert.match(linux, /Custom cleanup server API key/);
  assert.match(linux, /Session only/);
  const windows = render(CleanupPage, props("windows", { cleanup }));
  assert.doesNotMatch(windows, /Custom cleanup server API key/);
  assert.match(windows, />Add key</);
  assert.equal(seeded, 2, "Seed only the provider list in each platform render");
});

test("Built-in runtime has no download or delete action while ordinary models retain both", () => {
  const item: ComponentProps<typeof DownloadItem> = {
    local: { info: null, progress: {}, errors: {}, reload: unexpectedAction, download: unexpectedAction },
    kind: "runtime", item: { id: "cpu", label: "CPU", note: "Embedded", sizeMb: 0, installed: true, wantsGpu: false, replaced: false },
    selected: true, onSelect: unexpectedAction, toast: unexpectedAction,
  };
  const builtIn = render(DownloadItem, item);
  assert.match(builtIn, /Built into Amber; no download needed/);
  assert.doesNotMatch(builtIn, />Delete<|>Download<|0 MB/);
  const model: ComponentProps<typeof DownloadItem> = { ...item, kind: "model", item: { ...item.item, sizeMb: 75 } };
  assert.match(render(DownloadItem, model), />Delete</);
  assert.match(render(DownloadItem, { ...model, item: { ...model.item, installed: false } }), />Download</);
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
