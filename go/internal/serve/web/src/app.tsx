import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api, subscribe } from "./api";
import type { Line, Project, Row } from "./types";
import { StatusMark, Working } from "./status";
import { ProjectsView } from "./projects";
import { Markdown, codeLabel, doneSummary, groupSubs, groupTurns, isQuiet, plainTitle, stepCount, stripRunFences, type Item, type SubAgent, type Turn, lineCount } from "./render";
import { Code, parseCall, langForPath } from "./code";
import { SkillPicker } from "./skills";
import { Mentions, triggerAt, type Trigger } from "./mention";
import { HooksPage } from "./hooks";
import { ContextPage } from "./context";
import { Palette, usePaletteKey, type Command } from "./palette";

export type View = "sessions" | "projects" | "hooks";

const POLL_MS = 4000; // sessions we are not streaming still change status

function bucket(iso: string): string {
  const d = new Date(iso), now = new Date();
  const day = (x: Date) => new Date(x.getFullYear(), x.getMonth(), x.getDate()).getTime();
  const days = Math.round((day(now) - day(d)) / 86_400_000);
  if (days <= 0) return "Today";
  if (days === 1) return "Yesterday";
  if (days <= 7) return "Earlier this week";
  return "Older";
}
const clock = (iso: string) => new Date(iso).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });

export function Sprout({ size = 18 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="var(--accent)"
         strokeWidth="1.5" strokeLinecap="round" aria-hidden="true">
      <path d="M12 21V9" /><path d="M12 9c0-3 2-5 5-5 0 3-2 5-5 5z" />
      <path d="M12 13c0-2.5-1.8-4.5-4.5-4.5 0 2.5 2 4.5 4.5 4.5z" />
    </svg>
  );
}

export /** A path as a person reads it: ~ for home, and no repetition of it. */
function shortPath(p: string, home: string): string {
  return home && p.startsWith(home) ? "~" + p.slice(home.length) : p;
}

/** ⌘ on a Mac, Ctrl everywhere else. */
function modKey(): string {
  return typeof navigator !== "undefined" && /Mac|iP/.test(navigator.platform) ? "\u2318" : "Ctrl+";
}

function Sidebar({ rows, selected, onSelect, query, onQuery, showArchived, onToggleArchived, view, onView }: {
  rows: Row[]; selected: string | null; onSelect: (id: string) => void;
  query: string; onQuery: (q: string) => void; showArchived: boolean; onToggleArchived: () => void;
  view: View; onView: (v: View) => void;
}) {
  const groups = useMemo(() => {
    const out = new Map<string, Row[]>();
    for (const r of rows) {
      const k = bucket(r.modified);
      if (!out.has(k)) out.set(k, []);
      out.get(k)!.push(r);
    }
    return [...out.entries()];
  }, [rows]);

  return (
    <div className="sidebar">
      <div className="brand"><Sprout /><span>bough</span></div>
      <nav className="nav" aria-label="Views">
        <button className={"nav-item" + (view === "sessions" ? " nav-on" : "")}
                aria-current={view === "sessions" ? "page" : undefined}
                onClick={() => onView("sessions")}>Conversations</button>
        <button className={"nav-item" + (view === "projects" ? " nav-on" : "")}
                aria-current={view === "projects" ? "page" : undefined}
                onClick={() => onView("projects")}>Projects</button>
        <button className={"nav-item" + (view === "hooks" ? " nav-on" : "")}
                aria-current={view === "hooks" ? "page" : undefined}
                onClick={() => onView("hooks")}>Hooks</button>
      </nav>
      <div style={{ padding: "0 20px 18px" }}>
        <label htmlFor="q" className="field-label">
          Search sessions
          {/* A palette nobody knows about is not a feature. */}
          <span className="pal-key" aria-hidden="true">{modKey()}K</span>
        </label>
        <input id="q" className="field" value={query} placeholder="Title, repo or branch"
               onChange={(e) => onQuery(e.target.value)} />
      </div>
      <div className="scroll" style={{ display: "flex", flexDirection: "column", gap: 22 }}>
        {groups.length === 0 && (
          <p style={{ padding: "0 20px", color: "var(--text-3)", fontSize: 13 }}>
            {query ? `No sessions match “${query}”.` : "No sessions yet."}
          </p>
        )}
        {groups.map(([name, list]) => (
          <div key={name} style={{ display: "flex", flexDirection: "column", gap: 2 }}>
            <div className="group-head">{name}</div>
            {list.map((r) => (
              <button key={r.id} onClick={() => onSelect(r.id)}
                      className={"row" + (r.id === selected ? " row-on" : "")}
                      aria-current={r.id === selected ? "true" : undefined}>
                <span className="row-line">
                  <span className="row-title">{plainTitle(r.title) || "Untitled session"}</span>
                  <span className="num">{clock(r.modified)}</span>
                </span>
                <span className="row-meta">
                  <StatusMark status={r.status} />
                  {(r.repo || r.branch) && (
                    <span className="mono">
                      {r.repo?.split("/").pop()}
                      {r.branch && <span style={{ color: "var(--line-strong)" }}>/</span>}{r.branch}
                    </span>
                  )}
                </span>
              </button>
            ))}
          </div>
        ))}
      </div>
      <div className="sidebar-foot">
        <button className="link" onClick={onToggleArchived}>
          {showArchived ? "Hide archived" : "Show archived"}
        </button>
      </div>
    </div>
  );
}

