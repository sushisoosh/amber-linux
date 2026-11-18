/// <reference types="node" />

import assert from "node:assert/strict";
import test from "node:test";
import React, { createElement, type ComponentProps, type ComponentType } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import type { ChatProvider, Overview, Settings, TestResult } from "../shared/types";
import type { PageProps } from "./App";
import { DownloadItem } from "./downloads";
import { AppPage } from "./pages/AppPage";
import { CleanupPage } from "./pages/Cleanup";
import { CredentialLink, CredentialsPage } from "./pages/Credentials";
import { EnginePage } from "./pages/Engine";
import { HistoryPage } from "./pages/History";
import { HotkeysPage, pretty } from "./pages/Hotkeys";
import { InsertPage } from "./pages/Insert";
import { MicrophonePage } from "./pages/Microphone";
import { Recorder } from "./pages/Recorder";
import { TestingPage, TestResultDetails } from "./pages/Testing";
import { phaseLabel } from "./ui";

const settings: Settings = {
  engine: "server", local: { runtime: "cpu", model: "tiny", threads: 0, keepLoaded: false, chunking: false },
  server: { provider: "custom", baseUrl: "https://speech.example/v1", model: "whisper", uploadFormat: "wav", localFallback: false },
  language: "en", vocabulary: "", microphone: "", hotkey: { dictate: "Super+KeyD", mode: "toggle", toggleBubble: "" },
  bubble: { visibility: "hidden", edge: "left", y: 0.5, monitor: "", size: 64, opacity: 1, hiddenIn: [], hideFullscreen: false },
  insert: { method: "paste", pasteOnCompletion: false, restoreClipboard: false, trailingSpace: false }, sounds: false,
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

test("Delivery exposes optional Ctrl+V without promising unsupported Linux insertion", () => {
  const linux = render(InsertPage, props());
  assert.match(linux, /Copy and paste completed dictation \(Ctrl\+V\)/);
  assert.match(linux, /Off by default/);
  assert.match(linux, /Manual paste on this desktop/);
  assert.match(linux, /Wayland and unknown sessions/);
  assert.match(linux, /Linux does not restore/);
  assert.match(linux, /completed text goes to the clipboard even when automatic paste is unavailable/);
  assert.match(linux, /Testing recordings never change the clipboard/);
  assert.doesNotMatch(linux, /Restore the clipboard|Try it|As keystrokes/);
  const p = props();
  p.desktop.status!.session = "x11";
  const x11 = render(InsertPage, p);
  assert.match(x11, /Restricted X11 paste/);
  assert.match(x11, /Terminals, browsers, dialogs and unknown targets/);
  const windows = render(InsertPage, props("windows"));
  assert.match(windows, /Paste \(Ctrl\+V\)/);
  assert.match(windows, /Restore the clipboard/);
  assert.match(windows, /Try it/);
  assert.doesNotMatch(windows, /Manual paste on this desktop|Off by default/);
  assert.match(render(Recorder, props()), /Configure delivery/);
  assert.match(render(HotkeysPage, props()), /does not open or focus Amber/);
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
      keyUrl: "https://dash.tinfoil.sh", needsKey: true, models: [],
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
      keyUrl: "https://dash.tinfoil.sh", needsKey: true,
      models: [{ id: "voxtral-small-24b", label: "Voxtral Small 24B", note: "recommended" }],
    }];
    const engine = render(EnginePage, p);
    assert.match(engine, /selected="">Voxtral Small 24B</);
    assert.match(engine, /Public catalogs, including Tinfoil&#x27;s, do not validate the API key/);
    assert.match(engine, /Audio upload/);
    assert.match(engine, /WAV \(lossless, default\)/);
    const compressed = render(EnginePage, { ...p, s: { ...p.s, server: { ...p.s.server, uploadFormat: "opus" } } });
    assert.match(compressed, /selected="">Ogg Opus \(lossy, ~32 kbps\)/);
    assert.match(compressed, /published Voxtral server source accepts Ogg Opus/);
    assert.match(compressed, /Live authenticated inference has not been verified/);
    assert.match(compressed, /local recognition, fallback and retained test recordings keep the original WAV/);
    const cleanup = render(CleanupPage, p);
    assert.match(cleanup, /selected="">DeepSeek V4.1 Flash</);
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
