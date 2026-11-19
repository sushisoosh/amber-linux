import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { CleanupPreview, Instruction } from "../../shared/types";
import type { PageProps } from "../App";
import { Row, Section, Switch } from "../ui";

export const BASH_EXAMPLE: Omit<Instruction, "id"> = {
  name: "Bash command", keyword: "bash", enabled: false, output: "shell",
  prompt: "Turn the spoken request into a Bash command. Return only the command, without Markdown fences or explanation. Do not execute anything.",
};

export function instructionIssue(rule: Omit<Instruction, "id"> & { id?: string }, rules: Instruction[]): string | null {
  if (!rule.name.trim() || rule.name.length > 80) return "Enter a name of 1-80 characters.";
  if (!/^[\p{L}\p{N}]{1,40}$/u.test(rule.keyword.trim())) return "Use a single keyword of 1-40 letters or digits, without spaces or punctuation.";
  if (rules.some((r) => r.id !== rule.id && r.keyword.toLowerCase() === rule.keyword.trim().toLowerCase())) return "That keyword is already used by another instruction.";
  if (!rule.prompt.trim() || rule.prompt.length > 4000) return "Enter an instruction of 1-4000 characters.";
  return null;
}

export function instructionKeyword(text: string): string | null {
  return text.trimStart().match(/^([\p{L}\p{N}]+)(?=[\s,:;.!?]|$)/u)?.[1].toLowerCase() ?? null;
}

export function InstructionsPage(p: PageProps) {
  const [draft, setDraft] = useState<Instruction | null>(null);
  const newRule = (example?: Omit<Instruction, "id">) => setDraft({ id: `instruction-${crypto.randomUUID()}`,
    name: "", keyword: "", prompt: "", enabled: false, output: "text", ...example });
  return <>
    <h1>Instructions</h1>
    <p className="lead">Transform a dictation by starting with a configured keyword. The keyword is removed and your saved instruction is applied to the remaining words. This is separate from ordinary cleanup, which still preserves meaning.</p>
    <Section title="How it works">
      <Row label="First word only" hint="For example, 'bash list all files modified today' applies the Bash instruction. 'Please bash...' and 'bashful...' do not trigger it. A keyword without a request does nothing." />
      <Row label={p.s.cleanup.smart ? "Uses your Cleanup engine" : "Smart cleanup is off"} hint="Rules must be enabled and smart cleanup must be on. A program's Never setting still wins. Server cleanup sends the request text to that selected provider; there is no extra provider or automatic cloud fallback.">
        <button className="btn" onClick={() => p.go("cleanup")}>Open Cleanup</button>
      </Row>
      <Row label="Shell output is review/copy-only" hint="Shell instructions never automatically paste or execute commands, even if Terminal auto-run is enabled. Review the result before copying it into a terminal. Text instructions use normal delivery. These desktop rules are not applied to phone API requests." />
      {p.s.instructions.some((r) => r.enabled) && <Row label="Whole dictation" hint="With enabled instructions, streaming chunks are deferred so the first keyword applies once to the entire dictation. This can increase the wait after you stop speaking." />}
    </Section>
    <Section title="Bash example">
      <Row label="bash list all files modified today" hint="Example output: find . -type f -mtime -1. The actual result comes from your selected cleanup model and must be checked.">
        <button className="btn" disabled={!!draft || p.settingsSaving || p.s.instructions.some((r) => r.keyword.toLowerCase() === "bash")} onClick={() => newRule(BASH_EXAMPLE)}>Use Bash example</button>
      </Row>
      <Row label="Custom prompts" hint="Use the same mechanism for other explicit transformations, such as 'email' to rewrite a request as a draft email. Choose Shell for any prompt that generates commands." />
    </Section>
    <Section title="Your instructions">
      <Row label={`${p.s.instructions.length} saved instructions`} hint="New instructions start disabled. Edit the prompt and enable one deliberately; saving a rule does not turn smart cleanup on.">
        <button className="btn primary" disabled={!!draft || p.settingsSaving || p.s.instructions.length >= 64} onClick={() => newRule()}>Add instruction</button>
      </Row>
      {p.s.instructions.map((rule) => <Row key={rule.id} label={`${rule.name} (${rule.keyword})`} hint={rule.output === "shell" ? "Shell: review/copy-only" : "Text: normal delivery"}>
        <div className="model-actions">
          <Switch on={rule.enabled} onChange={(on) => { void p.save((s) => { const found = s.instructions.find((r) => r.id === rule.id); if (found) found.enabled = on; }); }} />
          <button className="btn" disabled={!!draft || p.settingsSaving} onClick={() => setDraft({ ...rule })}>Edit</button>
          <button className="btn danger" disabled={p.settingsSaving} onClick={() => void p.save((s) => { s.instructions = s.instructions.filter((r) => r.id !== rule.id); })}>Remove</button>
        </div>
      </Row>)}
    </Section>
    {draft && <InstructionEditor key={draft.id} {...p} initial={draft} close={() => setDraft(null)} />}
    <InstructionPreview {...p} />
  </>;
}

