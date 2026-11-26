// The last dictations, kept on this PC: nothing dictated into the wrong window, or that
// could not be inserted, is lost.

import { useCallback, useDeferredValue, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { DictationEvent, EstimateSource, HistoryComparisons, HistoryEntry } from "../../shared/types";
import type { PageProps } from "../App";
import { DEFAULT_HISTORY_FILTERS, filterHistory, historyPrograms, type HistoryFilters } from "../history";
import { Row, Section, Switch } from "../ui";

function CleanupMethod({ cleanup }: { cleanup: HistoryEntry["cleanup"] }) {
  if (!cleanup) return <span>Not recorded</span>;
  const label = { none: "No cleanup", rules: "Rules only", snippet: "Snippet", smart: "Smart cleanup", instruction: "Keyword instruction" }[cleanup.method];
  return <>
    <span>{label}</span>
    {cleanup.engine && <small>{cleanup.engine}</small>}
    {cleanup.outcome !== "success" && <small>{cleanup.outcome === "failed" ? "Failed" : "Skipped"}; fallback text kept</small>}
  </>;
}

function estimateBasis(source: EstimateSource, samples: number) {
  switch (source) {
    case "notNeeded": return "No model stage";
    case "default": return "Default; no measured samples";
    case "legacy": return "Legacy timing prior";
    case "learned": return `${samples.toLocaleString()} successful sample${samples === 1 ? "" : "s"}; ${samples < 4 ? "limited data" : "learned"}`;
  }
}

export function HistoryEstimateTable({ comparison }: { comparison: HistoryComparisons }) {
  const rows = [...comparison.rows].sort((a, b) => a.totalMs - b.totalMs || a.id.localeCompare(b.id));
  return <>
    <div className="comparison-legend" id="history-estimates-legend">
      Same reference workload for every combination: {comparison.referenceAudioMs / 1000} seconds of audio + {comparison.referenceChars} text characters (Unicode, not words or tokens).
      These are current predictions from the same estimator as the progress ring, including fixed overhead, not averages of the wall times below.
      Sorted by combined estimate, fastest first. Lower is faster, not necessarily better recognition or cleanup quality.
      Scroll horizontally for all columns.
    </div>
    <div className="comparison-scroll" role="region" aria-label="Comparable timing estimates" aria-describedby="history-estimates-legend" tabIndex={0}>
      <table className="comparison-table history-estimates">
        <caption className="visually-hidden">Normalized estimates for recorded engine and cleanup combinations</caption>
        <thead><tr>
          <th scope="col" className="comparison-model">Speech engine / Model</th>
          <th scope="col" className="comparison-model">Cleanup method / Model</th>
          <th scope="col" className="comparison-number">Dictations<span>in filtered history</span></th>
          <th scope="col" className="comparison-number">Recognition<span>ms / {comparison.referenceAudioMs / 1000} s audio</span></th>
          <th scope="col" className="comparison-number">Cleanup model<span>ms / {comparison.referenceChars} characters</span></th>
          <th scope="col" className="comparison-number">Combined<span>ms / reference workload</span></th>
        </tr></thead>
        <tbody>{rows.map((row) => <tr key={row.id}>
          <th scope="row" className="comparison-model"><span>{row.speech}</span></th>
          <td className="comparison-model"><span>{row.cleanup}</span></td>
          <td className="comparison-number">{row.dictations.toLocaleString()}</td>
          <td className="comparison-number">{row.recognitionMs.toLocaleString()}<small>{estimateBasis(row.recognitionSource, row.recognitionSamples)}</small></td>
          <td className="comparison-number">{row.cleanupMs.toLocaleString()}<small>{estimateBasis(row.cleanupSource, row.cleanupSamples)}</small></td>
          <td className="comparison-number comparison-total">{row.totalMs.toLocaleString()}</td>
        </tr>)}</tbody>
      </table>
    </div>
  </>;
}

function HistoryEstimates({ entries, visibleEntries }: { entries: readonly HistoryEntry[]; visibleEntries: readonly HistoryEntry[] }) {
  const [result, setResult] = useState<{ selection: string; comparison?: HistoryComparisons; error?: string } | null>(null);
  const [revision, setRevision] = useState(0);
  const selection = JSON.stringify(visibleEntries.map((entry) => entry.at));
  useEffect(() => {
    let cancelled = false;
    setResult(null);
    void invoke<HistoryComparisons>("history_comparisons", { ats: JSON.parse(selection) as number[] }).then(
      (comparison) => { if (!cancelled) setResult({ selection, comparison }); },
      (error: unknown) => { if (!cancelled) setResult({ selection, error: `Could not load estimates: ${error}` }); },
    );
    return () => { cancelled = true; };
  }, [selection, revision, entries]);
  const current = result?.selection === selection ? result : null;
  return <Section title="Comparable estimates">
    <Row label="Compare engine and cleanup combinations" hint="Successful combinations in your saved dictations. The history filters below apply here too.">
      <button className="btn" onClick={() => setRevision((value) => value + 1)}>Refresh estimates</button>
    </Row>
    {!current && <div className="row note" role="status">Loading comparable estimates...</div>}
    {current?.error && <div className="row note error history-estimates-error" role="alert">{current.error}</div>}
    {current?.comparison && (current.comparison.rows.length > 0
      ? <HistoryEstimateTable comparison={current.comparison} />
      : <div className="row note">No comparable combinations in this selection yet. Complete a new dictation with history enabled to record its execution details.</div>)}
    <details className="comparison-help">
      <summary>How to read these estimates</summary>
      <div className="comparison-legend">
        Older entries without execution details, mixed-route streaming runs, and failed or skipped model cleanup are excluded.
        Bracketed IDs distinguish execution settings without displaying private prompts or connection addresses.
        Sample counts belong to each processing stage across all successful dictations, not just this filter or combination.
        Sparse or similar-length data is blended with prior estimates; legacy estimates have no reliable sample count.
        Rules and snippets have no cleanup-model budget, but their actual pipeline still takes time.
        Model loading, failed attempts and retries are not part of these reference estimates; live chunking overlaps recording.
        Estimates are not a controlled benchmark. Use Testing to compare the same recording.
      </div>
    </details>
  </Section>;
}

export function HistoryTimings({ entries }: { entries: readonly HistoryEntry[] }) {
  const duration = (ms: number | null | undefined) => ms == null ? "Not measured" : ms.toLocaleString();
  return <>
    <div className="comparison-legend" id="history-timings-legend">
      Everyday dictation timings in milliseconds, kept and deleted with history. The current filters apply.
      Transcription measures work after recording stops (only the remaining work for live chunking), including model setup and retries.
      Cleanup includes rules and any smart-cleanup attempt, not just successful model work.
      Processing total runs from stop or retry to cleaned text, including preparation but excluding recording and insertion.
      Retries have no new recording. Older entries, phone requests and terminal commands may have no measurements.
      Scroll horizontally for all columns.
    </div>
    <div className="comparison-scroll" role="region" aria-label="Everyday dictation timings" aria-describedby="history-timings-legend" tabIndex={0}>
      <table className="comparison-table">
        <caption className="visually-hidden">Performance of saved dictations</caption>
        <thead><tr>
          <th scope="col">Dictation / Engine</th>
          <th scope="col" className="comparison-model">Cleanup method / Model</th>
          <th scope="col" className="comparison-number">Recording<span>ms</span></th>
          <th scope="col" className="comparison-number">Transcription after stop<span>ms</span></th>
          <th scope="col" className="comparison-number">Cleanup pipeline<span>ms</span></th>
          <th scope="col" className="comparison-number">Processing total<span>ms</span></th>
          <th scope="col" className="comparison-preview">Text preview</th>
        </tr></thead>
        <tbody>{entries.map((e) => <tr key={e.at}>
          <th scope="row" className="comparison-model"><span>{new Date(e.at).toLocaleString()}</span><small>{e.program.trim() || "Unknown program"} / {e.engine}</small>{e.timings?.live && <small>Live chunking: remaining work only</small>}</th>
          <td className="comparison-model"><CleanupMethod cleanup={e.cleanup} /></td>
          <td className="comparison-number">{e.timings && e.timings.recordingMs === null ? "Retry: no new recording" : duration(e.timings?.recordingMs)}</td>
          <td className="comparison-number">{duration(e.timings?.transcriptionMs)}</td>
          <td className="comparison-number">{duration(e.timings?.cleanupMs)}{e.timings?.cleanupSkipped && !e.cleanup && <small>Smart cleanup skipped or failed; fallback text kept</small>}</td>
          <td className="comparison-number comparison-total">{duration(e.timings?.processingMs)}</td>
          <td className="comparison-preview"><div>{e.text.slice(0, 160)}{e.text.length > 160 && "..."}</div></td>
        </tr>)}</tbody>
      </table>
    </div>
  </>;
}

export function HistoryPage({ s, save, toast, ov, desktop, phase, go }: PageProps) {
  const linux = ov.platform === "linux";
  const [entries, setEntries] = useState<HistoryEntry[]>([]);
  const [busy, setBusy] = useState(false);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [filters, setFilters] = useState<HistoryFilters>(DEFAULT_HISTORY_FILTERS);
  const loadGeneration = useRef(0);
  const deferredQuery = useDeferredValue(filters.query);
  const searching = filters.query !== deferredQuery;
  const visibleEntries = filterHistory(entries, { ...filters, query: deferredQuery }, Date.now());
  const programs = historyPrograms(entries);
  const filtered = filters.query !== "" || filters.program !== null || filters.date !== "all" || filters.status !== "all";
  const load = useCallback(async () => {
    const generation = ++loadGeneration.current;
    try {
      const loaded = await invoke<HistoryEntry[]>("history_list");
      if (generation !== loadGeneration.current) return;
      setEntries(loaded);
      setLoadError(null);
    } catch (e) {
      if (generation === loadGeneration.current) setLoadError(`Could not load history: ${e}`);
    } finally {
      if (generation === loadGeneration.current) await desktop.refresh();
    }
  }, [desktop.refresh]);

  useEffect(() => {
    void load();
    // A finished dictation shows up at once.
    const un = listen<DictationEvent>("dictation-state", (e) => {
      if (e.payload.state === "ready" || e.payload.state === "done" || e.payload.state === "error") void load();
    });
    return () => {
      ++loadGeneration.current;
      void un.then((f) => f());
    };
  }, [load, s.history]);

  const again = async () => {
    setBusy(true);
    try {
      await invoke("recognize_again");
      toast(linux ? "Recognition finished. Review the result in the recorder; nothing was copied automatically." : "Recognized again - the text is on the clipboard.", true);
      if (linux) go("recorder");
      void load();
    } catch (e) {
      toast(String(e));
    }
    setBusy(false);
  };

  return (
    <>
      <h1>History</h1>
      <p className="lead">{linux ? "Persistent history is opt-in and requires an available Secret Service wallet for encryption. The recorder keeps its latest result in memory independently. Copying is always your choice." : "Your last dictations, kept on this PC only and encrypted for your Windows account. Copy one to put it where it should have gone."}</p>
      <p className="note">Progress estimates learn separately from successful recognition and smart-cleanup requests for your execution settings. Compact counts, averages and variation are kept locally indefinitely, even with history and statistics off or after deleting history; no audio, transcripts or individual timing records are retained for this learning. The ring budgets both stages and refines cleanup after recognition. Model loading, retries, fallback and system load can extend the wait; the ring never signals completion while work is running.</p>
      <Section>
        <Row label="Keep a history" hint={linux ? "Turning this off requests deletion of saved history. A storage failure is reported, not silently replaced with plaintext." : "Off deletes what is kept."}>
          <Switch on={s.history} onChange={(v) => save((x) => (x.history = v))} />
        </Row>
        {s.history && (
          <Row label="Keep dictations for">
            <select value={s.historyDays} onChange={(e) => save((x) => (x.historyDays = Number(e.target.value)))}>
              {[1, 3, 7, 14, 30, 90].map((d) => (
                <option key={d} value={d}>
                  {d === 1 ? "1 day" : `${d} days`}
                </option>
              ))}
            </select>
          </Row>
        )}
        <Row label="Recognize again" hint="When a recognition failed (no connection, a refused key), the recording is kept for ten minutes - in memory only.">
          <button className="btn" disabled={busy || (linux && (phase === "recording" || phase === "transcribing" || phase === "inserting"))} onClick={again}>
            {busy ? "Recognizing…" : "Recognize again"}
          </button>
        </Row>
        {linux && <Row label="Encrypted storage" hint={desktop.storage?.history ?? desktop.error ?? "Checking history storage..."}>
          <button className="btn" disabled={desktop.loading} onClick={() => void load()}>Refresh</button>
        </Row>}
        {loadError && <div className="row"><div className="note error" role="alert">{loadError}</div></div>}
      </Section>
      {linux && <p className="note">If your wallet was unavailable when Amber started, unlock it and restart Amber to reload encrypted history. Keep any needed result before restarting. Do not turn history off to retry: that deletes stored dictations.</p>}
      {s.history && <HistoryEstimates entries={entries} visibleEntries={visibleEntries} />}
      {s.history && (
        <Section title="Saved dictations">
          <div className="row column" role="search" aria-label="Filter history" style={{ gap: 12 }}>
            <label style={{ display: "grid", gap: 6, minWidth: 0 }}>
              Search history
              <input
                type="text"
                value={filters.query}
                placeholder="Search text or program"
                autoComplete="off"
                autoCorrect="off"
                spellCheck={false}
                style={{ width: "100%", minWidth: 0 }}
                onChange={(e) => setFilters({ ...filters, query: e.target.value })}
              />
            </label>
            <div style={{ display: "flex", flexWrap: "wrap", gap: 12 }}>
              <label style={{ display: "grid", gap: 6, flex: "1 1 160px", minWidth: 0 }}>
                Program
                <select
                  value={filters.program === null ? "" : `program:${filters.program}`}
                  style={{ minWidth: 0, width: "100%" }}
                  onChange={(e) => setFilters({ ...filters, program: e.target.value === "" ? null : e.target.value.slice(8) })}
                >
                  <option value="">All programs</option>
                  {filters.program !== null && !programs.some((p) => p.value === filters.program) && (
                    <option value={`program:${filters.program}`}>{filters.program || "Unknown program"} (no saved dictations)</option>
                  )}
                  {programs.map((p) => <option key={p.value} value={`program:${p.value}`}>{p.label}</option>)}
                </select>
              </label>
              <label style={{ display: "grid", gap: 6, flex: "1 1 160px", minWidth: 0 }}>
                Date range
                <select
                  value={filters.date}
                  aria-describedby="history-date-hint"
                  style={{ minWidth: 0, width: "100%" }}
                  onChange={(e) => setFilters({ ...filters, date: e.target.value as HistoryFilters["date"] })}
                >
                  <option value="all">All dates</option>
                  <option value="today">Today</option>
                  <option value="7days">Last 7 days</option>
                  <option value="30days">Last 30 days</option>
                </select>
              </label>
              <label style={{ display: "grid", gap: 6, flex: "1 1 160px", minWidth: 0 }}>
                Insertion status
                <select
                  value={filters.status}
                  style={{ minWidth: 0, width: "100%" }}
                  onChange={(e) => setFilters({ ...filters, status: e.target.value as HistoryFilters["status"] })}
                >
                  <option value="all">All statuses</option>
                  <option value="inserted">Inserted</option>
                  <option value="not-inserted">Not inserted</option>
                </select>
              </label>
            </div>
            <div className="note" id="history-date-hint">Date ranges use local calendar days and include today. Search stays in memory only.</div>
          </div>
          <div className="row" style={{ flexWrap: "wrap" }}>
            <div className="label note" role="status" aria-live="polite" aria-atomic="true">
              {searching ? "Updating results..." : `${visibleEntries.length} of ${entries.length} saved dictations shown`}
              {loadError && entries.length > 0 && " (last loaded history; refresh failed)"}
            </div>
            <button className="btn" disabled={!filtered && !searching} onClick={() => setFilters(DEFAULT_HISTORY_FILTERS)}>Clear filters</button>
          </div>
          {visibleEntries.length === 0 && !searching && (
            <div className="row note">
              {entries.length > 0
                ? "No dictations match these filters. Try another search or clear the filters."
                : loadError
                  ? "History unavailable. Saved dictations could not be loaded."
                  : linux
                    ? "No loaded dictations. New dictations appear here when encrypted history storage is available."
                    : "No saved dictations yet. New dictations will appear here while history is on."}
            </div>
          )}
          {visibleEntries.length > 0 && <HistoryTimings entries={visibleEntries} />}
          {visibleEntries.map((e) => (
            <div className="item" key={e.at} style={{ alignItems: "flex-start", flexWrap: "wrap" }}>
              <div className="name" style={{ flexBasis: 240 }}>
                <div className="hint">
                  {new Date(e.at).toLocaleString()} · {e.program.trim() || "unknown program"}
                  {!e.inserted && " · not inserted"}
                </div>
                <div style={{ whiteSpace: "pre-wrap", userSelect: "text", marginTop: 2 }}>{e.text}</div>
              </div>
              <button
                className="btn"
                onClick={async () => {
                  try {
                    await invoke("copy_text", { text: e.text });
                    toast("Copied.", true);
                  } catch (err) {
                    toast(linux ? `Could not copy: ${err}` : `Could not copy - another program holds the clipboard: ${err}`);
                  }
                }}
              >
                Copy
              </button>
              <button
                className="btn danger"
                onClick={async () => {
                  try {
                    await invoke("history_delete", { at: e.at });
                  } catch (err) {
                    toast(`Could not delete history entry: ${err}`);
                  } finally {
                    await load();
                  }
                }}
              >
                Delete
              </button>
            </div>
          ))}
          {entries.length > 0 && (
            <div className="row" style={{ flexWrap: "wrap" }}>
              <div className="label note" id="history-delete-hint">Deletes all saved dictations, including those hidden by filters.</div>
              <button
                className="btn danger"
                aria-describedby="history-delete-hint"
                onClick={async () => {
                  try {
                    await invoke("history_clear");
                  } catch (err) {
                    toast(`Could not clear history: ${err}`);
                  } finally {
                    await load();
                  }
                }}
              >
                Delete all history
              </button>
            </div>
          )}
        </Section>
      )}
    </>
  );
}
