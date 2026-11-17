import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { LogEntry, UpdateInfo } from "../../shared/types";
import type { PageProps } from "../App";
import { Row, Section, Switch } from "../ui";

export function AppPage({ s, save, ov, toast, phase, quit }: PageProps & { quit: () => Promise<void> }) {
  const linux = ov.platform === "linux";
  const [quitting, setQuitting] = useState(false);
  const [update, setUpdate] = useState<UpdateInfo | null | undefined>(undefined);
  const [busy, setBusy] = useState<"check" | "install" | null>(null);
  const [log, setLog] = useState<LogEntry[]>([]);

  const loadLog = async () => {
    try {
      setLog((await invoke<LogEntry[]>("get_log")).reverse());
    } catch (e) {
      toast(`Could not load diagnostics: ${e}`);
    }
  };
  useEffect(() => {
    void loadLog();
  }, []);

  const check = async () => {
    setBusy("check");
    try {
      setUpdate(await invoke<UpdateInfo | null>("check_update"));
    } catch (e) {
      toast(`Update check failed: ${e}`);
    }
    setBusy(null);
  };

  const install = async () => {
    setBusy("install");
    try {
      // True: Amber restarts into the new version; false: there was none after all.
      if (!(await invoke<boolean>("install_update"))) {
        toast("No update available.", true);
        setBusy(null);
      }
    } catch (e) {
      toast(`Update failed: ${e}`);
      setBusy(null);
    }
  };

  return (
    <>
      <h1>App</h1>
      <p className="lead">Amber {ov.version}</p>
      <Section>
        <Row label={linux ? "Start at desktop login" : "Start with Windows"} hint={linux ? "Writes a user XDG autostart entry. Bare i3 needs an XDG launcher such as dex, or an explicit exec line in its config; this switch alone does not start programs in i3." : "Amber waits quietly in the tray until you dictate."}>
          <Switch on={s.autostart} onChange={(v) => save((x) => (x.autostart = v))} />
        </Row>
        {linux && <Row label="Quit Amber" hint="Stops Amber and discards the in-memory result. Stop or cancel any active dictation first. A tray icon is not needed to quit.">
          <button className="btn danger" disabled={quitting || phase === "recording" || phase === "transcribing" || phase === "inserting"} onClick={async () => {
            setQuitting(true);
            try { await quit(); } finally { setQuitting(false); }
          }}>{quitting ? "Quitting..." : "Quit Amber"}</button>
        </Row>}
        {!linux && <><Row label="Update automatically" hint="New versions are installed in the background when you are not dictating.">
          <Switch on={s.autoUpdate} onChange={(v) => save((x) => (x.autoUpdate = v))} />
        </Row>
        <Row
          label="Updates"
          hint={
            update === undefined
              ? undefined
              : update
                ? `Version ${update.version} is available.`
                : "Amber is up to date."
          }
        >
          {update ? (
            <button className="btn primary" disabled={busy !== null} onClick={install}>
              {busy === "install" ? "Installing…" : `Install ${update.version}`}
            </button>
          ) : (
            <button className="btn" disabled={busy !== null} onClick={check}>
              {busy === "check" ? "Checking…" : "Check now"}
            </button>
          )}
        </Row>
        <Row label="Project page" hint="Release notes, the installer, and where to report problems.">
          <button className="btn" onClick={() => invoke("open_url", { url: "https://github.com/EinFabo/amberpc/releases" })}>
            Open
          </button>
        </Row></>}
      </Section>
      <Section title="Diagnostics">
        {linux ? <Row label="Text logging disabled" hint="Recognized text logging is disabled on Linux. Review diagnostics before sharing them." /> : <Row label="Keep the recognized text in the log" hint="Off: only lengths and timings are logged, never what you said.">
          <Switch on={s.logText} onChange={(v) => save((x) => (x.logText = v))} />
        </Row>}
        <div className="row">
          <div className="label">
            <div>Recent events</div>
          </div>
          <button className="btn" onClick={loadLog}>
            Refresh
          </button>
          <button
            className="btn"
            onClick={async () => {
              try {
                const text = log.map((l) => `${l.time} ${l.level} ${l.message}`).join("\n");
                if (linux) await invoke("copy_text", { text });
                else await navigator.clipboard.writeText(text);
                toast("Log copied.", true);
              } catch (e) {
                toast(`Could not copy: ${e}`);
              }
            }}
          >
            Copy
          </button>
        </div>
        <div className="log">
          {log.length === 0 && <div className="note">Nothing yet.</div>}
          {log.map((l, i) => (
            <div key={i} className={l.level}>
              <span className="time">{l.time.slice(11, 19)}</span>
              {l.message}
            </div>
          ))}
        </div>
      </Section>
    </>
  );
}
