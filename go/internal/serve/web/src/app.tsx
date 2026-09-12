import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api, subscribe } from "./api";
import type { Line, Project, Row } from "./types";
import { StatusMark } from "./status";
import { ProjectsView } from "./projects";
import { Markdown, codeLabel, doneSummary, groupTurns, isQuiet, plainTitle, stripRunFences, type Turn } from "./render";
import { SkillPicker } from "./skills";

type View = "sessions" | "projects";

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

function Sprout({ size = 18 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="var(--accent)"
         strokeWidth="1.5" strokeLinecap="round" aria-hidden="true">
      <path d="M12 21V9" /><path d="M12 9c0-3 2-5 5-5 0 3-2 5-5 5z" />
      <path d="M12 13c0-2.5-1.8-4.5-4.5-4.5 0 2.5 2 4.5 4.5 4.5z" />
    </svg>
  );
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
      </nav>
      <div style={{ padding: "0 20px 18px" }}>
        <label htmlFor="q" className="field-label">Search sessions</label>
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
                <span className="row-title">{plainTitle(r.title) || "Untitled session"}</span>
                <span style={{ display: "flex", alignItems: "center", gap: 8 }}>
                  <StatusMark status={r.status} />
                  <span className="num" style={{ marginLeft: "auto", fontSize: 12, color: "var(--text-3)" }}>
                    {clock(r.modified)}
                  </span>
                </span>
                {(r.repo || r.branch) && (
                  <span className="mono" style={{ fontSize: 12, color: "var(--text-3)" }}>
                    {r.repo?.split("/").pop()}
                    {r.branch && <span style={{ color: "var(--line-strong)" }}>/</span>}{r.branch}
                  </span>
                )}
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

function CodeBlock({ line }: { line: Line }) {
  const { label, detail } = codeLabel(line.text);
  const lines = line.text.split("\n").length;
  return (
    <details className="block" open>
      <summary>
        <span className="block-label">{label}</span>
        {detail && <span className="mono block-detail">{detail}</span>}
        <span className="num block-lines">{lines} lines</span>
      </summary>
      <pre className="mono">{line.text}</pre>
    </details>
  );
}

function ResultBlock({ line }: { line: Line }) {
  // history.EntryText prepends a result's own code to its text (the
  // command is as memorable as its output). Here the code already has
  // its own block directly above, so showing it again doubles every
  // result. data.code is that prefix.
  const code = typeof line.data?.code === "string" ? (line.data.code as string) : "";
  const body = code && line.text.startsWith(code) ? line.text.slice(code.length).trimStart() : line.text;
  const lines = (body || "(no output)").split("\n");
  const head = lines.find((l) => l.trim()) ?? "";
  return (
    <details className="block">
      <summary>
        <span className="block-label">Result</span>
        <span className="mono block-detail">{head.slice(0, 90)}</span>
        <span className="num block-lines">{lines.length} lines</span>
      </summary>
      <pre className="mono">{body || "(no output)"}</pre>
    </details>
  );
}

function Entry({ line, codes }: { line: Line; codes: string[] }) {
  const k = line.kind;
  if (k === "assistant" || k === "sub:assistant") {
    const body = stripRunFences(line.text, codes);
    if (!body) return null; // the reply was only the program it ran
    return (
      <div className="say">
        <div className="say-who">{k === "assistant" ? <Sprout size={14} /> : <span className="sub-dot" />}
          <span>{k === "assistant" ? "bough" : "subagent"}</span></div>
        <Markdown text={body} />
      </div>
    );
  }
  if (k === "code" || k === "sub:code") return <CodeBlock line={line} />;
  if (k === "result" || k === "sub:result") return <ResultBlock line={line} />;
  if (k === "thinking") {
    return (
      <details className="block thinking">
        <summary><span className="block-label">Thinking</span></summary>
        <pre className="mono">{line.text}</pre>
      </details>
    );
  }
  if (k === "error" || k === "sub:error") return <div className="err">{line.text}</div>;
  if (k === "ask") return null; // the live ask renders as its own card below
  if (isQuiet(k)) return <div className="meta-line">{line.text || k}</div>;
  return <div className="meta-line">{line.text || k}</div>;
}

function TurnView({ turn }: { turn: Turn }) {
  const summary = turn.done ? doneSummary(turn.done) : "";
  const codes = turn.body.filter((l) => l.kind === "code" || l.kind === "sub:code").map((l) => l.text);
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
        {turn.body.map((l) => <Entry key={l.seq} line={l} codes={codes} />)}
      </div>
      {turn.done && (
        <div className="turn-done">
          {turn.done.kind === "cancelled" ? "Stopped" : summary || "Finished"}
        </div>
      )}
    </section>
  );
}

/* ---------------- model + effort ---------------- */

interface ModelInfo { id: string; context?: number; efforts?: string[]; input?: number; output?: number }
interface ProviderInfo { plugin: string; models?: ModelInfo[] }

