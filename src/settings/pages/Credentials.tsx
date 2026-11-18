import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { ProviderInfo } from "../../shared/types";
import type { PageProps } from "../App";
import { Row, Section } from "../ui";

export function CredentialsPage({ ov, s, refresh, desktop, toast }: PageProps) {
  const [refreshing, setRefreshing] = useState(false);
  const linux = ov.platform === "linux";
  const addresses: [string, string][] = [
    ["Speech", s.server.baseUrl],
    ["Cleanup", s.cleanup.baseUrl.trim() || s.server.baseUrl],
  ];
  if (s.cleanup.engine === "server" && s.cleanup.provider === "custom") addresses.reverse();
  const refreshStatus = async () => {
    setRefreshing(true);
    try {
      await Promise.all([refresh(), linux ? desktop.refresh() : Promise.resolve()]);
    } catch (e) {
      toast(`Could not refresh credential status: ${e}`);
    } finally {
      setRefreshing(false);
    }
  };
  return (
    <div className="credentials">
      <h1>Credentials</h1>
      <p className="lead">Manage API keys independently of your engine and cleanup choices. A provider's key is shared by recognition and cleanup; adding or removing one does not change either selection. Saved keys are never displayed.</p>
      <Section title="Storage">
        <Row label="Credential storage" hint={linux ? desktop.storage?.credentials ?? desktop.error ?? "Checking secure storage..." : "Saved keys are encrypted for your Windows account."}>
          <button className="btn" disabled={refreshing || desktop.loading} onClick={() => void refreshStatus()}>{refreshing ? "Refreshing..." : "Refresh status"}</button>
        </Row>
        {linux && <Row label="Persistence" hint="If your Secret Service wallet is locked or unavailable, a key change may work for this session only. Check the save result before relying on it after a restart." />}
      </Section>
      {ov.providers.map((provider) => <CredentialEditor
        key={provider.id} provider={provider} addresses={addresses}
        refresh={refreshStatus} toast={toast}
      />)}
    </div>
  );
}

function CredentialEditor({ provider, addresses, refresh, toast }: {
  provider: ProviderInfo;
  addresses: [string, string][];
  refresh: () => Promise<void>;
  toast: PageProps["toast"];
}) {
  const custom = provider.id === "custom";
  const [baseUrl, setBaseUrl] = useState(addresses.find(([, value]) => value.trim())?.[1] ?? "");
  const [key, setKey] = useState("");
  const [hasKey, setHasKey] = useState<boolean | null>(custom ? null : provider.hasKey);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [statusError, setStatusError] = useState<string | null>(null);
  useEffect(() => {
    setStatusError(null);
    if (!custom) {
      setHasKey(provider.hasKey);
      return;
    }
    setHasKey(null);
    if (!baseUrl.trim()) return;
    let disposed = false;
    void invoke<boolean>("has_api_key", { provider: provider.id, baseUrl: baseUrl.trim() }).then((available) => {
      if (!disposed) setHasKey(available);
    }).catch((e) => {
      if (!disposed) setStatusError(`Could not check this address: ${e}`);
    });
    return () => { disposed = true; };
  }, [provider, custom, baseUrl]);

  const chooseAddress = (value: string) => {
    setBaseUrl(value);
    setKey("");
    setHasKey(null);
    setError(null);
  };
  const changeKey = async (value: string | null) => {
    if (saving || (custom && !baseUrl.trim()) || (value !== null && !value.trim())) return;
    setSaving(true);
    setError(null);
    const args = { provider: provider.id, ...(custom ? { baseUrl: baseUrl.trim() } : {}) };
    try {
      setHasKey(await invoke<boolean>("set_api_key", { ...args, key: value?.trim() ?? null }));
      setKey("");
      toast(`${provider.name} key ${value === null ? "removed" : "saved"}.`, true);
    } catch (e) {
      setError(`The key change could not be confirmed. It may be active for this session only. ${e}`);
    } finally {
      // A failed persistent save can still change the session's credential.
      await Promise.all([
        refresh(),
        invoke<boolean>("has_api_key", args).then(setHasKey).catch((e) => setStatusError(`Could not refresh key availability: ${e}`)),
      ]);
      setSaving(false);
    }
  };

  return (
    <Section title={provider.name}>
      <form onSubmit={(e) => { e.preventDefault(); void changeKey(key); }}>
        {custom && <>
          <Row label="Address for this key" hint="Keys are scoped to the server's scheme, host and port, on both desktop platforms. This field chooses the key to manage; it does not change your Engine or Cleanup addresses.">
            <input className="credential-address" type="text" aria-label="Credential server address" autoComplete="off" spellCheck={false}
              placeholder="https://your-server.example/v1" value={baseUrl} disabled={saving} onChange={(e) => chooseAddress(e.target.value)} />
          </Row>
          {addresses.some(([, value]) => value.trim()) && <Row label="Configured addresses">
            <div className="credential-controls">
              {addresses.filter(([, value]) => value.trim()).map(([label, value]) => <button
                key={label} type="button" className="btn" title={value} disabled={saving} onClick={() => chooseAddress(value)}
              >Use {label.toLowerCase()} address</button>)}
            </div>
          </Row>}
        </>}
        <Row label="API key" hint={hasKey === null ? baseUrl.trim() ? "Checking key availability..." : "Enter a server address to manage its key." : hasKey ? "A key is available for this session. Save results and storage status indicate persistence." : provider.needsKey ? "No key available yet." : "Optional for servers that do not require authentication."}>
          <div className="credential-controls">
            <input type="password" aria-label={`${provider.name} API key`} autoComplete="off" autoCapitalize="none" spellCheck={false}
              value={key} disabled={saving} placeholder={hasKey ? "Replace key" : "Paste key"} onChange={(e) => setKey(e.target.value)} />
            <button type="submit" className="btn primary" disabled={saving || !key.trim() || (custom && !baseUrl.trim())}>{saving ? "Saving..." : "Save key"}</button>
            {hasKey && <button type="button" className="btn danger" disabled={saving} onClick={() => void changeKey(null)}>Remove key</button>}
          </div>
        </Row>
        {provider.keyUrl && <Row label="Provider dashboard" hint="Create or manage your API keys with this provider.">
          <button type="button" className="btn" onClick={() => void invoke("open_url", { url: provider.keyUrl }).catch((e) => toast(String(e)))}>Get a key</button>
        </Row>}
        {(error || statusError) && <div className="row"><div className="note error" role="alert">{error || statusError}</div></div>}
      </form>
    </Section>
  );
}

export function CredentialLink({ provider, go }: {
  provider: Pick<ProviderInfo, "id" | "name" | "hasKey">;
  go: PageProps["go"];
}) {
  return <Row label="API key" hint={provider.id === "custom"
    ? "Manage address-scoped keys in Credentials. Keys from a different scheme, host or port are never reused."
    : provider.hasKey ? `Uses the ${provider.name} key in Credentials.` : `No ${provider.name} key yet. Add one in Credentials without changing your engine.`}>
    <button className="btn" onClick={() => go("credentials")}>Manage keys</button>
  </Row>;
}
