import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { api, subscribe, type Change, type TurnLine } from "./api";
import type { Line, Project, Row } from "./types";
import { STATUS, StatusMark, Working } from "./status";
import { ProjectsView } from "./projects";
import { Select, type Option } from "./select";
import { DialogHost, askText } from "./dialog";
import { Markdown, codeLabel, groupSubs, groupTools, groupTurns, isHookLine, isQuiet, untitled, blank, sessionUsage, usageOf, tokenCount, money, duration, plainTitle, stepCount, stripRunFences, type Item, type SubAgent, type Turn, lineCount } from "./render";
import { Code, parseCall, langForPath } from "./code";
import { SkillPicker } from "./skills";
import { Mentions, triggerAt, type Trigger } from "./mention";
import { HooksPage } from "./hooks";
import { ContextPage } from "./context";
import { Palette, usePaletteKey, type Command } from "./palette";
import { WikiPage, parseWikiHash, wikiApi, wikiHash, type WikiRoute } from "./wiki";

export type View = "sessions" | "projects" | "hooks" | "wiki";

const POLL_MS = 4000; // sessions we are not streaming still change status

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

/** "8m", "3h", "2d": how long ago, as a sidebar reads it. */
function ago(iso: string): string {
  const s = Math.max(0, (Date.now() - Date.parse(iso)) / 1000);
  if (s < 60) return "<1m";
  if (s < 3600) return `${Math.floor(s / 60)}m`;
  if (s < 86400) return `${Math.floor(s / 3600)}h`;
  return `${Math.floor(s / 86400)}d`;
}

/** ⌘ on a Mac, Ctrl everywhere else. */
function modKey(): string {
  return typeof navigator !== "undefined" && /Mac|iP/.test(navigator.platform) ? "\u2318" : "Ctrl+";
}

const capital = (s: string) => s.charAt(0).toUpperCase() + s.slice(1);

const readSet = (key: string): Set<string> => {
  try { return new Set(JSON.parse(localStorage.getItem(key) ?? "[]")); } catch { return new Set(); }
};
const writeSet = (key: string, s: Set<string>) => {
  try { localStorage.setItem(key, JSON.stringify([...s].slice(-200))); } catch { /* storage off */ }
};

/** Sessions untouched this long leave their workspace for the Inactive section. */
const INACTIVE_MS = 72 * 3_600_000;

/** Where a session ran, as the sidebar names it: the repo, else the folder. */
function workspaceOf(r: Row): string {
  return r.repo?.split("/").filter(Boolean).pop() || r.cwd.split("/").filter(Boolean).pop() || "/";
}

/** What needs you, then what is moving, then the most recent. */
function byUrgency(a: Row, b: Row): number {
  const rank = (r: Row) => (r.status === "needs-you" || r.trouble ? 0 : r.status === "running" ? 1 : 2);
  return rank(a) - rank(b) || Date.parse(b.lastAt) - Date.parse(a.lastAt);
}

/** Rows under their workspace, workspaces by their latest activity. */
function byWorkspace(rows: Row[]): [string, Row[]][] {
  const out = new Map<string, Row[]>();
  for (const r of rows) {
    const k = workspaceOf(r);
    if (!out.has(k)) out.set(k, []);
    out.get(k)!.push(r);
  }
  const latest = (list: Row[]) => Math.max(...list.map((r) => Date.parse(r.lastAt)));
  return [...out.entries()]
    .map(([k, list]) => [k, list.sort(byUrgency)] as [string, Row[]])
    .sort((a, b) => latest(b[1]) - latest(a[1]));
}

/** A 24-box stroked icon in currentColor, the status glyphs' idiom. */
function Icon({ d, size = 18 }: { d: React.ReactNode; size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.6"
         strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">{d}</svg>
  );
}
const ICONS = {
  panel: <><rect x="3.5" y="4.5" width="17" height="15" rx="2" /><path d="M9.5 4.5v15" /></>,
  back: <path d="M19 12H5m6-6l-6 6 6 6" />,
  forward: <path d="M5 12h14m-6-6l6 6-6 6" />,
  search: <><circle cx="11" cy="11" r="6.5" /><path d="M20 20l-4.3-4.3" /></>,
  compose: <><path d="M12 4H6a2 2 0 0 0-2 2v12a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2v-6" /><path d="M17.5 3.5a2.1 2.1 0 0 1 3 3L12 15l-4 1 1-4z" /></>,
  folder: <path d="M3.5 7a1.5 1.5 0 0 1 1.5-1.5h4l2 2h8a1.5 1.5 0 0 1 1.5 1.5v8.5a1.5 1.5 0 0 1-1.5 1.5H5A1.5 1.5 0 0 1 3.5 17.5z" />,
  projects: <><rect x="3.5" y="7.5" width="13" height="12" rx="1.5" /><path d="M7.5 4.5h11a2 2 0 0 1 2 2v9" /></>,
  hooks: <path d="M13 3L5 13.5h6L10 21l8-10.5h-6z" />,
  wiki: <><path d="M4.5 5.5A1.5 1.5 0 0 1 6 4h13.5v14H6a1.5 1.5 0 0 0-1.5 1.5z" /><path d="M4.5 19.5A1.5 1.5 0 0 0 6 21h13.5v-3" /><path d="M9 8.5h6" /></>,
  chevron: <path d="M9 6l6 6-6 6" />,
};

