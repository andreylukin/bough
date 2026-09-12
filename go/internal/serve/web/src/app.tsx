import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api, subscribe } from "./api";
import type { Event, Line, Row } from "./types";
import { StatusMark } from "./status";

const POLL_MS = 4000; // statuses of sessions we are not streaming still move

/** Sessions group by when they were last active, newest bucket first. */
function bucket(iso: string): string {
  const d = new Date(iso);
  const now = new Date();
  const day = (x: Date) => new Date(x.getFullYear(), x.getMonth(), x.getDate()).getTime();
  const days = Math.round((day(now) - day(d)) / 86_400_000);
  if (days <= 0) return "Today";
  if (days === 1) return "Yesterday";
  if (days <= 7) return "Earlier this week";
  return "Older";
}

function clock(iso: string): string {
  return new Date(iso).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}

function Brand() {
  return (
    <div style={{ display: "flex", alignItems: "center", gap: 10, padding: "18px 20px 14px" }}>
      <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="var(--accent)" strokeWidth="1.5" strokeLinecap="round" aria-hidden="true">
        <path d="M12 21V9" />
        <path d="M12 9c0-3 2-5 5-5 0 3-2 5-5 5z" />
        <path d="M12 13c0-2.5-1.8-4.5-4.5-4.5 0 2.5 2 4.5 4.5 4.5z" />
      </svg>
      <span style={{ fontSize: 15, fontWeight: 600, letterSpacing: "-0.01em" }}>bough</span>
    </div>
  );
}

