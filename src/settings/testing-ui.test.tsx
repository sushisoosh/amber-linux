/// <reference types="node" />

import assert from "node:assert/strict";
import test from "node:test";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import type { Overview, Phase, Settings, TestResult, TestingState } from "../shared/types";
import type { PageProps } from "./App";
import { ComparisonTable, TestingPage, TestResultDetails } from "./pages/Testing";

const result: TestResult = {
  id: 7, at: 1700000000000, recordingId: 3, sourceTestId: 3, canReplay: true,
  config: {
    engine: "server", local: { runtime: "cpu", model: "local-snapshot", threads: 4, keepLoaded: true, chunking: true },
    server: { provider: "custom", baseUrl: "https://speech.example/v1", model: "requested-cloud", uploadFormat: "opus", localFallback: true },
    cleanup: { smart: true, engine: "server", fillers: true, spokenPunctuation: false, germanCommas: true, localRuntime: "cpu", localModel: "local-cleaner", provider: "custom", baseUrl: "https://cleanup.example/v1", model: "old-cleaner" },
    language: "en", vocabulary: "Amber", microphone: "Selected microphone",
  },
  microphone: "Actual microphone", microphoneFallback: true, interrupted: false,
  audioMs: 4500, preparedAudioMs: 3200, recognitionMs: 1234, cleanupMs: 567,
  attempts: [{ engine: { engine: "server", provider: "custom", model: "requested-cloud", baseUrl: "https://speech.example/v1", runtime: null }, ms: 100, error: "Unavailable", upload: { format: "opus", inputBytes: 320044, uploadBytes: 40000, encodingMs: 4, note: "Request did not complete." } }],
  actualEngine: { engine: "local", provider: "local", model: "actual-local", runtime: "cpu", baseUrl: null },
  cleanupEngine: { engine: "server", provider: "custom", model: "old-cleaner", baseUrl: "https://cleanup.example/v1", runtime: null },
  fallback: true, cleanupStatus: "error", cleanupError: "Cleanup unavailable", remoteAudio: true, remoteText: true,
  raw: "Raw <speech>", text: "Rule text", error: null,
};

const controls = { busy: false, replayDisabled: false, remove: () => assert.fail("SSR action"), replay: () => assert.fail("SSR action") };

