// First start: one decision (local or server), the downloads it needs, and how to dictate.

import { useEffect, useState } from "react";
import type { PageProps } from "../App";
import { Section, Row, Progress, size } from "../ui";
import { useLocal } from "../downloads";
import { pretty } from "./Hotkeys";

export function WelcomePage(p: PageProps) {
  const { s, save, go, toast } = p;
  const linux = p.ov.platform === "linux";
  const { info, progress, errors, download } = useLocal();

  // Seeing this page once is enough; next start opens the normal settings.
  useEffect(() => {
    if (!s.onboarded) void save((x) => (x.onboarded = true));
  }, [s.onboarded, save]);
  const [wantLocal, setWantLocal] = useState(false);
  const bothThere = !!info && info.runtimes.some((r) => r.id === info.recommended[0] && r.installed) && info.models.some((m) => m.id === info.recommended[1] && m.installed);
  useEffect(() => {
    if (!wantLocal || !bothThere || !info) return;
    setWantLocal(false);
    void save((x) => {
      x.engine = "local";
      x.local.runtime = info.recommended[0];
      x.local.model = info.recommended[1];
    });
  }, [wantLocal, bothThere, info, save]);

  if (!info) return null;
  const [rtId, modelId] = info.recommended;
  const rt = info.runtimes.find((r) => r.id === rtId)!;
  const model = info.models.find((m) => m.id === modelId)!;
  const ready = rt.installed && model.installed;
  const rtDl = progress[`runtime:${rtId}`];
  const modelDl = progress[`model:${modelId}`];
  const downloading = !!(rtDl || modelDl) || info.downloads.length > 0;

  // The engine changes only once both parts are there: until then, whatever works now keeps
  // working (a server user would otherwise be left without dictation for the download).
  const setupLocal = async () => {
    setWantLocal(true);
    try {
      if (!linux && !rt.installed) await download("runtime", rtId, true);
      if (!model.installed) await download("model", modelId, true);
    } catch (e) {
      setWantLocal(false);
      toast(String(e));
    }
  };

  return (
    <>
      <h1>Welcome to Amber</h1>
      <p className="lead">
        {linux ? "Speak, review your text, then copy and paste into the app you choose. Pick where your voice is recognized; you can change everything later." : "Speak, and the text appears where your cursor is - in any app. Pick where your voice is recognized; you can change everything later."}
      </p>
      <div className="welcome-steps">
        <Section title="1 · Engine">
          <Row
            label={`On this PC: ${model.label} on ${rt.label}`}
            hint={
              linux
                ? `The CPU runtime is built in. ${model.installed ? "The model is installed." : `Model download: ${size(model.sizeMb)}.`} Local recognition works offline after setup.`
                : info.hasNvidia
                ? `Your NVIDIA GPU makes this fast. Download: ${size((rt.installed ? 0 : rt.sizeMb) + (model.installed ? 0 : model.sizeMb))}.`
                : `Runs on your processor. Download: ${size((rt.installed ? 0 : rt.sizeMb) + (model.installed ? 0 : model.sizeMb))}.`
            }
          >
            {ready && s.engine === "local" ? (
              <span className="tag ok">ready</span>
            ) : downloading ? (
              <Progress done={(rtDl?.done ?? 0) + (modelDl?.done ?? 0)} total={(rtDl?.total ?? 0) + (modelDl?.total ?? 0) || null} />
            ) : (
              <button className="btn primary" onClick={setupLocal}>
                {ready ? "Use it" : "Set up"}
              </button>
            )}
          </Row>
          {(errors[`runtime:${rtId}`] || errors[`model:${modelId}`]) && <div className="row"><div className="note error" role="alert">{errors[`runtime:${rtId}`] ?? errors[`model:${modelId}`]}</div></div>}
          <Row label="Or a server with your API key" hint={linux ? "Optional: send audio to Groq, OpenAI or your own server. Provider charges may apply; local recognition needs no key." : "Groq is fast and costs about 4 cents per hour of speech."}>
            <button
              className="btn"
              onClick={async () => {
                await save((x) => (x.engine = "server"));
                go("engine");
              }}
            >
              Use a server
            </button>
          </Row>
        </Section>
        <Section title="2 · Microphone">
          <Row label={s.microphone || "System default microphone"}>
            <button className="btn" onClick={() => go("microphone")}>
              Test
            </button>
          </Row>
        </Section>
        <Section title="3 · Dictate">
          <Row
            label={
              linux ? "Record and stop in the recorder window. Review the result before copying." : s.hotkey.dictate ? (
                <>
                  Press <b>{pretty(s.hotkey.dictate)}</b> - {s.hotkey.mode === "toggle" ? "once to start, again to stop" : s.hotkey.mode === "hold" ? "and hold it while you speak" : "tap to start and stop, or hold while you speak"}.
                </>
              ) : (
                "Set a hotkey to dictate from anywhere."
              )
            }
            hint={linux ? "No tray or bubble is needed. Copy replaces the clipboard; clipboard managers may retain text. Automatic insertion is not promised." : "Or click the amber bubble at the edge of your screen."}
          >
            <button className="btn" onClick={() => go(linux ? "recorder" : "insert")}>
              Try it
            </button>
          </Row>
        </Section>
        {linux && <Section title="4 · Desktop integration">
          <Row label="Check shortcuts and permissions" hint={<>If your desktop cannot register a global shortcut, configure <code>amber-dictation --toggle</code> and <code>amber-dictation --cancel</code> in its keyboard settings or i3 config.</>}>
            <button className="btn" onClick={() => go("hotkeys")}>Check capabilities</button>
          </Row>
          <Row label="Private by default" hint="History is opt-in. Persistent credentials and encrypted history require an available Secret Service wallet. A failed key save may leave a session-only key; check the storage status on the Engine page." />
        </Section>}
      </div>
    </>
  );
}