function Sidebar({
  rows, selected, onSelect, query, onQuery, showArchived, onToggleArchived,
}: {
  rows: Row[]; selected: string | null; onSelect: (id: string) => void;
  query: string; onQuery: (q: string) => void;
  showArchived: boolean; onToggleArchived: () => void;
}) {
  const groups = useMemo(() => {
    const out = new Map<string, Row[]>();
    for (const r of rows) {
      const k = bucket(r.modified);
      (out.get(k) ?? out.set(k, []).get(k)!).push(r);
    }
    return [...out.entries()];
  }, [rows]);

  return (
    <div className="sidebar">
      <Brand />
      <div style={{ padding: "0 20px 18px" }}>
        <label htmlFor="q" className="field-label">Search sessions</label>
        <input id="q" className="field" value={query} placeholder="Title, repo or branch"
               onChange={(e) => onQuery(e.target.value)} />
      </div>

      <div className="scroll" style={{ display: "flex", flexDirection: "column", gap: 22 }}>
        {groups.length === 0 && (
          <p style={{ padding: "0 20px", color: "var(--text-3)", fontSize: 13 }}>
            {query ? `No sessions match “${query}”.` : "No sessions yet. Start one below."}
          </p>
        )}
        {groups.map(([name, list]) => (
          <div key={name} style={{ display: "flex", flexDirection: "column", gap: 2 }}>
            <div className="group-head">{name}</div>
            {list.map((r) => (
              <button key={r.id} onClick={() => onSelect(r.id)}
                      className={"row" + (r.id === selected ? " row-on" : "")}
                      aria-current={r.id === selected ? "true" : undefined}>
                <span className="row-title">{r.title || "Untitled session"}</span>
                <span style={{ display: "flex", alignItems: "center", gap: 8 }}>
                  <StatusMark status={r.status} />
                  <span className="num" style={{ marginLeft: "auto", fontSize: 12, color: "var(--text-3)" }}>
                    {clock(r.modified)}
                  </span>
                </span>
                {(r.repo || r.branch) && (
                  <span className="mono" style={{ fontSize: 12, color: "var(--text-3)" }}>
                    {r.repo?.split("/").pop()}
                    {r.branch && <span style={{ color: "var(--line-strong)" }}>/</span>}
                    {r.branch}
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

/** One transcript entry. Tool output is quieter than what was said. */
function Entry({ line }: { line: Line }) {
  const k = line.kind;
  if (k === "meta" || k === "title") return null;

  if (k === "input") {
    return (
      <div style={{ display: "flex", gap: 12, maxWidth: "74ch" }}>
        <span className="mono" style={{ color: "var(--accent)", flexShrink: 0 }}>&gt;</span>
        <p style={{ margin: 0, textWrap: "pretty" }}>{line.text}</p>
      </div>
    );
  }
  if (k === "assistant" || k === "sub:assistant") {
    return (
      <div style={{ display: "flex", flexDirection: "column", gap: 10, maxWidth: "74ch" }}>
        <div style={{ display: "flex", alignItems: "center", gap: 9 }}>
          <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="var(--accent)" strokeWidth="1.5" aria-hidden="true">
            <circle cx="12" cy="12" r="4.5" />
          </svg>
          <span style={{ fontSize: 13, fontWeight: 500 }}>{k === "assistant" ? "bough" : "subagent"}</span>
        </div>
        <p style={{ margin: 0, paddingLeft: 24, color: "var(--text-2)", whiteSpace: "pre-wrap", textWrap: "pretty" }}>
          {line.text}
        </p>
      </div>
    );
  }
  if (k === "code" || k === "sub:code" || k === "result" || k === "sub:result") {
    const isCode = k.endsWith("code");
    return (
      <details className="block" open={isCode}>
        <summary>
          <span style={{ fontSize: 13, color: "var(--text-2)" }}>{isCode ? "Code" : "Result"}</span>
          <span className="num" style={{ marginLeft: "auto", fontSize: 12, color: "var(--text-3)" }}>
            {line.text.split("\n").length} lines
          </span>
        </summary>
        <pre className="mono">{line.text}</pre>
      </details>
    );
  }
  if (k === "thinking") {
    return (
      <details className="block">
        <summary><span style={{ fontSize: 13, color: "var(--text-3)" }}>Thinking</span></summary>
        <pre className="mono" style={{ color: "var(--text-3)" }}>{line.text}</pre>
      </details>
    );
  }
  if (k === "error" || k === "sub:error") {
    return (
      <div className="block" style={{ padding: "12px 14px", color: "var(--red)", whiteSpace: "pre-wrap" }}>
        {line.text}
      </div>
    );
  }
  if (k === "done") return <div className="rule" />;
  return (
    <div style={{ fontSize: 12, color: "var(--text-3)" }}>
      {k}
      {line.text ? ` — ${line.text}` : ""}
    </div>
  );
}

function Thread({ row, lines, onSend, onAnswer, onInterrupt, onArchive, onRename, busy }: {
  row: Row; lines: Line[];
  onSend: (t: string) => void; onAnswer: (t: string) => void;
  onInterrupt: () => void; onArchive: () => void; onRename: (t: string) => void;
  busy: boolean;
}) {
  const [draft, setDraft] = useState("");
  const end = useRef<HTMLDivElement>(null);
  useEffect(() => { end.current?.scrollIntoView({ block: "end" }); }, [lines.length]);

  const send = () => {
    const t = draft.trim();
    if (!t) return;
    setDraft("");
    if (row.ask) onAnswer(t); else onSend(t);
  };

  return (
    <div className="thread">
      <header className="thread-head">
        <h1 title={row.title}>{row.title || "Untitled session"}</h1>
        {(row.repo || row.branch) && (
          <span className="mono" style={{ fontSize: 13, color: "var(--text-3)" }}>
            {row.repo?.split("/").pop()}
            {row.branch && <span style={{ color: "var(--line-strong)" }}>/</span>}
            {row.branch}
          </span>
        )}
        <StatusMark status={row.status} />
        <div style={{ marginLeft: "auto", display: "flex", gap: 10 }}>
          <button className="btn" onClick={() => {
            const t = prompt("Rename session", row.title);
            if (t !== null) onRename(t);
          }}>Rename</button>
          <button className="btn" onClick={onArchive}>
            {row.archived ? "Unarchive" : "Archive"}
          </button>
        </div>
      </header>

      <div className="scroll transcript">
        {lines.map((l) => <Entry key={l.seq} line={l} />)}

        {row.ask && (
          <div className="ask">
            <div style={{ display: "flex", alignItems: "center", gap: 9 }}>
              <StatusMark status="needs-you" size={16} />
            </div>
            <p style={{ margin: 0, fontSize: 15, fontWeight: 500 }}>{row.ask.text}</p>
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
          <textarea
            id="composer" value={draft} rows={2}
            placeholder={
              row.ask
                ? "Answer the question above"
                : row.status === "running"
                  ? "Send a message — it steers the turn already running"
                  : "Send a message to start the next turn"
            }
            onChange={(e) => setDraft(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); send(); }
            }}
          />
          <div style={{ display: "flex", alignItems: "center", gap: 10 }}>
            <span className="hint">Return to send</span>
            <span className="hint">Shift + Return for a newline</span>
            <div style={{ marginLeft: "auto", display: "flex", gap: 10 }}>
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

  const refresh = useCallback(async () => {
    try {
      setRows(await api.sessions(archived));
      setErr(null);
    } catch (e) {
      setErr(e instanceof Error ? e.message : String(e));
    }
  }, [archived]);

  useEffect(() => {
    refresh();
    const t = setInterval(refresh, POLL_MS);
    return () => clearInterval(t);
  }, [refresh]);

  // Open a session: load its transcript, then stream everything after it.
  useEffect(() => {
    if (!selected) return;
    let live = true;
    api.session(selected).then((r) => {
      if (!live) return;
      setLines(r.entries);
      setRows((prev) => prev.map((x) => (x.id === r.session.id ? r.session : x)));
    }).catch((e) => setErr(String(e)));

    // An event is a CHANGE SIGNAL, not a transcript line. Event.Seq is
    // a supervisor counter that starts at 1 per session; transcript
    // entries carry history seqs. Merging the two silently dropped
    // every event whose counter collided with an existing line. So the
    // stream only says "something happened" and the transcript
    // endpoint — one id space, server-authoritative — says what.
    let timer: ReturnType<typeof setTimeout> | undefined;
    const stop = subscribe(selected, () => {
      clearTimeout(timer);
      timer = setTimeout(() => {
        setLines((prev) => {
          const since = prev.length ? prev[prev.length - 1].seq : 0;
          api.session(selected, since).then((r) => {
            if (!live || r.entries.length === 0) return;
            setLines((cur) => {
              const seen = new Set(cur.map((l) => l.seq));
              return [...cur, ...r.entries.filter((e) => !seen.has(e.seq))];
            });
            setRows((rs) => rs.map((x) => (x.id === r.session.id ? r.session : x)));
          }).catch(() => { /* a dropped catch-up retries on the next event */ });
          return prev;
        });
      }, 120); // coalesce a burst of deltas into one catch-up
    });
    return () => { live = false; clearTimeout(timer); stop(); };
  }, [selected, refresh]);

  const visible = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return rows;
    return rows.filter((r) =>
      [r.title, r.repo, r.branch, r.cwd].some((v) => v?.toLowerCase().includes(q)));
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
      <Sidebar rows={visible} selected={selected} onSelect={setSelected}
               query={query} onQuery={setQuery}
               showArchived={archived} onToggleArchived={() => setArchived((v) => !v)} />
      {row ? (
        <Thread
          row={row} lines={lines} busy={busy}
          onSend={(t) => act(() => api.prompt(row.id, t))}
          onAnswer={(t) => act(() => api.answer(row.id, t))}
          onInterrupt={() => act(() => api.interrupt(row.id))}
          onArchive={() => act(() => (row.archived ? api.unarchive(row.id) : api.archive(row.id)))}
          onRename={(t) => act(() => api.rename(row.id, t))}
        />
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
