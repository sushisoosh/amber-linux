import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { PageProps } from "../App";
import { Row, Section, Switch } from "../ui";
import { DownloadItem, SpeechModelActions, StatusLabel, useLocal } from "../downloads";
import type { LocalItem } from "../../shared/types";
import { CredentialLink } from "./Credentials";
import { ConnectionChoice, ConnectionModelPicker } from "../connections";
import { ModelImports } from "../modelImports";

const LANGUAGES: [string, string][] = [
  ["de", "German"],
  ["en", "English"],
  ["fr", "French"],
  ["es", "Spanish"],
  ["it", "Italian"],
  ["nl", "Dutch"],
  ["pl", "Polish"],
  ["tr", "Turkish"],
];

export function EnginePage(p: PageProps) {
  const { s, save } = p;
  const linux = p.ov.platform === "linux";
  return (
    <>
      <h1>Engine</h1>
      <p className="lead">{linux ? "Choose where your audio is recognized. Local recognition stays on this PC; server recognition sends audio to the selected provider. Optional cleanup has its own destination." : "Where your voice is turned into text. Locally nothing leaves this PC; a server is quick on any PC but needs an API key."}</p>
      <div className="cards">
        <button className={`card ${s.engine === "local" ? "on" : ""}`} onClick={() => save((x) => (x.engine = "local"))}>
          <div className="title">On this PC</div>
          <div className="hint">{linux ? "Whisper, Parakeet or Moonshine on your CPU. Recognition works offline after setup." : "Whisper on CPU or NVIDIA GPU; Parakeet and Moonshine on CPU. Private, offline after setup."}</div>
        </button>
        <button className={`card ${s.engine === "server" ? "on" : ""}`} onClick={() => save((x) => (x.engine = "server"))}>
          <div className="title">Server with API key</div>
          <div className="hint">Use a speech provider or your own compatible server. Your recording is sent there.</div>
        </button>
      </div>
      {s.engine === "local" ? <LocalEngine {...p} /> : <ServerEngine {...p} />}
      <Section title="Language">
        <div className="row column">
          <div className="label">
            <div>Spoken languages</div>
            <div className="hint">
              Whisper uses these as recognition hints, not a reliable translation mode. Several: local Whisper picks among them; none: automatic detection.
              Parakeet v3 detects its supported languages automatically. Parakeet v2 and Moonshine are English-only. Server support varies.
            </div>
          </div>
          <Languages value={s.language} onChange={(v) => save((x) => (x.language = v))} />
        </div>
        <div className="row column">
          <div className="label">
            <div>Vocabulary</div>
            <div className="hint">Names and terms for engines that support a vocabulary prompt, separated by commas. Parakeet and Moonshine do not use this hint.</div>
          </div>
          <Vocabulary value={s.vocabulary} onSave={(v) => save((x) => (x.vocabulary = v))} />
        </div>
      </Section>
    </>
  );
}

/** "auto", "de" or "de,en" - in the order picked: the first wins when Whisper is unsure. */
function Languages({ value, onChange }: { value: string; onChange: (v: string) => void }) {
  const picked = value.split(",").map((c) => c.trim()).filter((c) => c && c !== "auto");
  // A code without a chip of its own (typed into an older version) still shows, and can go.
  const chips: [string, string][] = [...LANGUAGES, ...picked.filter((c) => !LANGUAGES.some(([v]) => v === c)).map((c): [string, string] => [c, c])];
  const toggle = (code: string) => {
    const next = picked.includes(code) ? picked.filter((c) => c !== code) : [...picked, code];
    onChange(next.length ? next.join(",") : "auto");
  };
  return (
    <div className="seg wrap">
      <button className={picked.length === 0 ? "on" : ""} onClick={() => onChange("auto")}>
        Any
      </button>
      {chips.map(([code, label]) => (
        <button key={code} className={picked.includes(code) ? "on" : ""} onClick={() => toggle(code)}>
          {label}
        </button>
      ))}
    </div>
  );
}

function Vocabulary({ value, onSave }: { value: string; onSave: (v: string) => void }) {
  const [text, setText] = useState(value);
  useEffect(() => setText(value), [value]);
  return (
    <textarea
      value={text}
      placeholder="e.g. Tauri, Groq, Kubernetes, Anna"
      onChange={(e) => setText(e.target.value)}
      onBlur={() => text !== value && onSave(text)}
    />
  );
}

type ModelView = "recommended" | "whisper" | "parakeet" | "moonshine" | "installed" | "imported";

