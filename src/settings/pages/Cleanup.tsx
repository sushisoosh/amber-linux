// What happens to the text between recognition and inserting: fillers out (rules, always),
// and optionally a chat model that resolves self-corrections and false starts.

import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { ChatProvider, CleanupPreview, WritingStyle } from "../../shared/types";
import type { PageProps } from "../App";
import { DownloadItem, StatusLabel, useLocal } from "../downloads";
import { Row, Section, Segmented, Switch } from "../ui";
import { STYLES } from "./Programs";
import { CredentialLink } from "./Credentials";
import { ConnectionChoice, ConnectionModelPicker } from "../connections";
import { CleanupModelImports } from "../modelImports";

const EXAMPLE = "Ähm, lass uns morgen um drei Uhr treffen, nee stopp, um vier Uhr, äh, im Café weil es da ruhig ist.";

export function CleanupPage(p: PageProps) {
  const { s, save } = p;
  const linux = p.ov.platform === "linux";
  const c = s.cleanup;
  return (
    <>
      <h1>Cleanup</h1>
      <p className="lead">{linux ? "Optional processing before you review or paste the result. Rules run on this PC; smart cleanup uses the selected local model or sends recognized text to your chosen server." : "What happens to your words before they are inserted. Hesitation sounds go out by rule; a language model can also untangle what you corrected while speaking."}</p>
      <Section>
        <Row
          label="Enable smart cleanup"
          hint={`${c.smart ? "Enabled: your selected model cleans up self-corrections, false starts and punctuation. If it fails or changes too much, the recognized text is retained." : "Disabled: model-based cleanup is off. Your model selection is kept."} Rule-based cleanup below is independent.${linux ? "" : " Per-program cleanup overrides still apply."}`}
        >
          <Switch on={c.smart} disabled={p.settingsSaving} onChange={(v) => save((x) => (x.cleanup.smart = v))} />
        </Row>
      </Section>
      <Section title="Rule-based cleanup">
        <Row label="Remove filler words" hint="äh, ähm, öhm, hm … - by fixed rules, on this PC, instantly. Real words like “um” stay.">
          <Switch on={c.fillers} onChange={(v) => save((x) => (x.cleanup.fillers = v))} />
        </Row>
        <Row label="Spoken punctuation" hint="Say “neue Zeile”, “neuer Absatz”, “Komma”, “Fragezeichen”, “Punkt” at a sentence end - or “new line”, “comma”, “question mark”.">
          <Switch on={c.spokenPunctuation} onChange={(v) => save((x) => (x.cleanup.spokenPunctuation = v))} />
        </Row>
        <Row label="German commas" hint="Before “dass”, “weil”, “wenn”, “um … zu”, “sondern” - only where the comma is nearly certain, only in German text.">
          <Switch on={c.germanCommas} onChange={(v) => save((x) => (x.cleanup.germanCommas = v))} />
        </Row>
      </Section>
      {c.smart && (
        <>
          <div className="cards">
            <button className={`card ${c.engine === "local" ? "on" : ""}`} onClick={() => save((x) => (x.cleanup.engine = "local"))}>
              <div className="title">On this PC</div>
              <div className="hint">{linux ? "llama.cpp on your CPU. Works offline after downloading the runtime and model; speed and memory needs depend on the model." : "llama.cpp on your GPU or CPU. Choose a model to download or import your own GGUF. Private, offline after setup."}</div>
            </button>
            <button className={`card ${c.engine === "server" ? "on" : ""}`} onClick={() => save((x) => (x.cleanup.engine = "server"))}>
              <div className="title">Server with API key</div>
              <div className="hint">Choose a text provider, local Ollama, or a compatible server. Recognized text is sent to that endpoint.</div>
            </button>
          </div>
          {c.engine === "local" ? <LocalModel {...p} /> : <ServerModel {...p} />}
        </>
      )}
      <TryIt {...p} />
    </>
  );
}

