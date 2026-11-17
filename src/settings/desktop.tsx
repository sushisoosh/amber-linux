import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { DesktopStatus, StorageStatus } from "../shared/types";
import { Row, Section } from "./ui";

export function useDesktop(enabled: boolean) {
  const [status, setStatus] = useState<DesktopStatus | null>(null);
  const [storage, setStorage] = useState<StorageStatus | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const sequence = useRef(0);

  const refresh = useCallback(async () => {
    if (!enabled) return;
    const mine = ++sequence.current;
    setLoading(true);
    try {
      const [next, stored] = await Promise.allSettled([
        invoke<DesktopStatus>("get_desktop_status"),
        invoke<StorageStatus>("get_storage_status"),
      ]);
      if (mine !== sequence.current) return;
      setStatus(next.status === "fulfilled" ? next.value : null);
      setStorage(stored.status === "fulfilled" ? stored.value : null);
      const errors = [next, stored].flatMap((result) => result.status === "rejected" ? [String(result.reason)] : []);
      setError(errors.length ? `Could not refresh capabilities: ${errors.join("; ")}` : null);
    } catch (e) {
      if (mine === sequence.current) setError(`Could not refresh desktop capabilities: ${e}`);
    } finally {
      if (mine === sequence.current) setLoading(false);
    }
  }, [enabled]);

  useEffect(() => {
    if (!enabled) return;
    let disposed = false;
    const unlisten = listen("linux-desktop-status", () => void refresh());
    void unlisten.then(() => { if (!disposed) void refresh(); }).catch((e) => {
      if (!disposed) setError(`Could not watch desktop capabilities: ${e}`);
    });
    return () => {
      disposed = true;
      sequence.current++;
      void unlisten.then((un) => un()).catch(() => {});
    };
  }, [enabled, refresh]);

  return { status, storage, error, loading, refresh };
}

export function DesktopDiagnostics({ desktop }: { desktop: ReturnType<typeof useDesktop> }) {
  const { status, storage, error, loading, refresh } = desktop;
  return (
    <Section title="Desktop capabilities">
      <Row label="Current session" hint={status?.session ?? "Checking desktop services..."}>
        <button className="btn" disabled={loading} onClick={() => void refresh()}>{loading ? "Checking..." : "Refresh"}</button>
      </Row>
      {error && <div className="row"><div className="note error" role="alert">{error}</div></div>}
      {status && <>
        <Row label="Global shortcuts" hint={status.shortcuts} />
        <Row label="Text insertion" hint={status.insertion} />
        <Row label="Clipboard" hint={status.clipboard} />
        {status.lifecycle && <Row label="Lock and suspend" hint={status.lifecycle} />}
        <Row label="Secure storage" hint={status.secureStorage} />
      </>}
      {storage && <>
        <Row label="Credentials" hint={storage.credentials} />
        <Row label="History storage" hint={storage.history} />
      </>}
    </Section>
  );
}
