import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { ChatProvider, Connection } from "../../shared/types";
import type { PageProps } from "../App";
import { Row, Section } from "../ui";
import { useModelDiscovery } from "../connections";

export function ConnectionsPage(p: PageProps) {
  const [adding, setAdding] = useState(false);
  return <div className="connections-page credentials">
    <h1>Connections</h1>
    <p className="lead">Save named provider accounts and compatible endpoints. Each connection has its own encrypted key; existing provider keys are never copied automatically. Select a connection under Engine or Cleanup.</p>
    <Section title="Saved connections">
      <Row label={`${p.s.connections.length} connections`} hint="Discovery only reads the provider's model catalog. It does not send recordings, text, or a paid inference request.">
        <button className="btn primary" disabled={adding || p.settingsSaving || p.s.connections.length >= 32} onClick={() => setAdding(true)}>Add connection</button>
      </Row>
      {!p.s.connections.length && <Row label="No saved connections" hint="Your existing Engine, Cleanup and Credentials choices keep working. Add a connection to use multiple accounts or discover models." />}
    </Section>
    {adding && <ConnectionEditor {...p} onDone={() => setAdding(false)} />}
    {p.s.connections.map((c) => <SavedConnection key={c.id} {...p} connection={c} />)}
  </div>;
}

function ConnectionEditor({ connection, onDone, ...p }: PageProps & { connection?: Connection; onDone: () => void }) {
  const [name, setName] = useState(connection?.name ?? "");
  const [provider, setProvider] = useState(connection?.provider ?? "custom");
  const [baseUrl, setBaseUrl] = useState(connection?.baseUrl ?? "");
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const preset = p.ov.providers.find((v) => v.id === provider);
  const submit = async () => {
    setSaving(true);
    setError(null);
    const next: Connection = { id: connection?.id ?? `conn_${crypto.randomUUID()}`, name: name.trim(), provider, baseUrl: preset?.baseUrl || baseUrl.trim() };
    let cleanupModel = "";
    if (connection && connection.provider !== next.provider && p.s.cleanup.connectionId === next.id) {
      try { cleanupModel = (await invoke<ChatProvider[]>("chat_providers")).find((v) => v.id === next.provider)?.models[0]?.id ?? ""; }
      catch (e) { setError(String(e)); setSaving(false); return; }
    }
    const saved = await p.save((s) => {
      const index = s.connections.findIndex((c) => c.id === next.id);
      if (index < 0) s.connections.push(next);
      else s.connections[index] = next;
      if (connection && connection.provider !== next.provider) {
        if (s.server.connectionId === next.id) s.server.model = p.ov.providers.find((v) => v.id === next.provider)?.models[0]?.id ?? "";
        if (s.cleanup.connectionId === next.id) s.cleanup.model = cleanupModel;
      }
    });
    setSaving(false);
    if (saved) onDone();
    else setError("The connection was not saved. Check the error and try again.");
  };
  return <Section title={connection ? `Edit ${connection.name}` : "New connection"}>
    <Row label="Name"><input type="text" aria-label="Connection name" maxLength={80} value={name} onChange={(e) => setName(e.target.value)} /></Row>
    <Row label="Provider / protocol">
      <select aria-label="Connection provider" value={provider} onChange={(e) => { setProvider(e.target.value); setBaseUrl(p.ov.providers.find((v) => v.id === e.target.value)?.baseUrl ?? ""); }}>
        {p.ov.providers.map((v) => <option key={v.id} value={v.id}>{v.name}</option>)}
      </select>
    </Row>
    <Row label="Address" hint={provider === "custom" ? "OpenAI-compatible base URL. HTTPS required except for a literal loopback IP. No credentials, query strings or fragments in the URL." : "Named providers use their official fixed endpoint. Choose Own server for a different compatible address."}>
      <input type="text" className="credential-address" aria-label="Connection address" value={preset?.baseUrl || baseUrl} readOnly={provider !== "custom"} maxLength={2048} placeholder="https://your-server.example/v1" onChange={(e) => setBaseUrl(e.target.value)} />
    </Row>
    {connection && <Row label="Key safety" hint="Changing provider or endpoint invalidates stale requests. A key bound to another provider or origin is never reused; save a key for the edited connection if required." />}
    <div className="row model-actions">
      <button className="btn primary" disabled={saving || p.settingsSaving || !name.trim() || !(preset?.baseUrl || baseUrl.trim())} onClick={() => void submit()}>{saving ? "Saving..." : "Save connection"}</button>
      <button className="btn" disabled={saving} onClick={onDone}>Cancel</button>
      {error && <span role="alert" className="note error">{error}</span>}
    </div>
  </Section>;
}