function LocalModel(p: PageProps) {
  const { s, save, toast, ov } = p;
  const local = useLocal();
  const [selectOnDownload, setSelectOnDownload] = useState(true);
  const info = local.info;
  if (!info) return <Section title="Local cleanup"><Row label="Loading model library..." /></Section>;
  const st = info.llmStatus;
  const selected = info.llmModels.find((m) => m.id === s.cleanup.localModel);
  const runtime = info.llmRuntimes.find((r) => r.id === s.cleanup.localRuntime);
  const imported = info.llmModels.filter((m) => m.imported);
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
            disabled={p.settingsSaving}
            toast={toast}
          />
        ))}
      </Section>
      <Section title="Cleanup model library">
        <Row label="Choose a model" hint="Download sizes are shown below. Speed labels compare these models on similar hardware, not measured tokens per second. Memory estimates are approximate: CPU uses RAM, GPU uses VRAM plus RAM. Leave room for speech recognition and other apps." />
        <Row label="Use downloads when finished" hint="Applies to newly started model downloads, not downloads already running or runtime setup. Off: download for later. On: select the model after its checksum has been verified.">
          <Switch on={selectOnDownload} onChange={setSelectOnDownload} />
        </Row>
        {info.llmModels.filter((m) => !m.imported).map((m) => (
          <DownloadItem
            key={m.id}
            local={local}
            kind="llmModel"
            item={m}
            selected={s.cleanup.localModel === m.id}
            onSelect={() => save((x) => (x.cleanup.localModel = m.id))}
            recommended={m.id === "qwen3-4b-instruct-2507-q4km"}
            selectOnDownload={selectOnDownload}
            disabled={p.settingsSaving}
            toast={toast}
          />
        ))}
        <Row label="Compare on this PC" hint="Q4_K_M is a compact 4-bit quantization. Smaller models usually run faster but can miss corrections. Use Try it below for a quick check, or Testing to compare the same recording. Larger does not guarantee better cleanup." />
      </Section>
      {imported.length > 0 && <Section title="Imported cleanup models">
        {imported.map((m) => <DownloadItem key={m.id} local={local} kind="llmModel" item={m}
          selected={s.cleanup.localModel === m.id} onSelect={() => save((x) => (x.cleanup.localModel = m.id))}
          disabled={p.settingsSaving} toast={toast} />)}
      </Section>}
      <CleanupModelImports {...p} local={local} />
      <Section title="Selected cleanup model">
        <Row label={selected?.label ?? s.cleanup.localModel} hint="Downloaded, selected and loaded are separate states. Deleting an imported model removes only Amber's copy, not your original file." />
        {(!selected || !selected.installed) && <Row label="Selected model is unavailable" hint="Download it, import the source again, or select an installed model above. Amber will not silently substitute a different cleanup model." />}
        {!runtime?.installed && <Row label="Cleanup runtime required" hint="Download a runtime under Runs on before loading the model. Importing a model does not download the runtime." />}
        <Row label={<StatusLabel status={st} />} hint={st.message ?? "The selected model loads while local smart cleanup is on. Loading needs more memory than the model's download size; disable smart cleanup to release it."}>
          {st.state !== "ready" && (
            <button className="btn" disabled={p.settingsSaving || st.state === "starting" || !selected?.installed || !runtime?.installed} onClick={() => invoke("start_llm").catch((e) => toast(String(e)))}>
              Load now
            </button>
          )}
        </Row>
      </Section>
    </>
  );
}

