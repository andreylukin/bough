import { useCallback, useEffect, useState } from "react";
import { Back } from "./app";
import {
  OffToggle, RuleRow, hooksApi, offId, setOffApi, useOffs,
  type Load, type Rule, type Save, type SetOff,
} from "./hooks";

// "What is shaping THIS conversation" — resolved against the session's
// own cwd, because a rule or a context file three directories away is
// not in force here. The wire types live with the view, as the hooks
// ones do: GET /api/sessions/{id}/context always sends every field.

export interface ContextFile {
  path: string;
  found: boolean;
  dropped: number;
  same: string;
}

export interface ContextSkill {
  id: string;
  name: string;
  summary: string;
  source: "plugin" | "pool";
  off: boolean;
}

export interface ContextData {
  cwd: string;
  rules: Rule[];
  contextFiles: ContextFile[];
  skills: ContextSkill[];
}

const base = (path: string) => path.split("/").pop() || path;

export const contextApi = {
  get: (session: string) =>
    fetch(`/api/sessions/${session}/context`).then(async (res) => {
      if (!res.ok) {
        let detail = `${res.status} ${res.statusText}`;
        try {
          const body = (await res.json()) as { error?: string };
          if (body.error) detail = body.error;
        } catch {
          /* a non-JSON error body is not worth masking the status */
        }
        throw new Error(detail);
      }
      return (await res.json()) as ContextData;
    }),
};

/**
 * A context file that was read, or one bough looked for and did not
 * find. The dedup line is the point of the row: two files that say the
 * same thing cost context twice, and the user cannot see that anywhere
 * else.
 */
function FileRow({ f }: { f: ContextFile }) {
  return (
    <details className="proj-row hk-row ctx-file">
      <summary className="ctx-file-main">
        <span className="mono hk-name">{base(f.path)}</span>
        {f.dropped > 0
          ? (
            <span className="hk-when">
              {f.dropped} {f.dropped === 1 ? "section" : "sections"} dropped, identical to {base(f.same)}
            </span>
          )
          : <span className="hk-when">In full</span>}
      </summary>
      <p className="mono hk-path">{f.path}</p>
    </details>
  );
}

/** A section with nothing in it: its name, "none", and the how-to behind a click — one row. */
export function EmptySection({ title, children }: { title: string; children?: React.ReactNode }) {
  return (
    <section className="proj">
      {children
        ? (
          <details className="proj-empty ctx-empty">
            <summary><h2 className="proj-empty-title">{title}</h2> <span className="ctx-none">none</span> <span className="link">How to add</span></summary>
            <p>{children}</p>
          </details>
        )
        : <div className="proj-empty ctx-empty"><div className="ctx-empty-row"><h2 className="proj-empty-title">{title}</h2> <span className="ctx-none">none</span></div></div>}
    </section>
  );
}

function SkillRow({ s, off, setOff, onOff }: {
  s: ContextSkill; off: boolean; setOff: SetOff; onOff: (off: boolean) => void;
}) {
  return (
    <div className={"proj-row hk-row" + (off ? " hk-is-off" : "")}>
      <div className="hk-main">
        <span className="mono hk-name">/{s.name}</span>
        {off && <span className="hk-state hk-offword">Off</span>}
        <span className="hk-tag">{s.source === "plugin" ? "Plugin" : "Pool"}</span>
        <OffToggle id={offId("skill", s.id)} off={off} what={`the skill ${s.name}`}
                   setOff={setOff} onChange={onOff} />
      </div>
      {s.summary && <p className="hk-when hk-summary">{s.summary}</p>}
    </div>
  );
}

/** "49,210 / 1,050,000 tokens · 1,000,790 remaining", or says what is not known. */
function usageLine(used?: number, limit?: number): string {
  if (!used) return "Context usage unavailable";
  if (!limit) return `Last input ${used.toLocaleString()} tokens · limit unknown`;
  return `${used.toLocaleString()} / ${limit.toLocaleString()} tokens · ${Math.max(0, limit - used).toLocaleString()} remaining`;
}

