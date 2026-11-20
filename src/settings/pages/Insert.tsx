import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { DictationEvent, InsertMethod } from "../../shared/types";
import type { PageProps } from "../App";
import { Row, Section, Segmented, Switch } from "../ui";
import { pretty } from "./Hotkeys";

const METHODS: [InsertMethod, string][] = [
  ["paste", "Paste (Ctrl+V)"],
  ["type", "Type"],
];

export function InsertPage({ s, save, phase, ov, desktop }: PageProps) {
  const linux = ov.platform === "linux";
  const x11 = desktop.status?.session.toLowerCase() === "x11";
  const [last, setLast] = useState<string | null>(null);
  useEffect(() => {
    const un = listen<DictationEvent>("dictation-state", (e) => {
      if (e.payload.state === "error") setLast(e.payload.message ?? null);
      if (e.payload.state === "done") setLast(null);
    });
    return () => void un.then((f) => f());
  }, []);

  return (
    <>
      <h1>Delivery</h1>
      <p className="lead">{linux ? "Review and copy completed dictation, or opt in to clipboard delivery and X11 Ctrl+V. The hotkey leaves Amber in the background." : "How completed dictation reaches the window you were in when recording started. The hotkey leaves Amber in the background."}</p>
      <Section>
        {linux ? <>
          <Row label="Copy and paste completed dictation (Ctrl+V)" hint="Off by default: review the result and copy it yourself. When enabled, completed text goes to the clipboard even when automatic paste is unavailable. On X11, Amber attempts one paste according to your target options, without bringing either window forward.">
            <Switch on={s.insert.pasteOnCompletion} onChange={(v) => save((x) => {
              x.insert.pasteOnCompletion = v;
              x.insert.method = "paste";
            })} />
          </Row>
          {x11 ? <>
            <Row label="Paste into any window" hint={<>On by default: allows X11 Ctrl+V in browsers, chat apps, dialogs and other window types, not just known native editors. Applies to completion and <code>--insert-last</code>; this does not enable completion paste. Turn off to restrict targets to known native editors.</>}>
              <Switch on={s.insert.pasteIntoAnyWindow} onChange={(v) => save((x) => (x.insert.pasteIntoAnyWindow = v))} />
            </Row>
            <Row label="Allow pasting into a different window" hint="Off by default: completion requires the window captured at recording start. When enabled, Amber chooses the currently focused window once, when delivery begins, not when recording stops. Focus changes during paste still block it.">
              <Switch on={s.insert.pasteIntoDifferentWindow} onChange={(v) => save((x) => (x.insert.pasteIntoDifferentWindow = v))} />
            </Row>
            <Row label="X11 paste limits" hint="Only single-line text without control characters. The destination must be observable, outside Amber, with stable window and input focus while sending Ctrl+V. Amber never switches focus or presses Enter. Editable fields (including password fields) and paste acceptance cannot be verified. Terminals may need a different paste shortcut." />
          </> : <Row label="Manual paste on this desktop" hint="Wayland and unknown sessions cannot safely identify or paste into the target yet. Background clipboard access can also be restricted: on Wayland, Amber must be focused to copy. Otherwise open Recorder and use Copy." />}
          <Row label="Clipboard replacement" hint="Copy, opted-in completion and --insert-last replace clipboard text, images or files; Linux does not restore them. Clipboard managers may retain the text. Keep Amber running until you paste. Interrupted recordings always require review. Results remain in Recorder, even when a paste is blocked or cannot be confirmed. Testing recordings never change the clipboard." />
        </> : <Row
          label="Completed dictation"
          hint={
            s.insert.method === "paste"
              ? "Through the clipboard and Ctrl+V - fast and works almost everywhere."
              : "As keystrokes - leaves the clipboard alone, slower for long text, a few apps drop characters."
          }
        >
          <Segmented value={s.insert.method} options={METHODS} onChange={(v) => save((x) => (x.insert.method = v))} />
        </Row>}
        {!linux && s.insert.method === "paste" && (
          <Row label="Restore the clipboard" hint="Puts back the previous clipboard text unless something else was copied meanwhile. With images or files, Amber types instead so they are preserved. Dictations are excluded from Windows clipboard history (Win+V).">
            <Switch on={s.insert.restoreClipboard} onChange={(v) => save((x) => (x.insert.restoreClipboard = v))} />
          </Row>
        )}
        <Row label="Space after the text" hint="So the next dictation does not stick to the last word.">
          <Switch on={s.insert.trailingSpace} onChange={(v) => save((x) => (x.insert.trailingSpace = v))} />
        </Row>
      </Section>
      {!linux && <Section title="Try it">
        <div className="row column">
          <div className="note">
            Click into the field and press {s.hotkey.dictate ? <b>{pretty(s.hotkey.dictate)}</b> : "the dictate button"}, or use the button. Microphone, engine and inserting all
            work if text appears here.
          </div>
          <textarea id="test-field" placeholder="Your dictation lands here…" style={{ minHeight: 110 }} />
          <div style={{ display: "flex", gap: 12, alignItems: "center" }}>
            <button
              className="btn primary"
              // Keeps the focus in the text field, where the text should go.
              onMouseDown={(e) => e.preventDefault()}
              onClick={() => {
                document.getElementById("test-field")?.focus();
                void invoke("toggle_dictation");
              }}
            >
              {phase === "recording" ? "Stop" : "Dictate"}
            </button>
            {last && <span className="note error">{last}</span>}
          </div>
        </div>
      </Section>}
    </>
  );
}