function Controls({ row, projects, onModel, onEffort, onAssign }: {
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
          <option value="">{row.model ? row.model : "as configured"}</option>
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

function Thread({ row, lines, projects, onSend, onAnswer, onInterrupt, onArchive, onRename, onModel, onEffort, onAssign, busy }: {
  row: Row; lines: Line[]; projects: Project[]; busy: boolean;
  onSend: (t: string) => void; onAnswer: (t: string) => void; onInterrupt: () => void;
  onArchive: () => void; onRename: (t: string) => void;
  onModel: (m: string) => void; onEffort: (e: string) => void; onAssign: (p: string) => void;
}) {
  const [draft, setDraft] = useState("");
  const end = useRef<HTMLDivElement>(null);
  useEffect(() => { end.current?.scrollIntoView({ block: "end" }); }, [lines.length]);
  const turns = useMemo(() => groupTurns(lines), [lines]);

  const send = () => {
    const t = draft.trim();
    if (!t) return;
    setDraft("");
    if (row.ask) onAnswer(t); else onSend(t);
  };

  return (
    <div className="thread">
      <header className="thread-head">
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
        <div className="head-side">
          <Controls row={row} projects={projects} onModel={onModel} onEffort={onEffort} onAssign={onAssign} />
          <button className="btn" onClick={() => {
            const t = prompt("Rename session", plainTitle(row.title));
            if (t !== null) onRename(t);
          }}>Rename</button>
          <button className="btn" onClick={onArchive}>{row.archived ? "Unarchive" : "Archive"}</button>
        </div>
      </header>

      <div className="scroll transcript">
        {turns.map((t) => <TurnView key={t.seq} turn={t} />)}
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
          <textarea id="composer" value={draft} rows={2}
            placeholder={row.ask ? "Answer the question above"
              : row.status === "running" ? "Send a message — it steers the turn already running"
              : "Send a message to start the next turn"}
            onChange={(e) => setDraft(e.target.value)}
            onKeyDown={(e) => { if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); send(); } }} />
          <div style={{ display: "flex", alignItems: "center", gap: 14 }}>
            <span className="hint">Return to send</span>
            <span className="hint">Shift + Return for a newline</span>
            <div style={{ marginLeft: "auto", display: "flex", gap: 10 }}>
              <SkillPicker onPick={(name) => {
                setDraft((d) => (d.trimStart().startsWith("/") ? d : `/${name} ${d.trimStart()}`));
                document.getElementById("composer")?.focus();
              }} />
              {row.status === "running" && <button className="btn" onClick={onInterrupt}>Stop</button>}
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
  const [query, setQuery] = useState("");
  const [archived, setArchived] = useState(false);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [view, setView] = useState<View>("sessions");
  const [projects, setProjects] = useState<Project[]>([]);

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

    // An event is a CHANGE SIGNAL, not a transcript line: Event.Seq is a
    // supervisor counter, transcript entries carry history seqs, and
    // merging the two silently drops events whose numbers collide.
    let timer: ReturnType<typeof setTimeout> | undefined;
    const stop = subscribe(selected, () => {
      clearTimeout(timer);
      timer = setTimeout(() => {
        setLines((prev) => {
          const since = prev.length ? prev[prev.length - 1].seq : 0;
          api.session(selected, since).then((r) => {
            if (!live) return;
            setRows((rs) => rs.map((x) => (x.id === r.session.id ? r.session : x)));
            if (!r.entries.length) return;
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

  const act = async (fn: () => Promise<unknown>) => {
    setBusy(true);
    try { await fn(); setErr(null); }
    catch (e) { setErr(e instanceof Error ? e.message : String(e)); }
    finally { setBusy(false); await refresh(); }
  };

  return (
    <div className="app">
      <Sidebar rows={visible} selected={selected} onSelect={(id) => { setSelected(id); setView("sessions"); }}
               query={query} onQuery={setQuery} view={view} onView={setView}
               showArchived={archived} onToggleArchived={() => setArchived((v) => !v)} />
      {view === "projects" ? (
        <ProjectsView
          projects={projects} rows={rows}
          onOpen={(id) => { setSelected(id); setView("sessions"); }}
          onAssign={(id, p) => act(() => api.assign(id, p))}
          onCreate={(name) => act(() => api.newProject(name))}
          onRename={(id, name) => act(() => api.renameProject(id, name))}
          onDelete={(id) => act(() => api.deleteProject(id))} />
      ) : row ? (
        <Thread row={row} lines={lines} projects={projects} busy={busy}
          onSend={(t) => act(() => api.prompt(row.id, t))}
          onAnswer={(t) => act(() => api.answer(row.id, t))}
          onInterrupt={() => act(() => api.interrupt(row.id))}
          onArchive={() => act(() => (row.archived ? api.unarchive(row.id) : api.archive(row.id)))}
          onRename={(t) => act(() => api.rename(row.id, t))}
          onModel={(m) => act(() => api.model(row.id, m))}
          onEffort={(e) => act(() => api.effort(row.id, e))}
          onAssign={(p) => act(() => api.assign(row.id, p))} />
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