/* ---------------- transcript ---------------- */

/**
 * Copy what a block holds.
 *
 * It sits OUTSIDE the <details>, below it, because a <details> hides
 * every child but its summary when closed — and the whole point is to
 * copy a command or its output without opening the block first.
 */
function CopyButton({ text, what }: { text: string; what: string }) {
  const [done, setDone] = useState(false);
  useEffect(() => {
    if (!done) return;
    const t = setTimeout(() => setDone(false), 1400);
    return () => clearTimeout(t);
  }, [done]);

  const copy = async () => {
    try {
      // navigator.clipboard needs a secure context; localhost is one.
      // The textarea fallback is for anything that is not.
      if (navigator.clipboard?.writeText) {
        await navigator.clipboard.writeText(text);
      } else {
        const ta = document.createElement("textarea");
        ta.value = text;
        ta.style.position = "fixed";
        ta.style.opacity = "0";
        document.body.appendChild(ta);
        ta.select();
        document.execCommand("copy");
        ta.remove();
      }
      setDone(true);
    } catch {
      setDone(false);
    }
  };

  return (
    <div className="block-foot">
      <button className="copy-btn" onClick={copy} aria-label={`Copy ${what}`}>
        {/* The word changes, not just a colour: a state carried by
            colour alone says nothing to half the people reading it. */}
        {done ? "Copied" : "Copy"}
      </button>
    </div>
  );
}

export function CodeBlock({ line }: { line: Line }) {
  const call = useMemo(() => parseCall(line.text), [line.text]);
  const lines = call.body ? call.body.split("\n").length : 0;
  // Collapsed by default. A turn is a list of things the agent did; the
  // point of the list is to be scanned, and an open block for every one
  // of them buries the reply that follows.
  return (
    <div className="block-wrap">
    <details className="block">
      <summary>
        <span className="block-label">{call.verb}</span>
        <span className="mono block-detail">{firstLine(call.gist)}</span>
        {lines > 1 && <span className="num block-lines">{lineCount(lines)}</span>}
      </summary>
      <div className="block-body">
        {call.body && <Code text={call.body} lang={call.lang} />}
        {call.body !== call.raw && (
          // The program bough actually ran, one layer further in. The
          // block above is the readable version of it, not a substitute.
          <details className="block-inner">
            <summary><span className="block-label">The call</span></summary>
            <Code text={call.raw} lang="javascript" />
          </details>
        )}
      </div>
    </details>
    <CopyButton text={call.body || call.raw} what={call.verb.toLowerCase() + " block"} />
    </div>
  );
}

/** The first non-empty line, for a one-line summary. */
function firstLine(text: string): string {
  const l = text.split("\n").find((x) => x.trim()) ?? "";
  return l.length > 110 ? l.slice(0, 110) + "…" : l;
}

export function ResultBlock({ line }: { line: Line }) {
  // history.EntryText prepends a result's own code to its text (the
  // command is as memorable as its output). Here the code already has
  // its own block directly above, so showing it again doubles every
  // result. data.code is that prefix.
  const code = typeof line.data?.code === "string" ? (line.data.code as string) : "";
  const body = code && line.text.startsWith(code) ? line.text.slice(code.length).trimStart() : line.text;
  const lines = (body || "(no output)").split("\n");
  const head = lines.find((l) => l.trim()) ?? "";
  return (
    <div className="block-wrap">
    <details className="block">
      <summary>
        <span className="block-label">Result</span>
        <span className="mono block-detail">{head.slice(0, 90)}</span>
        <span className="num block-lines">{lineCount(lines.length)}</span>
      </summary>
      <div className="block-body">
        <Code text={body || "(no output)"} lang={resultLang(line)} />
      </div>
    </details>
    <CopyButton text={body} what="output" />
    </div>
  );
}

/**
 * Colour a result by what produced it: a file that was read is coloured
 * as that file, everything else is shell output.
 */
function resultLang(line: Line): string {
  const code = typeof line.data?.code === "string" ? (line.data.code as string) : "";
  const call = code ? parseCall(code) : null;
  if (call?.verb === "Read" && call.target) return langForPath(call.target);
  return "";
}

/**
 * A background job records its whole run as one entry: a header line
 * ("job 49 [exited 0] <cmd> (3s)") then everything it printed. Shown
 * as running text that is an unreadable wall — a push with a diff in
 * it fills the pane. It is a result, so it reads like one.
 */
export function JobBlock({ line }: { line: Line }) {
  const all = (line.text || "").split("\n");
  const head = all[0] ?? "";
  const body = all.slice(1).join("\n").trim();
  const exit = /\[exited ([0-9]+)\]/.exec(head);
  const failed = exit ? exit[1] !== "0" : false;
  return (
    <details className={"block" + (failed ? " block-failed" : "")}>
      <summary>
        <span className="block-label">{failed ? "Job failed" : "Job"}</span>
        <span className="mono block-detail">{head.replace(/^job\s+/, "").slice(0, 90)}</span>
        {body && <span className="num block-lines">{lineCount(body.split("\n").length)}</span>}
      </summary>
      <pre className="mono">{body || "(no output)"}</pre>
    </details>
  );
}

