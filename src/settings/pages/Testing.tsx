import { Fragment, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { SavedTestTable, TestConfig, TestEngine, TestResult, TestingState } from "../../shared/types";
import type { PageProps } from "../App";
import { Row, Section } from "../ui";

const labels: Record<TestingState["phase"], string> = {
  idle: "Ready for a test", recording: "Listening", recognizing: "Recognizing speech",
  cleaning: "Cleaning up text", cancelling: "Cancelling; waiting for native work to finish",
};

function engineLabel(engine: TestEngine | null) {
  if (!engine) return "Not run";
  return engine.engine === "local" ? `Local / ${engine.runtime} / ${engine.model}` : `${engine.provider} / ${engine.model}`;
}

function duration(ms: number | null) {
  return ms === null ? "Not run" : `${(ms / 1000).toFixed(2)} s (${ms} ms)`;
}

function bytes(count: number) {
  return count >= 1024 * 1024 ? `${(count / (1024 * 1024)).toFixed(2)} MiB` : `${(count / 1024).toFixed(1)} KiB`;
}

function Selection({ config }: { config: TestConfig }) {
  const speech = config.engine === "local" ? `Local / ${config.local.runtime} / ${config.local.model}` : `${config.server.provider} / ${config.server.model}`;
  const cleanup = !config.cleanup.smart ? "Smart cleanup off" : config.cleanup.engine === "local"
    ? `Local / ${config.cleanup.localRuntime} / ${config.cleanup.localModel}` : `${config.cleanup.provider} / ${config.cleanup.model}`;
  return <>
    <div>Recognition: {speech}</div>
    <div>Cleanup: {cleanup}</div>
    {config.engine === "server" && <div>Requested upload: {config.server.uploadFormat === "opus" ? "Ogg Opus (32 kbps, lossy)" : "WAV (uncompressed)"}</div>}
    <div className="hint">{config.engine === "server" ? `Audio goes to ${config.server.provider === "custom" ? config.server.baseUrl || "your selected server" : config.server.provider}.` : "Audio stays on this computer."} {config.cleanup.smart && config.cleanup.engine === "server" ? `Text goes to ${config.cleanup.provider === "custom" ? config.cleanup.baseUrl || config.server.baseUrl || "your selected cleanup server" : config.cleanup.provider}.` : "Cleanup stays on this computer."}</div>
  </>;
}

function resultStatus(r: TestResult) {
  return r.error ? "Failed" : r.fallback ? "Local fallback" : r.cleanupStatus === "error" || r.cleanupStatus === "skipped" ? "Ready with cleanup skipped" : "Complete";
}

function Checkmark({ value, label, hint }: { value: boolean | null; label: string; hint?: string }) {
  const description = `${label}: ${value === null ? "Not applicable / not run" : value ? "Yes" : "No"}`;
  return <span className={`comparison-check ${value === null ? "na" : value ? "yes" : "no"}`} role="img" aria-label={description} title={hint ? `${description}. ${hint}` : description}>
    <svg viewBox="0 0 20 20" width="19" height="19" aria-hidden="true">
      {value === null ? <path d="M5 10h10" /> : <><rect x="2" y="2" width="16" height="16" rx="3" />{value && <path d="m5.5 10 3 3 6-6" />}</>}
    </svg>
  </span>;
}

/** Full transcripts/configuration stay available without making every table row a card. */
export function TestResultDetails({ result: r, busy, replayDisabled, remove, replay }: {
  result: TestResult; busy: boolean; replayDisabled: boolean; remove?: (id: number) => void; replay: (id: number) => void;
}) {
  const c = r.config.cleanup;
  return <div className="comparison-details">
    <Row label={<>Test {r.id} / {resultStatus(r)} / <time dateTime={new Date(r.at).toISOString()}>{new Date(r.at).toLocaleString()}</time></>} hint={r.microphone ? `Microphone: ${r.microphone}${r.microphoneFallback ? " (default device fallback)" : ""}` : "Microphone did not open"}>
      {remove && <button className="btn danger" disabled={busy} onClick={() => remove(r.id)} aria-label={`Discard test ${r.id}`}>Discard</button>}
    </Row>
    <Row label={r.recordingId === null ? "No reusable recording" : `Recording #${r.recordingId}${r.sourceTestId === null ? " / original capture" : ` / replay of test #${r.sourceTestId}`}`}
      hint={r.canReplay ? "Reuses the exact original, uncompressed audio with your current Engine, upload-format and Cleanup settings. This starts a new comparison and may send the audio/text to a newly selected server." : "Audio was not retained or has expired from the memory budget. Record a new comparison to enable replay."}>
      <button className="btn" disabled={replayDisabled || !r.canReplay} onClick={() => replay(r.id)} aria-label={`Replay test ${r.id} with current settings`}>Run again with current settings</button>
    </Row>
    {r.error && <Row label={<span className="note error" role="alert">{r.error}</span>} />}
    {r.interrupted && <Row label="Microphone interrupted" hint="The recording may be incomplete." />}
    <Row label={`Recognition engine: ${engineLabel(r.actualEngine)}`} hint={r.actualEngine?.baseUrl ?? undefined} />
    <Row label={`Cleanup engine: ${engineLabel(r.cleanupEngine)}`} hint={r.cleanupEngine?.baseUrl ?? undefined} />
    <div className="row column testing-timings">
      <div>Recorded audio: <strong>{duration(r.audioMs)}</strong></div>
      <div>Prepared speech: <strong>{duration(r.preparedAudioMs)}</strong></div>
      <div>Recognition wall time: <strong>{duration(r.recognitionMs)}</strong></div>
      <div>Cleanup wall time: <strong>{duration(r.cleanupMs)}</strong></div>
    </div>
    {r.attempts.map((attempt, i) => attempt.upload && <Row key={i} label={`Audio file for attempt ${i + 1}: ${attempt.upload.format === "opus" ? "Ogg Opus" : "WAV"}`}
      hint={<>{bytes(attempt.upload.inputBytes)} original WAV to {bytes(attempt.upload.uploadBytes)} upload file. Encoding: {duration(attempt.upload.encodingMs)} (included in recognition time). File sizes exclude multipart overhead and do not prove all bytes were sent.{attempt.upload.note && <> {attempt.upload.note}</>}</>} />)}
    <div className="row column testing-texts">
      <label>Raw recognized text<textarea className="result-text" readOnly spellCheck={false} value={r.raw} aria-label={`Raw recognized text for test ${r.id}`} /></label>
      <label>Final cleaned text<textarea className="result-text" readOnly spellCheck={false} value={r.text} aria-label={`Final cleaned text for test ${r.id}`} /></label>
    </div>
    <Row label={`Smart cleanup: ${{ notRun: "not run", disabled: "off (rules only)", empty: "skipped (no text)", success: "succeeded", skipped: "unavailable (rules kept)", error: "failed (rules kept)" }[r.cleanupStatus]}`} hint={r.cleanupError ?? undefined} />
    <div className="row column">
      <div className="note">{r.remoteAudio ? "Audio may have been sent to the recognition server, including before local fallback." : "No remote audio attempt."} {r.remoteText ? "Text may have been sent to the cleanup server, even if cleanup failed." : "No remote text attempt."}</div>
      <details className="testing-config">
        <summary>Configuration captured at run start</summary>
        <Selection config={r.config} />
        <p>Language: {r.config.language}. Requested speech threads: {r.config.local.threads || "automatic"}. Local fallback: {r.config.server.localFallback ? "allowed" : "off"}.</p>
        <p>Rules: fillers {c.fillers ? "on" : "off"}; spoken punctuation {c.spokenPunctuation ? "on" : "off"}; German commas {c.germanCommas ? "on" : "off"}.</p>
        <p>{r.sourceTestId === null ? `Requested microphone: ${r.config.microphone || "system default"}.` : "This replay did not open a microphone; it used the original recording."} Live chunking is not used in Testing.</p>
        <label>Vocabulary prompt<textarea readOnly value={r.config.vocabulary} aria-label={`Vocabulary for test ${r.id}`} /></label>
        <p>Recognition attempts (request time, excluding model setup):</p>
        {r.attempts.length ? <ol>{r.attempts.map((attempt, i) => <li key={i}>{engineLabel(attempt.engine)}: {duration(attempt.ms)}. {attempt.error ?? "Succeeded"}</li>)}</ol> : <p>No engine request started.</p>}
      </details>
    </div>
  </div>;
}

export function ComparisonTable({ id, title, results, busy, replayDisabled, remove, replay }: {
  id: string; title: string; results: TestResult[]; busy: boolean; replayDisabled: boolean;
  remove?: (id: number) => void; replay: (id: number) => void;
}) {
  const [expanded, setExpanded] = useState<Set<number>>(() => new Set());
  const toggle = (testId: number) => setExpanded((before) => {
    const next = new Set(before);
    if (next.has(testId)) next.delete(testId); else next.add(testId);
    return next;
  });
  return <>
    <div className="comparison-legend" id={`${id}-legend`}>All step times are milliseconds. Recognition includes encoding, model setup and retries; total is recognition + cleanup, not encoding twice. Opus shows the prepared upload format, Smart the captured cleanup setting. Scroll horizontally for all columns; Details shows full text and configuration.</div>
    <div className="comparison-scroll" role="region" aria-label={`${title} comparison table`} aria-describedby={`${id}-legend`} tabIndex={0}>
      <table className="comparison-table">
        <caption className="visually-hidden">{title}: recognition, cleanup and audio-upload comparisons</caption>
        <thead><tr>
          <th scope="col" className="comparison-run">Test / Audio</th>
          <th scope="col" className="comparison-model">Recognition model</th>
          <th scope="col" className="comparison-model">Cleanup model</th>
          <th scope="col" className="comparison-flag">Opus</th>
          <th scope="col" className="comparison-flag">Smart</th>
          <th scope="col" className="comparison-number">Encode<span>ms</span></th>
          <th scope="col" className="comparison-number">Recognize<span>ms</span></th>
          <th scope="col" className="comparison-number">Cleanup<span>ms</span></th>
          <th scope="col" className="comparison-number">Total<span>ms</span></th>
          <th scope="col" className="comparison-preview">Result preview</th>
          <th scope="col" className="comparison-actions">Actions</th>
        </tr></thead>
        <tbody>{results.map((r) => {
          const uploads = r.attempts.flatMap((attempt) => attempt.upload ? [attempt.upload] : []);
          const prepared = uploads.filter((upload) => upload.uploadBytes > 0);
          const compressed = prepared.length ? prepared.some((upload) => upload.format === "opus") : null;
          const encodeMs = uploads.some((upload) => upload.uploadBytes > 0 || upload.encodingMs > 0)
            ? uploads.reduce((sum, upload) => sum + upload.encodingMs, 0) : null;
          const uploadBytes = prepared.reduce((sum, upload) => sum + upload.uploadBytes, 0);
          const totalMs = r.recognitionMs !== null && r.cleanupMs !== null ? r.recognitionMs + r.cleanupMs : null;
          const requestedOpus = r.config.engine === "server" && r.config.server.uploadFormat === "opus";
          const detailsId = `${id}-test-${r.id}`;
          const open = expanded.has(r.id);
          const status = resultStatus(r);
          return <Fragment key={r.id}>
            <tr className={`comparison-result ${r.error ? "failed" : ""}`} data-test-id={r.id}>
              <th scope="row" className="comparison-run"><strong>#{r.id}</strong><span title={status} className={`comparison-status ${r.error || r.cleanupStatus === "error" ? "error" : ""}`}>{status}</span><small>{r.recordingId === null ? "No audio" : `Audio #${r.recordingId}`}</small><small title={duration(r.audioMs)}>{(r.audioMs / 1000).toFixed(2)} s{r.sourceTestId !== null ? " / replay" : ""}</small></th>
              <td className="comparison-model"><span>{r.actualEngine?.model ?? "Not run"}</span><small>{r.actualEngine ? r.actualEngine.engine === "local" ? `Local / ${r.actualEngine.runtime}` : r.actualEngine.provider : "No completed recognition"}</small>{r.actualEngine?.engine === "local" && <small title="Requested settings; not a measurement of model warm-up or actual worker count">Threads: {r.config.local.threads || "auto"}<br />Keep loaded: {r.config.local.keepLoaded ? "yes" : "no"}</small>}</td>
              <td className="comparison-model"><span>{r.cleanupEngine?.model ?? (r.config.cleanup.smart ? "Not run" : "Rules only")}</span><small>{r.cleanupEngine ? r.cleanupEngine.engine === "local" ? `Local / ${r.cleanupEngine.runtime}` : r.cleanupEngine.provider : r.cleanupStatus}</small>{r.cleanupError && <small className="error">Cleanup unavailable</small>}</td>
              <td className="comparison-flag"><Checkmark value={compressed} label="Compressed upload prepared" hint={`${requestedOpus ? "Requested Opus" : "Requested WAV or local recognition"}. File size excludes multipart overhead and does not confirm network transfer.`} /><small>{prepared.length ? bytes(uploadBytes) : "No file"}</small>{requestedOpus && compressed === false && <small className="comparison-warning">Used WAV</small>}{prepared.length > 1 && <small>{prepared.length} files</small>}</td>
              <td className="comparison-flag"><Checkmark value={r.config.cleanup.smart} label="Smart cleanup enabled" /></td>
              {[encodeMs, r.recognitionMs, r.cleanupMs, totalMs].map((ms, index) => <td key={index} className={`comparison-number ${index === 3 ? "comparison-total" : ""}`} title={ms === null ? "Not run or incomplete; not a zero-time success" : duration(ms)}>{ms === null ? <span className="comparison-na">Not run</span> : ms.toLocaleString()}</td>)}
              <td className="comparison-preview"><div>{(r.text || r.raw).slice(0, 240) || "No text"}{(r.text || r.raw).length > 240 && "..."}</div>{r.error && <small className="error">Failed; open Details</small>}{r.interrupted && <small className="comparison-warning">Partial microphone recording</small>}</td>
              <td className="comparison-actions"><button className="btn" aria-expanded={open} aria-controls={detailsId} aria-label={`Details for test ${r.id} in ${title}`} onClick={() => toggle(r.id)}>{open ? "Close" : "Details"}</button><button className="btn" disabled={replayDisabled || !r.canReplay} aria-label={`Replay test ${r.id} with current settings`} title={r.canReplay ? "Run the original audio again with current settings; adds a row to the current table" : "Audio is no longer in memory; saved tables do not store recordings"} onClick={() => replay(r.id)}>Replay</button></td>
            </tr>
            <tr className="comparison-expanded" id={detailsId} hidden={!open}><td colSpan={11}>{open && <TestResultDetails result={r} busy={busy} replayDisabled={replayDisabled} remove={remove} replay={replay} />}</td></tr>
          </Fragment>;
        })}</tbody>
      </table>
    </div>
  </>;
}

function SavedTableSection({ table, busy, replayDisabled, replay, remove }: {
  table: SavedTestTable; busy: boolean; replayDisabled: boolean; replay: (id: number) => void; remove: (id: number) => Promise<boolean>;
}) {
  const [collapsed, setCollapsed] = useState(false);
  const [confirmDelete, setConfirmDelete] = useState(false);
  return <Section title={`Saved: ${table.name}`}>
      <Row label={`${table.results.length} saved ${table.results.length === 1 ? "comparison" : "comparisons"}`} hint={<time dateTime={new Date(table.savedAt).toISOString()}>Saved {new Date(table.savedAt).toLocaleString()} on this computer. Replays create rows in the current table.</time>}>
      <div className="comparison-toolbar">
        <button className="btn" aria-expanded={!collapsed} onClick={() => setCollapsed(!collapsed)}>{collapsed ? "Show table" : "Collapse"}</button>
        {confirmDelete ? <>
          <button className="btn danger" disabled={busy} onClick={() => void remove(table.id).then((removed) => { if (removed) setConfirmDelete(false); })}>Delete permanently</button>
          <button className="btn" disabled={busy} onClick={() => setConfirmDelete(false)}>Keep table</button>
        </> : <button className="btn danger" disabled={busy} onClick={() => setConfirmDelete(true)}>Delete saved table</button>}
      </div>
    </Row>
    {confirmDelete && <div className="comparison-legend error" role="alert">Delete this saved table and its transcripts from this computer? This cannot be undone.</div>}
    {!collapsed && <ComparisonTable id={`saved-${table.id}`} title={table.name} results={table.results} busy={busy} replayDisabled={replayDisabled} replay={replay} />}
  </Section>;
}

export function TestingPage({ s, phase, go, ov, settingsSaving, toast }: PageProps) {
  const [state, setState] = useState<TestingState | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [tableName, setTableName] = useState("");
  const pending = useRef(false);
  const mounted = useRef(false);
  const accept = (next: TestingState) => {
    if (mounted.current) setState((before) => !before || next.revision >= before.revision ? next : before);
  };
  const load = async () => {
    try { accept(await invoke<TestingState>("get_testing_state")); }
    catch (e) { if (mounted.current) setError(`Could not retrieve tests: ${e}`); }
  };

  useEffect(() => {
    mounted.current = true;
    let disposed = false;
    const unlisten = listen<TestingState>("testing-state", (event) => { if (!disposed) accept(event.payload); });
    void unlisten.then(() => { if (!disposed) void load(); }).catch((e) => {
      if (!disposed) { setError(`Could not watch tests: ${e}`); void load(); }
    });
    // Also recover a missed event or a temporarily unavailable event subscription.
    const timer = window.setInterval(() => { if (!disposed) void load(); }, 2000);
    return () => {
      mounted.current = false;
      disposed = true;
      window.clearInterval(timer);
      void unlisten.then((un) => un()).catch(() => {});
    };
  }, []);

  const run = async (command: "start_test_recording" | "replay_test_recording" | "stop_test_recording" | "cancel_test_recording" | "clear_test_results" | "save_test_table" | "delete_test_table" | "reload_test_tables", id?: number) => {
    if (pending.current) return false;
    pending.current = true;
    setBusy(true);
    setError(null);
    try {
      const args = command === "start_test_recording" || command === "reload_test_tables" ? undefined
        : command === "save_test_table" ? { name: tableName.trim() }
        : { id: command === "clear_test_results" ? id ?? null : command === "replay_test_recording" || command === "delete_test_table" ? id : state?.active?.id };
      await invoke(command, args);
      if (command === "save_test_table") { if (mounted.current) setTableName(""); toast("Table saved on this computer. A new comparison table is ready.", true); }
      if (command === "delete_test_table") toast("Saved table deleted.", true);
      await load();
      return true;
    } catch (e) { if (mounted.current) { setError(String(e)); toast(String(e)); } return false; }
    finally { pending.current = false; if (mounted.current) setBusy(false); }
  };
  const active = !!state && state.phase !== "idle";
  const ordinaryBusy = ["recording", "transcribing", "inserting"].includes(phase);
  const config = state?.active?.config ?? s;
  const savedTables = state?.savedTables ?? [];
  const locked = busy || !!state?.tableBusy;
  const replayDisabled = locked || active || ordinaryBusy || settingsSaving;
  const replay = (id: number) => { void run("replay_test_recording", id); };

  return <div className="testing-page">
    <h1>Testing</h1>
    <p className="lead">Compare models, settings and step timings on the same recording. Current results stay in memory; Save table keeps transcripts, configuration and timings encrypted on this computer. Audio is never saved to disk.</p>
    <Section title="Record a comparison">
      <div className="row column">
        <div role="status" aria-live="polite">{state?.tableBusy ? "Updating saved tables..." : ordinaryBusy && !active ? "Ordinary dictation in progress" : state ? labels[state.phase] : "Loading test session..."}</div>
        {state?.active?.sourceTestId != null && <div className="note">Replaying recording #{state.active.recordingId} from test #{state.active.sourceTestId}. The microphone is not used.</div>}
        <div className="recorder-actions testing-actions">
          <button className="btn primary" disabled={!state || replayDisabled} onClick={() => void run("start_test_recording")}>Start test recording</button>
          <button className="btn" disabled={busy || state?.phase !== "recording"} onClick={() => void run("stop_test_recording")}>Stop and recognize</button>
          <button className="btn danger" disabled={busy || !active || state?.phase === "cancelling"} onClick={() => void run("cancel_test_recording")}>Cancel test</button>
        </div>
        {error && <div className="note error" role="alert">{error}</div>}
        {settingsSaving && <div className="note" role="status">Saving settings. New recordings and replays wait until your selections are saved.</div>}
        {ordinaryBusy && !active && <>
          <div className="note">An ordinary dictation is running. Finish it before starting a test.</div>
          {ov.platform === "linux" && <div className="recorder-actions testing-actions">
            <button className="btn" onClick={() => go("recorder")}>Open Recorder</button>
            <span className="note">Stop or cancel there, or wait for delivery to finish.</span>
          </div>}
        </>}
        <div className="note">Tests never paste, type into other windows, or change the clipboard, even when automatic paste is enabled.</div>
      </div>
      <div className="row column"><details className="testing-config"><summary>{active ? "This run's captured selection" : "Selection for the next run"}</summary><Selection config={config} /></details></div>
      <div className="row recorder-actions testing-actions">
        <button className="btn" onClick={() => go("engine")}>Configure recognition</button>
        <button className="btn" onClick={() => go("cleanup")}>Configure cleanup</button>
        <button className="btn" onClick={() => go("microphone")}>Choose microphone</button>
      </div>
    </Section>
    <Section title="Current comparison">
      <Row label={`${state?.results.length ?? 0} / 20 tests in this table`} hint="Newest first. Save table moves these rows into a separate section below and starts an empty table. Clear table affects only these unsaved rows. Finish or cancel an active test before either action.">
        <div className="comparison-toolbar">
          <input type="text" aria-label="Name for saved comparison table" maxLength={80} value={tableName} disabled={locked} placeholder="Table name (optional)" onChange={(e) => setTableName(e.target.value)} />
          <button className="btn primary" disabled={locked || active || !state?.results.length || savedTables.length >= 10 || !!state?.tableStorageError} onClick={() => void run("save_test_table")}>Save table &amp; start new</button>
          <button className="btn danger" disabled={locked || active || !state?.results.length} onClick={() => void run("clear_test_results")}>Clear table</button>
        </div>
      </Row>
      {savedTables.length >= 10 && <div className="comparison-legend comparison-warning">The 10 saved-table limit has been reached. Delete a saved table before saving another; existing tables are not silently replaced.</div>}
      {state?.results.length ? <ComparisonTable id="current" title="Current" results={state.results} busy={locked || active} replayDisabled={replayDisabled} remove={(id) => void run("clear_test_results", id)} replay={replay} /> : <Row label="No test results yet" hint="Record and stop a short phrase, or replay a retained recording from a saved table. New results appear here." />}
      <Row label={`${state?.recordingCount ?? 0} shared ${state?.recordingCount === 1 ? "recording" : "recordings"} / ${bytes(state?.audioBytes ?? 0)} of 64 MiB retained`} hint="Audio is shared across current and saved tables for replay during this session. Saving a table never saves its audio; after restarting, the table remains but its Replay buttons are unavailable." />
    </Section>
    <Section title="Saved tables">
      <Row label={!state || (state.tableBusy && !savedTables.length) ? "Loading saved tables..." : `${savedTables.length} / 10 saved tables`} hint={ov.platform === "linux" ? "Saving requires an unlocked Secret Service wallet. If storage is unavailable, the current table stays intact; no plaintext copy is written. Unlock your wallet and reload saved tables to retry." : "Saved tables use Windows account encryption. Audio and unsaved comparisons remain session-only."}>
        <button className="btn" disabled={locked || active} onClick={() => void run("reload_test_tables")}>Reload saved tables</button>
      </Row>
      {state?.tableStorageError && <div className="row"><div className="note error" role="alert">{state.tableStorageError}</div></div>}
      {state && !state.tableBusy && !savedTables.length && !state.tableStorageError && <Row label="No saved tables yet" hint="Save a completed comparison above to keep it across restarts and begin another table." />}
    </Section>
    {savedTables.map((table) => <SavedTableSection key={table.id} table={table} busy={locked || active} replayDisabled={replayDisabled} replay={replay} remove={(id) => run("delete_test_table", id)} />)}
    <Section>
      <details className="comparison-help">
        <summary>Recording retention, timing and privacy</summary>
        <Row label="Up to 20 current results and 64 MiB of audio in memory" hint="Original uncompressed recordings stay in memory for replay, not on disk. Replays share audio instead of duplicating it. The least recently used audio is released when the memory budget is reached; comparison rows remain with replay disabled. Clearing or deleting rows releases audio when no remaining comparison uses it. Only explicitly saved tables survive quitting. Tests never enter ordinary history or statistics. Selected servers can retain what you send; cancellation cannot recall sent data." />
        <Row label="Reuse the same recording for a fair comparison" hint="Change Engine, upload format or Cleanup, then select Replay on a row. Each run starts from the same lossless WAV, never a previously compressed copy, and adds a row to the current table. Testing does not use live chunking, app profiles, snippets, terminal commands, or delivery formatting. Wall times include model setup, encoding, retries and local fallback; cleanup includes rules and smart model loading. A total is shown only when both recognition and cleanup timings exist. Network transfer and server processing are not timed separately." />
        <Row label="Settings are captured when you start each run" hint="Changing settings does not relabel old rows. Model names identify the actual selected local model or the requested remote model, not independently verified server internals. Local threads and keep-loaded values are captured requests, not measurements of whether a model was already warm. Leaving this page does not cancel a run; recordings stop after ten minutes. Native model loading may need to finish after cancellation." />
      </details>
    </Section>
  </div>;
}