export function visibleSpeechModels(models: LocalItem[], view: ModelView, selected: string): LocalItem[] {
  return models.filter((m) => m.id === selected || (view === "installed" ? m.installed : view === "imported" ? m.imported : view === "recommended"
    ? ["futo-multilingual-74", "futo-multilingual-244", "acft-large-v3-turbo-q8_0"].includes(m.id)
    : m.family === view));
}

function LocalEngine(p: PageProps) {
  const { s, save, toast, ov, phase } = p;
  // A download is selected by Amber once it is complete, so the engine that works now keeps
  // working until then.
  const local = useLocal();
  const [view, setView] = useState<ModelView>("recommended");
  const [selectOnDownload, setSelectOnDownload] = useState(true);
  const info = local.info;
  if (!info) return null;

  const st = info.status;
  const selected = info.models.find((m) => m.id === s.local.model);
  const onnx = selected?.family === "parakeet" || selected?.family === "moonshine";
  return (
    <>
      <Section title="Model library">
        <Row label="Browse models" hint="The tuned Whisper models remain Amber's defaults. Other families and stock Whisper variants are optional; downloading weights does not mean they are loaded into memory.">
          <select aria-label="Model library" value={view} onChange={(e) => setView(e.target.value as ModelView)}>
            <option value="recommended">Tuned for dictation</option>
            <option value="whisper">All Whisper variants</option>
            <option value="parakeet">Parakeet</option>
            <option value="moonshine">Moonshine</option>
            <option value="installed">Downloaded models</option>
            <option value="imported">Imported models</option>
          </select>
        </Row>
        <Row label="Use downloads when finished" hint="Off: download for later without changing the selected model. On: select it only after all files have been verified.">
          <Switch on={selectOnDownload} onChange={setSelectOnDownload} />
        </Row>
        {view === "whisper" && <Row label="Size and precision" hint="Tiny/Base favor speed; Small/Medium favor accuracy; Large needs more memory. English variants are English-only. Q5 is smaller than Q8; F16 uses original half-precision weights. Smaller weights can affect accuracy. Stock models keep the full encoder context." />}
        {(view === "parakeet" || view === "moonshine") && <Row label="CPU recognition" hint={view === "parakeet"
          ? "Parakeet v3 supports 25 European languages including German and English. v2 is English-only. Language is detected automatically; vocabulary prompts are not supported."
          : "Moonshine Tiny and Base are compact English-only models. They do not use language or vocabulary hints. Long recordings are processed in bounded chunks."} />}
        {(view === "parakeet" || view === "moonshine" || onnx) && <Row label={info.onnxRuntimeInstalled ? "ONNX runtime installed" : "ONNX runtime download"}
          hint={`The first Parakeet or Moonshine download also installs a shared, checksum-verified ONNX Runtime 1.24.4 package (about 119 MiB extra download).${ov.platform === "windows" ? " Windows also needs the Microsoft Visual C++ 2015-2022 x64 redistributable." : " Uses the system C/C++ libraries on x64 GNU/Linux."}`}>
          {onnx && selected?.installed && !selected.imported && !info.onnxRuntimeInstalled && <button className="btn" disabled={info.downloads.includes(`model:${selected.id}`) || !!local.progress[`model:${selected.id}`]}
            onClick={() => local.download("model", selected.id, false).catch((e) => toast(String(e)))}>Install runtime</button>}
        </Row>}
        {visibleSpeechModels(info.models, view, s.local.model).map((m) => (
          <DownloadItem key={m.id} local={local} kind="model" item={m}
            selected={s.local.model === m.id} onSelect={() => save((x) => (x.local.model = m.id))}
            selectOnDownload={selectOnDownload} recommended={info.recommended[1] === m.id}
            measured={info.measured[m.id]} toast={toast} />
        ))}
      </Section>
      <ModelImports {...p} local={local} />
      {!onnx && <Section title="Whisper runs on">
        {info.runtimes.map((r) => (
          <DownloadItem
            key={r.id}
            local={local}
            kind="runtime"
            item={r}
            builtIn={ov.platform === "linux"}
            selected={s.local.runtime === r.id}
            onSelect={() => save((x) => (x.local.runtime = r.id))}
            recommended={info.recommended[0] === r.id}
            warning={r.wantsGpu && !info.hasNvidia ? "no NVIDIA driver found on this PC" : undefined}
            toast={toast}
            selectOnDownload={selectOnDownload}
          />
        ))}
      </Section>}
      <Section title="Local engine">
        {!selected && s.local.model.startsWith("import-") && <Row label="Selected import is unavailable" hint="Its files or registry entry are missing. Import the source again or choose another model above; Amber will not silently use a different speech model." />}
        <Row label={`Selected: ${selected?.label ?? s.local.model}`} hint={onnx ? "Runs on the CPU using ONNX Runtime. The Whisper runtime choice does not apply." : selected?.note} />
        {s.local.runtime === "cpu" && info.models.find((m) => m.id === s.local.model)?.wantsGpu && (
          <div className="row">
            <div className="note">This model is slow on a CPU - expect several seconds per sentence.</div>
          </div>
        )}
        <Row label={<StatusLabel status={st} />} hint={st.message ?? undefined}>
          <SpeechModelActions status={st} busy={["recording", "transcribing", "inserting"].includes(phase)} toast={toast} />
        </Row>
        <Row label="Memory and downloads" hint="Load now puts the model in memory. Unload model releases it without deleting downloaded files. The next dictation loads it while you speak; tests load on demand. Otherwise it stays loaded until you unload, switch models or engines, or quit Amber. Delete removes the download from disk." />
        <Row label="Load when Amber starts" hint="The first dictation then does not wait for the model to load. Uses memory while Amber runs.">
          <Switch on={s.local.keepLoaded} onChange={(v) => save((x) => (x.local.keepLoaded = v))} />
        </Row>
        <Row
          label="Recognize while speaking"
          hint="Long dictations are cut at pauses and recognized part by part while you keep talking - after you stop, only the last part is left to wait for."
        >
          <Switch on={s.local.chunking} onChange={(v) => save((x) => (x.local.chunking = v))} />
        </Row>
        {!onnx && <Row label="CPU threads" hint="Automatic uses half of your processor's threads.">
          <select value={s.local.threads} onChange={(e) => save((x) => (x.local.threads = Number(e.target.value)))}>
            <option value={0}>Automatic</option>
            {[2, 4, 6, 8, 10, 12, 16].map((n) => (
              <option key={n} value={n}>
                {n}
              </option>
            ))}
          </select>
        </Row>}
      </Section>
    </>
  );
}