export function Sidebar({ rows, selected, onSelect, onTurn, query, onQuery, showArchived, onToggleArchived, view = "sessions", onView, wikiFlags = 0, onNew, onAck, active = true }: {
  rows: Row[]; selected: string | null; onSelect: (id: string) => void;
  /** Open a session at one turn of its log. */
  onTurn?: (id: string, turn: number) => void;
  /** showArchived is the Archived section being open: the rows then include archived ones. */
  query: string; onQuery: (q: string) => void; showArchived: boolean; onToggleArchived: () => void;
  view?: View; onView?: (v: View) => void;
  /** Claims the wiki's review is waiting on; shown beside the nav item. */
  wikiFlags?: number;
  /** Starting work is the other half of a control room; it opens the palette's Start group. */
  onNew?: () => void;
  /** Mark a troubled session seen without opening it. */
  onAck?: (id: string) => void;
  /** Whether the list is on screen; coming back to it puts focus on the row you left. */
  active?: boolean;
}) {
  // Status lives in the glyphs and the order; the sections are only
  // where a session ran, and whether it is still recent.
  const { recent, inactive, archived } = useMemo(() => {
    const now = Date.now();
    const recent: Row[] = [], inactive: Row[] = [], archived: Row[] = [];
    for (const r of rows) {
      if (r.archived) archived.push(r);
      else if (r.status === "needs-you" || r.status === "running" || r.trouble || now - Date.parse(r.lastAt) < INACTIVE_MS) recent.push(r);
      else inactive.push(r);
    }
    return { recent: byWorkspace(recent), inactive, archived };
  }, [rows]);

  // Inactive stays shut until asked, and the way you left it across reloads.
  const [unfolded, setUnfolded] = useState<Set<string>>(() => readSet("bough:unfolded"));
  const toggleFold = (name: string) => setUnfolded((cur) => {
    const next = new Set(cur);
    if (next.has(name)) next.delete(name); else next.add(name);
    writeSet("bough:unfolded", next);
    return next;
  });

  // On a desktop the whole sidebar folds away to a rail, and stays folded.
  const [closed, setClosed] = useState(() => { try { return localStorage.getItem("bough:side-closed") === "1"; } catch { return false; } });
  const setSide = (c: boolean) => {
    setClosed(c);
    try { localStorage.setItem("bough:side-closed", c ? "1" : "0"); } catch { /* storage off */ }
  };

  // Search sits behind the toolbar; a filter in force keeps it open.
  const [searching, setSearching] = useState(false);
  const searchRef = useRef<HTMLInputElement>(null);
  const showSearch = searching || Boolean(query);
  useEffect(() => { if (searching) searchRef.current?.focus(); }, [searching]);

  // A session's turn log, one line a turn, opens under its row. The open
  // session shows its own; the rest stay shut until asked, and stay the
  // way you left them across reloads.
  const [expanded, setExpanded] = useState<Set<string>>(() => readSet("bough:turns-open"));
  const setOpen = useCallback((id: string, open: boolean) => setExpanded((cur) => {
    if (cur.has(id) === open) return cur;
    const next = new Set(cur);
    if (open) next.add(id); else next.delete(id);
    writeSet("bough:turns-open", next);
    return next;
  }), []);
  useEffect(() => { if (selected) setOpen(selected, true); }, [selected, setOpen]);

  // Fetched on first open, kept per session, and read again once the
  // session has changed since.
  const [logs, setLogs] = useState<Record<string, { at: string; lines: TurnLine[] }>>({});
  const fetching = useRef(new Set<string>());
  useEffect(() => {
    for (const r of rows) {
      if (!r.turns || !expanded.has(r.id) || logs[r.id]?.at === r.modified || fetching.current.has(r.id)) continue;
      const at = r.modified;
      fetching.current.add(r.id);
      api.turns(r.id)
        .then((lines) => setLogs((m) => ({ ...m, [r.id]: { at, lines } })))
        .catch(() => {})
        .finally(() => fetching.current.delete(r.id));
    }
  }, [rows, expanded, logs]);

  // What a session is about, on hover or focus: the title names it, the
  // summary says where it stands. One card for the whole list, fixed to
  // the viewport beside the row, because the list scrolls and would clip
  // anything hung off a row. It waits a beat so skimming does not flash it.
  const [card, setCard] = useState<{ id: string; text: string; top: number; left: number } | null>(null);
  const peekTimer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  const quiet = useRef(false);
  const peek = (r: Row, el: HTMLElement) => {
    clearTimeout(peekTimer.current);
    if (!r.summary || quiet.current) { setCard(null); return; }
    const rect = el.getBoundingClientRect();
    // Beside the row, so it never covers the turn log under it.
    peekTimer.current = setTimeout(() => setCard({
      id: r.id, text: r.summary!,
      top: Math.max(8, Math.min(rect.top, window.innerHeight - 200)),
      left: Math.min(rect.right + 8, window.innerWidth - 332),
    }), 450);
  };
  const unpeek = () => { clearTimeout(peekTimer.current); setCard(null); };
  useEffect(() => () => clearTimeout(peekTimer.current), []);

  // Back on the list, the row you left is where you are: in view, and
  // focused so the arrows carry on from it. Not on every live update.
  useEffect(() => {
    if (!active) { unpeek(); return; }
    const el = document.querySelector<HTMLElement>(".sidebar .row-on");
    if (!el) return;
    el.scrollIntoView({ block: "nearest" });
    quiet.current = true;
    el.focus({ preventScroll: true });
    quiet.current = false;
  }, [active, selected]);

  // The tree by keyboard: up and down walk what is visible, right opens
  // a section or a log (or steps into it), left shuts it (or steps out);
  // Enter opens a session or jumps to a turn.
  const walk = (e: React.KeyboardEvent<HTMLDivElement>) => {
    if (!["ArrowDown", "ArrowUp", "ArrowLeft", "ArrowRight"].includes(e.key)) return;
    const items = [...e.currentTarget.querySelectorAll<HTMLElement>("button.sec-fold, button.row, button.turn-line")];
    if (!items.length) return;
    const at = document.activeElement as HTMLElement | null;
    const cur = at?.closest(".row-wrap")?.querySelector<HTMLElement>("button.row") ?? at;
    const go = (el?: HTMLElement | null) => { if (el) { el.focus(); el.scrollIntoView({ block: "nearest" }); } };
    e.preventDefault();
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      const i = cur ? items.indexOf(cur) : -1;
      go(items[i < 0 ? (e.key === "ArrowDown" ? 0 : items.length - 1)
        : Math.max(0, Math.min(items.length - 1, i + (e.key === "ArrowDown" ? 1 : -1)))]);
      return;
    }
    if (!cur) return;
    const right = e.key === "ArrowRight";
    if (cur.classList.contains("sec-fold")) {
      if (cur.getAttribute("aria-expanded") === String(!right)) cur.click();
      return;
    }
    const session = cur.closest<HTMLElement>(".session");
    const twist = session?.querySelector<HTMLElement>(".row-twist");
    if (cur.classList.contains("turn-line")) {
      if (!right) go(session?.querySelector<HTMLElement>("button.row"));
      return;
    }
    const id = cur.dataset.id!;
    const open = twist?.getAttribute("aria-expanded") === "true";
    if (right) {
      if (twist && !open) setOpen(id, true);
      else if (open) go(session?.querySelector<HTMLElement>("button.turn-line"));
    } else if (open) setOpen(id, false);
    else go(cur.closest(".sec")?.querySelector<HTMLElement>("button.sec-fold"));
  };

  const session = (r: Row) => {
    const open = Boolean(r.turns) && expanded.has(r.id);
    const log = logs[r.id]?.lines;
    const name = plainTitle(r.title);
    const on = r.id === selected;
    const why = r.trouble ? capital(r.trouble) : STATUS[r.status]?.label ?? r.status;
    return (
      <div key={r.id} className="session">
        <div className={"row-wrap" + (open ? " row-open" : "")}>
          <button onClick={() => onSelect(r.id)} data-id={r.id}
                  onMouseEnter={(e) => peek(r, e.currentTarget)} onMouseLeave={unpeek}
                  onFocus={(e) => peek(r, e.currentTarget)} onBlur={unpeek}
                  aria-describedby={card?.id === r.id ? "row-card" : undefined}
                  className={"row" + (on ? " row-on" : "") + (r.turns ? " row-has-log" : "") + (r.trouble && onAck ? " row-has-ack" : "")}
                  aria-current={on ? "true" : undefined}
                  title={`${why} · ${ago(r.lastAt)} ago${r.branch ? ` · ${r.branch}` : ""}`}>
            {/* A failure you have not seen is a red mark; the reason is its label. */}
            <span className="row-mark">
              {r.trouble
                ? <span className="status"><svg width={16} height={16} viewBox="0 0 24 24" fill="none" stroke="var(--red)" strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">{STATUS.error.glyph}</svg><span className="visually-hidden">{why}</span></span>
                : <StatusMark status={r.status} size={16} bare />}
            </span>
            {r.ask && name && askSaysTitle(r.ask.text, name)
              ? <span className="row-title" title={r.ask.text}>{plainTitle(r.ask.text)}</span>
              : name
              ? <span className="row-title">{name}</span>
              // No title: the id tail alone tells rows apart.
              : <span className="row-title mono row-untitled">{r.id.slice(-6)}</span>}
            {r.jobs && r.jobs.length > 0 && (
              <span className="num row-jobs" title={r.jobs.map((j) => j.cmd).join("\n")}>
                {r.jobs.length}<span className="visually-hidden"> {r.jobs.length === 1 ? "job" : "jobs"}</span>
              </span>
            )}
          </button>
          {/* The disclosure and Seen are siblings of the row, not inside
              it: a button in a button is invalid and would open it too. */}
          {r.trouble && onAck && (
            <button className="btn row-ack" onClick={() => onAck(r.id)} aria-label={`Mark ${name || "session"} seen`}>Seen</button>
          )}
          {r.turns ? (
            <button className="row-twist" tabIndex={-1} aria-expanded={open} aria-controls={`turns-${r.id}`}
                    aria-label={`${open ? "Hide" : "Show"} turn log of ${name || "session"}`}
                    onClick={() => setOpen(r.id, !open)}><Icon d={ICONS.chevron} size={14} /></button>
          ) : null}
        </div>
        {open && (
          <ol id={`turns-${r.id}`} className="turns" aria-label={`Turns of ${name || "session"}`}>
            {log ? log.map((l) => (
              <li key={l.turn}>
                <button className="turn-line" title={l.text} onClick={() => onTurn?.(r.id, l.turn)}>
                  <span className="num turn-n">{l.turn}</span><span className="turn-text">{l.text.replace(/^you (asked|said|wanted)( to| for| that)?\s+/i, "").replace(/^./, (c) => c.toUpperCase())}</span>
                </button>
              </li>
            )) : <li className="turn-wait">Loading…</li>}
          </ol>
        )}
      </div>
    );
  };

  const workspaces = (groups: [string, Row[]][]) => groups.map(([ws, list]) => (
    <div key={ws} className="ws">
      <div className="ws-head"><Icon d={ICONS.folder} size={15} /><span className="ws-name">{ws}</span><span className="ws-rule" /></div>
      {list.map(session)}
    </div>
  ));

  // A section folds; a search shows every match, folded or not.
  const section = (key: string, label: string, open: boolean, toggle: () => void, count: number | null, body: React.ReactNode) => (
    <div className="sec">
      <button className="sec-fold" aria-expanded={open} onClick={toggle} aria-controls={`sec-${key}`}>
        <span>{label}</span><span className="ws-rule" />{count !== null && <span className="num sec-count">{count}</span>}
      </button>
      {open && <div className="sec-body" id={`sec-${key}`}>{body}</div>}
    </div>
  );

  const toolbar = (
    <div className="side-bar">
      <button className="side-icon side-collapse" onClick={() => setSide(!closed)} aria-expanded={!closed}
              aria-label={closed ? "Show sidebar" : "Hide sidebar"} title={closed ? "Show sidebar" : "Hide sidebar"}>
        <Icon d={ICONS.panel} />
      </button>
      {!closed && <>
        <button className="side-icon" onClick={() => window.history.back()} aria-label="Back" title="Back"><Icon d={ICONS.back} /></button>
        <button className="side-icon" onClick={() => window.history.forward()} aria-label="Forward" title="Forward"><Icon d={ICONS.forward} /></button>
        <button className={"side-icon" + (showSearch ? " side-icon-on" : "")} aria-expanded={showSearch} aria-controls="q"
                onClick={() => { if (showSearch) { onQuery(""); setSearching(false); } else setSearching(true); }}
                aria-label="Search sessions" title={`Search sessions (${modKey()}K for everything)`}><Icon d={ICONS.search} /></button>
        {onNew && (
          <button className="side-new" onClick={onNew} aria-label="New conversation" title={`New conversation (${modKey()}K)`}>
            <Icon d={ICONS.compose} />
          </button>
        )}
      </>}
    </div>
  );

  // A phone has no room to fold the list into: there the list is the pane.
  if (closed && !window.matchMedia?.("(max-width:720px)").matches) return <div className="sidebar sidebar-closed">{toolbar}</div>;

  const total = recent.length + inactive.length + archived.length;
  return (
    <div className="sidebar">
      {card && (
        <div id="row-card" className="row-card" role="tooltip" style={{ top: card.top, left: card.left }}>{card.text}</div>
      )}
      {toolbar}
      {showSearch && (
        <div className="session-search">
          <label htmlFor="q" className="visually-hidden">Search sessions</label>
          <input id="q" ref={searchRef} className="field" value={query} placeholder="Search sessions"
                 onChange={(e) => onQuery(e.target.value)}
                 onKeyDown={(e) => {
                   if (e.key === "Escape") { e.preventDefault(); onQuery(""); setSearching(false); return; }
                   // Down from the search lands on the first match.
                   if (e.key !== "ArrowDown") return;
                   const first = document.querySelector<HTMLElement>(".sidebar button.row");
                   if (first) { e.preventDefault(); first.focus(); }
                 }} />
        </div>
      )}
      <div className="scroll" onScroll={unpeek} onKeyDown={walk}>
        {total === 0 && !showArchived && (
          <p className="list-none">{query ? `No sessions match “${query}”.` : "No sessions yet."}</p>
        )}
        {workspaces(recent)}
        {inactive.length > 0 && section("inactive", "Inactive last 72h", Boolean(query) || unfolded.has("inactive"), () => toggleFold("inactive"),
          inactive.length, workspaces(byWorkspace(inactive)))}
        {section("archived", "Archived", showArchived, onToggleArchived, showArchived ? archived.length : null,
          archived.length ? workspaces(byWorkspace(archived)) : <p className="list-none">Nothing archived.</p>)}
      </div>
      {onView && (
        <nav className="side-nav" aria-label="Views">
          {([["projects", "Projects", ICONS.projects], ["hooks", "Hooks", ICONS.hooks], ["wiki", "Wiki", ICONS.wiki]] as const).map(([v, label, icon]) => (
            <button key={v} className={"side-nav-item" + (view === v ? " side-nav-on" : "")}
                    aria-current={view === v ? "page" : undefined}
                    onClick={() => onView(v)}>
              <Icon d={icon} size={20} />
              <span>{label}</span>
              {v === "wiki" && wikiFlags > 0 && (
                <span className="nav-count" title={`${wikiFlags} claims to review`}>
                  {wikiFlags}<span className="visually-hidden"> claims to review</span>
                </span>
              )}
            </button>
          ))}
        </nav>
      )}
    </div>
  );
}

