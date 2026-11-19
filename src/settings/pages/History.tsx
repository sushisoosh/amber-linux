// The last dictations, kept on this PC: nothing dictated into the wrong window, or that
// could not be inserted, is lost.

import { useCallback, useDeferredValue, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { DictationEvent, HistoryEntry } from "../../shared/types";
import type { PageProps } from "../App";
import { DEFAULT_HISTORY_FILTERS, filterHistory, historyPrograms, type HistoryFilters } from "../history";
import { Row, Section, Switch } from "../ui";

export function HistoryPage({ s, save, toast, ov, desktop, phase, go }: PageProps) {
  const linux = ov.platform === "linux";
  const [entries, setEntries] = useState<HistoryEntry[]>([]);
  const [busy, setBusy] = useState(false);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [filters, setFilters] = useState<HistoryFilters>(DEFAULT_HISTORY_FILTERS);
  const deferredQuery = useDeferredValue(filters.query);
  const searching = filters.query !== deferredQuery;
  const visibleEntries = filterHistory(entries, { ...filters, query: deferredQuery }, Date.now());
  const programs = historyPrograms(entries);
  const filtered = filters.query !== "" || filters.program !== null || filters.date !== "all" || filters.status !== "all";
  const load = useCallback(async () => {
    try {
      setEntries(await invoke<HistoryEntry[]>("history_list"));
      setLoadError(null);
    } catch (e) {
      setLoadError(`Could not load history: ${e}`);
    } finally {
      await desktop.refresh();
    }
  }, [desktop.refresh]);

  useEffect(() => {
    void load();
    // A finished dictation shows up at once.
    const un = listen<DictationEvent>("dictation-state", (e) => {
      if (e.payload.state === "ready" || e.payload.state === "done" || e.payload.state === "error") void load();
    });
    return () => void un.then((f) => f());
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
