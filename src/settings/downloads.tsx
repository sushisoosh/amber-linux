// Downloadable parts (engine runtimes and models, the cleanup model) and the row that offers,
// tracks, selects and deletes one. Shared by the Engine, Cleanup and setup pages.

import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { DownloadEvent, LocalInfo, LocalItem, LocalStatus } from "../shared/types";
import { Progress, size } from "./ui";

export type DownloadKind = "runtime" | "model" | "llmRuntime" | "llmModel";

/** What is installed, what is downloading, and the state of both local servers. */
export function useLocal() {
  const [info, setInfo] = useState<LocalInfo | null>(null);
  const [progress, setProgress] = useState<Record<string, DownloadEvent>>({});
  const [errors, setErrors] = useState<Record<string, string>>({});

  const reload = useCallback(async () => setInfo(await invoke<LocalInfo>("local_info")), []);

  useEffect(() => {
    void reload();
    const subs = [
      listen<DownloadEvent>("download", (e) => {
        const d = e.payload;
        setProgress((p) => ({ ...p, [d.key]: d }));
        if (d.state === "failed") setErrors((x) => ({ ...x, [d.key]: d.message ?? "failed" }));
        if (d.state !== "running") {
          setProgress((p) => {
            const { [d.key]: _, ...rest } = p;
            return rest;
          });
          void reload();
        }
      }),
      listen<LocalStatus>("local-status", (e) => setInfo((i) => (i ? { ...i, status: e.payload } : i))),
      listen<LocalStatus>("llm-status", (e) => setInfo((i) => (i ? { ...i, llmStatus: e.payload } : i))),
      listen("local-models-changed", () => { void reload(); }),
    ];
    return () => subs.forEach((s) => s.then((un) => un()));
  }, [reload]);

  /** [select]: Amber makes it the one in use once it is installed. */
  const download = async (kind: DownloadKind, id: string, select: boolean) => {
    const key = `${kind}:${id}`;
    setErrors(({ [key]: _, ...rest }) => rest);
    setProgress((p) => ({ ...p, [key]: { key, done: 0, total: null, state: "running" } }));
    try {
      await invoke("download", { kind, id, select });
    } catch (e) {
      setProgress(({ [key]: _, ...rest }) => rest);
      setErrors((x) => ({ ...x, [key]: String(e) }));
      throw e;
    }
  };

  return { info, progress, errors, reload, download };
}

export type Local = ReturnType<typeof useLocal>;

export function DownloadItem({
  local,
  kind,
  item,
  selected,
  onSelect,
  recommended,
  warning,
  measured,
  builtIn = false,
  selectOnDownload = true,
  toast,
}: {
  local: Local;
  kind: DownloadKind;
  item: LocalItem;
  selected: boolean;
  onSelect: () => void;
  recommended?: boolean;
  warning?: string;
  /** Measured milliseconds for 10 s of speech on this PC. */
  measured?: number;
  builtIn?: boolean;
  selectOnDownload?: boolean;
  toast: (message: string) => void;
}) {
  const { info, progress, errors, reload, download } = local;
  const key = `${kind}:${item.id}`;
  const included = builtIn || (kind === "runtime" && item.sizeMb === 0 && item.installed);
  const running = progress[key] ?? (info?.downloads.includes(key) ? { key, done: 0, total: null, state: "running" as const } : null);
  return (
    <div className="item">
      <button
        type="button"
        className={`radio ${selected ? "on" : ""} ${!item.installed ? "disabled" : ""}`}
        aria-label={`Use ${item.label}`}
        aria-pressed={selected}
        disabled={!item.installed}
        onClick={onSelect}
        title={item.installed ? "" : item.imported ? "Imported files are missing; import the source again" : "Download it first"}
      />
      <div className="name">
        <div>
          {item.label}
          {recommended && <span className="tag amber">recommended</span>}
          {item.replaced && <span className="tag">replaced</span>}
          {item.installed && <span className="tag ok">installed</span>}
          {item.imported && <span className="tag">imported</span>}
          {item.imported && !item.installed && <span className="tag">files missing</span>}
        </div>
        <div className="hint">
          {item.note} · {included ? "Built into Amber; no download needed" : size(item.sizeMb)}
          {measured !== undefined && ` · ${(measured / 1000).toFixed(1)} s for 10 s of speech here`}
          {warning && ` · ${warning}`}
        </div>
        {errors[key] && <div className="note error">{errors[key]}</div>}
      </div>
      {included ? <span className="tag ok">built-in</span> : running ? (
        <>
          <Progress done={running.done} total={running.total} />
          <button className="btn" onClick={() => invoke("cancel_download", { kind, id: item.id })}>
            Cancel
          </button>
        </>
      ) : item.installed || item.imported ? (
        <button
          className="btn danger"
          onClick={async () => {
            try {
              if (item.imported) await invoke("remove_imported_model", { id: item.id });
              else await invoke("remove_download", { kind, id: item.id });
            } catch (e) {
              toast(String(e));
            }
            void reload();
          }}
        >
          Delete
        </button>
      ) : (
        <button className="btn" onClick={() => download(kind, item.id, selectOnDownload).catch((e) => toast(String(e)))}>
          {selectOnDownload ? "Download & use" : "Download"}
        </button>
      )}
    </div>
  );
}

export function SpeechModelActions({ status, busy, toast }: { status: LocalStatus; busy: boolean; toast: (message: string) => void }) {
  const [pending, setPending] = useState(false);
  const run = async (unload: boolean) => {
    setPending(true);
    try {
      await invoke(unload ? "stop_local" : "start_local");
    } catch (error) {
      toast(String(error));
    } finally {
      setPending(false);
    }
  };
  const disabled = busy || pending || status.state === "starting";
  return <div className="model-actions">
    {status.state !== "ready" && <button className="btn" disabled={disabled} onClick={() => run(false)}>
      {pending ? "Please wait..." : "Load now"}
    </button>}
    {(status.state === "ready" || status.state === "failed") && <button className="btn" disabled={disabled} onClick={() => run(true)}>
      {pending ? "Please wait..." : "Unload model"}
    </button>}
  </div>;
}

/** "Ready", "Loading…" and so on, with a dot in the matching colour. */
export function StatusLabel({ status }: { status: LocalStatus }) {
  return (
    <>
      <span className={`dot ${status.state}`} />
      {{ stopped: "Not loaded", starting: "Loading…", ready: "Ready", failed: "Not available" }[status.state]}
    </>
  );
}
