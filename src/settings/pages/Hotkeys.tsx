import { useEffect, useState } from "react";
import type { HotkeyMode } from "../../shared/types";
import type { PageProps } from "../App";
import { Row, Section, Segmented } from "../ui";
import { DesktopDiagnostics } from "../desktop";

const MODES: [HotkeyMode, string][] = [
  ["both", "Tap or hold"],
  ["toggle", "Tap to toggle"],
  ["hold", "Hold to talk"],
];

const MODE_HINT: Record<HotkeyMode, string> = {
  both: "A short tap starts, the next tap stops. Holding the keys dictates until you let go.",
  toggle: "Press once to start, again to stop.",
  hold: "Dictates while you hold the keys (push to talk).",
};

export function HotkeysPage({ s, save, ov, desktop, go }: PageProps) {
  const linux = ov.platform === "linux";
  return (
    <>
      <h1>Hotkeys</h1>
      <p className="lead">{linux ? "Global shortcuts depend on your desktop and its permissions. The recorder window always provides Record, Stop, and Cancel controls. Use Ctrl, Alt or Super, or a function key." : "Work in every app, also when Amber's windows are closed. Use Ctrl, Alt or Win (Shift alone is not enough) so normal typing is not caught - or a function key."}</p>
      <Section title="Dictation">
        <Row label="Keep working in the active window" hint="Starting or stopping with the hotkey does not open or focus Amber. Choose how completed text is delivered separately.">
          <button className="btn" onClick={() => go("insert")}>Delivery options</button>
        </Row>
        <Row label="Hotkey" hint={linux ? "On Wayland this is a preferred binding. The portal permission dialog or desktop settings may assign a different shortcut; check the status below." : undefined}>
          <HotkeyInput value={s.hotkey.dictate} linux={linux} onChange={(v) => save((x) => (x.hotkey.dictate = v))} />
        </Row>
        <Row label="Behaviour" hint={linux ? `${MODE_HINT[s.hotkey.mode]} Hold needs shortcut-release events; use toggle when your desktop cannot supply them.` : MODE_HINT[s.hotkey.mode]}>
          <Segmented value={s.hotkey.mode} options={MODES} onChange={(m) => save((x) => (x.hotkey.mode = m))} />
        </Row>
      </Section>
      {!linux && s.terminal.enabled && (
        <Section title="Terminal commands">
          <Row label="Command hotkey" hint="Optional. Like the dictation hotkey, but what you say becomes a shell command, typed without Enter - in any window (set up on the Programs page).">
            <HotkeyInput value={s.terminal.hotkey} onChange={(v) => save((x) => (x.terminal.hotkey = v))} />
          </Row>
        </Section>
      )}
      {!linux && <Section title="Bubble">
        <Row label="Show / hide the bubble" hint="Optional.">
          <HotkeyInput value={s.hotkey.toggleBubble} onChange={(v) => save((x) => (x.hotkey.toggleBubble = v))} />
        </Row>
      </Section>}
      {linux ? <>
        <Section title="Custom desktop shortcuts">
          <Row label="Start / stop recording" hint={<>Add a desktop or i3 shortcut for <code>amber-dictation --toggle</code>. Press once to start, again to stop. This fallback is toggle-only. Do not assign the same binding in both i3 and Amber.</>} />
          <Row label="Cancel dictation" hint={<>Assign a second shortcut to <code>amber-dictation --cancel</code>. Cancellation stops dictation and microphone testing, not a previously completed result.</>} />
          <Row label="Optional explicit X11 paste" hint={<>For supported native editors, assign <code>amber-dictation --insert-last</code> to a desktop shortcut. Review a single-line result, focus its destination, then invoke the shortcut. Submission is not verified. Wayland, terminals, multiline text and other unsupported targets require Copy and manual paste.</>} />
          <Row label="If a shortcut is denied or unavailable" hint="Keep the recorder window available. Portal support and permissions vary by desktop version; a tray icon or extension is not required." />
        </Section>
        <DesktopDiagnostics desktop={desktop} />
      </> : <p className="note">The bubble works with the mouse as well: click to start and stop, hold to talk, drag to move it, right-click for the settings.</p>}
    </>
  );
}

const MODIFIER_CODES = new Set(["ControlLeft", "ControlRight", "ShiftLeft", "ShiftRight", "AltLeft", "AltRight", "MetaLeft", "MetaRight", "OSLeft", "OSRight"]);

/** Windows' own key codes of the letter, digit and punctuation keys, as the hotkey library
 *  names them - it turns those names back into the same codes. Not e.code: that is the key's
 *  place on a US keyboard, so on a German one Ctrl+Alt+Z would become Ctrl+Alt+Y. */
const OEM: Record<number, string> = {
  186: "Semicolon", 187: "Equal", 188: "Comma", 189: "Minus", 190: "Period", 191: "Slash",
  192: "Backquote", 219: "BracketLeft", 220: "Backslash", 221: "BracketRight", 222: "Quote",
};

function keyName(e: KeyboardEvent): string {
  const vk = e.keyCode;
  if (vk >= 65 && vk <= 90) return `Key${String.fromCharCode(vk)}`;
  if (vk >= 48 && vk <= 57) return `Digit${vk - 48}`;
  return OEM[vk] ?? e.code;
}

/** Turns a key press into Tauri's accelerator syntax: "Ctrl+Alt+Space", "Ctrl+Shift+KeyD". */
export function accelerator(e: KeyboardEvent): string | null {
  if (MODIFIER_CODES.has(e.code)) return null;
  const mods: string[] = [];
  if (e.ctrlKey) mods.push("Ctrl");
  if (e.altKey) mods.push("Alt");
  if (e.shiftKey) mods.push("Shift");
  if (e.metaKey) mods.push("Super");
  const fkey = /^F([1-9]|1[0-9]|2[0-4])$/.test(e.code);
  // Function keys may stand alone; anything else needs Ctrl, Alt or Win - with Shift alone,
  // Shift+letter would swallow every capital letter typed anywhere.
  if (!fkey && !(e.ctrlKey || e.altKey || e.metaKey)) return null;
  return [...mods, keyName(e)].join("+");
}

export function pretty(acc: string, linux = false) {
  return acc
    .split("+")
    .map((k) => k.replace(/^Key([A-Z])$/, "$1").replace(/^Digit(\d)$/, "$1").replace("Super", linux ? "Super" : "Win"))
    .join(" + ");
}

function HotkeyInput({ value, onChange, linux = false }: { value: string; onChange: (v: string) => void; linux?: boolean }) {
  const [recording, setRecording] = useState(false);

  useEffect(() => {
    if (!recording) return;
    const onKey = (e: KeyboardEvent) => {
      e.preventDefault();
      e.stopPropagation();
      if (e.code === "Escape") {
        setRecording(false);
        return;
      }
      const acc = accelerator(e);
      if (acc) {
        setRecording(false);
        onChange(acc);
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [recording, onChange]);

  return (
    <>
      <button className={`btn hotkey ${recording ? "recording" : ""}`} onClick={() => setRecording(!recording)} onBlur={() => setRecording(false)}>
        {recording ? "Press the keys…" : value ? pretty(value, linux) : "Not set"}
      </button>
      {value && !recording && (
        <button className="btn" onClick={() => onChange("")}>
          Clear
        </button>
      )}
    </>
  );
}