function SavedConnection({ connection, ...p }: PageProps & { connection: Connection }) {
  const [editing, setEditing] = useState(false);
  const [key, setKey] = useState("");
  const [hasKey, setHasKey] = useState<boolean | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [test, setTest] = useState<string | null>(null);
  const discovery = useModelDiscovery(connection);
  const provider = p.ov.providers.find((v) => v.id === connection.provider);
  const selected = p.s.server.connectionId === connection.id || p.s.cleanup.connectionId === connection.id;
  useEffect(() => {
    let alive = true;
    setKey(""); setHasKey(null); setError(null); setTest(null);
    void invoke<boolean>("has_connection_key", { connection }).then((v) => { if (alive) setHasKey(v); }).catch((e) => { if (alive) setError(String(e)); });
    return () => { alive = false; };
  }, [connection.id, connection.provider, connection.baseUrl]);
  const changeKey = async (value: string | null) => {
    discovery.invalidate();
    setBusy(true); setError(null); setTest(null);
    try {
      setHasKey(await invoke<boolean>("set_connection_key", { connection, key: value }));
      setKey("");
      p.toast(value === null ? "Connection key removed." : "Connection key saved.", true);
    } catch (e) {
      setError(String(e));
      await invoke<boolean>("has_connection_key", { connection }).then(setHasKey).catch(() => setHasKey(null));
    } finally { setBusy(false); }
  };
  const check = async () => {
    setBusy(true); setError(null); setTest(null);
    try {
      const ms = await invoke<number>("test_connection", { connection, key: null });
      setTest(`Connection responded in ${ms} ms. This does not verify inference access or billing.`);
    } catch (e) { setError(String(e)); }
    finally { setBusy(false); }
  };
  if (editing) return <ConnectionEditor key={`${connection.id}:${connection.provider}:${connection.baseUrl}`} {...p} connection={connection} onDone={() => setEditing(false)} />;
  return <Section title={connection.name}>
    <Row label={provider?.name ?? connection.provider} hint={connection.baseUrl}>
      <div className="model-actions">
        <button className="btn" disabled={busy || p.settingsSaving} onClick={() => setEditing(true)}>Edit</button>
        <button className="btn danger" disabled={busy || selected || p.settingsSaving} onClick={async () => {
          // Remove the key first; a storage failure must not claim full removal.
          setBusy(true); setError(null);
          discovery.invalidate();
          try {
            await invoke("set_connection_key", { connection, key: null });
            setHasKey(false);
            await p.save((s) => { s.connections = s.connections.filter((c) => c.id !== connection.id); });
          } catch (e) { setError(String(e)); }
          finally { setBusy(false); }
        }}>Remove</button>
      </div>
    </Row>
    {selected && <Row label="In use" hint="Select another connection under Engine and Cleanup before removing this one." />}
    <Row label="Connection key" hint={hasKey === null ? "Checking secure storage..." : hasKey ? "An independent key is available. Stored keys are never displayed." : provider?.needsKey ? "Save a key for this connection; the provider-default key is not reused." : "Optional for endpoints that do not require authentication."}>
      <div className="credential-controls">
        <input type="password" aria-label={`${connection.name} connection key`} autoComplete="off" autoCapitalize="none" spellCheck={false} value={key} disabled={busy} onChange={(e) => setKey(e.target.value)} />
        <button className="btn" disabled={busy || !key.trim()} onClick={() => void changeKey(key.trim())}>Save key</button>
        {hasKey && <button className="btn danger" disabled={busy} onClick={() => void changeKey(null)}>Remove key</button>}
      </div>
    </Row>
    <Row label="Connection check" hint={test ?? "Uses the saved connection and key without making an inference request."}>
      <button className="btn" disabled={busy} onClick={() => void check()}>{busy ? "Please wait..." : "Check connection"}</button>
    </Row>
    <Row label="Available models" hint="Capability labels are based on supported API information, not guesses from model names. Unclassified models need manual compatibility checks.">
      <button className="btn" disabled={busy || discovery.busy} onClick={() => void discovery.discover()}>{discovery.busy ? "Discovering..." : "Discover models"}</button>
    </Row>
    {discovery.result && <>
      <Row label={`${discovery.result.models.length} models${discovery.result.truncated ? " (limited)" : ""}`} hint={discovery.result.notice} />
      <div className="connection-models" role="list" aria-label={`${connection.name} discovered models`}>
        {discovery.result.models.map((m) => <div className="item" role="listitem" key={m.id}><div className="name"><div>{m.label}</div><div className="hint">{m.id} | {m.capabilities.length ? m.capabilities.join(", ") : "Unclassified"}{m.note ? ` | ${m.note}` : ""}</div></div></div>)}
      </div>
    </>}
    {(error || discovery.error) && <div className="row"><div className="note error" role="alert">{error || discovery.error}</div></div>}
  </Section>;
}
