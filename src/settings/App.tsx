// The settings window: a short list of pages, each showing its current value. Every change
// is saved at once - there is no "Apply".

import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import type { DictationEvent, Overview, Phase, Settings } from "../shared/types";
import { EnginePage } from "./pages/Engine";
import { MicrophonePage } from "./pages/Microphone";
import { HotkeysPage } from "./pages/Hotkeys";
import { BubblePage } from "./pages/Bubble";
import { InsertPage } from "./pages/Insert";
import { AppPage } from "./pages/AppPage";
import { WelcomePage } from "./pages/Welcome";
import { PhonePage } from "./pages/Phone";
import { CleanupPage } from "./pages/Cleanup";
import { ProgramsPage } from "./pages/Programs";
import { HistoryPage } from "./pages/History";
import { SnippetsPage } from "./pages/Snippets";
import { StatisticsPage } from "./pages/Statistics";
import { Recorder } from "./pages/Recorder";
import { useDesktop } from "./desktop";
import { phaseLabel } from "./ui";

export interface PageProps {
  ov: Overview;
  s: Settings;
  save: (change: (s: Settings) => void) => Promise<void>;
  refresh: () => Promise<void>;
  go: (page: PageId) => void;
  toast: (message: string, ok?: boolean) => void;
  phase: Phase;
  phaseMessage: string | null;
  desktop: ReturnType<typeof useDesktop>;
}

export type PageId = "recorder" | "welcome" | "engine" | "cleanup" | "programs" | "snippets" | "microphone" | "hotkeys" | "bubble" | "insert" | "phone" | "history" | "statistics" | "app";

const LINUX_HIDDEN = new Set<PageId>(["programs", "bubble", "insert", "phone"]);

const PAGES: { id: PageId; label: string; value: (o: Overview) => string }[] = [
  { id: "recorder", label: "Recorder", value: () => "" },
  { id: "engine", label: "Engine", value: (o) => (o.settings.engine === "local" ? "Local" : providerName(o)) },
  {
    id: "cleanup",
    label: "Cleanup",
    value: (o) => (o.settings.cleanup.smart ? (o.settings.cleanup.engine === "local" ? "Local" : "Server") : o.settings.cleanup.fillers ? "Fillers" : "Off"),
  },
  { id: "programs", label: "Programs", value: (o) => String(o.settings.profiles.length) },
  { id: "snippets", label: "Snippets", value: (o) => String(o.settings.snippets.length) },
  { id: "microphone", label: "Microphone", value: (o) => o.settings.microphone || "Default" },
  { id: "hotkeys", label: "Hotkeys", value: (o) => o.settings.hotkey.dictate || "None" },
  { id: "bubble", label: "Bubble", value: (o) => ({ always: "Shown", whileDictating: "When used", hidden: "Hidden" })[o.settings.bubble.visibility] },
  { id: "insert", label: "Inserting", value: (o) => (o.settings.insert.method === "paste" ? "Paste" : "Type") },
  { id: "phone", label: "Phone", value: (o) => (o.settings.phone.enabled ? "On" : "Off") },
  { id: "history", label: "History", value: (o) => (o.settings.history ? `${o.settings.historyDays} days` : "Off") },
  { id: "statistics", label: "Statistics", value: (o) => (o.settings.statistics ? "" : "Off") },
  { id: "app", label: "App", value: (o) => `v${o.version}` },
];

function providerName(o: Overview) {
  return o.providers.find((p) => p.id === o.settings.server.provider)?.name ?? "Server";
}

function initialPage(): PageId {
  const p = new URLSearchParams(location.search).get("page");
  return (["recorder", "welcome", "engine", "cleanup", "programs", "snippets", "microphone", "hotkeys", "bubble", "insert", "phone", "history", "statistics", "app"] as PageId[]).includes(p as PageId)
    ? (p as PageId)
    : "recorder";
}