export /** Entry data is JSON: read a field as a string without trusting it. */
function str(v: unknown): string {
  return typeof v === "string" ? v : "";
}

function Entry({ line, codes, nested }: { line: Line; codes: string[]; nested?: boolean }) {
  const k = line.kind;
  if (k === "assistant" || k === "sub:assistant") {
    const body = stripRunFences(line.text, codes);
    if (!body) return null; // the reply was only the program it ran
    // Inside a subagent card the rail and the card's own header
    // already say whose words these are; repeating "subagent" above
    // every paragraph of a five-step run is noise.
    return (
      <div className="say">
        {!nested && (
          <div className="say-who">{k === "assistant" ? <Sprout size={14} /> : <span className="sub-dot" />}
            <span>{k === "assistant" ? "bough" : "subagent"}</span></div>
        )}
        <Markdown text={body} />
      </div>
    );
  }
  if (k === "code" || k === "sub:code") return <CodeBlock line={line} />;
  if (k === "result" || k === "sub:result") return <ResultBlock line={line} />;
  if (k === "thinking") {
    // A column of rows all reading just "Thinking" says nothing about
    // which one is worth opening. Carry the same preview and line
    // count every other block has.
    const lines = (line.text || "").split("\n");
    const head = lines.find((l) => l.trim()) ?? "";
    return (
      <details className="block thinking">
        <summary>
          <span className="block-label">Thinking</span>
          <span className="block-detail">{plainTitle(head).slice(0, 90)}</span>
        </summary>
        {/* Reasoning is markdown like any other reply: left raw it
            shows its own backticks and list markers as punctuation. */}
        <div className="think-body"><Markdown text={line.text} /></div>
      </details>
    );
  }
  if (k === "error" || k === "sub:error") return <div className="err">{line.text}</div>;
  if (k === "ask") return null; // the live ask renders as its own card below
  if (k === "job") return <JobBlock line={line} />;
  if (k === "hook") {
    // Hooks fire on every tool call. One that passed through is not
    // news — the Hooks view is where the full ledger lives. Only a
    // fire that decided something, or threw, earns a line here.
    const d = line.data ?? {};
    const decision = str(d.decision);
    const err = str(d.error);
    const notice = str(d.notice);
    // A hook that passed through silently is not news. One that decided
    // something, threw, or had something to say to you, is.
    if (!decision && !err && !notice) return null;
    return (
      <p className="hook-line">
        <span className="mono">{str(d.name)}</span>
        {" · "}{str(d.event)}
        {(decision || err) && <>
          {" · "}
          <span className={err ? "hook-bad" : "hook-act"}>{err ? "errored" : decision}</span>
        </>}
        {err && <span className="hook-why"> {err}</span>}
        {notice && <span className="hook-why"> — {notice}</span>}
      </p>
    );
  }
  if (isQuiet(k)) return <div className="meta-line">{line.text || k}</div>;
  return <div className="meta-line">{line.text || k}</div>;
}

/* ---------------- subagents ---------------- */

/** How a finished subagent is described: a word, a glyph, never a hue alone. */
function subState(status: string): { word: string; cls: string } {
  if (status === "error") return { word: "Failed", cls: "sub-failed" };
  if (status === "ok") return { word: "Finished", cls: "sub-ok" };
  return { word: "Working", cls: "sub-live" };
}

export function SubAgentView({ agent }: { agent: SubAgent }) {
  const st = subState(agent.status);
  const codes = agent.lines.filter((l) => l.kind === "sub:code").map((l) => l.text);
  const task = firstLine(plainTitle(agent.task)) || "a subagent";
  // A run that failed is the one you opened the thread to read, so it
  // opens itself. The rest stay folded: the point of the card is that
  // a subagent reads as one thing that happened.
  return (
    <details className={"sub " + st.cls} open={agent.status === "error"}>
      <summary>
        <span className="sub-tag">Subagent {agent.worker}</span>
        <span className="sub-task">{task}</span>
        <span className="sub-state">
          {agent.status === "" && (
            <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2"
                 strokeLinecap="round" className="spin-mark" aria-hidden="true">
              <circle cx="12" cy="12" r="8.5" strokeDasharray="40 14" />
            </svg>
          )}
          {st.word}
        </span>
        {agent.steps > 0 && <span className="num sub-steps">{stepCount(agent.steps)}</span>}
      </summary>
      <div className="sub-body">
        {agent.lines.map((l) => <Entry key={l.seq} line={l} codes={codes} nested />)}
        {agent.lines.length === 0 && <p className="meta-line">Nothing recorded yet.</p>}
      </div>
    </details>
  );
}

/**
 * One run of subagent work, spliced into the parent's turn. Several
 * can be in flight at once and their entries interleave step by step,
 * so they are dealt back into one card per agent — otherwise the
 * transcript reads as one agent with a split personality.
 */