function InstructionEditor({ initial, close, ...p }: PageProps & { initial: Instruction; close: () => void }) {
  const [draft, setDraft] = useState(initial);
  const [error, setError] = useState<string | null>(null);
  const issue = instructionIssue(draft, p.s.instructions);
  const edit = (change: Partial<Instruction>) => { setDraft((d) => ({ ...d, ...change })); setError(null); };
  return <Section title="Edit instruction">
    <Row label="Name"><input type="text" aria-label="Instruction name" maxLength={80} value={draft.name} onChange={(e) => edit({ name: e.target.value })} /></Row>
    <Row label="First keyword"><input type="text" aria-label="Instruction keyword" maxLength={40} value={draft.keyword} onChange={(e) => edit({ keyword: e.target.value })} /></Row>
    <Row label="Output / delivery">
      <select aria-label="Instruction output" value={draft.output} onChange={(e) => edit({ output: e.target.value as Instruction["output"] })}>
        <option value="text">Text: normal delivery</option><option value="shell">Shell: review/copy-only</option>
      </select>
    </Row>
    <div className="row column"><label htmlFor="instruction-prompt">Custom instruction</label>
      <textarea id="instruction-prompt" rows={6} maxLength={4000} value={draft.prompt} onChange={(e) => edit({ prompt: e.target.value })} />
      <div className="hint">Describe how the model should transform the request after the keyword. Do not put API keys or secrets in the prompt; saved prompts are ordinary settings, not encrypted credentials.</div>
    </div>
    <Row label="Enabled"><Switch on={draft.enabled} onChange={(enabled) => edit({ enabled })} /></Row>
    <div className="row model-actions">
      <button className="btn primary" disabled={!!issue || p.settingsSaving} onClick={async () => {
        const next = { ...draft, name: draft.name.trim(), keyword: draft.keyword.trim(), prompt: draft.prompt.trim() };
        const saved = await p.save((s) => {
          const index = s.instructions.findIndex((r) => r.id === next.id);
          if (index < 0) s.instructions.push(next); else s.instructions[index] = next;
        });
        if (saved) close(); else setError("The instruction was not saved. Correct the reported error and try again.");
      }}>Save instruction</button>
      <button className="btn" disabled={p.settingsSaving} onClick={close}>Cancel</button>
      {(error || issue) && <span className="note error" role="alert">{error ?? issue}</span>}
    </div>
  </Section>;
}

function InstructionPreview(p: PageProps) {
  const [text, setText] = useState("bash list all files modified today");
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<CleanupPreview | null>(null);
  const generation = useRef(0);
  const matched = p.s.instructions.find((r) => r.enabled && r.keyword.toLowerCase() === instructionKeyword(text));
  useEffect(() => {
    generation.current++; setResult(null); setBusy(false);
    return () => { generation.current++; };
  }, [text, p.s.instructions, p.s.cleanup]);
  return <Section title="Try a saved instruction">
    <div className="row column">
      <label htmlFor="instruction-test">Include the first keyword, just as you would say it</label>
      <textarea id="instruction-test" rows={3} maxLength={16000} value={text} disabled={busy} onChange={(e) => setText(e.target.value)} />
      <div className="model-actions"><button className="btn" disabled={busy || p.settingsSaving || !p.s.cleanup.smart || !matched} onClick={async () => {
        const mine = ++generation.current;
        setBusy(true); setResult(null);
        try { const next = await invoke<CleanupPreview>("cleanup_preview", { text, style: "asSpoken" }); if (mine === generation.current) setResult(next); }
        catch (e) { if (mine === generation.current) p.toast(String(e)); }
        finally { if (mine === generation.current) setBusy(false); }
      }}>{busy ? "Transforming..." : "Try instruction"}</button>
        <span className="note">{matched ? `Uses ${matched.name}. Nothing is inserted or executed.` : "Enable a saved rule matching the first keyword to try it."}</span>
      </div>
      {p.s.cleanup.engine === "server" && <p className="note">This test sends the text to your selected cleanup connection and may incur its usual inference charge.</p>}
      {result && <div className="preview" role="status"><div className={result.error ? "note error" : "note"}>{result.error ?? `Result (${result.ms} ms)`}</div><pre className="instruction-result">{result.smart ?? result.fillers}</pre></div>}
    </div>
  </Section>;
}
