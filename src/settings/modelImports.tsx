import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { PageProps } from "./App";
import type { Local } from "./downloads";
import { Progress, Row, Section, Switch } from "./ui";

type Variant = "whisper" | "parakeet_int8" | "moonshine_tiny" | "moonshine_base";
type Transfer = { done: number; total: number | null };

export function CleanupModelImports({ local, ...p }: PageProps & { local: Local }) {
  const [sourcePath, setSourcePath] = useState("");
  const [name, setName] = useState("");
  const [trust, setTrust] = useState(false);
  const [select, setSelect] = useState(false);
  const [busy, setBusy] = useState(false);
  const [browsing, setBrowsing] = useState(false);
  const [progress, setProgress] = useState<Transfer | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [success, setSuccess] = useState<string | null>(null);
  const importing = busy || !!local.info?.downloads.includes("cleanup-model-import");
  useEffect(() => {
    let active = true;
    const sub = listen<Transfer>("cleanup-model-import-progress", (e) => { if (active) setProgress(e.payload); });
    return () => { active = false; void sub.then((unlisten) => unlisten()); };
  }, []);
  const run = async () => {
    setBusy(true); setProgress(null); setError(null); setSuccess(null);
    try {
      const imported = await invoke<{ id: string; name: string; selectionError: string | null }>("import_cleanup_model", {
        request: { sourcePath: sourcePath.trim(), name: name.trim() }, selectAfterImport: select,
      });
      if (imported.selectionError) setError(`${imported.name} was imported, but selection was not saved: ${imported.selectionError}`);
      else setSuccess(`${imported.name} imported. Your original file was not changed.${select ? " Selected for local cleanup." : " Select it from Imported cleanup models when ready."}`);
      setSourcePath(""); setName(""); setTrust(false);
    } catch (e) { setError(String(e)); }
    finally { setBusy(false); setProgress(null); await local.reload().catch((e) => p.toast(String(e))); }
  };
  return <Section title="Import a cleanup model">
    <Row label="Already downloaded a model?" hint="Choose a single GGUF text-generation instruct/chat model compatible with llama.cpp, with an embedded chat template. Whisper speech models, adapters and split GGUF files are not supported. Compatibility and cleanup quality are not guaranteed; prefer non-thinking models and test the result." />
    <Row label="Trusted sources only" hint="Native libraries parse model files. Import checks structure and records a checksum, but cannot prove publisher trust or safety. Amber copies the file into its own storage, requiring extra disk space. Your original is never changed or deleted. Importing is offline." />
    {local.info?.llmImportsError && <div className="row"><div role="alert" className="note error">Cleanup model registry: {local.info.llmImportsError}</div></div>}
    <Row label="GGUF file" hint="Browse to an existing file or enter its absolute path, not a URL. Symlinks are not supported.">
      <div className="credential-controls">
        <input type="text" aria-label="Cleanup GGUF source path" value={sourcePath} disabled={importing || browsing} onChange={(e) => { setSourcePath(e.target.value); setTrust(false); }} />
        <button className="btn" disabled={importing || browsing} onClick={async () => {
          setBrowsing(true); setError(null);
          try { const path = await invoke<string | null>("browse_cleanup_model"); if (path) { setSourcePath(path); setTrust(false); } }
          catch (e) { setError(String(e)); }
          finally { setBrowsing(false); }
        }}>{browsing ? "Choosing..." : "Browse..."}</button>
      </div>
    </Row>
    <Row label="Model name"><input type="text" aria-label="Imported cleanup model name" maxLength={80} value={name} disabled={importing} onChange={(e) => setName(e.target.value)} /></Row>
    <Row label="I trust this cleanup model's source"><Switch on={trust} onChange={setTrust} disabled={importing} /></Row>
    <Row label="Use after import" hint="Off: import for later without changing your current cleanup model. On: save the selection after the import finishes, even if this window is closed."><Switch on={select} onChange={setSelect} disabled={importing} /></Row>
    <div className="row model-actions">
      <button className="btn primary" disabled={importing || browsing || p.settingsSaving || !trust || !name.trim() || !sourcePath.trim()} onClick={run}>{importing ? "Importing..." : "Import cleanup model"}</button>
      {importing && <button className="btn" onClick={() => void invoke("cancel_cleanup_model_import").catch((e) => setError(String(e)))}>Cancel import</button>}
      {importing && progress && <Progress done={progress.done} total={progress.total} />}
    </div>
    {(error || success) && <div className="row"><div role={error ? "alert" : "status"} className={`note ${error ? "error" : "ok"}`}>{error ?? success}</div></div>}
  </Section>;
}