function ServerEngine(p: PageProps) {
  const { s, save, ov, go } = p;
  const providers = ov.providers.filter((p) => p.supportsSpeech);
  const connection = s.connections.find((c) => c.id === s.server.connectionId);
  const provider = providers.find((p) => p.id === (s.server.connectionId ? connection?.provider : s.server.provider));
  const [baseUrl, setBaseUrl] = useState(s.server.baseUrl);
  const [test, setTest] = useState<{ ok: boolean; text: string } | null>(null);
  const testGeneration = useRef(0);
  const [testing, setTesting] = useState(false);
  useEffect(() => setBaseUrl(s.server.baseUrl), [s.server.baseUrl]);
  useEffect(() => {
    testGeneration.current++; setTest(null); setTesting(false);
    return () => { testGeneration.current++; };
  }, [provider?.id, baseUrl, connection]);

  if (!provider) return <Section title="Server"><ConnectionChoice {...p} purpose="speech" /><Row label="Connection unavailable" hint="Select a saved speech connection or Provider default. Amber will not silently use a different provider." /></Section>;

  const custom = provider.id === "custom";

  const runTest = async () => {
    const mine = ++testGeneration.current;
    setTesting(true);
    setTest(null);
    try {
      const ms = connection ? await invoke<number>("test_connection", { connection, key: null })
        : await invoke<number>("test_server", { provider: provider.id, baseUrl, key: null });
      if (mine === testGeneration.current) setTest({ ok: true, text: `Model catalog reachable - ${ms} ms. This does not verify inference or keys for public catalogs.` });
    } catch (e) {
      if (mine === testGeneration.current) setTest({ ok: false, text: String(e) });
    }
    if (mine === testGeneration.current) setTesting(false);
  };

  return (
    <Section title="Server">
      <ConnectionChoice {...p} purpose="speech" />
      {connection ? <Row label={provider.name} hint={`${connection.name}: ${connection.baseUrl}`} /> : <Row label="Provider">
        <select aria-label="Speech provider" value={provider.id} onChange={(e) => {
          const id = e.target.value;
            void save((x) => {
              x.server.provider = id;
              x.server.model = providers.find((p) => p.id === id)?.models[0]?.id ?? x.server.model;
            });
          }}>
          {providers.map((p) => <option key={p.id} value={p.id}>{p.name}</option>)}
        </select>
      </Row>}
      {provider.id === "tinfoil" && <Row label="Tinfoil connection" hint="Uses https://inference.tinfoil.sh/v1 for audio. HTTPS only: unlike Amber for Android, this desktop client does not verify Tinfoil's enclave attestation. The same saved key is available for cleanup." />}
      {provider.id === "elevenlabs" && <Row label="ElevenLabs options" hint="Transcription only, without speaker labels. Vocabulary keyterms are not sent because ElevenLabs charges extra for them. Provider retention and billing policies still apply." />}
      {provider.id === "deepgram" && <Row label="Deepgram options" hint="Vocabulary keyterms are not sent because Deepgram charges extra for them on recorded audio. Automatic detection is used when more than one language is selected. Provider retention and billing policies still apply." />}
      {custom && !connection && (
        <Row label="Address" hint="An OpenAI-compatible HTTPS base URL. Plain HTTP is allowed only on a literal loopback address, such as http://127.0.0.1:8000/v1.">
          <input
            type="text"
            style={{ width: 300 }}
            value={baseUrl}
            placeholder="https://…/v1"
            onChange={(e) => setBaseUrl(e.target.value)}
            onBlur={() => baseUrl !== s.server.baseUrl && save((x) => (x.server.baseUrl = baseUrl.trim()))}
          />
        </Row>
      )}
      {connection ? <ConnectionModelPicker key={`${connection.id}:${connection.provider}:${connection.baseUrl}`} connection={connection} purpose="speech" model={s.server.model}
        presets={provider.models} disabled={p.settingsSaving} onSelect={(id) => void save((x) => { x.server.model = id; })} /> : <Row label="Model" hint={provider.models.find((m) => m.id === s.server.model)?.note}>
        {provider.models.length > 0 ? (
          <select aria-label="Speech model" value={s.server.model} onChange={(e) => save((x) => (x.server.model = e.target.value))}>
            {provider.models.map((m) => <option key={m.id} value={m.id}>{m.label}</option>)}
          </select>
        ) : (
          <input
            type="text"
            aria-label="Speech model ID"
            key={`${provider.id}:${s.server.model}`}
            defaultValue={s.server.model}
            placeholder="whisper-1"
            onBlur={(e) => e.target.value.trim() && e.target.value.trim() !== s.server.model && save((x) => (x.server.model = e.target.value.trim()))}
          />
        )}
      </Row>}
      {!connection && <CredentialLink provider={provider} go={go} />}
      <Row label="Audio upload" hint="WAV is lossless. Ogg Opus uses lossy voice compression at about 32 kbps: smaller uploads, but recognition may change. Only server recognition is compressed; local recognition, fallback and retained test recordings keep the original WAV. No automatic retry with another format.">
        <select aria-label="Audio upload" value={s.server.uploadFormat} onChange={(e) => save((x) => (x.server.uploadFormat = e.target.value as "wav" | "opus"))}>
          <option value="wav">WAV (lossless, default)</option>
          <option value="opus">Ogg Opus (lossy, ~32 kbps)</option>
        </select>
      </Row>
      {s.server.uploadFormat === "opus" && <Row label="Format compatibility" hint={["deepgram", "elevenlabs", "mistral"].includes(provider.id)
        ? "This connector currently sends lossless WAV. The test details report the actual upload and the reason Opus was not used. Amber never retries a rejected paid request with another format."
        : custom
        ? "Amber servers must advertise Opus; otherwise their supported WAV format is used. Older Amber catalogs without an audio list imply WAV. Other OpenAI-compatible servers receive the requested Ogg Opus format without a guarantee of support. If rejected, select WAV yourself; Amber will not retry a paid request."
        : provider.id === "tinfoil"
          ? s.server.model === "voxtral-small-24b"
            ? "Tinfoil's published Voxtral server source accepts Ogg Opus. Live authenticated inference has not been verified here; its model page lists MP3/WAV. A public model catalog does not confirm format support or validate your key."
            : "Ogg Opus support for this Tinfoil model is unverified; the reviewed server source applies to Voxtral. Select WAV for compatibility. Amber will not retry automatically with WAV."
          : "The provider's transcription API documents Ogg input. Live authenticated inference with this encoder has not been verified here. A reachable catalog does not confirm format support."} />}
      <Row
        label="Fall back to this PC"
        hint="When the server cannot be reached (offline, down), the recording is recognized by the model set under “On this PC” instead of being lost - if it is downloaded. Never the other way round."
      >
        <Switch on={s.server.localFallback} onChange={(v) => save((x) => (x.server.localFallback = v))} />
      </Row>
      <Row label="Connection" hint={test ? <span className={`note ${test.ok ? "ok" : "error"}`}>{test.text}</span> : "Fetches the model catalog without sending audio. Public catalogs, including Tinfoil's, do not validate the API key."}>
        <button className="btn" disabled={testing} onClick={runTest}>
          {testing ? "Testing…" : "Test connection"}
        </button>
      </Row>
    </Section>
  );
}
