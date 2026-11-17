// What happens to the text between recognition and inserting: fillers out (rules, always),
// and optionally a chat model that resolves self-corrections and false starts.

import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { ChatProvider, CleanupPreview, Overview, WritingStyle } from "../../shared/types";
import type { PageProps } from "../App";
import { DownloadItem, StatusLabel, useLocal } from "../downloads";
import { Row, Section, Segmented, Switch } from "../ui";
import { STYLES } from "./Programs";

const EXAMPLE = "Ähm, lass uns morgen um drei Uhr treffen, nee stopp, um vier Uhr, äh, im Café weil es da ruhig ist.";

export function CleanupPage(p: PageProps) {
  const { s, save } = p;
  const linux = p.ov.platform === "linux";
  const c = s.cleanup;
  return (
    <>
      <h1>Cleanup</h1>
      <p className="lead">{linux ? "Optional processing before you review the result. Rules run on this PC; smart cleanup uses the selected local model or sends recognized text to your chosen server." : "What happens to your words before they are inserted. Hesitation sounds go out by rule; a language model can also untangle what you corrected while speaking."}</p>
      <Section>
        <Row label="Remove filler words" hint="äh, ähm, öhm, hm … - by fixed rules, on this PC, instantly. Real words like “um” stay.">
          <Switch on={c.fillers} onChange={(v) => save((x) => (x.cleanup.fillers = v))} />
        </Row>
        <Row label="Spoken punctuation" hint="Say “neue Zeile”, “neuer Absatz”, “Komma”, “Fragezeichen”, “Punkt” at a sentence end - or “new line”, “comma”, “question mark”.">
          <Switch on={c.spokenPunctuation} onChange={(v) => save((x) => (x.cleanup.spokenPunctuation = v))} />
        </Row>
        <Row label="German commas" hint="Before “dass”, “weil”, “wenn”, “um … zu”, “sondern” - only where the comma is nearly certain, only in German text.">
          <Switch on={c.germanCommas} onChange={(v) => save((x) => (x.cleanup.germanCommas = v))} />
        </Row>
        <Row
          label="Smart cleanup"
           hint={linux ? "A language model can resolve self-corrections, false starts and punctuation. If it fails or changes too much, the recognized text is retained for review." : "A language model resolves self-corrections (“at three, no wait, at four” → “at four”), false starts and punctuation. If it fails or changes too much, the text goes in as recognized."}
        >
          <Switch on={c.smart} onChange={(v) => save((x) => (x.cleanup.smart = v))} />
        </Row>
      </Section>
      {c.smart && (
        <>
          <div className="cards">
            <button className={`card ${c.engine === "local" ? "on" : ""}`} onClick={() => save((x) => (x.cleanup.engine = "local"))}>
              <div className="title">On this PC</div>
              <div className="hint">{linux ? "llama.cpp on your CPU. Works offline after downloading the runtime and model; speed and memory needs depend on the model." : "llama.cpp with Qwen3 4B on your GPU. Private, offline, free."}</div>
            </button>
            <button className={`card ${c.engine === "server" ? "on" : ""}`} onClick={() => save((x) => (x.cleanup.engine = "server"))}>
              <div className="title">Server with API key</div>
              <div className="hint">Groq, OpenAI or your own server. The text is sent there.</div>
            </button>
          </div>
          {c.engine === "local" ? <LocalModel {...p} /> : <ServerModel {...p} />}
        </>
      )}
      <TryIt {...p} />
    </>
  );
}