function ServerModel(p: PageProps) {
  const { s, save, go, ov, toast } = p;
  const [providers, setProviders] = useState<ChatProvider[]>([]);
  const [baseUrl, setBaseUrl] = useState(s.cleanup.baseUrl);
  useEffect(() => setBaseUrl(s.cleanup.baseUrl), [s.cleanup.baseUrl]);
  useEffect(() => {
    void invoke<ChatProvider[]>("chat_providers").then(setProviders).catch((e) => toast(`Could not load cleanup providers: ${e}`));
  }, []);
  const c = s.cleanup;
  const connection = s.connections.find((v) => v.id === c.connectionId);
  const provider = providers.find((x) => x.id === (c.connectionId ? connection?.provider : c.provider));

  if (!provider) return <Section title="Server"><ConnectionChoice {...p} purpose="cleanup" cleanupProviders={providers} /><Row label={providers.length ? "Connection unavailable" : "Loading cleanup providers..."} hint="A missing or incompatible connection is never replaced with another provider automatically." /></Section>;
  return (
    <Section title="Server">
      <ConnectionChoice {...p} purpose="cleanup" cleanupProviders={providers} />
      {connection ? <Row label={provider.name} hint={`${connection.name}: ${connection.baseUrl}`} /> : <Row label="Provider">
        <select aria-label="Cleanup provider" value={provider.id} onChange={(e) => {
          const id = e.target.value;
            void save((x) => {
              x.cleanup.provider = id;
              x.cleanup.model = providers.find((q) => q.id === id)?.models[0]?.id ?? "";
            });
          }}>
          {providers.map((p) => <option key={p.id} value={p.id}>{p.name}</option>)}
        </select>
      </Row>}
      {provider.id === "tinfoil" && <Row label="Tinfoil connection" hint="Uses https://inference.tinfoil.sh/v1 for cleanup text. HTTPS only: unlike Amber for Android, this desktop client does not verify Tinfoil's enclave attestation. DeepSeek and Gemma run with thinking disabled." />}
      {provider.id === "ollama" && <Row label="Local Ollama" hint="Connects to http://127.0.0.1:11434/v1 without an API key. Install Ollama separately and pull the model first; Amber does not download or manage Ollama models. Use Own server for a different address." />}
      {provider.id === "custom" && !connection && (
        <Row label="Address" hint="Empty: the speech engine's own-server address. Use HTTPS, or HTTP only on a literal loopback address.">
          <input
            type="text"
            style={{ width: 300 }}
            value={baseUrl}
            placeholder={s.server.baseUrl || "https://…/v1"}
            onChange={(e) => setBaseUrl(e.target.value)}
            onBlur={() => baseUrl !== c.baseUrl && save((x) => (x.cleanup.baseUrl = baseUrl.trim()))}
          />
        </Row>
      )}
      {connection ? <ConnectionModelPicker key={`${connection.id}:${connection.provider}:${connection.baseUrl}`} connection={connection} purpose="cleanup" model={c.model}
        presets={provider.models} disabled={p.settingsSaving} onSelect={(id) => void save((x) => { x.cleanup.model = id; })} /> : <Row label="Chat model" hint={provider.models.find((m) => m.id === c.model)?.note}>
        {provider.models.length > 0 ? (
          <select aria-label="Cleanup model" value={c.model} onChange={(e) => save((x) => (x.cleanup.model = e.target.value))}>
            {provider.models.map((m) => <option key={m.id} value={m.id}>{m.label}</option>)}
          </select>
        ) : (
          <input type="text" aria-label="Cleanup model ID" key={`${provider.id}:${c.model}`} defaultValue={c.model} placeholder="Model ID"
            onBlur={(e) => e.target.value.trim() && e.target.value.trim() !== c.model && save((x) => (x.cleanup.model = e.target.value.trim()))} />
        )}
      </Row>}
      {!connection && provider.id !== "ollama" && <CredentialLink provider={ov.providers.find((p) => p.id === provider.id) ?? provider} go={go} />}
    </Section>
  );
}

function TryIt({ s, toast, settingsSaving }: PageProps) {
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
            <button className="btn primary" disabled={busy || settingsSaving || !text.trim()} onClick={run}>
              {busy ? "Cleaning…" : settingsSaving ? "Saving settings..." : "Clean up"}
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