export function App() {
  const [ov, setOv] = useState<Overview | null>(null);
  const [page, setPage] = useState<PageId>(initialPage);
  const [toastMsg, setToast] = useState<{ text: string; ok: boolean } | null>(null);
  const [phase, setPhase] = useState<Phase>("idle");
  const [phaseMessage, setPhaseMessage] = useState<string | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const phaseVersion = useRef(0);
  const linux = ov?.platform === "linux";
  const desktop = useDesktop(linux);
  // The latest settings, so two quick changes in a row build on each other - set by save()
  // and by answers that are still current, not by every render.
  const latest = useRef<Settings | null>(null);
  // Saves in flight: an answer to an older one must not undo a newer change, and closing the
  // window waits for them.
  const saveSeq = useRef(0);
  const saveTail = useRef<Promise<void>>(Promise.resolve());
  const pending = useRef(new Set<Promise<void>>());
  if (latest.current === null && ov) latest.current = ov.settings;

  const refresh = useCallback(async () => {
    const version = phaseVersion.current;
    const settingsVersion = saveSeq.current;
    const o = await invoke<Overview>("get_overview");
    setLoadError(null);
    if (settingsVersion === saveSeq.current && pending.current.size === 0) {
      latest.current = o.settings;
      setOv(o);
    } else {
      setOv((current) => current ? { ...o, settings: current.settings } : o);
    }
    if (version === phaseVersion.current) setPhase(o.phase);
  }, []);

  const toast = useCallback((text: string, ok = false) => {
    setToast({ text, ok });
    window.setTimeout(() => setToast((t) => (t?.text === text ? null : t)), ok ? 2500 : 6000);
  }, []);

  useEffect(() => {
    let disposed = false;
    const subs = [
      listen<Settings>("settings-changed", (e) => {
        // While a save of this window is under way, its own answer is the one to take.
        if (pending.current.size > 0) return;
        latest.current = e.payload;
        setOv((o) => (o ? { ...o, settings: e.payload } : o));
      }),
      listen<string>("navigate", (e) => setPage(e.payload as PageId)),
      listen<DictationEvent>("dictation-state", (e) => {
        phaseVersion.current++;
        setPhase(e.payload.state);
        setPhaseMessage(e.payload.message ?? null);
      }),
    ];
    void Promise.all(subs).then(() => {
      if (!disposed) return refresh();
    }).catch((e) => setLoadError(String(e)));
    // A field saves when it loses the focus - closing the window must not skip that: the
    // focus is taken first, and the window closes once the saves are through.
    const closing = getCurrentWindow().onCloseRequested(async (event) => {
      (document.activeElement as HTMLElement | null)?.blur();
      await new Promise((r) => setTimeout(r, 0));
      if (!linux) await Promise.race([Promise.allSettled([...pending.current]), new Promise((r) => setTimeout(r, 2000))]);
      if (pending.current.size > 0) {
        event.preventDefault();
        toast("Settings are still saving. This window has been kept open so queued changes are not lost. Close it again after the save finishes.");
      }
    });
    return () => {
      disposed = true;
      subs.forEach((p) => p.then((un) => un()).catch(() => {}));
      void closing.then((un) => un());
    };
  }, [refresh, toast, linux]);

  const save = useCallback(
    async (change: (s: Settings) => void) => {
      if (!latest.current) return;
      const next: Settings = structuredClone(latest.current);
      change(next);
      latest.current = next;
      setOv((o) => (o ? { ...o, settings: next } : o));
      const mine = ++saveSeq.current;
      // A later full snapshot must not commit before a slow wallet-backed save.
      const run = saveTail.current.then(async () => {
        try {
          const saved = await invoke<Settings>("save_settings", { settings: next });
          if (mine === saveSeq.current) {
            latest.current = saved;
            setOv((o) => (o ? { ...o, settings: saved } : o));
          }
        } catch (e) {
          toast(String(e));
          if (mine === saveSeq.current) {
            try {
              const o = await invoke<Overview>("get_overview");
              if (mine === saveSeq.current) {
                latest.current = o.settings;
                setOv(o);
              }
            } catch (refreshError) {
              toast(`Could not reload settings after the save failed: ${refreshError}`);
            }
          }
        }
      });
      saveTail.current = run;
      pending.current.add(run);
      await run;
      pending.current.delete(run);
      await desktop.refresh();
    },
    [desktop.refresh, toast],
  );

  const quit = async () => {
    (document.activeElement as HTMLElement | null)?.blur();
    await new Promise((r) => setTimeout(r, 0));
    if (pending.current.size > 0) {
      toast("Settings are still saving. Amber will stay open so queued changes are not lost. Quit again after the save finishes.");
      return;
    }
    try {
      await invoke("quit_app");
    } catch (e) {
      toast(`Could not quit Amber: ${e}`);
    }
  };

  if (!ov) return <main><h1>Amber</h1><p className="lead">{loadError ?? "Loading settings..."}</p>
    {loadError && <button className="btn" onClick={() => void refresh().catch((e) => setLoadError(String(e)))}>Retry</button>}
  </main>;
  const currentPage = linux && LINUX_HIDDEN.has(page) ? "recorder" : !linux && page === "recorder" ? "engine" : page;
  const props: PageProps = { ov, s: ov.settings, save, refresh, go: setPage, toast, phase, phaseMessage, desktop };

  return (
    <>
      <nav aria-label="Settings pages" className={linux ? "linux-nav" : undefined}>
        <div className="brand">
          <div className="disc" />
          Amber
        </div>
        {PAGES.filter((p) => linux ? !LINUX_HIDDEN.has(p.id) : p.id !== "recorder").map((p) => (
          <button key={p.id} className={currentPage === p.id ? "active" : ""} onClick={() => setPage(p.id)}>
            {p.label}
            <span className="value">{p.value(ov)}</span>
          </button>
        ))}
        <div className="spacer" />
        <button className={currentPage === "welcome" ? "active" : ""} onClick={() => setPage("welcome")}>
          Setup guide
        </button>
        <div className="status" role="status">{phaseLabel(phase)}</div>
      </nav>
      <main className={linux ? "linux-settings" : undefined}>
        {linux && <Recorder {...props} compact={currentPage !== "recorder"} />}
        {currentPage === "welcome" && <WelcomePage {...props} />}
        {currentPage === "engine" && <EnginePage {...props} />}
        {currentPage === "cleanup" && <CleanupPage {...props} />}
        {currentPage === "programs" && <ProgramsPage {...props} />}
        {currentPage === "snippets" && <SnippetsPage {...props} />}
        {currentPage === "microphone" && <MicrophonePage {...props} />}
        {currentPage === "hotkeys" && <HotkeysPage {...props} />}
        {currentPage === "bubble" && <BubblePage {...props} />}
        {currentPage === "insert" && <InsertPage {...props} />}
        {currentPage === "phone" && <PhonePage {...props} />}
        {currentPage === "history" && <HistoryPage {...props} />}
        {currentPage === "statistics" && <StatisticsPage {...props} />}
        {currentPage === "app" && <AppPage {...props} quit={quit} />}
      </main>
      {toastMsg && <div role="status" className={`toast ${toastMsg.ok ? "ok" : ""}`}>{toastMsg.text}</div>}
    </>
  );
}