function LocalModel({ s, save, toast, ov }: PageProps) {
  const local = useLocal();
  const info = local.info;
  if (!info) return null;
  const st = info.llmStatus;
  return (
    <>
      <Section title="Runs on">
        {info.llmRuntimes.map((r) => (
          <DownloadItem
            key={r.id}
            local={local}
            kind="llmRuntime"
            item={r}
            selected={s.cleanup.localRuntime === r.id}
            onSelect={() => save((x) => (x.cleanup.localRuntime = r.id))}
            recommended={(ov.platform !== "linux" && info.hasNvidia ? "cuda" : "cpu") === r.id}
            warning={r.wantsGpu && !info.hasNvidia ? "no NVIDIA driver found on this PC" : undefined}
            toast={toast}
          />
        ))}
      </Section>
      <Section title="Model">
        {info.llmModels.map((m) => (
          <DownloadItem
            key={m.id}
            local={local}
            kind="llmModel"
            item={m}
            selected={s.cleanup.localModel === m.id}
            onSelect={() => save((x) => (x.cleanup.localModel = m.id))}
            toast={toast}
          />
        ))}
        <Row label={<StatusLabel status={st} />} hint={st.message ?? (ov.platform === "linux" ? "Loaded while smart cleanup is on. Uses system memory; leave cleanup off on a low-memory PC." : "Loaded with Amber while the smart cleanup is on; uses about 3 GB of graphics memory.")}>
          {st.state !== "ready" && (
            <button className="btn" disabled={st.state === "starting"} onClick={() => invoke("start_llm").catch((e) => toast(String(e)))}>
              Load now
            </button>
          )}
        </Row>
      </Section>
    </>
  );
}

function ServerModel({ s, save, go, ov, refresh, desktop, toast }: PageProps) {
  const linux = ov.platform === "linux";
  const [providers, setProviders] = useState<ChatProvider[]>([]);
  const [baseUrl, setBaseUrl] = useState(s.cleanup.baseUrl);
  const [key, setKey] = useState("");
  const [hasCustomKey, setHasCustomKey] = useState<boolean | null>(null);
  const [keyError, setKeyError] = useState<string | null>(null);
  const [savingKey, setSavingKey] = useState(false);
  const effectiveBase = baseUrl.trim() || s.server.baseUrl;
  useEffect(() => setBaseUrl(s.cleanup.baseUrl), [s.cleanup.baseUrl]);
  useEffect(() => {
    void invoke<ChatProvider[]>("chat_providers").then(setProviders);
  }, []);
  useEffect(() => {
    if (!linux || s.cleanup.provider !== "custom") return;
    let disposed = false;
    setKey("");
    setHasCustomKey(null);
    setKeyError(null);
    void invoke<boolean>("has_api_key", { provider: "custom", baseUrl: effectiveBase }).then((available) => {
      if (!disposed) setHasCustomKey(available);
    }).catch((e) => {
      if (!disposed) setKeyError(`Could not check this server's key: ${e}`);
    });
    return () => { disposed = true; };
  }, [linux, s.cleanup.provider, effectiveBase]);
  const c = s.cleanup;
  const provider = providers.find((x) => x.id === c.provider);

  const saveCustomKey = async (value: string | null) => {
    setSavingKey(true);
    setKeyError(null);
    try {
      // Blur also saves the address; verify its committed value before binding a credential.
      await save((x) => (x.cleanup.baseUrl = baseUrl.trim()));
      const current = (await invoke<Overview>("get_overview")).settings;
      if (current.cleanup.provider !== "custom" || current.cleanup.baseUrl !== baseUrl.trim() || (current.cleanup.baseUrl || current.server.baseUrl) !== effectiveBase) {
        throw new Error("The cleanup address was not saved. The key was not changed.");
      }
      await invoke("set_api_key", { provider: "custom", baseUrl: effectiveBase, key: value });
      setKey("");
      toast(value ? "Key saved for this server's origin." : "Key removed for this server's origin.", true);
    } catch (e) {
      setKeyError(`Key update was not confirmed. If credential storage failed, the changed key may be active for this session only. ${e}`);
    } finally {
      await Promise.all([
        refresh(),
        desktop.refresh(),
        invoke<boolean>("has_api_key", { provider: "custom", baseUrl: effectiveBase }).then(setHasCustomKey),
      ]).catch((e) => setKeyError((error) => error ?? `Could not refresh key status: ${e}`));
      setSavingKey(false);
    }
  };

  if (!provider) return null;
  return (
    <Section title="Server">
      <Row label="Provider">
        <Segmented
          value={provider.id}
          options={providers.map((x) => [x.id, x.name] as [string, string])}
          onChange={(id) => {
            if (savingKey) return;
            void save((x) => {
              x.cleanup.provider = id;
              x.cleanup.model = providers.find((q) => q.id === id)?.models[0]?.id ?? "";
            });
          }}
        />
      </Row>
      {provider.id === "custom" && (
        <Row label="Address" hint="Empty: the speech engine's own-server address. Use HTTPS, or HTTP only on a literal loopback address.">
          <input
            type="text"
            style={{ width: 300 }}
            value={baseUrl}
            disabled={savingKey}
            placeholder={s.server.baseUrl || "https://…/v1"}
            onChange={(e) => setBaseUrl(e.target.value)}
            onBlur={() => baseUrl !== c.baseUrl && save((x) => (x.cleanup.baseUrl = baseUrl.trim()))}
          />
        </Row>
      )}
      <Row label="Chat model" hint={provider.models.find((m) => m.id === c.model)?.note}>
        {provider.models.length > 0 ? (
          <select value={c.model} onChange={(e) => save((x) => (x.cleanup.model = e.target.value))}>
            {provider.models.map((m) => (
              <option key={m.id} value={m.id}>
                {m.label}
              </option>
            ))}
          </select>
        ) : (
          <input type="text" defaultValue={c.model} placeholder="llama3.2" onBlur={(e) => e.target.value !== c.model && save((x) => (x.cleanup.model = e.target.value.trim()))} />
        )}
      </Row>
      {linux && provider.id === "custom" ? <>
        <Row label="API key for this server" hint={<>Keys are scoped to scheme, host and port. A key from another server is never reused. {hasCustomKey === null ? "Key availability is not yet confirmed." : hasCustomKey ? "A key is available for this origin in the current session." : "No key is available for this origin; unauthenticated servers can leave it empty."}</>}>
          <input type="password" style={{ width: 220 }} aria-label="Custom cleanup server API key" autoComplete="off" value={key} disabled={savingKey} placeholder={hasCustomKey ? "Key available" : "Paste key"} onChange={(e) => setKey(e.target.value)} />
          <button className="btn primary" disabled={savingKey || !key.trim() || !effectiveBase} onClick={() => void saveCustomKey(key)}>{savingKey ? "Saving..." : "Save key"}</button>
          {hasCustomKey && <button className="btn danger" disabled={savingKey} onClick={() => void saveCustomKey(null)}>Remove</button>}
        </Row>
        {keyError && <div className="row"><div className="note error" role="alert">{keyError}</div></div>}
        <Row label="Credential storage" hint={desktop.storage?.credentials ?? desktop.error ?? "Checking secure storage..."} />
      </> : <Row
        label="API key"
        hint={provider.hasKey ? `Uses the ${provider.name} key ${ov.platform === "linux" ? "available" : "saved"} on the Engine page.` : provider.id === "custom" ? "Optional for your own server." : `No ${provider.name} key yet.`}
      >
        <button className="btn" onClick={() => go("engine")}>
          {provider.hasKey ? "Change" : "Add key"}
        </button>
      </Row>}
    </Section>
  );
}