test("comparison uses saved actual engines, separate timings and both texts", () => {
  const html = renderToStaticMarkup(<TestResultDetails result={result} {...controls} />);
  for (const expected of [/Local fallback/, /actual-local/, /old-cleaner/, /1234 ms/, /567 ms/, /Raw &lt;speech&gt;/, /Rule text/, /rules kept/, /Cleanup unavailable/, /default device fallback/, /4.*Local fallback/, /requested-cloud/]) {
    assert.match(html, expected);
  }
  assert.match(html, /Audio may have been sent/);
  assert.match(html, /Text may have been sent/);
  assert.match(html, /Recording #3 \/ replay of test #3/);
  assert.match(html, /Run again with current settings/);
  assert.match(html, /This replay did not open a microphone/);
  assert.match(html, /Audio file for attempt 1: Ogg Opus/);
  assert.match(html, /312.5 KiB original WAV to 39.1 KiB upload file/);
  assert.match(html, /4 ms/);
  assert.match(html, /included in recognition time/);
  assert.match(html, /Request did not complete/);
  assert.doesNotMatch(html, />Copy<|>Paste<|quality score/i);
});

test("failed and skipped stages remain visible and cannot masquerade as zero-time success", () => {
  const html = renderToStaticMarkup(<TestResultDetails result={{ ...result, error: "Nothing heard", actualEngine: null, recognitionMs: null, cleanupMs: null, cleanupEngine: null, cleanupStatus: "notRun", raw: "", text: "", attempts: [] }} {...controls} busy={true} />);
  assert.match(html, /Nothing heard/);
  assert.match(html, /Not run/);
  assert.match(html, /No engine request started/);
  assert.match(html, /disabled=""[^>]*>Discard/);
});

test("replay is disabled for expired audio and while another run is active", () => {
  for (const unavailable of [{ ...controls, result: { ...result, canReplay: false } }, { ...controls, result, replayDisabled: true }]) {
    const html = renderToStaticMarkup(<TestResultDetails {...unavailable} />);
    assert.match(html, /disabled=""[^>]*aria-label="Replay test 7 with current settings"/);
  }
  const expired = renderToStaticMarkup(<TestResultDetails result={{ ...result, canReplay: false }} {...controls} />);
  assert.match(expired, /expired from the memory budget/);
  assert.match(expired, /Raw &lt;speech&gt;/);
});

test("comparison table aligns timings and actual settings without double-counting encoding", () => {
  const html = renderToStaticMarkup(<ComparisonTable id="current" title="Current" results={[result]} {...controls} />);
  assert.match(html, /<table class="comparison-table"/);
  for (const label of ["Recognition model", "Cleanup model", "Opus", "Smart", "Encode", "Recognize", "Cleanup", "Total", "Result preview"]) assert.match(html, new RegExp(label));
  assert.match(html, /scope="row"/);
  assert.match(html, /aria-label="Compressed upload prepared: Yes"/);
  assert.match(html, /aria-label="Smart cleanup enabled: Yes"/);
  assert.match(html, /title="0.00 s \(4 ms\)"/);
  assert.match(html, /title="1.23 s \(1234 ms\)"/);
  assert.match(html, /title="0.57 s \(567 ms\)"/);
  assert.match(html, /title="1.80 s \(1801 ms\)"/);
  assert.doesNotMatch(html, /1805 ms/);
  assert.match(html, /actual-local/);
  assert.match(html, /old-cleaner/);
  assert.match(html, /Local fallback/);
  assert.match(html, /Rule text/);
  assert.match(html, /aria-expanded="false"/);
  assert.doesNotMatch(html, /<textarea/);
});

test("compression flags reflect actual uploads, with unknown stages kept distinct from zero", () => {
  const wav = { ...result, attempts: result.attempts.map((a) => ({ ...a, upload: { ...a.upload!, format: "wav", uploadBytes: 320044, encodingMs: 0 } })) };
  const local = { ...result, id: 8, attempts: [], recognitionMs: null, cleanupMs: null, actualEngine: null, cleanupEngine: null, error: "No model", config: { ...result.config, cleanup: { ...result.config.cleanup, smart: false } } };
  const html = renderToStaticMarkup(<ComparisonTable id="formats" title="Formats" results={[wav, local]} {...controls} />);
  assert.match(html, /aria-label="Compressed upload prepared: No"/);
  assert.match(html, /Used WAV/);
  assert.match(html, /title="0.00 s \(0 ms\)"/);
  assert.match(html, /aria-label="Compressed upload prepared: Not applicable \/ not run"/);
  assert.match(html, /aria-label="Smart cleanup enabled: No"/);
  assert.match(html, /Not run or incomplete; not a zero-time success/);
  assert.match(html, /Failed; open Details/);
  const noUpload = { ...result, attempts: result.attempts.map((a) => ({ ...a, upload: { ...a.upload!, uploadBytes: 0, encodingMs: 0 } })) };
  const notStarted = renderToStaticMarkup(<ComparisonTable id="blocked" title="Blocked" results={[noUpload]} {...controls} />);
  assert.doesNotMatch(notStarted, /title="0.00 s \(0 ms\)"/);
  assert.match(notStarted, /Compressed upload prepared: Not applicable \/ not run/);
});

function pageProps(phase: Phase, platform: Overview["platform"]): PageProps {
  const unexpected = () => assert.fail("SSR must not perform an application action");
  const s: Settings = {
    ...result.config,
    hotkey: { dictate: "", mode: "toggle", toggleBubble: "" },
    bubble: { visibility: "hidden", edge: "left", y: 0.5, monitor: "", size: 56, opacity: 1, hiddenIn: [], hideFullscreen: false },
    insert: { method: "paste", pasteOnCompletion: true, restoreClipboard: false, trailingSpace: true },
    sounds: false, autostart: false, autoUpdate: false, logText: false, onboarded: true,
    phone: { enabled: false, port: 8990, cleanup: false, tunnel: false, publicHost: "", keys: [] },
    profiles: [], history: false, historyDays: 7, statistics: false, snippets: [], discord: { muteWhileDictating: false },
    terminal: { enabled: false, automatic: false, safeMode: true, runCommands: false, hotkey: "", prefixes: [] },
  };
  return {
    s, ov: { platform, settings: s, phase, providers: [], version: "test" }, phase, phaseMessage: null, settingsSaving: false,
    save: unexpected, refresh: unexpected, toast: unexpected, go: unexpected,
    desktop: { status: null, storage: null, loading: false, error: null, refresh: unexpected },
  };
}

test("ordinary Linux dictation shows a busy status and route back to Recorder", () => {
  for (const phase of ["recording", "transcribing", "inserting"] as const) {
    const html = renderToStaticMarkup(<TestingPage {...pageProps(phase, "linux")} />);
    assert.match(html, /role="status"[^>]*>Ordinary dictation in progress/);
    assert.match(html, /disabled=""[^>]*>Start test recording/);
    assert.match(html, /Finish it before starting a test/);
    assert.match(html, />Open Recorder</);
    assert.match(html, /wait for delivery to finish/);
  }
});

test("Recorder navigation is Linux-only and the conflict notice disappears when idle", () => {
  const windows = renderToStaticMarkup(<TestingPage {...pageProps("recording", "windows")} />);
  assert.match(windows, /Ordinary dictation in progress/);
  assert.doesNotMatch(windows, />Open Recorder</);
  const idle = renderToStaticMarkup(<TestingPage {...pageProps("idle", "linux")} />);
  assert.doesNotMatch(idle, /Ordinary dictation in progress|>Open Recorder</);
});

test("comparisons wait for pending settings instead of replaying an old selection", () => {
  const html = renderToStaticMarkup(<TestingPage {...pageProps("idle", "linux")} settingsSaving={true} />);
  assert.match(html, /New recordings and replays wait until your selections are saved/);
  assert.match(html, /disabled=""[^>]*>Start test recording/);
});

test("current and saved comparisons are separate tables with explicit durable storage", (t) => {
  const snapshot: TestingState = {
    revision: 5, phase: "idle", active: null, results: [result], audioBytes: 320044, recordingCount: 1,
    savedTables: [{ id: 1, name: "CPU <baseline>", savedAt: result.at, results: [{ ...result, canReplay: false }] }],
    tableBusy: false, tableStorageError: null,
  };
  const original = React.useState;
  let calls = 0;
  t.mock.method(React, "useState", ((initial: unknown) => original(calls++ === 0 ? snapshot : initial)) as typeof React.useState);
  const html = renderToStaticMarkup(<TestingPage {...pageProps("idle", "linux")} />);
  assert.equal((html.match(/<table class="comparison-table"/g) ?? []).length, 2);
  assert.match(html, />Save table &amp; start new</);
  assert.match(html, />Clear table</);
  assert.match(html, /Saved: CPU &lt;baseline&gt;/);
  assert.match(html, />Delete saved table</);
  assert.match(html, /requires an unlocked Secret Service wallet/);
  assert.match(html, /after restarting, the table remains/);
  assert.match(html, /no plaintext copy is written/);
  assert.match(html, /Replay[^>]*|Replay test/);
});

test("table mutations wait for the active run and storage failures do not hide current results", (t) => {
  const original = React.useState;
  let calls = 0;
  const snapshot: TestingState = { revision: 1, phase: "recognizing", active: result, results: [result], audioBytes: 0, recordingCount: 0, savedTables: [], tableBusy: false, tableStorageError: "Wallet locked; existing tables were not changed" };
  t.mock.method(React, "useState", ((initial: unknown) => original(calls++ === 0 ? snapshot : initial)) as typeof React.useState);
  const html = renderToStaticMarkup(<TestingPage {...pageProps("idle", "linux")} />);
  assert.match(html, /disabled=""[^>]*>Save table &amp; start new/);
  assert.match(html, /disabled=""[^>]*>Clear table/);
  assert.match(html, /role="alert"[^>]*>Wallet locked/);
  assert.match(html, /Rule text/);
  assert.doesNotMatch(html, /No saved tables yet/);
});