export function SubRun({ agents }: { agents: SubAgent[] }) {
  return (
    <div className="subrun">
      <p className="subrun-head">
        {agents.length === 1 ? "A subagent worked on this" : `${agents.length} subagents worked on this`}
      </p>
      {agents.map((a) => <SubAgentView key={a.worker + ":" + a.seq} agent={a} />)}
    </div>
  );
}

export function TurnView({ turn, tail }: { turn: Turn; tail?: React.ReactNode }) {
  const summary = turn.done ? doneSummary(turn.done) : "";
  const codes = turn.body.filter((l) => l.kind === "code" || l.kind === "sub:code").map((l) => l.text);
  const items = useMemo<Item[]>(() => groupSubs(turn.body), [turn.body]);
  return (
    <section className="turn">
      {turn.prompt && (
        <div className="prompt">
          <span className="mono prompt-mark">&gt;</span>
          <p>{turn.prompt.text}</p>
          <span className="num prompt-time">{clock(turn.prompt.at)}</span>
        </div>
      )}
      <div className="turn-body">
        {items.map((it) => it.kind === "sub"
          ? <SubRun key={"sub" + it.seq} agents={it.agents} />
          : <Entry key={it.seq} line={it.line} codes={codes} />)}
        {tail}
      </div>
      {turn.done && (
        <div className="turn-done">
          {turn.done.kind === "cancelled" ? "Stopped" : summary || "Finished"}
        </div>
      )}
    </section>
  );
}

/* ---------------- live preview ---------------- */

/**
 * A fragment of a reply that has not been recorded yet. The server
 * sends these coalesced every 50ms and never keeps them: they are a
 * preview of an entry that does not exist, and the moment the real
 * entry lands through the ?since= refetch the run that produced it is
 * dropped. Nothing here is ever a source of truth.
 */
export interface DeltaRun { kind: "assistant" | "thinking"; text: string }

export function StreamView({ runs }: { runs: DeltaRun[] }) {
  if (!runs.length) return null;
  return (
    <>
      {runs.map((r, i) => r.kind === "thinking" ? (
        // Only the run still being written carries the caret; an
        // earlier one is finished text waiting to be recorded.
        <div key={i} className={"block thinking stream-think" + (i === runs.length - 1 ? " stream-tip" : "")}>
          <div className="stream-think-head"><span className="block-label">Thinking</span></div>
          <div className="think-body"><Markdown text={r.text} /></div>
        </div>
      ) : (
        <div key={i} className={"say stream-say" + (i === runs.length - 1 ? " stream-tip" : "")}>
          <div className="say-who"><Sprout size={14} /><span>bough</span></div>
          <Markdown text={r.text} />
        </div>
      ))}
    </>
  );
}

/* ---------------- model + effort ---------------- */

interface ModelInfo { id: string; context?: number; efforts?: string[]; input?: number; output?: number }
interface ProviderInfo { plugin: string; models?: ModelInfo[] }

export function Controls({ row, projects, onModel, onEffort, onAssign }: {
  row: Row; projects: Project[];
  onModel: (m: string) => void; onEffort: (e: string) => void; onAssign: (p: string) => void;
}) {
  const [cat, setCat] = useState<{ providers: ProviderInfo[]; efforts: string[] } | null>(null);
  useEffect(() => {
    fetch("/api/models").then((r) => r.json()).then(setCat).catch(() => setCat(null));
  }, []);

  return (
    <div className="controls">
      <label className="ctl">
        <span className="ctl-label">Model</span>
        <select value={row.model ?? ""} onChange={(e) => e.target.value && onModel(e.target.value)}>
          {/* A session that has not answered yet genuinely has no model
              to name; everything else says the one that is answering. */}
          <option value="">{row.model ? row.model : "not set yet"}</option>
          {cat?.providers.map((p) => (
            <optgroup key={p.plugin} label={p.plugin.replace(/^llm-/, "")}>
              {(p.models ?? []).map((m) => (
                <option key={p.plugin + m.id} value={m.id}>
                  {m.id}{m.context ? ` — ${Math.round(m.context / 1000)}k` : ""}
                </option>
              ))}
            </optgroup>
          ))}
        </select>
      </label>
      <label className="ctl">
        <span className="ctl-label">Project</span>
        <select value={row.project ?? ""} onChange={(e) => onAssign(e.target.value)}>
          <option value="">Unassigned</option>
          {projects.map((p) => <option key={p.id} value={p.id}>{p.name}</option>)}
        </select>
      </label>
      <label className="ctl">
        <span className="ctl-label">Thinking</span>
        <select value={row.effort ?? ""} onChange={(e) => e.target.value && onEffort(e.target.value)}>
          <option value="">{row.effort ? row.effort : "default"}</option>
          {(cat?.efforts ?? []).map((e) => <option key={e} value={e}>{e}</option>)}
        </select>
      </label>
    </div>
  );
}

/* ---------------- thread ---------------- */