export function ContextView({ data, onBack, load = hooksApi.read, save = hooksApi.write, setOff = setOffApi, used, limit }: {
  data: ContextData; onBack?: () => void; load?: Load; save?: Save; setOff?: SetOff;
  /** Input tokens the last finished turn reported, and the model's window from the catalogue. */
  used?: number; limit?: number;
}) {
  const { cwd, rules, contextFiles, skills } = data;
  const { isOff, mark } = useOffs();
  const found = contextFiles.filter((f) => f.found);
  const missing = contextFiles.filter((f) => !f.found);

  return (
    <div className="thread">
      <header className="thread-head page-head">
        <Back onBack={onBack} />
        <div className="head-main">
          <h1>Context</h1>
          <span className="mono head-repo">{cwd}</span>
        </div>
      </header>

      <div className="scroll proj-body">
        <p className="ctx-lede">
          <span className="num ctx-usage">{usageLine(used, limit)}</span>
          <span>Disabling anything here disables it in every session.</span>
        </p>

        {found.length === 0 && missing.length === 0
          ? (
            <EmptySection title="Context files">
              bough reads <code className="mono">AGENTS.md</code> and <code className="mono">CLAUDE.md</code> from
                 this directory and every one above it.
            </EmptySection>
          )
          : (
            <section className="proj">
              <div className="proj-head">
                <h2>Context files</h2>
                <span className="num proj-count">read from this directory up</span>
              </div>
              {found.map((f) => <FileRow key={f.path} f={f} />)}
              {missing.length > 0 && (
                <details className="ctx-missing">
                  <summary>{missing.length} {missing.length === 1 ? "file" : "files"} not found</summary>
                  {missing.map((f) => <p key={f.path} className="mono hk-path">{f.path}</p>)}
                </details>
              )}
            </section>
          )}

        {rules.length === 0
          ? (
            <EmptySection title="Rules">
              Write a <code className="mono">.md</code> file in <code className="mono">~/.claude/rules</code> to have
                 it apply everywhere, or in <code className="mono">.claude/rules</code> in this repo to have it apply here alone.
            </EmptySection>
          )
          : (
            <section className="proj">
              <div className="proj-head">
                <h2>Rules</h2>
                <span className="num proj-count">
                  {rules.length} {rules.length === 1 ? "rule" : "rules"} apply here
                </span>
              </div>
              {rules.map((r) => (
                <RuleRow key={r.id} r={r} load={load} save={save} setOff={setOff}
                         off={isOff(r.id, r.off)} onOff={mark(r.id)} />
              ))}
            </section>
          )}

        {skills.length === 0
          ? (
            <EmptySection title="Skills">
              A skill is a <code className="mono">SKILL.md</code> folder under
                 <code className="mono"> ~/.claude/skills</code>, or one a plugin brings with it. Add one and it can be
                 run from the composer as <code className="mono">/name</code>.
            </EmptySection>
          )
          : (
            <section className="proj">
              <div className="proj-head">
                <h2>Skills</h2>
                <span className="num proj-count">
                  {skills.length} {skills.length === 1 ? "skill" : "skills"}
                </span>
              </div>
              {skills.map((s) => (
                <SkillRow key={s.id} s={s} setOff={setOff} off={isOff(s.id, s.off)} onOff={mark(s.id)} />
              ))}
            </section>
          )}
      </div>
    </div>
  );
}

/** The live view: reads the session's context once, and on reopen. */
export function ContextPage({ session, model, used, onBack }: { session: string; model?: string; used?: number; onBack?: () => void }) {
  const [data, setData] = useState<ContextData | null>(null);
  const [limit, setLimit] = useState<number | undefined>();
  useEffect(() => {
    if (!model) { setLimit(undefined); return; }
    fetch("/api/models").then((r) => r.json())
      .then((c: { providers?: { models?: { id: string; context?: number }[] }[] }) =>
        setLimit(c.providers?.flatMap((p) => p.models ?? []).find((m) => m.id === model)?.context))
      .catch(() => setLimit(undefined));
  }, [model]);
  const [err, setErr] = useState("");

  const refresh = useCallback(() => {
    contextApi.get(session).then((d) => { setData(d); setErr(""); })
      .catch((e: unknown) => setErr(e instanceof Error ? e.message : String(e)));
  }, [session]);

  useEffect(() => { refresh(); }, [refresh]);

  if (!data) return <Pending title="Context" what="context" err={err} onBack={onBack} onRetry={refresh} />;
  return <ContextView data={data} onBack={onBack} used={used} limit={limit} />;
}

/** Loading or failed: the header and Back stay, so the page is never a dead end. */
export function Pending({ title, what, err, onBack, onRetry }: {
  title: string; what: string; err: string; onBack?: () => void; onRetry: () => void;
}) {
  return (
    <div className="thread">
      <header className="thread-head page-head">
        <Back onBack={onBack} />
        <div className="head-main"><h1>{title}</h1></div>
      </header>
      <div className="scroll proj-body">
        {err
          ? <p className="err">{title} did not load: {err} <button className="link" onClick={onRetry}>Retry</button></p>
          : <p className="proj-none">Loading {what}…</p>}
      </div>
    </div>
  );
}
