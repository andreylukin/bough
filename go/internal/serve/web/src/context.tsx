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
    <div className="proj-row hk-row">
      <div className="hk-main">
        <span className="mono hk-name">{base(f.path)}</span>
        {f.found
          ? f.dropped > 0
            ? (
              <span className="hk-when">
                {f.dropped} {f.dropped === 1 ? "section" : "sections"} dropped, identical to {base(f.same)}
              </span>
            )
            : <span className="hk-when">In full</span>
          : <span className="hk-state hk-offword">Not there — nothing is read from it</span>}
      </div>
      <p className="mono hk-path">{f.path}</p>
    </div>
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

export function ContextView({ data, onBack, load = hooksApi.read, save = hooksApi.write, setOff = setOffApi }: {
  data: ContextData; onBack?: () => void; load?: Load; save?: Save; setOff?: SetOff;
}) {
  const { cwd, rules, contextFiles, skills } = data;
  const { isOff, mark } = useOffs();

  return (
    <div className="thread">
      <header className="thread-head">
        <Back onBack={onBack} />
        <div className="head-main">
          <h1>Context</h1>
          <span className="mono head-repo">{cwd}</span>
        </div>
      </header>

      <div className="scroll proj-body">
        <p className="ctx-lede">
          Everything below is in force for this conversation, resolved against
          <code className="mono"> {cwd}</code>. Turning something off here leaves it listed, off, for every session.
        </p>

        <section className="proj">
          <div className="proj-head">
            <h2>Rules</h2>
            <span className="num proj-count">
              {rules.length} {rules.length === 1 ? "rule" : "rules"} apply here
            </span>
          </div>
          {rules.length === 0
            ? (
              <div className="proj-empty">
                <p className="proj-empty-title">No rules apply here</p>
                <p>Write a <code className="mono">.md</code> file in <code className="mono">~/.claude/rules</code> to have
                   it apply everywhere, or in <code className="mono">.claude/rules</code> under
                   <code className="mono"> {cwd}</code> to have it apply to this repo alone.</p>
              </div>
            )
            : rules.map((r) => (
              <RuleRow key={r.id} r={r} load={load} save={save} setOff={setOff}
                       off={isOff(r.id, r.off)} onOff={mark(r.id)} />
            ))}
        </section>

        <section className="proj">
          <div className="proj-head">
            <h2>Context files</h2>
            <span className="num proj-count">read from this directory up</span>
          </div>
          {contextFiles.length === 0
            ? (
              <div className="proj-empty">
                <p className="proj-empty-title">No context files</p>
                <p>bough reads <code className="mono">AGENTS.md</code> and <code className="mono">CLAUDE.md</code> from
                   <code className="mono"> {cwd}</code> and every directory above it. Write one to tell every session here
                   what it should know.</p>
              </div>
            )
            : contextFiles.map((f) => <FileRow key={f.path} f={f} />)}
        </section>

        <section className="proj">
          <div className="proj-head">
            <h2>Skills</h2>
            <span className="num proj-count">
              {skills.length} {skills.length === 1 ? "skill" : "skills"}
            </span>
          </div>
          {skills.length === 0
            ? (
              <div className="proj-empty">
                <p className="proj-empty-title">No skills reachable</p>
                <p>A skill is a <code className="mono">SKILL.md</code> folder under
                   <code className="mono"> ~/.claude/skills</code>, or one a plugin brings with it. Add one and it can be
                   run from the composer as <code className="mono">/name</code>.</p>
              </div>
            )
            : skills.map((s) => (
              <SkillRow key={s.id} s={s} setOff={setOff} off={isOff(s.id, s.off)} onOff={mark(s.id)} />
            ))}
        </section>
      </div>
    </div>
  );
}

/** The live view: reads the session's context once, and on reopen. */
export function ContextPage({ session, onBack }: { session: string; onBack?: () => void }) {
  const [data, setData] = useState<ContextData | null>(null);
  const [err, setErr] = useState("");

  const refresh = useCallback(() => {
    contextApi.get(session).then((d) => { setData(d); setErr(""); })
      .catch((e: unknown) => setErr(e instanceof Error ? e.message : String(e)));
  }, [session]);

  useEffect(() => { refresh(); }, [refresh]);

  if (!data) {
    return (
      <div className="thread empty">
        <div>
          <h1>{err ? "Context did not load" : "Loading context…"}</h1>
          {err && <p>{err}</p>}
        </div>
      </div>
    );
  }
  return <ContextView data={data} onBack={onBack} />;
}