export function Back({ onBack }: { onBack?: () => void }) {
  if (!onBack) return null;
  return (
    <button className="back" onClick={onBack} aria-label="Back to sessions">
      <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.7"
           strokeLinecap="round" strokeLinejoin="round" aria-hidden="true"><path d="M15 5l-7 7 7 7" /></svg>
    </button>
  );
}

export function Thread({ row, lines, stream = [], projects, onSend, onAnswer, onInterrupt, onArchive, onRename, onModel, onEffort, onAssign, onBack, onContext, busy }: {
  row: Row; lines: Line[]; stream?: DeltaRun[]; projects: Project[]; busy: boolean; onBack?: () => void;
  onSend: (t: string) => void; onAnswer: (t: string) => void; onInterrupt: () => void;
  onArchive: () => void; onRename: (t: string) => void; onContext?: () => void;
  onModel: (m: string) => void; onEffort: (e: string) => void; onAssign: (p: string) => void;
}) {
  const [draft, setDraft] = useState("");
  const [trigger, setTrigger] = useState<Trigger | null>(null);
  // A phone hides the model, project and thinking controls behind one
  // button: they change rarely, and the thread is what the screen is for.
  const [more, setMore] = useState(false);
  const end = useRef<HTMLDivElement>(null);
  const composer = useRef<HTMLTextAreaElement>(null);
  // A paste fires no keydown, so anything that reads the caret off a
  // key event is stale for exactly one change. Worse, the pasted text
  // itself can end in "@foo" or "/bar" and open a picker over a token
  // nobody typed. The guard closes the picker for that one change and
  // is lifted by the next real key.
  const pasted = useRef(false);
  const streamLen = stream.reduce((n, r) => n + r.text.length, 0);
  const scroller = useRef<HTMLDivElement>(null);
  // Stick to the bottom only while you are already there. Scrolling up
  // during a streaming turn used to be impossible: every fragment
  // re-scrolled to the end and dragged you back down mid-sentence.
  const atBottom = useRef(true);
  const onScroll = () => {
    const el = scroller.current;
    if (!el) return;
    // A slack of a couple of lines: "near the bottom" is what a reader
    // means by "at the bottom", and an exact test loses the stick the
    // moment a fragment arrives a pixel early.
    atBottom.current = el.scrollHeight - el.scrollTop - el.clientHeight < 40;
  };
  useEffect(() => {
    if (atBottom.current) end.current?.scrollIntoView({ block: "end" });
  }, [lines.length, streamLen]);
  // Opening a different conversation starts at the bottom again.
  useEffect(() => { atBottom.current = true; }, [row.id]);
  const turns = useMemo(() => groupTurns(lines), [lines]);
  const running = row.status === "running";

  // A long paste should be visible, not a two-row porthole you have to
  // drag open. Grow to the text and stop at a third of the window.
  useEffect(() => {
    const el = composer.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = Math.min(el.scrollHeight, Math.round(window.innerHeight / 3)) + "px";
  }, [draft]);

  const caretTrigger = (el: HTMLTextAreaElement) => setTrigger(triggerAt(el.value, el.selectionStart ?? 0));

  const send = () => {
    const t = draft.trim();
    if (!t) return;
    setDraft("");
    if (row.ask) onAnswer(t); else onSend(t);
  };

  return (
    <div className="thread">
      <header className="thread-head" data-more={more ? "1" : "0"}>
        <Back onBack={onBack} />
        <div className="head-main">
          <h1 title={row.title}>{plainTitle(row.title) || "Untitled session"}</h1>
          {(row.repo || row.branch) && (
            <span className="mono head-repo">
              {row.repo?.split("/").pop()}
              {row.branch && <span style={{ color: "var(--line-strong)" }}>/</span>}{row.branch}
            </span>
          )}
          <StatusMark status={row.status} />
        </div>
        <button className="more" aria-label="Session settings" aria-expanded={more}
                onClick={() => setMore((v) => !v)}>
          <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.7"
               strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
            <path d="M4 7h10M18 7h2M4 17h4M12 17h8" /><circle cx="15" cy="7" r="2" /><circle cx="9" cy="17" r="2" />
          </svg>
        </button>
        <div className="head-side">
          <Controls row={row} projects={projects} onModel={onModel} onEffort={onEffort} onAssign={onAssign} />
          <div className="head-actions">
            <button className="btn" onClick={() => {
              const t = prompt("Rename session", plainTitle(row.title));
              if (t !== null) onRename(t);
            }}>Rename</button>
            <button className="btn" onClick={onArchive}>{row.archived ? "Unarchive" : "Archive"}</button>
            {onContext && <button className="btn" onClick={onContext}>Context</button>}
          </div>
        </div>
      </header>

      <div className="scroll transcript" ref={scroller} onScroll={onScroll}>
        {turns.map((t, i) => (
          <TurnView key={t.seq} turn={t}
            // The preview belongs to the turn that is still open, so it
            // sits where the recorded entry will appear and is replaced
            // in place rather than jumping up the page.
            tail={i === turns.length - 1 && !t.done ? (
              <>
                <StreamView runs={stream} />
                {running && !row.ask && <Working label={stream.length && stream[stream.length - 1].kind === "thinking" ? "Thinking" : "Working"} />}
              </>
            ) : undefined} />
        ))}
        {running && (turns.length === 0 || turns[turns.length - 1].done) && !row.ask && (
          <div className="turn"><div className="turn-body"><StreamView runs={stream} /><Working /></div></div>
        )}
        {row.ask && (
          <div className="ask">
            <StatusMark status="needs-you" size={16} />
            <p className="ask-q">{row.ask.text}</p>
            {row.ask.options.length > 0 && (
              <div style={{ display: "flex", gap: 10, flexWrap: "wrap" }}>
                {row.ask.options.map((o) => (
                  <button key={o} className="btn btn-primary" onClick={() => onAnswer(o)}>{o}</button>
                ))}
              </div>
            )}
          </div>
        )}
        <div ref={end} />
      </div>

      <div className="composer-wrap">
        <div className="composer">
          <Mentions trigger={trigger} session={row.id}
            onPick={(t, value) => {
              // Replace the token being typed, and leave a trailing
              // space so the next word is not glued to it.
              const next = draft.slice(0, t.from) + t.kind + value + " " + draft.slice(t.to);
              setDraft(next);
              setTrigger(null);
              const el = composer.current;
              const caret = t.from + value.length + 2;
              requestAnimationFrame(() => { el?.focus(); el?.setSelectionRange(caret, caret); });
            }}
            onClose={() => setTrigger(null)} />
          <textarea id="composer" ref={composer} value={draft} rows={2}
            aria-label="Message"
            placeholder={row.ask ? "Answer the question above"
              : running ? "Send a message — it steers the turn already running"
              : "Send a message to start the next turn"}
            onPaste={() => { pasted.current = true; }}
            onChange={(e) => {
              setDraft(e.target.value);
              // The change a paste produces carries a caret at the end
              // of text nobody typed. Leave the picker shut rather than
              // opening one over a pasted path or address.
              if (pasted.current) { setTrigger(null); return; }
              caretTrigger(e.currentTarget);
            }}
            onKeyUp={(e) => {
              // Moving the caret changes what is being typed, so the
              // picker follows arrows and clicks as well as letters.
              // The keyup of the paste chord itself is not a caret move.
              if (pasted.current) return;
              caretTrigger(e.currentTarget);
            }}
            onClick={(e) => { pasted.current = false; caretTrigger(e.currentTarget); }}
            onBlur={() => { pasted.current = false; setTrigger(null); }}
            onKeyDown={(e) => {
              // The paste chord's own keydown comes before the paste, so
              // the next keydown after one is a genuine later keystroke.
              if (!(e.metaKey || e.ctrlKey)) pasted.current = false;
              // While the picker is up it owns Enter and the arrows.
              if (trigger && ["Enter", "Tab", "ArrowUp", "ArrowDown", "Escape"].includes(e.key)) return;
              if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); send(); }
            }} />
          <div style={{ display: "flex", alignItems: "center", gap: 14 }}>
            <span className="hint">Return to send</span>
            <span className="hint">Shift + Return for a newline</span>
            <div style={{ marginLeft: "auto", display: "flex", gap: 10 }}>
              <SkillPicker onPick={(name) => {
                setDraft((d) => (d.trimStart().startsWith("/") ? d : `/${name} ${d.trimStart()}`));
                document.getElementById("composer")?.focus();
              }} />
              {running && <button className="btn" onClick={onInterrupt}>Stop</button>}
              <button className="btn btn-primary" onClick={send} disabled={busy || !draft.trim()}>Send</button>
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}