/* ---------------- transcript ---------------- */

/**
 * Copy what a block holds.
 *
 * An icon, not a word. Set as text it read as one more piece of
 * metadata on a row that already ends in "1 line" — two grey words in
 * a row, neither of them obviously a control. It follows the status
 * glyphs' idiom: a 24-box stroked in currentColor, 1.5px beside
 * regular text.
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
    <button
      className={"copy-btn" + (done ? " copy-done" : "")}
      title={done ? "Copied" : `Copy ${what}`}
      aria-label={done ? "Copied" : `Copy ${what}`}
      onClick={(e) => {
        // It sits inside the <summary>, so a click would toggle the
        // block as well as copy it. Stop both: this is its own control.
        e.preventDefault();
        e.stopPropagation();
        copy();
      }}
    >
      <svg viewBox="0 0 24 24" width="15" height="15" fill="none"
           stroke="currentColor" strokeWidth="1.5"
           strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
        {done
          ? <path d="M5 12.5l4.5 4.5L19 7.5" />
          : <>
              <rect x="9" y="9" width="11" height="11" rx="2" />
              <path d="M5 15V6a2 2 0 0 1 2-2h9" />
            </>}
      </svg>
      {/* The change of shape is the cue; this is for a reader who
          cannot see it. */}
      <span className="visually-hidden" role="status">{done ? "Copied" : ""}</span>
    </button>
  );
}

