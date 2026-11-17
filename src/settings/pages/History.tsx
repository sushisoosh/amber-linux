// The last dictations, kept on this PC: nothing dictated into the wrong window, or that
// could not be inserted, is lost.

import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { DictationEvent, HistoryEntry } from "../../shared/types";
import type { PageProps } from "../App";
import { Row, Section, Switch } from "../ui";

export function HistoryPage({ s, save, toast, ov, desktop, phase, go }: PageProps) {
  const linux = ov.platform === "linux";
  const [entries, setEntries] = useState<HistoryEntry[]>([]);
  const [busy, setBusy] = useState(false);
  const [loadError, setLoadError] = useState<string | null>(null);
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
        <Section title={entries.length ? `${entries.length} dictations` : loadError ? "History unavailable" : linux ? "No loaded dictations" : "Nothing yet"}>
          {entries.map((e) => (
            <div className="item" key={e.at} style={{ alignItems: "flex-start" }}>
              <div className="name">
                <div className="hint">
                  {new Date(e.at).toLocaleString()} · {e.program || "unknown program"}
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
            <div className="row">
              <button
                className="btn danger"
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
                Delete all
              </button>
            </div>
          )}
        </Section>
      )}
    </>
  );
}