export default function App() {
  const [rows, setRows] = useState<Row[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  const [lines, setLines] = useState<Line[]>([]);
  // Live fragments of the reply being written, newest last. Never
  // merged into `lines`: these carry no history seq and the recorded
  // entry always supersedes them.
  const [stream, setStream] = useState<DeltaRun[]>([]);
  const [query, setQuery] = useState("");
  const [archived, setArchived] = useState(false);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [view, setView] = useState<View>("sessions");
  const [projects, setProjects] = useState<Project[]>([]);
  // Only a narrow window reads this (see the 720px media query): a
  // phone shows the list or the thread, never both.
  const [pane, setPane] = useState<"list" | "thread">("list");
  // The Context panel takes over the thread pane for the open session,
  // and closes when a different one is opened.
  const [context, setContext] = useState(false);

  const refresh = useCallback(async () => {
    try {
      const [rs, ps] = await Promise.all([api.sessions(archived), api.projects()]);
      setRows(rs); setProjects(ps); setErr(null);
    } catch (e) { setErr(e instanceof Error ? e.message : String(e)); }
  }, [archived]);

  useEffect(() => { refresh(); const t = setInterval(refresh, POLL_MS); return () => clearInterval(t); }, [refresh]);

  useEffect(() => {
    if (!selected) return;
    let live = true;
    api.session(selected).then((r) => {
      if (!live) return;
      setLines(r.entries);
      setRows((prev) => prev.map((x) => (x.id === r.session.id ? r.session : x)));
    }).catch((e) => setErr(String(e)));

    setStream([]);
    // How many delta runs were already on screen when the last recorded
    // event arrived. The server drains its delta buffer synchronously
    // before it forwards a recorded entry, so every run up to this mark
    // is part of what that entry contains — and only those are dropped
    // when the refetch lands. Fragments that arrived after it are the
    // beginning of the next entry and must survive.
    let superseded = 0;
    let runs = 0;

    // An event is a CHANGE SIGNAL, not a transcript line: Event.Seq is a
    // supervisor counter, transcript entries carry history seqs, and
    // merging the two silently drops events whose numbers collide.
    let timer: ReturnType<typeof setTimeout> | undefined;
    const stop = subscribe(selected, (ev) => {
      if (ev.kind === "assistant-delta" || ev.kind === "thinking-delta") {
        const kind = ev.kind === "thinking-delta" ? "thinking" : "assistant";
        setStream((prev) => {
          const n = prev.length;
          let next: DeltaRun[];
          if (n && prev[n - 1].kind === kind) {
            next = prev.slice();
            next[n - 1] = { kind, text: next[n - 1].text + ev.text };
          } else {
            next = [...prev, { kind, text: ev.text }];
          }
          runs = next.length;
          return next;
        });
        return;
      }
      superseded = runs;
      clearTimeout(timer);
      timer = setTimeout(() => {
        setLines((prev) => {
          const since = prev.length ? prev[prev.length - 1].seq : 0;
          api.session(selected, since).then((r) => {
            if (!live) return;
            setRows((rs) => rs.map((x) => (x.id === r.session.id ? r.session : x)));
            if (!r.entries.length) return;
            const drop = superseded;
            superseded = 0;
            // The recorded entries are in hand; the fragments they were
            // built from go in the same commit, so the text is never
            // absent for a frame and never shown twice.
            setStream((cur) => { runs = Math.max(0, runs - drop); return cur.slice(drop); });
            setLines((cur) => {
              const seen = new Set(cur.map((l) => l.seq));
              return [...cur, ...r.entries.filter((e) => !seen.has(e.seq))];
            });
          }).catch(() => { /* the next event retries the catch-up */ });
          return prev;
        });
      }, 120);
    });
    return () => { live = false; clearTimeout(timer); stop(); };
  }, [selected]);

  const visible = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return rows;
    return rows.filter((r) => [r.title, r.repo, r.branch, r.cwd].some((v) => v?.toLowerCase().includes(q)));
  }, [rows, query]);

  const row = rows.find((r) => r.id === selected) ?? null;

  // A preview outlives its turn only if the entry it was previewing
  // never arrived. Once the session is no longer running there is
  // nothing left to be a preview of.
  const status = row?.status;
  useEffect(() => { if (status && status !== "running") setStream([]); }, [status]);

  const [palette, setPalette] = useState(false);
  const [home, setHome] = useState("");
  useEffect(() => { api.home().then(setHome).catch(() => setHome("")); }, []);

  /**
   * Where you are lives in the URL.
   *
   * It was React state alone, so reloading the tab dropped you back on
   * "no session open" with the conversation you had just started
   * somewhere in a list of a hundred and fifty — which reads as the
   * agent having been interrupted, though it never stops. It also
   * makes Back work, and makes a conversation a link you can send
   * yourself.
   */
  useEffect(() => {
    const read = () => {
      const h = window.location.hash.replace(/^#\/?/, "");
      if (h === "hooks" || h === "projects") { setView(h); setContext(false); return; }
      const m = /^s\/([^/]+)(\/context)?$/.exec(h);
      if (m) {
        setView("sessions"); setSelected(m[1]); setContext(Boolean(m[2])); setPane("thread");
      } else if (h === "") {
        setView("sessions"); setSelected(null); setContext(false);
      }
    };
    read();
    window.addEventListener("popstate", read);
    window.addEventListener("hashchange", read);
    return () => {
      window.removeEventListener("popstate", read);
      window.removeEventListener("hashchange", read);
    };
  }, []);

  // Writing it back is replaceState, not push: every keystroke in the
  // sidebar filter would otherwise become a history entry to walk back
  // through. Opening a conversation pushes (see openSession).
  useEffect(() => {
    const want = view === "hooks" ? "#/hooks"
      : view === "projects" ? "#/projects"
      : selected ? `#/s/${selected}${context ? "/context" : ""}`
      : "#/";
    if (window.location.hash !== want) {
      window.history.replaceState(null, "", want);
    }
  }, [view, selected, context]);

  /**
   * An open block closes when you click anywhere in it — the whole
   * body, not just its one-line header. Two things must still work:
   * selecting text to copy a command ends in a click and must not
   * slam the block shut, and a nested disclosure or button owns its
   * own clicks.
   */
  useEffect(() => {
    const onClick = (e: MouseEvent) => {
      const t = e.target as HTMLElement | null;
      if (!t) return;
      const block = t.closest("details.block[open]") as HTMLDetailsElement | null;
      if (!block) return;
      if (t.closest("summary")) return;                       // already toggles
      if (t.closest("a,button,input,textarea,select,label")) return;
      if (t.closest("details.block-inner") !== null) return;  // the inner call
      if ((window.getSelection()?.toString() ?? "") !== "") return;
      block.open = false;
    };
    document.addEventListener("click", onClick);
    return () => document.removeEventListener("click", onClick);
  }, []);
  usePaletteKey(useCallback(() => setPalette(true), []));

  const openSession = useCallback((id: string) => {
    setSelected(id); setContext(false); setView("sessions"); setPane("thread");
    // A push, so Back returns to where you were rather than leaving.
    if (window.location.hash !== `#/s/${id}`) {
      window.history.pushState(null, "", `#/s/${id}`);
    }
  }, []);

  const act = async (fn: () => Promise<unknown>) => {
    setBusy(true);
    try { await fn(); setErr(null); }
    catch (e) { setErr(e instanceof Error ? e.message : String(e)); }
    finally { setBusy(false); await refresh(); }
  };

  // What the chrome can do, the keyboard can do. Session-scoped
  // commands only appear when one is open, so the list never offers
  // something that would fail.
  const start = (cwd: string, prompt: string) => act(async () => {
    const created = await api.create(cwd, prompt);
    openSession(created.id);
  });

  const commands: Command[] = [
    ...(home ? [{
      id: "new:here", group: "Start", label: "New conversation",
      hint: shortPath(home, home),
      run: () => start(home, ""),
    }] : []),
    ...(home && row?.cwd && row.cwd !== home ? [{
      id: "new:cwd", group: "Start", label: "New conversation where this one is",
      hint: shortPath(row.cwd, home),
      run: () => start(row.cwd, ""),
    }] : []),
    { id: "new:project", group: "Start", label: "New project…",
      run: () => { const n = prompt("Name the project"); if (n?.trim()) act(() => api.newProject(n.trim())); } },
    { id: "go:sessions", group: "Go to", label: "Conversations",
      run: () => { setView("sessions"); setContext(false); setPane("thread"); } },
    { id: "go:projects", group: "Go to", label: "Projects",
      run: () => { setView("projects"); setPane("thread"); } },
    { id: "go:hooks", group: "Go to", label: "Hooks",
      run: () => { setView("hooks"); setPane("thread"); } },
    { id: "go:archived", group: "Go to",
      label: archived ? "Hide archived conversations" : "Show archived conversations",
      run: () => setArchived((v) => !v) },
    ...(row ? [
      { id: "s:context", group: "This conversation", label: "Show what is shaping this conversation",
        hint: "Context", run: () => { setContext(true); setPane("thread"); } },
      { id: "s:archive", group: "This conversation",
        label: row.archived ? "Unarchive this conversation" : "Archive this conversation",
        run: () => act(() => (row.archived ? api.unarchive(row.id) : api.archive(row.id))) },
      ...(row.status === "running" ? [{
        id: "s:stop", group: "This conversation", label: "Stop this turn",
        run: () => act(() => api.interrupt(row.id)),
      }] : []),
    ] : []),
  ];

  return (
    <div className="app" data-pane={pane}>
      <Palette open={palette} onClose={() => setPalette(false)} rows={rows}
               commands={commands} onOpenSession={openSession}
               onStart={home ? (text) => start(home, text) : undefined} />
      <Sidebar rows={visible} selected={selected}
               onSelect={(id) => { setSelected(id); setContext(false); setView("sessions"); setPane("thread"); }}
               query={query} onQuery={setQuery} view={view} onView={(v) => { setView(v); setPane("thread"); }}
               showArchived={archived} onToggleArchived={() => setArchived((v) => !v)} />
      {view === "hooks" ? (
        <HooksPage onBack={() => setPane("list")} />
      ) : view === "projects" ? (
        <ProjectsView
          projects={projects} rows={rows}
          onOpen={(id) => { setSelected(id); setView("sessions"); }}
          onBack={() => setPane("list")}
          onAssign={(id, p) => act(() => api.assign(id, p))}
          onCreate={(name) => act(() => api.newProject(name))}
          onRename={(id, name) => act(() => api.renameProject(id, name))}
          onDelete={(id) => act(() => api.deleteProject(id))} />
      ) : row && context ? (
        <ContextPage session={row.id} onBack={() => setContext(false)} />
      ) : row ? (
        <Thread row={row} lines={lines} stream={stream} projects={projects} busy={busy} onBack={() => setPane("list")}
          onSend={(t) => act(() => api.prompt(row.id, t))}
          onAnswer={(t) => act(() => api.answer(row.id, t))}
          onInterrupt={() => act(() => api.interrupt(row.id))}
          onArchive={() => act(() => (row.archived ? api.unarchive(row.id) : api.archive(row.id)))}
          onRename={(t) => act(() => api.rename(row.id, t))}
          onModel={(m) => act(() => api.model(row.id, m))}
          onEffort={(e) => act(() => api.effort(row.id, e))}
          onAssign={(p) => act(() => api.assign(row.id, p))}
          onContext={() => setContext(true)} />
      ) : (
        <div className="thread empty">
          <div>
            <h1>No session open</h1>
            <p>Pick a session on the left to watch it, steer it, or answer what it is waiting on.</p>
          </div>
        </div>
      )}
      {err && <div className="toast" role="status">{err}</div>}
    </div>
  );
}