export function ModelImports({ local, ...p }: PageProps & { local: Local }) {
  const [variant, setVariant] = useState<Variant>("whisper");
  const [sourcePath, setSourcePath] = useState("");
  const [name, setName] = useState("");
  const [languages, setLanguages] = useState("");
  const [trust, setTrust] = useState(false);
  const [select, setSelect] = useState(false);
  const [busy, setBusy] = useState(false);
  const [browsing, setBrowsing] = useState(false);
  const [runtimeBusy, setRuntimeBusy] = useState(false);
  const [progress, setProgress] = useState<Transfer | null>(null);
  const [runtimeProgress, setRuntimeProgress] = useState<Transfer | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [success, setSuccess] = useState<string | null>(null);
  const importing = busy || !!local.info?.downloads.includes("model-import");
  const installingRuntime = runtimeBusy || !!local.info?.downloads.includes("onnx-runtime");
  useEffect(() => {
    let active = true;
    const subs = [
      listen<Transfer>("model-import-progress", (e) => { if (active) setProgress(e.payload); }),
      listen<Transfer>("onnx-runtime-progress", (e) => { if (active) setRuntimeProgress(e.payload); }),
    ];
    return () => { active = false; subs.forEach((s) => { void s.then((unlisten) => unlisten()); }); };
  }, []);
  const run = async () => {
    setBusy(true); setProgress(null); setError(null); setSuccess(null);
    try {
      const imported = await invoke<{ id: string; name: string; selectionError: string | null }>("import_local_model", { selectAfterImport: select, request: {
        sourcePath: sourcePath.trim(), name: name.trim(), variant,
        languages: languages.split(/[,\s]+/).map((s) => s.trim().toLowerCase()).filter(Boolean),
      } });
      if (imported.selectionError) {
        setError(`${imported.name} was imported, but selection was not saved: ${imported.selectionError}`);
      } else setSuccess(`${imported.name} imported. The original source was not changed.${select ? " Selected for local recognition." : " Select it from Imported models when ready."}`);
      setSourcePath(""); setName(""); setTrust(false);
    } catch (e) { setError(String(e)); }
    finally { setBusy(false); setProgress(null); await local.reload().catch((e) => p.toast(String(e))); }
  };
  return <Section title="Import a local speech model">
    <Row label="Trusted sources only" hint="Model files are parsed by native libraries. Checksums detect later changes, but do not prove a custom model's publisher or safety. Amber copies validated files into its own storage; your source files are never changed or deleted. Importing is offline; optional selection can preload according to your engine preference." />
    {local.info?.importsError && <div className="row"><div role="alert" className="note error">Imported model registry: {local.info.importsError}</div></div>}
    <Row label="Format">
      <select aria-label="Import model format" value={variant} disabled={importing || browsing} onChange={(e) => {
        const next = e.target.value as Variant;
        setVariant(next); setSourcePath(""); setTrust(false); setError(null);
        setLanguages(next === "whisper" ? "" : "en");
      }}>
        <option value="whisper">Whisper GGML (.bin file)</option>
        <option value="parakeet_int8">Parakeet int8 (ONNX folder)</option>
        <option value="moonshine_tiny">Moonshine Tiny (ONNX folder)</option>
        <option value="moonshine_base">Moonshine Base (ONNX folder)</option>
      </select>
    </Row>
    <Row label={variant === "whisper" ? "Model file" : "Model folder"} hint="Choose an existing local file or folder using its absolute path, not a URL. Symlinks and ONNX external-data references are rejected.">
      <div className="credential-controls"><input type="text" aria-label="Import source path" value={sourcePath} disabled={importing || browsing} onChange={(e) => { setSourcePath(e.target.value); setTrust(false); }} />
        <button className="btn" disabled={importing || browsing} onClick={async () => {
          setBrowsing(true); setError(null);
          try { const path = await invoke<string | null>("browse_local_model", { variant }); if (path) { setSourcePath(path); setTrust(false); } }
          catch (e) { setError(String(e)); }
          finally { setBrowsing(false); }
        }}>{browsing ? "Choosing..." : "Browse..."}</button></div>
    </Row>
    <Row label="Model name"><input type="text" aria-label="Imported model name" maxLength={80} value={name} disabled={importing} onChange={(e) => setName(e.target.value)} /></Row>
    <Row label="Supported languages" hint={variant === "whisper" ? "Comma-separated language codes, such as en or de. Leave empty for multilingual Whisper; use en for an English-only model." : "Declare the actual model's supported languages using comma-separated codes. English Moonshine and Parakeet v2: en. For v3, include its supported languages, such as en,de. This declaration does not add new language support."}>
      <input type="text" aria-label="Imported model languages" value={languages} disabled={importing} onChange={(e) => setLanguages(e.target.value)} placeholder={variant === "whisper" ? "Multilingual" : "en"} />
    </Row>
    {variant !== "whisper" && <Row label="Required files" hint={variant === "parakeet_int8"
      ? "encoder-model.int8.onnx, decoder_joint-model.int8.onnx, nemo128.onnx, vocab.txt"
      : "encoder_model.onnx, decoder_model_merged.onnx, tokenizer.json. Choose Tiny or Base to match the actual model architecture."} />}
    <Row label="Not a cleanup model" hint="This importer accepts the supported speech formats above, not arbitrary ONNX graphs, Whisper GGUF, or cleanup GGUF files. Format checks cannot guarantee that a custom architecture will load or recognize speech correctly." />
    <Row label="I trust this model's source"><Switch on={trust} onChange={setTrust} /></Row>
    <Row label="Select after import" hint="Off by default. This preference survives closing the settings window. Loading follows your existing local-engine preference after selection."><Switch on={select} onChange={setSelect} /></Row>
    <div className="row model-actions">
      <button className="btn primary" disabled={importing || browsing || p.settingsSaving || !trust || !name.trim() || !sourcePath.trim() || (variant !== "whisper" && !languages.trim())} onClick={() => void run()}>{importing ? "Importing..." : "Import model"}</button>
      {importing && <button className="btn" onClick={() => void invoke("cancel_model_import").catch((e) => setError(String(e)))}>Cancel import</button>}
      {importing && progress && <Progress done={progress.done} total={progress.total} />}
    </div>
    {(error || success) && <div className="row"><div role={error ? "alert" : "status"} className={`note ${error ? "error" : "ok"}`}>{error ?? success}</div></div>}
    {!local.info?.onnxRuntimeInstalled && (variant !== "whisper" || local.info?.models.some((m) => m.imported && m.family !== "whisper")) && <Row label="ONNX runtime required" hint="Import does not download anything. Install the verified shared ONNX Runtime separately before loading a Parakeet/Moonshine import (about 119 MiB download). Windows also requires the Microsoft Visual C++ x64 redistributable.">
      <div className="model-actions"><button className="btn" disabled={installingRuntime} onClick={async () => {
        setRuntimeBusy(true); setRuntimeProgress(null); setError(null);
        try { await invoke("setup_onnx_runtime"); await local.reload(); }
        catch (e) { setError(String(e)); }
        finally { setRuntimeBusy(false); }
      }}>{installingRuntime ? "Installing runtime..." : "Install ONNX runtime"}</button>
        {installingRuntime && <button className="btn" onClick={() => void invoke("cancel_onnx_runtime_setup").catch((e) => setError(String(e)))}>Cancel runtime setup</button>}
        {installingRuntime && runtimeProgress && <Progress done={runtimeProgress.done} total={runtimeProgress.total} />}
      </div>
    </Row>}
  </Section>;
}