export function CodeBlock({ line }: { line: Line }) {
  const call = useMemo(() => parseCall(line.text), [line.text]);
  const lines = call.body ? call.body.split("\n").length : 0;
  // Collapsed by default. A turn is a list of things the agent did; the
  // point of the list is to be scanned, and an open block for every one
  // of them buries the reply that follows.
  return (
    <details className="block thin">
      <summary>
        <span className="block-label">{call.verb}</span>
        <span className="mono block-detail">{firstLine(call.gist)}</span>
        {lines > 1 && <span className="num block-lines">{lineCount(lines)}</span>}
        <CopyButton text={call.body || call.raw} what={call.verb.toLowerCase() + " block"} />
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
    <details className="block thin">
      <summary>
        <span className="block-label">Result</span>
        <span className="mono block-detail">{head.slice(0, 90)}</span>
        <span className="num block-lines">{lineCount(lines.length)}</span>
        <CopyButton text={body} what="output" />
      </summary>
      <div className="block-body">
        <Code text={body || "(no output)"} lang={resultLang(line)} />
      </div>
    </details>
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
    <details className={"block thin" + (failed ? " block-failed" : "")}>
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

export function Entry({ line, codes, nested }: { line: Line; codes: string[]; nested?: boolean }) {
  const k = line.kind;
  if (k === "assistant" || k === "sub:assistant") {
    const body = stripRunFences(line.text, codes);
    if (blank(body)) return null; // the reply was only the program it ran
    // Inside a subagent card the rail and the card's own header
    // already say whose words these are; repeating "subagent" above
    // every paragraph of a five-step run is noise.
    return (
      <div className="say">
        {/* There is one assistant; naming it above every reply said nothing. */}
        {!nested && k !== "assistant" && <div className="say-who"><span className="sub-dot" /><span>subagent</span></div>}
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
      <details className="block thin thinking">
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
      {agents.map((a) => <SubAgentView key={a.worker + ":" + a.seq} agent={a} />)}
    </div>
  );
}

/** A command cut to its first step, short enough to sit on a thin line. */
function gistOf(text: string): string {
  const line = firstLine(text);
  const g = line.split(/\s*(?:;|&&|\|\|)\s*/)[0] ?? "";
  if (g.length > 60) return g.slice(0, 60) + "…";
  return g.length < line.length ? g + " …" : g;
}

/** One call's recorded facts, for its thin line and the hover list. */
interface CallFacts { verb: string; gist: string; exit?: number; ms?: number; failed: boolean }

function callFacts(code: Line, result?: Line): CallFacts {
  const call = parseCall(code.text);
  const out = result ? resultBody(result) : "";
  const exit = typeof result?.data?.exit === "number" ? (result.data.exit as number) : undefined;
  const ms = typeof result?.data?.ms === "number" ? (result.data.ms as number) : undefined;
  return { verb: call.verb, gist: gistOf(call.gist), exit, ms, failed: (exit !== undefined && exit !== 0) || /^error\b/i.test(out) };
}

const canHover = () => typeof window !== "undefined" && window.matchMedia?.("(hover: hover)").matches;

/**
 * Hover (or keyboard focus) on a thin line shows its details after a
 * beat, in a fixed layer the scrolling transcript cannot clip. Touch
 * screens get none: a tap opens the line instead.
 */
function useThinPop(rows: CallFacts[]) {
  const [at, setAt] = useState<DOMRect | null>(null);
  const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  const hide = useCallback(() => { clearTimeout(timer.current); setAt(null); }, []);
  useEffect(() => {
    if (!at) return;
    // Escape closes the popover only; it must not also leave the thread.
    const key = (e: KeyboardEvent) => { if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); hide(); } };
    window.addEventListener("keydown", key, true);
    window.addEventListener("scroll", hide, true);
    return () => { window.removeEventListener("keydown", key, true); window.removeEventListener("scroll", hide, true); };
  }, [at, hide]);
  useEffect(() => () => clearTimeout(timer.current), []);
  const show = (e: React.SyntheticEvent<HTMLElement>) => {
    const el = e.currentTarget;
    if (!canHover() || (el.parentElement as HTMLDetailsElement | null)?.open) return;
    clearTimeout(timer.current);
    timer.current = setTimeout(() => setAt(el.getBoundingClientRect()), 350);
  };
  const handlers = { onMouseEnter: show, onFocus: show, onMouseLeave: hide, onBlur: hide, onClick: hide };
  // Placed from its own measured size: below the line if it fits, else
  // above, else pinned to the top (its max-height keeps it on screen).
  const ref = useRef<HTMLDivElement>(null);
  useLayoutEffect(() => {
    const el = ref.current;
    if (!at || !el) return;
    const r = el.getBoundingClientRect();
    const top = at.bottom + 4 + r.height <= window.innerHeight - 16 ? at.bottom + 4
      : at.top - 4 - r.height >= 16 ? at.top - 4 - r.height : 16;
    el.style.top = top + "px";
    el.style.left = Math.max(16, Math.min(at.left, window.innerWidth - 16 - r.width)) + "px";
    el.style.visibility = "visible";
  }, [at]);
  let pop: React.ReactNode = null;
  if (at && rows.length) {
    pop = createPortal(
      <div ref={ref} className="thin-pop" role="tooltip" style={{ top: 0, left: 0, visibility: "hidden" }}>
        {rows.map((r, i) => (
          <div key={i} className={"thin-pop-row" + (r.failed ? " thin-pop-failed" : "")}>
            <span>{r.verb}</span>
            <span className="mono thin-pop-cmd">{r.gist}</span>
            <span className="num">{r.exit !== undefined ? `exit ${r.exit}` : ""}</span>
            <span className="num">{r.ms !== undefined ? duration(r.ms) : ""}</span>
          </div>
        ))}
      </div>, document.body);
  }
  return { handlers, pop };
}

/** A result's text minus the code history prefixes onto it. */
function resultBody(l: Line): string {
  const code = str(l.data?.code);
  return code && l.text.startsWith(code) ? l.text.slice(code.length).trimStart() : l.text;
}

/**
 * A run of tool calls as one row: how many, the last thing it did, and
 * whether any failed. Opened, each call is its own block again.
 */
export function ToolRun({ lines, codes }: { lines: Line[]; codes: string[] }) {
  // Pair each call with the result recorded for it: one row per thing
  // done, not a "Ran" row and a "Result" row saying half each.
  const rows: React.ReactNode[] = [];
  const facts: CallFacts[] = [];
  for (let i = 0; i < lines.length; i++) {
    const l = lines[i];
    if (l.kind === "code") {
      const next = lines[i + 1];
      const result = next?.kind === "result" ? next : undefined;
      if (result) i++;
      facts.push(callFacts(l, result));
      rows.push(<ToolCall key={l.seq} code={l} result={result} />);
    } else {
      rows.push(<Entry key={l.seq} line={l} codes={codes} />);
    }
  }
  const { handlers, pop } = useThinPop(facts);
  const calls = facts.length;
  if (calls < 2) return <>{rows}</>;
  const failed = facts.filter((f) => f.failed).length;
  const timed = facts.every((f) => f.ms !== undefined);
  const totalMs = facts.reduce((n, f) => n + (f.ms ?? 0), 0);
  // The line is only the count, time and failures; each command lives
  // once in the hover list and once in the expansion.
  return (
    <details className="block thin toolrun">
      <summary {...handlers}>
        <span className={"block-label" + (failed ? " thin-failed" : "")}>{calls} tool calls</span>
        {timed && <span className="num tool-meta">{duration(totalMs)}</span>}
        {failed > 0 && <span className="num toolrun-failed">{failed} failed</span>}
      </summary>
      {pop}
      <div className="toolrun-body">{rows}</div>
    </details>
  );
}

/**
 * One call and what came back, as a single row. The summary says what
 * was done and how much it printed; opened, the program and its output
 * sit together, with the raw call one level further in.
 */
export function ToolCall({ code, result }: { code: Line; result?: Line }) {
  const call = useMemo(() => parseCall(code.text), [code.text]);
  const out = result ? resultBody(result) : "";
  // Recorded evidence, when the loop stamped it: the block's own exit code
  // and how long it ran. Older results carry neither and show neither.
  const exit = typeof result?.data?.exit === "number" ? (result.data.exit as number) : undefined;
  const ms = typeof result?.data?.ms === "number" ? (result.data.ms as number) : undefined;
  const failed = (exit !== undefined && exit !== 0) || /^error\b/i.test(out);
  // A question nobody answered is an outcome, not an exception to parse.
  const timedOut = /ask: no answer after (\S+)/.exec(out);
  // A single call's line already says everything the hover list would,
  // so its hover shows what the line cannot: the full first line and output size.
  const outLines = (out || "(no output)").split("\n").length;
  // The full first line only when the thin line had to cut it.
  const cut = gistOf(call.gist) !== firstLine(call.gist);
  const { handlers, pop } = useThinPop(timedOut ? [] : [{
    verb: result ? lineCount(outLines) : "no result yet", gist: cut ? firstLine(call.gist) : "", failed,
  }]);
  return (
    <details className={"block thin" + (failed ? " block-failed" : "")} data-seq={result?.seq}>
      <summary {...handlers}>
        <span className="block-label">{timedOut ? "Question timed out" : call.verb}</span>
        <span className="mono block-detail">{timedOut ? timedOut[1] : gistOf(call.gist)}</span>
        {(exit !== undefined || ms !== undefined) && (
          <span className={"num tool-meta" + (exit !== undefined && exit !== 0 ? " tool-meta-failed" : "")}>
            {[exit !== undefined && exit !== 0 ? `exit ${exit}` : "", ms !== undefined ? duration(ms) : ""].filter(Boolean).join(" · ")}
          </span>
        )}
        <CopyButton text={out || call.body || call.raw} what={result ? "output" : call.verb.toLowerCase() + " block"} />
      </summary>
      {pop}
      <div className="block-body">
        {call.body && <Code text={call.body} lang={call.lang} />}
        {/* Output keeps its columns: a docker ps or a table wrapped at the
            block's edge scatters every row across three lines. */}
        {result && <div className="tool-output"><Code text={out || "(no output)"} lang={resultLang(result)} /></div>}
        {call.body !== call.raw && (
          <details className="block-inner">
            <summary><span className="block-label">The call</span></summary>
            <Code text={call.raw} lang="javascript" />
          </details>
        )}
      </div>
    </details>
  );
}

/**
 * Every hook and rule that fired in a turn, as one quiet row. They fire
 * on every tool call, so inline they drowned the transcript; folded,
 * the count says whether anything happened and the ledger is one click in.
 */
export function TurnHooks({ lines }: { lines: Line[] }) {
  const fires = lines.filter((l) => l.kind === "hook");
  if (!fires.length) return null;
  const decided = fires.filter((l) => str(l.data?.decision)).length;
  const errored = fires.filter((l) => str(l.data?.error)).length;
  const rules = new Set<string>();
  for (const l of fires) {
    const n = str(l.data?.notice);
    if (n.startsWith("applied ")) n.slice(8).split(", ").forEach((r) => rules.add(r));
  }
  const parts = [`${fires.length} fired`];
  if (rules.size) parts.push(`${rules.size} ${rules.size === 1 ? "rule" : "rules"} applied`);
  if (decided) parts.push(`${decided} decided`);
  return (
    <details className="block thin turn-hooks">
      <summary>
        <span className="block-label">Hooks</span>
        <span className="block-detail">{parts.join(" · ")}</span>
        {errored > 0 && <span className="num toolrun-failed">{errored} errored</span>}
      </summary>
      <div className="turn-hooks-body">
        {fires.map((l) => {
          const d = l.data ?? {};
          const err = str(d.error), decision = str(d.decision), notice = str(d.notice);
          return (
            <p key={l.seq} className="hook-line">
              <span className="mono">{str(d.name)}</span>{" · "}{str(d.event)}{" · "}
              <span className={err ? "hook-bad" : decision ? "hook-act" : "hook-why"}>
                {err ? "errored" : decision || "passed"}
              </span>
              {notice && <span className="hook-why"> — {notice}</span>}
              {err && <span className="hook-why"> {err}</span>}
              {typeof d.ms === "number" && <span className="num hook-why"> · {d.ms} ms</span>}
            </p>
          );
        })}
      </div>
    </details>
  );
}

/**
 * How a turn ended and what it took, from what the loop recorded on its
 * done entry: elapsed, tokens, cost, the context it left, the files it
 * changed. A bare "Finished" told a programmer none of that. Nothing is
 * estimated — a provider that recorded no usage shows only the outcome.
 */
function TurnFooter({ turn }: { turn: Turn }) {
  const done = turn.done!;
  const u = usageOf(done);
  const files = Array.isArray(done.data?.files) ? (done.data!.files as string[]) : [];
  const exit = done.data?.exit;
  const failed = typeof exit === "number" && exit !== 0;
  const facts: string[] = [];
  if (turn.prompt?.at) facts.push(duration(Date.parse(done.at) - Date.parse(turn.prompt.at)));
  // The strip above owns session totals; a turn says what it took, with
  // its tokens on the price rather than as a third figure.
  const tokens = u ? `${tokenCount(u.in)} in · ${tokenCount(u.out)} out` : "";
  if (u?.cost !== undefined) facts.push(money(u.cost));
  else if (u) facts.push(tokens);
  return (
    <div className="turn-foot">
      <span className={"turn-outcome" + (failed ? " turn-failed" : "")}>
        {done.kind === "cancelled" ? "Stopped" : failed ? `Finished · last command exit ${exit}` : "Finished"}
      </span>
      {facts.map((f) => <span key={f} className="num" title={tokens || undefined}>{f}</span>)}
      {files.length > 0 && (
        <details className="turn-files">
          <summary>{files.length} {files.length === 1 ? "file" : "files"} changed</summary>
          <ul>{files.map((f) => <li key={f} className="mono">{f}</li>)}</ul>
        </details>
      )}
    </div>
  );
}

/**
 * The session's running budget, always in view: what the context holds
 * against the model's window, what the session has spent, and the model
 * actually answering. The window is only named when the session records
 * its model; a default model is not guessed at.
 */
function RuntimeStrip({ row, lines }: { row: Row; lines: Line[] }) {
  const [limits, setLimits] = useState<Record<string, number>>({});
  useEffect(() => {
    fetch("/api/models").then((r) => r.json()).then((c: { providers?: ProviderInfo[] }) => {
      const m: Record<string, number> = {};
      for (const p of c.providers ?? []) for (const x of p.models ?? []) if (x.context) m[x.id] = x.context;
      setLimits(m);
    }).catch(() => setLimits({}));
  }, []);
  const u = useMemo(() => sessionUsage(lines), [lines]);
  // Jobs, cache and changes stand on their own: a session with no usage
  // recorded can still have a server running.
  const limit = row.model ? limits[row.model] : undefined;
  const pct = u && limit ? Math.min(100, Math.round((u.lastIn / limit) * 100)) : undefined;
  return (
    <div className="runtime-strip">
      {u && (
        <span className="rt" title={limit ? `${u.lastIn.toLocaleString()} of ${limit.toLocaleString()} tokens` : undefined}>
          <span className="rt-label">{limit ? "Context" : "Last input"}</span>
          <span className="num rt-value">{tokenCount(u.lastIn)}{limit ? ` / ${contextSize(limit)}` : ""}</span>
          {pct !== undefined && (
            <span className="rt-bar" role="meter" aria-label="Context used" aria-valuenow={pct} aria-valuemin={0} aria-valuemax={100}>
              <span className={pct >= 80 ? "rt-hot" : undefined} style={{ width: `${pct}%` }} />
            </span>
          )}
        </span>
      )}
      {u?.cost !== undefined && (
        // Tokens in/out ride on the cost's tooltip: the price is the figure
        // a person acts on. The model is named once, in the header picker.
        <span className="rt" title={`${u.in.toLocaleString()} tokens in · ${u.out.toLocaleString()} out`}>
          <span className="rt-label">Cost</span><span className="num rt-value">{money(u.cost)}</span>
        </span>
      )}
      <ChangesChip id={row.id} tick={lines.length} />
      <TestsChip lines={lines} />
      {row.cache && <CacheChip cache={row.cache} model={row.model} />}
      {row.jobs && row.jobs.length > 0 && <JobsChip session={row.id} jobs={row.jobs} />}
    </div>
  );
}

/**
 * What is uncommitted where the session works, read from git whenever the
 * transcript grows. It is the repository's state, not a tally of what the
 * agent claimed, so an edit made by hand shows too. Absent outside a repo
 * and when the tree is clean.
 */
function ChangesChip({ id, tick }: { id: string; tick: number }) {
  const [files, setFiles] = useState<Change[]>([]);
  useEffect(() => {
    let live = true;
    // A hand edit or another session changes the tree without a transcript
    // entry, so it is re-read on a timer too. A failed read keeps the last
    // snapshot rather than claiming the tree is clean.
    const read = () => api.changes(id).then((r) => { if (live) setFiles(r.files); }).catch(() => {});
    read();
    const t = setInterval(read, 10_000);
    return () => { live = false; clearInterval(t); };
  }, [id, tick]);
  if (!files.length) return null;
  const add = files.reduce((n, f) => n + Math.max(0, f.add), 0);
  const del = files.reduce((n, f) => n + Math.max(0, f.del), 0);
  return (
    <details className="rt rt-jobs">
      <summary>
        <span className="rt-label">Changes</span>
        <span className="num rt-value">{files.length} <span className="rt-add">+{add}</span> <span className="rt-del">−{del}</span></span>
      </summary>
      <ul className="rt-pop">
        {files.map((f) => (
          <li key={f.path}>
            <span className="mono rt-job-cmd" title={f.path}>{f.path}</span>
            <span className="num">
              {f.new ? <span className="rt-add">new</span>
                : f.add < 0 ? <span className="rt-label">binary</span>
                : <><span className="rt-add">+{f.add}</span> <span className="rt-del">−{f.del}</span></>}
            </span>
          </li>
        ))}
      </ul>
    </details>
  );
}

/** A command that runs a test suite, by the runners people actually type. */
const TEST_CMD = /\b(go test|(?:npm|pnpm|yarn|bun)(?: run)? test|pytest|cargo (?:test|nextest)|vitest|jest|make (?:test|check)|mvn test|gradle test|rspec|phpunit)\b/;

/**
 * The last test run and how it ended, from the exit code the loop
 * recorded on its result — never from what the reply said about it.
 * Absent when the session ran no tests, or ran them before exit codes
 * were recorded.
 */
function TestsChip({ lines }: { lines: Line[] }) {
  const last = useMemo(() => {
    for (let i = lines.length - 1; i > 0; i--) {
      const r = lines[i], c = lines[i - 1];
      if (r.kind !== "result" || c.kind !== "code" || typeof r.data?.exit !== "number") continue;
      const call = parseCall(c.text);
      if (call.verb === "Ran" && TEST_CMD.test(call.target)) return { cmd: call.gist, exit: r.data.exit as number, at: r.at, seq: r.seq };
    }
    return null;
  }, [lines]);
  if (!last) return null;
  const failed = last.exit !== 0;
  return (
    // The chip is a way to the evidence, not a second copy of it: it opens
    // the call in the transcript (and the run folding it) and lands there.
    <button className="rt rt-link" title={`${last.cmd} · exit ${last.exit} · ${new Date(last.at).toLocaleString()}`}
            onClick={() => {
              const el = document.querySelector<HTMLDetailsElement>(`details.block[data-seq="${last.seq}"]`);
              if (!el) return;
              for (let d: HTMLElement | null = el; d; d = d.parentElement?.closest("details") ?? null) (d as HTMLDetailsElement).open = true;
              el.scrollIntoView({ block: "center" });
              el.querySelector("summary")?.focus();
            }}>
      <span className="rt-label">Tests</span>
      <span className={"num rt-value " + (failed ? "rt-del" : "rt-add")}>{failed ? "failed" : "passed"}</span>
      <span className="num rt-label">{ago(last.at)} ago</span>
    </button>
  );
}

/** Now, re-read every second while `on`. */
function useNow(on: boolean): number {
  const [now, setNow] = useState(Date.now());
  useEffect(() => {
    if (!on) return;
    const t = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(t);
  }, [on]);
  return now;
}

/**
 * Whether the next turn re-reads the conversation from the provider's
 * prompt cache (a tenth of the price) or pays full input again. The
 * window opens when a turn ends and the countdown says how long is left.
 */
function CacheChip({ cache, model }: { cache: NonNullable<Row["cache"]>; model?: string }) {
  const end = Date.parse(cache.at) + cache.ttl * 1000;
  const now = useNow(end > Date.now());
  const left = Math.max(0, Math.round((end - now) / 1000));
  const hit = cache.in ? Math.round((cache.read / cache.in) * 100) : 0;
  const provider = model?.split("/")[0]?.replace(/^~/, "") || "provider";
  return (
    <span className={"rt" + (left ? " rt-cache-hot" : " rt-cache-cold")}
          title={`${provider} prompt cache · last turn read ${tokenCount(cache.read)} of ${tokenCount(cache.in)} input tokens from it (${hit}%), wrote ${tokenCount(cache.write)}`}>
      <span className="rt-label">Cache</span>
      <span className="num rt-value">
        {/* "~": the window is the provider's documented minimum, not a reading. */}
        {left ? `hot · ~${Math.floor(left / 60)}:${String(left % 60).padStart(2, "0")}` : "cold"}
      </span>
    </span>
  );
}

/** Background jobs still running, one click from their commands. */
function JobsChip({ session, jobs }: { session: string; jobs: NonNullable<Row["jobs"]> }) {
  const now = useNow(true);
  // A stop is asked of the child and lands when the job's own entry does;
  // until then the row says so, and a refused ask says that instead.
  const [stop, setStop] = useState<Record<number, "stopping" | "failed">>({});
  const kill = (id: number) => {
    setStop((m) => ({ ...m, [id]: "stopping" }));
    api.killJob(session, id).catch(() => setStop((m) => ({ ...m, [id]: "failed" })));
  };
  return (
    <details className="rt rt-jobs">
      <summary>
        <span className="rt-label">Jobs</span>
        <span className="num rt-value">{jobs.length} running</span>
      </summary>
      <ul className="rt-pop">
        {jobs.map((j) => (
          <li key={j.id}>
            <span className="mono rt-job-cmd" title={j.cmd}>{j.cmd}</span>
            <span className="num rt-label">{duration(now - Date.parse(j.started))}</span>
            <button className="btn rt-stop" disabled={stop[j.id] === "stopping"} onClick={() => kill(j.id)}>
              {stop[j.id] === "stopping" ? "Stopping…" : stop[j.id] === "failed" ? "Couldn’t stop · Retry" : "Stop"}
            </button>
          </li>
        ))}
      </ul>
    </details>
  );
}

/** The ask repeats the title when it carries most of the title's words. */
function askSaysTitle(ask: string, title: string): boolean {
  const words = (t: string) => t.toLowerCase().match(/[a-z0-9]+/g) ?? [];
  const have = new Set(words(ask));
  const want = words(title);
  return want.length > 0 && want.filter((w) => have.has(w)).length / want.length >= 2 / 3;
}

export function TurnView({ turn, tail, n }: { turn: Turn; tail?: React.ReactNode; /** 1-based position, so the turn log can land on it. */ n?: number }) {
  const codes = turn.body.filter((l) => l.kind === "code" || l.kind === "sub:code").map((l) => l.text);
  const hooks = useMemo(() => turn.body.filter(isHookLine), [turn.body]);
  const items = useMemo<Item[]>(
    () => groupTools(groupSubs(turn.body.filter((l) => !isHookLine(l))), codes),
    // codes is derived from turn.body on every render.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [turn.body]);
  // The loop appends "[skill: name]\n<SKILL.md>" blocks to the prompt a
  // skill was invoked from. What you typed is the part before them.
  // An @file is attached the same way, as "[file: path]\n<contents>":
  // pasted source is context, not the words of the prompt.
  const [said, ...skills] = (turn.prompt?.text ?? "").split(/\n+(?=\[(?:skill|file): [^\]\n]+\]\n)/);
  const [full, setFull] = useState(false);
  const long = said.length > 420 || said.split("\n").length > 4;
  return (
    <section className="turn" data-turn={n}>
      {turn.prompt && (
        <div className="prompt">
          <span className="mono prompt-mark">&gt;</span>
          <div className="prompt-text">
            {/* A long brief (pasted logs, a spec) is evidence, not the
                thing to navigate by: four lines, and one click for the rest. */}
            <p className={long && !full ? "prompt-clamp" : undefined}>{said}</p>
            {long && (
              <button className="link prompt-more" onClick={() => setFull((v) => !v)}>
                {full ? "Show less" : "Show full prompt"}
              </button>
            )}
            {skills.map((s, i) => {
              const [head, ...body] = s.split("\n");
              const m = /^\[(skill|file): (.+)\]$/.exec(head.trim());
              const isFile = m?.[1] === "file";
              return (
                <details key={i} className={"block prompt-skill" + (isFile ? " prompt-file" : "")}>
                  <summary>
                    <span className="block-label">{isFile ? "File" : "Skill"}</span>
                    <span className="mono block-detail">{m?.[2] ?? head}</span>
                  </summary>
                  <pre>{body.join("\n")}</pre>
                </details>
              );
            })}
          </div>
          <span className="num prompt-time">{clock(turn.prompt.at)}</span>
        </div>
      )}
      <div className="turn-body">
        {items.map((it) => it.kind === "sub"
          ? <SubRun key={"sub" + it.seq} agents={it.agents} />
          : it.kind === "tools"
          ? <ToolRun key={"tools" + it.seq} lines={it.lines} codes={codes} />
          : <Entry key={it.seq} line={it.line} codes={codes} />)}
        {tail}
        <TurnHooks lines={hooks} />
      </div>
      {turn.done && <TurnFooter turn={turn} />}
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
          <Markdown text={r.text} />
        </div>
      ))}
    </>
  );
}

