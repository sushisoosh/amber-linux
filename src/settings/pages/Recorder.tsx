import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { DictationResult } from "../../shared/types";
import type { PageProps } from "../App";
import { DesktopDiagnostics } from "../desktop";
import { phaseLabel, Row, Section } from "../ui";

export function Recorder({ s, phase, phaseMessage, refresh, toast, go, desktop, compact }: PageProps & { compact: boolean }) {
  const [result, setResult] = useState<DictationResult | null>(null);
  const [resultError, setResultError] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [cancelling, setCancelling] = useState(false);
  const commandPending = useRef(false);
  const resultVersion = useRef(0);
  const recording = phase === "recording";
  const processing = phase === "transcribing" || phase === "inserting";
  const active = recording || processing;

  const loadResult = async () => {
    const version = ++resultVersion.current;
    try {
      const latest = await invoke<DictationResult | null>("get_last_result");
      if (version === resultVersion.current) {
        setResult(latest);
        setResultError(null);
      }
    } catch (e) {
      if (version === resultVersion.current) setResultError(`Could not retrieve the latest result: ${e}`);
    }
  };

  useEffect(() => {
    let disposed = false;
    const unlisten = listen<DictationResult | null>("dictation-result", (e) => {
      resultVersion.current++;
      setResult(e.payload);
      setResultError(null);
    });
    void unlisten.then(() => { if (!disposed) void loadResult(); }).catch((e) => {
      if (!disposed) setResultError(`Could not watch dictation results: ${e}`);
    });
    return () => {
      disposed = true;
      resultVersion.current++;
      void unlisten.then((un) => un()).catch(() => {});
    };
  }, []);

  const run = async (command: "toggle_dictation" | "recognize_again" | "clear_last_result" | "copy_text") => {
    if (commandPending.current) return;
    commandPending.current = true;
    setBusy(true);
    setActionError(null);
    try {
      await invoke(command, command === "copy_text" ? { text: result?.text ?? "" } : undefined);
      if (command === "copy_text") toast("Copied. Focus your destination and paste manually.", true);
      if (command === "clear_last_result" || command === "recognize_again") await loadResult();
    } catch (e) {
      setActionError(String(e));
    } finally {
      commandPending.current = false;
      setBusy(false);
      void refresh().catch((e) => setActionError(`Could not refresh recording status: ${e}`));
      void desktop.refresh();
    }
  };

  const cancel = async () => {
    setCancelling(true);
    setActionError(null);
    try {
      await invoke("cancel_dictation");
      await refresh();
    } catch (e) {
      setActionError(String(e));
    } finally {
      setCancelling(false);
      void desktop.refresh();
    }
  };

  return (
    <div className={`recorder ${compact ? "compact" : ""}`}>
      {!compact && <>
        <h1>Recorder</h1>
        <p className="lead">Record without leaving your active app. Review and copy the result, or enable Ctrl+V on completion in Delivery.</p>
      </>}
      <Section title={compact ? undefined : "Dictation"}>
        <div className="row recorder-controls">
          <div className="label" role="status" aria-live="polite">
            <div><span className={`dot ${recording || processing ? "starting" : phase === "error" ? "failed" : "ready"}`} />{phaseLabel(phase)}</div>
            {!compact && <div className="hint">{s.engine === "local" ? `Local recognition: ${s.local.model}` : `Audio is sent to ${s.server.provider === "custom" ? s.server.baseUrl || "your configured server" : s.server.provider}.`}</div>}
            {!compact && <div className="hint">{!s.cleanup.smart ? "Smart cleanup is off." : s.cleanup.engine === "local" ? "Smart cleanup runs on this PC." : `Recognized text is sent for cleanup to ${s.cleanup.provider === "custom" ? s.cleanup.baseUrl || s.server.baseUrl || "your configured server" : s.cleanup.provider}.`}</div>}
          </div>
          <div className="recorder-actions">
            <button className="btn primary" disabled={processing || busy || cancelling} onClick={() => void run("toggle_dictation")}>
              {recording ? "Stop recording" : "Record"}
            </button>
            {active && <button className="btn danger" disabled={cancelling} onClick={() => void cancel()}>{cancelling ? "Cancelling..." : "Cancel"}</button>}
            {compact && <button className="btn" onClick={() => go("recorder")}>{result ? "View result" : "Open recorder"}</button>}
          </div>
        </div>
        {(phaseMessage || actionError) && <div className="row"><div className={`note ${actionError || phase === "error" ? "error" : ""}`} role="status">{actionError ?? phaseMessage}</div></div>}
        {!compact && processing && <Row label="Working on your recording" hint="Cancel discards this run. A native model load may need to finish before another recording can be processed." />}
        {!compact && phase === "error" && <Row label="Retry failed recognition" hint="Failed audio is kept in memory for up to ten minutes. Retry uses the current engine settings and can send audio to the configured server again.">
          <button className="btn" disabled={busy || cancelling} onClick={() => void run("recognize_again")}>Retry recognition</button>
        </Row>}
      </Section>
      {!compact && <>
        <Section title="Delivery">
          <Row label={s.insert.pasteOnCompletion ? "Paste on completion is enabled" : "Review and copy"} hint="The hotkey captures the destination before showing a non-focusing recording indicator. Ctrl+V is optional and restricted to supported X11 editors; Wayland uses manual Copy. Starting from this window does not select another app as a target.">
            <button className="btn" onClick={() => go("insert")}>Configure delivery</button>
          </Row>
        </Section>
        <Section title="Latest result">
          {resultError && <Row label={<span className="note error" role="alert">{resultError}</span>}>
            <button className="btn" onClick={() => void loadResult()}>Refresh</button>
          </Row>}
          {result ? <>
            <div className="row column">
              <div className="note">{result.message || "Ready for review. Nothing has been copied automatically."}</div>
              <textarea className="result-text" aria-label="Latest recognized text" value={result.text} readOnly spellCheck={false} />
              <div className="note result-meta">Engine: {result.engine}. {result.exposure}</div>
              <div className="recorder-actions">
                <button className="btn primary" disabled={busy || !result.text} onClick={() => void run("copy_text")}>Copy</button>
                <button className="btn danger" disabled={busy || active} onClick={() => void run("clear_last_result")}>Discard result</button>
              </div>
            </div>
            <Row label="Copy replaces your clipboard" hint="Clipboard managers may retain copied text. Review sensitive text and terminal commands before pasting. Automatic insertion is not guaranteed." />
          </> : <Row label="No result yet" hint="Stop a recording to recognize it. The latest result stays available in memory until replaced, discarded, or Amber exits." />}
          <Row label="In memory, not a transcript archive" hint={s.history ? "History is enabled separately. Discarding this result does not delete its history entry." : "Persistent history is off. Enable encrypted history separately only if you want to retain dictations."} />
        </Section>
        <Section title="Keyboard controls">
          <Row label="Use this window" hint="Tab to Record, Stop recording, or Cancel. Enter or Space activates the focused button. Tab to the result to select and copy text manually." />
          <Row label="Desktop shortcuts" hint={<>If global shortcuts are unavailable, add desktop or i3 bindings for <code>amber-dictation --toggle</code> and <code>amber-dictation --cancel</code>. Toggle once to start and again to stop.</>}>
            <button className="btn" onClick={() => go("hotkeys")}>Configure</button>
          </Row>
        </Section>
        <DesktopDiagnostics desktop={desktop} />
      </>}
    </div>
  );
}
