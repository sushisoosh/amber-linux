import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { ChatProvider, Connection, DiscoveredModel, ModelDiscovery, ProviderInfo, Settings } from "../shared/types";
import type { PageProps } from "./App";
import { Row, Switch } from "./ui";

export function availableConnections(connections: Connection[], providers: ProviderInfo[], purpose: "speech" | "cleanup") {
  return connections.filter((c) => providers.some((p) => p.id === c.provider && (purpose === "speech" ? p.supportsSpeech : p.supportsCleanup)));
}

export function eligibleModels(models: DiscoveredModel[], purpose: "speech" | "cleanup", unclassified: boolean) {
  return models.filter((m) => m.capabilities.includes(purpose) || (unclassified && m.capabilities.length === 0));
}

export function chooseConnection(settings: Settings, purpose: "speech" | "cleanup", id: string | null, providers: ProviderInfo[], cleanupProviders: ChatProvider[]) {
  const target = purpose === "speech" ? settings.server : settings.cleanup;
  target.connectionId = id;
  const connection = settings.connections.find((c) => c.id === id);
  const provider = connection?.provider ?? target.provider;
  target.model = purpose === "speech" ? providers.find((p) => p.id === provider)?.models[0]?.id ?? ""
    : cleanupProviders.find((p) => p.id === provider)?.models[0]?.id ?? "";
}

export function ConnectionChoice({ s, ov, save, go, purpose, cleanupProviders = [] }: PageProps & { purpose: "speech" | "cleanup"; cleanupProviders?: ChatProvider[] }) {
  const selected = purpose === "speech" ? s.server.connectionId : s.cleanup.connectionId;
  const connections = availableConnections(s.connections, ov.providers, purpose);
  return <>
    <Row label="Connection" hint="Provider default uses the original Credentials key. Saved connections have independent keys, even for the same provider.">
      <select aria-label={`${purpose === "speech" ? "Speech" : "Cleanup"} connection`} value={selected ?? ""} onChange={(e) => {
        const id = e.target.value || null;
        void save((next) => chooseConnection(next, purpose, id, ov.providers, cleanupProviders));
      }}>
        <option value="">Provider default</option>
        {selected && !connections.some((c) => c.id === selected) && <option value={selected}>Missing or incompatible connection</option>}
        {connections.map((c) => <option key={c.id} value={c.id}>{c.name}</option>)}
      </select>
    </Row>
    <Row label="Saved connections" hint={selected ? "The selected connection's endpoint and key are used. Editing its provider or address can require saving its key again." : "Save multiple accounts or compatible servers, then discover their available models without making an inference request."}>
      <button className="btn" onClick={() => go("connections")}>Manage connections</button>
    </Row>
  </>;
}

export function useModelDiscovery(connection: Connection) {
  const [result, setResult] = useState<ModelDiscovery | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const generation = useRef(0);
  const invalidate = () => {
    generation.current++;
    setResult(null);
    setError(null);
    setBusy(false);
  };
  useEffect(() => {
    invalidate();
    let active = true;
    const subscription = listen<string>("connection-key-changed", (event) => {
      if (active && event.payload === connection.id) invalidate();
    });
    return () => { active = false; generation.current++; void subscription.then((unlisten) => unlisten()).catch(() => {}); };
  }, [connection.id, connection.provider, connection.baseUrl, connection.name]);
  const discover = async () => {
    const mine = ++generation.current;
    setBusy(true);
    setError(null);
    setResult(null);
    try {
      const next = await invoke<ModelDiscovery>("discover_connection_models", { connection, key: null });
      if (mine === generation.current) setResult(next);
    } catch (e) {
      if (mine === generation.current) setError(String(e));
    } finally {
      if (mine === generation.current) setBusy(false);
    }
  };
  return { result, busy, error, discover, invalidate };
}

export function ConnectionModelPicker({ connection, purpose, model, presets, onSelect, disabled }: {
  connection: Connection;
  purpose: "speech" | "cleanup";
  model: string;
  presets: { id: string; label: string; note: string }[];
  onSelect: (id: string) => void;
  disabled: boolean;
}) {
  const discovery = useModelDiscovery(connection);
  const [unknown, setUnknown] = useState(false);
  const [manual, setManual] = useState(model);
  useEffect(() => { setUnknown(false); }, [connection.id, connection.provider, connection.baseUrl]);
  useEffect(() => setManual(model), [model]);
  const models = discovery.result ? eligibleModels(discovery.result.models, purpose, unknown) : presets;
  return <>
    <Row label={purpose === "speech" ? "Speech model" : "Cleanup model"} hint={models.find((m) => m.id === model)?.note ?? "Choose a provider preset or discover models from this saved connection."}>
      <select aria-label={`${purpose} connection model`} value={model} disabled={disabled} onChange={(e) => onSelect(e.target.value)}>
        {!model && <option value="">Choose a model</option>}
        {model && !models.some((m) => m.id === model) && <option value={model}>{model} (current selection)</option>}
        {models.map((m) => <option key={m.id} value={m.id}>{m.label}</option>)}
      </select>
    </Row>
    <Row label="Model discovery" hint="Fetches a bounded catalog using this connection's key. A listed model is not proof of inference access, pricing or support for every request parameter.">
      <button className="btn" disabled={discovery.busy || disabled} onClick={() => void discovery.discover()}>{discovery.busy ? "Discovering..." : "Discover models"}</button>
    </Row>
    {discovery.result && <Row label={`${discovery.result.models.length} models returned${discovery.result.truncated ? " (limited)" : ""}`} hint={discovery.result.notice} />}
    {discovery.result?.models.some((m) => m.capabilities.length === 0) && <Row label="Show unclassified models" hint="The endpoint did not provide enough compatible capability information. These may not support this operation or Amber's request parameters; select only a model you know is compatible.">
      <Switch on={unknown} onChange={setUnknown} />
    </Row>}
    {discovery.error && <div className="row"><div role="alert" className="note error">{discovery.error}</div></div>}
    <div className="row column"><details>
      <summary>Manual model ID (advanced)</summary>
      <p className="note">For compatible models missing from the catalog. This does not change the provider protocol. Reasoning models with different token or temperature parameters may not work.</p>
      <div className="model-actions">
        <input type="text" aria-label={`Manual ${purpose} model ID`} value={manual} maxLength={256} onChange={(e) => setManual(e.target.value)} />
        <button className="btn" disabled={disabled || !manual.trim() || manual.trim() === model} onClick={() => onSelect(manual.trim())}>Use model ID</button>
      </div>
    </details></div>
  </>;
}