/* ---------------- model + effort ---------------- */

interface ModelInfo { id: string; context?: number; efforts?: string[]; input?: number; output?: number }

/** 1050000 → "1M", 262144 → "262k": the size a person says, not a unit conversion. */
function contextSize(tokens: number): string {
  if (tokens >= 1_000_000) return `${+(tokens / 1_000_000).toFixed(tokens % 1_000_000 < 50_000 ? 0 : 1)}M`;
  return `${Math.round(tokens / 1000)}k`;
}

/** The API's effort enums ("xhigh") read as words in the menu. */
function effortLabel(e: string): string {
  const words: Record<string, string> = { xhigh: "Extra high", minimal: "Minimal", max: "Max" };
  return words[e] ?? e.charAt(0).toUpperCase() + e.slice(1);
}
interface ProviderInfo { plugin: string; models?: ModelInfo[] }

export function Controls({ row, projects, onModel, onEffort, onAssign, only }: {
  row: Row; projects: Project[];
  onModel: (m: string) => void; onEffort: (e: string) => void; onAssign: (p: string) => void;
  /** Render just the model picker, or everything but it. */
  only?: "model" | "rest";
}) {
  const [cat, setCat] = useState<{ providers: ProviderInfo[]; efforts: string[] } | null>(null);
  useEffect(() => {
    fetch("/api/models").then((r) => r.json()).then(setCat).catch(() => setCat(null));
  }, []);

  // A session that has not answered yet genuinely has no model to name;
  // one running a model the catalogue does not list still shows it.
  // "Default" can only be where a session starts: the supervisor has no
  // way back to it, so once a model is set it is not offered.
  const models: Option[] = row.model ? [] : [{ value: "", label: "Default model" }];
  for (const p of cat?.providers ?? []) {
    for (const m of p.models ?? []) {
      models.push({ value: m.id, label: m.id, group: p.plugin.replace(/^llm-/, ""),
                    detail: m.context ? contextSize(m.context) : undefined });
    }
  }
  if (row.model && !models.some((o) => o.value === row.model)) {
    models.push({ value: row.model, label: row.model, group: "In use" });
  }

  // Effort is offered for what the chosen model supports; a model the
  // catalogue does not describe falls back to every level it knows.
  const chosen = cat?.providers.flatMap((p) => p.models ?? []).find((m) => m.id === row.model);
  const efforts = chosen?.efforts?.length ? chosen.efforts : (cat?.efforts ?? []);

  return (
    <div className="controls">
      {only !== "rest" && (
        // What the next turn runs as is one setting: the model and how hard
        // it thinks, side by side, on every screen.
        <div className="ctl ctl-run">
          <Select label="Model" value={row.model ?? ""} options={models} searchable align="end"
                  onChange={(v) => v && onModel(v)} />
          {efforts.length > 0 && (
            <Select label="Effort" value={row.effort ?? ""} align="end" onChange={(v) => v && onEffort(v)}
                    options={[...(row.effort ? [] : [{ value: "", label: "Default effort" }]), ...efforts.map((e) => ({ value: e, label: effortLabel(e) }))]} />
          )}
        </div>
      )}
      {only !== "model" && (
        <div className="ctl">
          <span className="ctl-label">Project</span>
          <Select label="Project" value={row.project ?? ""} align="end" onChange={onAssign}
                  options={[{ value: "", label: "Unassigned" }, ...projects.map((p) => ({ value: p.id, label: p.name }))]} />
        </div>
      )}
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

export function Thread({ row, lines, loading = false, stream = [], activity = "", projects, onAck, onSend, onAnswer, onInterrupt, onArchive, onRename, onModel, onEffort, onAssign, onBack, onContext, busy, jump }: {
  row: Row; lines: Line[]; loading?: boolean; stream?: DeltaRun[];
  /** The small model's live label for the running turn; never recorded. */
  activity?: string; projects: Project[]; busy: boolean; onBack?: () => void;
  /** Scroll to this turn (1-based) once it is on screen; `at` makes a repeat click count. */
  jump?: { turn: number; at: number } | null;
  onSend: (t: string) => Promise<boolean> | void; onAnswer: (t: string) => Promise<boolean> | void; onInterrupt: () => void;
  onArchive: () => void; onRename: (t: string) => void; onContext?: () => void; onAck?: () => void;
  onModel: (m: string) => void; onEffort: (e: string) => void; onAssign: (p: string) => void;
}) {
  // One draft per session: switching away and back keeps what you were
  // typing there, and never carries it into another conversation.
  const draftKey = "bough:draft:" + row.id;
  const [draft, setDraft] = useState(() => { try { return sessionStorage.getItem(draftKey) ?? ""; } catch { return ""; } });
  useEffect(() => {
    try { draft ? sessionStorage.setItem(draftKey, draft) : sessionStorage.removeItem(draftKey); } catch { /* storage off */ }
  }, [draft, draftKey]);
  const [trigger, setTrigger] = useState<Trigger | null>(null);
  // A phone hides the model, project and thinking controls behind one
  // button: they change rarely, and the thread is what the screen is for.
  const [more, setMore] = useState(false);
  const end = useRef<HTMLDivElement>(null);
  const ask = useRef<HTMLDivElement>(null);
  // The reminder above the composer is for a question scrolled out of
  // sight; with the question itself on screen it only repeats it.
  const [askSeen, setAskSeen] = useState(false);
  useEffect(() => {
    const el = ask.current, root = scroller.current;
    if (!el || !root) { setAskSeen(false); return; }
    const io = new IntersectionObserver(([e]) => setAskSeen(e.isIntersecting), { root, threshold: 0.5 });
    io.observe(el);
    return () => io.disconnect();
  }, [row.ask?.text]);
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
  // Shown only while you have scrolled away from the end, so reading
  // history during a live turn has a way back that does not yank you.
  const [away, setAway] = useState(false);
  const onScroll = () => {
    const el = scroller.current;
    if (!el) return;
    // A slack of a couple of lines: "near the bottom" is what a reader
    // means by "at the bottom", and an exact test loses the stick the
    // moment a fragment arrives a pixel early.
    atBottom.current = el.scrollHeight - el.scrollTop - el.clientHeight < 40;
    setAway(!atBottom.current);
  };
  useEffect(() => {
    if (atBottom.current) end.current?.scrollIntoView({ block: "end" });
  }, [lines.length, streamLen]);
  // Opening a different conversation starts at the bottom again.
  useEffect(() => { atBottom.current = true; }, [row.id]);
  const turns = useMemo(() => groupTurns(lines), [lines]);
  // A turn picked from the log: land on it once the transcript holds it,
  // flash it, and stop following the bottom so it stays put.
  const jumped = useRef(0);
  useEffect(() => {
    if (!jump || loading || jumped.current === jump.at) return;
    const el = scroller.current?.querySelector<HTMLElement>(`.turn[data-turn="${jump.turn}"]`);
    if (!el) return;
    jumped.current = jump.at;
    atBottom.current = false;
    el.scrollIntoView({ block: "start" });
    el.classList.remove("turn-flash");
    void el.offsetWidth;
    el.classList.add("turn-flash");
    const t = setTimeout(() => el.classList.remove("turn-flash"), 1600);
    return () => clearTimeout(t);
  }, [jump, loading, turns.length]);
  // A fast read shows nothing at all; only a slow one earns a word.
  const [slow, setSlow] = useState(false);
  useEffect(() => {
    if (!loading) { setSlow(false); return; }
    const t = setTimeout(() => setSlow(true), 200);
    return () => clearTimeout(t);
  }, [loading]);
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

  // A message that did not go through is kept on its own, not folded back
  // into the draft: you may already be typing the next one, and switching
  // sessions must not lose it.
  const failedKey = "bough:failed:" + row.id;
  const [failed, setFailed] = useState<{ text: string; answer: boolean } | null>(() => {
    try { return JSON.parse(sessionStorage.getItem(failedKey) ?? "null"); } catch { return null; }
  });
  useEffect(() => {
    try { failed ? sessionStorage.setItem(failedKey, JSON.stringify(failed)) : sessionStorage.removeItem(failedKey); } catch { /* storage off */ }
  }, [failed, failedKey]);

  // What you just sent, shown the moment you send it. The recorded input
  // can take seconds to land (the child may be starting), and a message
  // that vanished from the composer with nothing in its place read as lost.
  // It stands until any newer input is recorded, or the send fails.
  const [sending, setSending] = useState<{ text: string; after: number } | null>(null);
  const newest = lines.length ? lines[lines.length - 1].seq : 0;
  const landed = sending !== null && lines.some((l) => l.kind === "input" && l.seq > sending.after);
  useEffect(() => { if (landed) setSending(null); }, [landed]);

  const deliver = async (t: string, answer: boolean) => {
    if (!answer) setSending({ text: t, after: newest });
    const ok = await (answer ? onAnswer(t) : onSend(t));
    if (ok === false) setSending(null);
    setFailed(ok === false ? { text: t, answer } : null);
  };
  const send = async () => {
    const t = draft.trim();
    // Enter reaches here even while the Send button is disabled.
    if (!t || busy) return;
    setDraft("");
    await deliver(t, Boolean(row.ask));
  };

  return (
    <div className="thread">
      <header className="thread-head" data-more={more ? "1" : "0"}>
        <Back onBack={onBack} />
        <div className="head-main">
          <h1 title={row.title}>{plainTitle(row.title) || untitled(row.id)}</h1>
          {(row.repo || row.branch) && (
            <span className="mono head-repo">
              {row.repo?.split("/").pop()}
              {row.branch && <span style={{ color: "var(--line-strong)" }}>/</span>}{row.branch}
            </span>
          )}
          {row.trouble ? (
            // One status: the reason replaces "Done".
            <span className="status head-trouble"><StatusMark status="error" bare />{capital(row.trouble)}</span>
          ) : <StatusMark status={row.status} />}
          {row.trouble && onAck && <button className="btn head-ack" onClick={onAck}>Mark seen</button>}
        </div>
        <button className="more" aria-label="Session settings" aria-expanded={more}
                onClick={() => setMore((v) => !v)}>
          <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.7"
               strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
            <path d="M4 7h10M18 7h2M4 17h4M12 17h8" /><circle cx="15" cy="7" r="2" /><circle cx="9" cy="17" r="2" />
          </svg>
        </button>
        <div className="head-side">
          <RuntimeStrip row={row} lines={lines} />
          <Controls row={row} projects={projects} onModel={onModel} onEffort={onEffort} onAssign={onAssign} only="model" />
          {onContext && <div className="head-actions"><button className="btn" onClick={onContext}>Context</button></div>}
        </div>
        <div className="head-extra">
          <Controls row={row} projects={projects} onModel={onModel} onEffort={onEffort} onAssign={onAssign} only="rest" />
          <div className="head-actions">
            <button className="btn" onClick={async () => {
              // Empty is allowed: it hands the title back to the session.
              const t = await askText("Rename session", { initial: plainTitle(row.title), action: "Rename", allowEmpty: true });
              if (t !== null) onRename(t);
            }}>Rename</button>
            <button className="btn" onClick={onArchive}>{row.archived ? "Unarchive" : "Archive"}</button>
          </div>
        </div>
      </header>

      <div className="scroll transcript" ref={scroller} onScroll={onScroll}>
        {loading && slow && <p className="meta-line transcript-state" role="status">Loading transcript…</p>}
        {!loading && turns.length === 0 && !running && !row.ask && (
          <p className="meta-line transcript-state">No recorded turns.</p>
        )}
        {turns.map((t, i) => (
          // Numbered by prompt, as the turn log counts: a leading /model
          // section has no prompt and no number.
          <TurnView key={t.seq} turn={t} n={t.prompt ? turns.slice(0, i + 1).filter((x) => x.prompt).length : undefined}
            // The preview belongs to the turn that is still open, so it
            // sits where the recorded entry will appear and is replaced
            // in place rather than jumping up the page.
            tail={i === turns.length - 1 && !t.done ? (
              <>
                <StreamView runs={stream} />
                {running && !row.ask && <Working label={stream.length && stream[stream.length - 1].kind === "thinking" ? "Thinking" : activity || "Working"} />}
              </>
            ) : undefined} />
        ))}
        {sending && !landed && (
          <section className="turn turn-sending" aria-live="polite">
            <div className="prompt">
              <span className="mono prompt-mark">&gt;</span>
              <div className="prompt-text"><p>{sending.text}</p></div>
              <span className="num prompt-time">Sending…</span>
            </div>
          </section>
        )}
        {running && (turns.length === 0 || turns[turns.length - 1].done) && !row.ask && (
          <div className="turn"><div className="turn-body"><StreamView runs={stream} /><Working label={activity || "Working"} /></div></div>
        )}
        {row.ask && (
          <div className="ask" ref={ask}>
            <StatusMark status="needs-you" size={16} />
            {/* Questions carry paths and commands in backticks; raw, they read as noise. */}
            <div className="ask-q"><Markdown text={row.ask.text} /></div>
            {row.ask.options.length > 0 && (
              <div style={{ display: "flex", gap: 10, flexWrap: "wrap" }}>
                {/* Equal alternatives, so none of them is dressed as the primary action. */}
                {row.ask.options.map((o) => (
                  <button key={o} className="btn" onClick={() => onAnswer(o)}>{o}</button>
                ))}
              </div>
            )}
          </div>
        )}
        <div ref={end} />
      </div>

      <div className="composer-wrap">
        {away && (
          <button className="btn jump-latest" onClick={() => {
            atBottom.current = true; setAway(false);
            end.current?.scrollIntoView({ block: "end", behavior: "smooth" });
          }}>Jump to latest</button>
        )}
        {row.ask && !askSeen && (
          <div className="ask-bar">
            <p><strong>Needs your answer.</strong> {row.ask.text}</p>
            <button className="btn" onClick={() => {
              // Land on the answer, not just near it: the first option if
              // there are any, otherwise the composer the answer is typed in.
              ask.current?.scrollIntoView({ block: "center" });
              (ask.current?.querySelector("button") ?? composer.current)?.focus({ preventScroll: true });
            }}>
              Answer question
            </button>
          </div>
        )}
        {failed && (
          <div className="send-failed" role="alert">
            <span className="send-failed-text" title={failed.text}>Not sent: {failed.text}</span>
            <button className="btn" disabled={busy} onClick={() => deliver(failed.text, failed.answer)}>Retry</button>
            <button className="btn" onClick={() => {
              // Put back beside what is already being typed, never over it.
              setDraft((d) => (d.trim() ? d.trimEnd() + "\n\n" + failed.text : failed.text));
              setFailed(null);
              composer.current?.focus();
            }}>Put back</button>
            <button className="link" onClick={() => setFailed(null)}>Dismiss</button>
          </div>
        )}
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
          <textarea id="composer" ref={composer} value={draft} rows={1}
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
              // Enter that confirms an IME composition is not a send.
              if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) { e.preventDefault(); send(); }
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
  // The catch-up cursor: the newest history seq on screen, read outside
  // any state updater.
  const lastSeq = useRef(0);
  // Whose transcript is in hand. An empty list means "loading" until the
  // first read for that session lands, and only then "nothing recorded".
  const [loadedFor, setLoadedFor] = useState<string | null>(null);
  useEffect(() => { lastSeq.current = lines.length ? lines[lines.length - 1].seq : 0; }, [lines]);
  // Live fragments of the reply being written, newest last. Never
  // merged into `lines`: these carry no history seq and the recorded
  // entry always supersedes them.
  const [stream, setStream] = useState<DeltaRun[]>([]);
  const [activity, setActivity] = useState("");
  const [query, setQuery] = useState("");
  const [archived, setArchived] = useState(false);
  const [busy, setBusy] = useState(false);
  // Two kinds of failure. A refresh that failed is stale data and heals on
  // the next poll; a send or action that failed is something you did that
  // did not happen, so it stays until you dismiss it — the 4s poll used to
  // clear it before it could be read.
  const [err, setErr] = useState<string | null>(null);
  const [loadErr, setLoadErr] = useState<string | null>(null);
  const [view, setView] = useState<View>("sessions");
  const [projects, setProjects] = useState<Project[]>([]);
  // Only a narrow window reads this (see the 720px media query): a
  // phone shows the list or the thread, never both.
  const [pane, setPane] = useState<"list" | "thread">("list");
  // The Context panel takes over the thread pane for the open session,
  // and closes when a different one is opened.
  const [context, setContext] = useState(false);
  const [wikiRoute, setWikiRoute] = useState<WikiRoute>({ at: "index" });
  // The review count on the nav item. Polled slowly: it changes when an
  // ingest lands, which is minutes apart at the fastest.
  const [wikiFlags, setWikiFlags] = useState(0);
  useEffect(() => {
    const load = () => wikiApi.index()
      .then((ix) => setWikiFlags(ix.health.unsupported + ix.health.superseded + ix.health.uncited))
      .catch(() => setWikiFlags(0));
    load();
    const t = setInterval(load, 60_000);
    return () => clearInterval(t);
  }, []);

  const refresh = useCallback(async () => {
    try {
      const [rs, ps] = await Promise.all([api.sessions(archived), api.projects()]);
      setRows(rs); setProjects(ps); setLoadErr(null);
    } catch (e) { setLoadErr(e instanceof Error ? e.message : String(e)); }
  }, [archived]);

  useEffect(() => { refresh(); const t = setInterval(refresh, POLL_MS); return () => clearInterval(t); }, [refresh]);

  useEffect(() => {
    if (!selected) return;
    let live = true;
    // Never show one session's transcript under another's header while loading.
    setLines([]);
    api.session(selected).then((r) => {
      if (!live) return;
      setLines(r.entries);
      setLoadedFor(selected);
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
    // A catch-up that fails used to wait for the next event, and the last
    // event of a turn has no next one: the transcript stayed short with
    // nothing saying so. Retry on a backoff until one lands.
    let backoff = 4000;
    const catchUp = () => {
      // The fetch used to start inside a setLines updater, which React may
      // run twice. The cursor comes from a ref instead, and the stream
      // boundary is captured when the request starts: a response must not
      // drop fragments that belong to an event after it.
      const since = lastSeq.current;
      const drop = superseded;
      api.session(selected, since).then((r) => {
        if (!live) return;
        backoff = 4000;
        setRows((rs) => rs.map((x) => (x.id === r.session.id ? r.session : x)));
        if (!r.entries.length) return;
        superseded = Math.max(0, superseded - drop);
        // The recorded entries are in hand; the fragments they were
        // built from go in the same commit, so the text is never
        // absent for a frame and never shown twice.
        setStream((cur) => { runs = Math.max(0, runs - drop); return cur.slice(drop); });
        setLines((cur) => {
          const seen = new Set(cur.map((l) => l.seq));
          return [...cur, ...r.entries.filter((e) => !seen.has(e.seq))];
        });
      }).catch(() => {
        if (!live) return;
        clearTimeout(timer);
        timer = setTimeout(catchUp, backoff);
        backoff = Math.min(backoff * 2, 30_000);
      });
    };
    setActivity("");
    const stop = subscribe(selected, (ev) => {
      // The small model's label for what the turn is doing right now. A
      // status, not a record: it changes nothing to catch up on.
      if (ev.kind === "activity") { setActivity(ev.text); return; }
      if (ev.kind === "assistant-delta" || ev.kind === "thinking-delta") {
        setActivity(""); // the program it named is over; its label must not come back
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
      timer = setTimeout(catchUp, 120);
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
  useEffect(() => { if (status && status !== "running") { setStream([]); setActivity(""); } }, [status]);

  const [palette, setPalette] = useState(false);
  // What the palette opens with, when something other than ⌘K opened it
  // (Review's "Search history" hands it the claim).
  const [palQuery, setPalQuery] = useState("");
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
      // A direct link to a page shows that page, on a phone too.
      if (h === "hooks" || h === "projects") { setView(h); setContext(false); setPane("thread"); return; }
      const wr = parseWikiHash(h);
      if (wr) { setView("wiki"); setWikiRoute(wr); setContext(false); setPane("thread"); return; }
      const m = /^s\/([^/]+)(\/context)?$/.exec(h);
      if (m) {
        setView("sessions"); setSelected(m[1]); setContext(Boolean(m[2])); setPane("thread");
      } else if (h === "") {
        // No session named: on a phone that is the list. The thread pane
        // held only "Choose a session", with no list and no way back to it.
        setView("sessions"); setSelected(null); setContext(false); setPane("list");
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
      : view === "wiki" ? `#/${wikiHash(wikiRoute)}`
      : selected ? `#/s/${selected}${context ? "/context" : ""}`
      : "#/";
    if (window.location.hash !== want) {
      window.history.replaceState(null, "", want);
    }
  }, [view, selected, context, wikiRoute]);

  // Moving around the wiki pushes, like opening a conversation: Back
  // from a cited entry returns to the page, and from a page to the index.
  const goWiki = useCallback((r: WikiRoute) => {
    setWikiRoute(r); setView("wiki"); setContext(false); setPane("thread");
    const want = `#/${wikiHash(r)}`;
    if (window.location.hash !== want) window.history.pushState(null, "", want);
  }, []);

  usePaletteKey(useCallback(() => setPalette(true), []));

  // Back on a phone goes to the list and says so in the URL: it only
  // swapped panes, so a reload landed back in the thread it had left.
  const goList = useCallback(() => {
    setSelected(null); setContext(false); setView("sessions"); setPane("list");
    if (window.location.hash !== "#/") window.history.pushState(null, "", "#/");
  }, []);

  // The session last opened, so the list comes back with your place in it.
  const [lastId, setLastId] = useState<string | null>(null);
  // A turn picked from a session's log, for its thread to scroll to.
  const [jump, setJump] = useState<{ id: string; turn: number; at: number } | null>(null);
  useEffect(() => { if (selected) setLastId(selected); }, [selected]);

  // Esc leaves a session for the list, unless something nearer owns it:
  // the composer or a field, a picker, a dialog, the palette.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape" || e.defaultPrevented || palette || view !== "sessions" || !selected) return;
      const a = document.activeElement as HTMLElement | null;
      if (a && (a.tagName === "TEXTAREA" || a.tagName === "INPUT" || a.isContentEditable)) return;
      if (document.querySelector(".dlg-scrim, .sel-pop, .skills-pop, .mention")) return;
      e.preventDefault();
      if (context) setContext(false); else goList();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [palette, view, selected, context, goList]);

  const openSession = useCallback((id: string) => {
    setSelected(id); setContext(false); setView("sessions"); setPane("thread");
    // A push, so Back returns to where you were rather than leaving.
    if (window.location.hash !== `#/s/${id}`) {
      window.history.pushState(null, "", `#/s/${id}`);
    }
  }, []);

  /** A send or answer: its failure is shown beside the composer it came from, not as a toast too. */
  const deliverTo = async (fn: () => Promise<unknown>): Promise<boolean> => {
    setBusy(true);
    try { await fn(); return true; }
    catch { return false; }
    finally { setBusy(false); await refresh(); }
  };

  const act = async (fn: () => Promise<unknown>): Promise<boolean> => {
    setBusy(true);
    try { await fn(); setErr(null); return true; }
    catch (e) { setErr(e instanceof Error ? e.message : String(e)); return false; }
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
      run: async () => {
        const n = await askText("New project", { placeholder: "What is this work?", action: "Create" });
        if (n) act(() => api.newProject(n));
      } },
    { id: "go:sessions", group: "Go to", label: "Sessions", run: () => goList() },
    { id: "go:projects", group: "Go to", label: "Projects",
      run: () => { setView("projects"); setPane("thread"); } },
    { id: "go:hooks", group: "Go to", label: "Hooks",
      run: () => { setView("hooks"); setPane("thread"); } },
    { id: "go:wiki", group: "Go to", label: "Wiki", run: () => goWiki({ at: "index" }) },
    { id: "wiki:review", group: "Wiki", label: "Review flagged claims",
      hint: wikiFlags ? `${wikiFlags} flagged` : undefined, run: () => goWiki({ at: "review" }) },
    { id: "wiki:activity", group: "Wiki", label: "Wiki activity", run: () => goWiki({ at: "activity" }) },
    { id: "wiki:ingest", group: "Wiki", label: "Ingest now",
      hint: "compiles finished sessions into the wiki",
      run: () => act(() => wikiApi.ingest()).then((ok) => { if (ok) goWiki({ at: "activity" }); }) },
    // Draining a backlog one "Mark seen" at a time is a chore; this is the
    // once-a-week sweep, kept off the screen because it is rare.
    ...(rows.some((r) => r.trouble) ? [{
      id: "ack:all", group: "Start", label: `Mark every failure seen (${rows.filter((r) => r.trouble).length})`,
      run: () => act(() => Promise.all(rows.filter((r) => r.trouble).map((r) => api.ack(r.id)))),
    }] : []),
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
      <Palette open={palette} onClose={() => { setPalette(false); setPalQuery(""); }} rows={rows}
               commands={commands} onOpenSession={openSession} initialQuery={palQuery}
               onOpenWikiPage={(path) => goWiki({ at: "page", path })}
               onStart={home ? (text) => start(home, text) : undefined} />
      <Sidebar rows={visible} selected={selected ?? lastId} active={pane === "list"}
               onSelect={openSession} query={query} onQuery={setQuery}
               onTurn={(id, turn) => { if (id !== selected || view !== "sessions" || context) openSession(id); else setPane("thread"); setJump({ id, turn, at: Date.now() }); }}
               view={view} wikiFlags={wikiFlags} onNew={() => setPalette(true)}
               onView={(v) => { if (v === "wiki") goWiki({ at: "index" }); else if (v === "sessions") { setView(v); setContext(false); } else { setView(v); setPane("thread"); } }}
               showArchived={archived} onToggleArchived={() => setArchived((v) => !v)}
               onAck={(id) => act(() => api.ack(id))} />
      {view === "wiki" ? (
        <WikiPage route={wikiRoute} onRoute={goWiki} onBack={goList} onOpenSession={openSession}
                  onSearch={(text) => { setPalQuery(text.replace(/\s+/g, " ").slice(0, 60)); setPalette(true); }} />
      ) : view === "hooks" ? (
        <HooksPage onBack={goList} />
      ) : view === "projects" ? (
        <ProjectsView
          projects={projects} rows={rows}
          onOpen={openSession}
          onBack={goList}
          onAssign={(id, p) => act(() => api.assign(id, p))}
          onCreate={(name) => act(() => api.newProject(name))}
          onRename={(id, name) => act(() => api.renameProject(id, name))}
          onDelete={(id) => act(() => api.deleteProject(id))} />
      ) : row && context ? (
        <ContextPage session={row.id} onBack={() => setContext(false)} />
      ) : row ? (
        <Thread key={row.id} row={row} lines={lines} jump={jump?.id === row.id ? jump : null} loading={loadedFor !== row.id} stream={stream} activity={activity} projects={projects} busy={busy} onBack={goList}
          onSend={(t) => deliverTo(() => api.prompt(row.id, t))}
          onAnswer={(t) => deliverTo(() => api.answer(row.id, t))}
          onInterrupt={() => act(() => api.interrupt(row.id))}
          onArchive={() => act(() => (row.archived ? api.unarchive(row.id) : api.archive(row.id)))}
          onRename={(t) => act(() => api.rename(row.id, t))}
          onModel={(m) => act(() => api.model(row.id, m))}
          onEffort={(e) => act(() => api.effort(row.id, e))}
          onAssign={(p) => act(() => api.assign(row.id, p))}
          onContext={() => setContext(true)}
          onAck={() => act(() => api.ack(row.id))} />
      ) : (
        <div className="thread empty">
          {!selected ? (
            <p>Open a session on the left, or press {modKey()}K to start one.</p>
          ) : rows.length > 0 && (
            // A link to a session this list does not hold.
            <div>
              <h1>Session not found</h1>
              <button className="btn" onClick={goList}>All sessions</button>
            </div>
          )}
        </div>
      )}
      <DialogHost />
      {err ? (
        <button className="toast" role="alert" onClick={() => setErr(null)} title="Dismiss">{err}</button>
      ) : loadErr && <div className="toast" role="status">{loadErr}</div>}
    </div>
  );
}