function TryIt({ s, toast }: PageProps) {
  const [text, setText] = useState(EXAMPLE);
  const [result, setResult] = useState<CleanupPreview | null>(null);
  const [busy, setBusy] = useState(false);
  const [style, setStyle] = useState<WritingStyle>("asSpoken");
  const run = async () => {
    setBusy(true);
    try {
      setResult(await invoke<CleanupPreview>("cleanup_preview", { text, style }));
    } catch (e) {
      toast(String(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <Section title="Try it">
      <div className="row column">
        <div className="note">Type or paste what a dictation might sound like:</div>
        <textarea value={text} onChange={(e) => setText(e.target.value)} />
        <div style={{ display: "flex", gap: 12, alignItems: "center" }}>
          <button className="btn primary" disabled={busy || !text.trim()} onClick={run}>
            {busy ? "Cleaning…" : "Clean up"}
          </button>
          {s.cleanup.smart && <Segmented value={style} options={STYLES} onChange={setStyle} />}
          {result && s.cleanup.smart && <span className="note">{result.error ? "" : `${result.ms} ms`}</span>}
        </div>
        {result && (
          <div className="preview">
            <div className="note">By rule (spoken punctuation, fillers, commas)</div>
            <div style={{ whiteSpace: "pre-wrap" }}>{result.fillers || <i>(nothing left)</i>}</div>
            {s.cleanup.smart && (
              <>
                <div className="note" style={{ marginTop: 10 }}>
                  Smart cleanup
                </div>
                {result.smart ? <div style={{ whiteSpace: "pre-wrap" }}>{result.smart}</div> : <div className="note error">{result.error}</div>}
              </>
            )}
          </div>
        )}
      </div>
    </Section>
  );
}
