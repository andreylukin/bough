import { Fragment, useCallback, useEffect, useId, useLayoutEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { api, subscribe, type Scope, type TurnLine } from "./api";
import type { Line, Project, Row } from "./types";
import { STATUS, StatusMark, Working, hasFailure, hasQuestion, sessionSignal } from "./status";
import { ProjectsView } from "./projects";
import { ModeChip, ModePicker, type ModeValue } from "./mode";
import { Select, type Option } from "./select";
import { DialogHost, askChoice, askConfirm, askText } from "./dialog";
import { Markdown, codeLabel, groupSubs, groupTools, groupTurns, isHookLine, isQuiet, untitled, blank, sessionUsage, usageOf, tokenCount, money, duration, plainTitle, stepCount, stripRunFences, splitBareProgram, foldRetries, foldModelSwitch, execNote, type Item, type SubAgent, type Turn, lineCount } from "./render";
import { Code, parseCall, langForPath, toolCallLabel } from "./code";
import { lastTestRun } from "./runs";
import { agentsFromRows, jobsFromLines, subagentsFromTurn, useReviewed, workCounts, workIndex, type Worker } from "./work";
import { ExecNote, JobLines, JobRow, LIFE_WORD, WorkButton, WorkContext, WorkDialog, WorkState, agentReports, jobIdOf, spokenDuration, splitExecNote, stateText, useChildren, useStopStore, useWork, useWorkAnnouncer, type WorkCtx } from "./work-ui";
import { SkillPicker } from "./skills";
import { Mentions, triggerAt, type Trigger } from "./mention";
import { FireInspection, HooksPage, type Fire, type Load, type Save } from "./hooks";
import { ContextPage } from "./context";
import { ChangesBody, ChangesPage, countOf, useChanges } from "./changes";
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

/** Focus a region's own stop: the composer, the tree's current row, the transcript, else its first control. */
function focusRegion(r: HTMLElement) {
  const el = r.querySelector<HTMLElement>("#composer, [role=tree] [tabindex='0']")
    ?? (r.tabIndex >= 0 ? r : [...r.querySelectorAll<HTMLElement>("button:not([disabled]), a[href], input, [tabindex='0']")].find((c) => c.offsetParent));
  (el ?? r).focus();
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
  return sessionSignal(a) - sessionSignal(b) || Date.parse(b.lastAt) - Date.parse(a.lastAt);
}

/**
 * Where the query hits a row, the one answer for filtering and marking:
 * the title, else the first other field that holds it.
 */
function getSearchMatch(r: Row, q: string): { field: "title" | "branch" | "repo" | "path" | "id"; text: string; at: number } | undefined {
  if (!q) return undefined;
  const fields = [["title", displayTitle(r)], ["branch", r.branch], ["repo", r.repo], ["path", r.cwd], ["id", r.id]] as const;
  for (const [field, text] of fields) {
    const at = text ? text.toLowerCase().indexOf(q) : -1;
    if (at >= 0) return { field, text: text!, at };
  }
  return undefined;
}

/** The title a row shows: the ask when it says what the session is, else the title. */
function displayTitle(r: Row): string {
  const name = plainTitle(r.title);
  return r.ask && name && askSaysTitle(r.ask.text, name) ? plainTitle(r.ask.text) : name;
}

/** Text cut to start at most `lead` characters before the hit, so an ellipsis never hides it. */
function excerpt(text: string, at: number, lead: number): { text: string; at: number } {
  if (at <= lead) return { text, at };
  // Cut at the word or path segment the hit sits in, when that is near.
  const cut = Math.max(text.lastIndexOf(" ", at - 1), text.lastIndexOf("/", at - 1));
  const from = cut >= 0 && at - cut <= lead + 12 ? cut + 1 : at - lead;
  return { text: "…" + text.slice(from), at: at - from + 1 };
}

/** A media query as state, following the window as it changes. */
export function useMedia(query: string): boolean {
  const [on, setOn] = useState(() => typeof window !== "undefined" && Boolean(window.matchMedia?.(query).matches));
  useEffect(() => {
    const m = window.matchMedia?.(query);
    if (!m) return;
    const sync = () => setOn(m.matches);
    sync();
    m.addEventListener("change", sync);
    return () => m.removeEventListener("change", sync);
  }, [query]);
  return on;
}

/** Rows under their workspace, workspaces by what needs you, then their latest activity. */
function byWorkspace(rows: Row[]): [string, Row[]][] {
  // Grouped by the whole path, so two checkouts named alike stay apart;
  // only then does a name take its parent to tell them apart.
  const out = new Map<string, Row[]>();
  for (const r of rows) {
    const k = r.repo || r.cwd;
    if (!out.has(k)) out.set(k, []);
    out.get(k)!.push(r);
  }
  const names = new Map<string, number>();
  for (const list of out.values()) names.set(workspaceOf(list[0]), (names.get(workspaceOf(list[0])) ?? 0) + 1);
  const label = (r: Row) => {
    const name = workspaceOf(r);
    if ((names.get(name) ?? 0) < 2) return name;
    return (r.repo || r.cwd).split("/").filter(Boolean).slice(-2).join("/");
  };
  const latest = (list: Row[]) => Math.max(...list.map((r) => Date.parse(r.lastAt)));
  const signal = (list: Row[]) => Math.min(...list.map(sessionSignal));
  return [...out.values()]
    .map((list) => [label(list[0]), list.sort(byUrgency)] as [string, Row[]])
    .sort((a, b) => signal(a[1]) - signal(b[1]) || latest(b[1]) - latest(a[1]));
}

/** What the sidebar's arrows walk; one of them at a time is the tab stop. */
const TREE_ITEMS = "button.sec-fold, button.ws-head, button.row, button.turn-line";

/** The query's first match in text, marked. */
function marked(text: string, q: string): React.ReactNode {
  const i = q ? text.toLowerCase().indexOf(q) : -1;
  if (i < 0) return text;
  return <>{text.slice(0, i)}<mark className="hit">{text.slice(i, i + q.length)}</mark>{text.slice(i + q.length)}</>;
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

export function Sidebar({ rows, selected, onSelect, onTurn, query, onQuery, showArchived, onToggleArchived, archivedState = "ready", onRetryArchived, view = "sessions", onView, wikiFlags = 0, onNew, onFind, onAck, active = true, onShowList, reveal, loadedAt = 0, loadErr = null, onRetry }: {
  rows: Row[]; selected: string | null; onSelect: (id: string) => void;
  /** The fleet's freshness: when the list last loaded (null before), and why the last refresh failed. */
  loadedAt?: number | null; loadErr?: string | null; onRetry?: () => void;
  /** Whether the rows hold archived sessions yet, once the section is open. */
  archivedState?: "loading" | "failed" | "ready"; onRetryArchived?: () => void;
  /** Open a session at one turn of its log. */
  onTurn?: (id: string, turn: number) => void;
  /** showArchived is the Archived section being open: the rows then include archived ones. */
  query: string; onQuery: (q: string) => void; showArchived: boolean; onToggleArchived: () => void;
  view?: View; onView?: (v: View) => void;
  /** Claims the wiki's review is waiting on; shown beside the nav item. */
  wikiFlags?: number;
  /** Starting work is the other half of a control room; it opens the palette's Start group. */
  onNew?: () => void;
  /** Search from the folded rail: the palette, so opening a result never unfolds it. */
  onFind?: () => void;
  /** Mark a troubled session seen without opening it. */
  onAck?: (id: string) => void;
  /** Whether the list is on screen; coming back to it puts focus on the row you left. */
  active?: boolean;
  /** Put the list on screen (a phone shows it or the thread, not both). */
  onShowList?: () => void;
  /** A row to bring into view and focus: its groups open, the search cleared. */
  reveal?: { id: string; at: number } | null;
}) {
  // Status lives in the glyphs and the order; the sections are only
  // where a session ran, and whether it is still recent.
  // Runs nobody started by hand always fold into Background — the person
  // chose that; one that needs attention lights the section header instead.
  const { recent, inactive, background, archived, kids } = useMemo(() => {
    const now = Date.now();
    const recent: Row[] = [], inactive: Row[] = [], background: Row[] = [], archived: Row[] = [];
    // A background agent is not a row of its own: its parent's row counts
    // the running ones, and the parent's Work panel lists them. Only an
    // agent whose parent is gone from the list keeps a row, so none is lost.
    const byId = new Map(rows.map((r) => [r.id, r]));
    const kids = new Map<string, Row[]>();
    for (const r of rows) {
      const parent = r.spawnedBy ? byId.get(r.spawnedBy) : undefined;
      if (parent) {
        kids.set(parent.id, [...(kids.get(parent.id) ?? []), r]);
        continue;
      }
      // A session opened and never sent a message holds nothing to go back
      // to. It shows while it is open, while its child is up (one just made
      // with New), or when a search asks for it.
      // A project session whose orb failed to start is empty and dead too,
      // but hiding it would swallow the failure and the prompt it lost.
      if (r.empty && !r.live && r.orb?.status !== "failed" && r.id !== selected && !query) continue;
      if (r.archived) archived.push(r);
      else if (r.background) background.push(r);
      else if (sessionSignal(r) < 2 || now - Date.parse(r.lastAt) < INACTIVE_MS) recent.push(r);
      else inactive.push(r);
    }
    return { recent: byWorkspace(recent), inactive, background, archived, kids };
  }, [rows, selected, query]);

  // Inactive stays shut until asked, and the way you left it across reloads.
  const [unfolded, setUnfolded] = useState<Set<string>>(() => readSet("bough:unfolded"));
  const toggleFold = (name: string) => setUnfolded((cur) => {
    const next = new Set(cur);
    if (next.has(name)) next.delete(name); else next.add(name);
    writeSet("bough:unfolded", next);
    return next;
  });
  // Background opens itself once, the first time it holds something running
  // or failed; after that it stays the way you leave it.
  const bgRunning = background.filter((r) => r.status === "running").length;
  const bgFailed = background.filter(hasFailure).length;
  useEffect(() => {
    if (!bgRunning && !bgFailed) return;
    try { if (localStorage.getItem("bough:bg-auto") === "1") return; localStorage.setItem("bough:bg-auto", "1"); } catch { return; }
    setUnfolded((cur) => { if (cur.has("background")) return cur; const next = new Set(cur).add("background"); writeSet("bough:unfolded", next); return next; });
  }, [bgRunning > 0 || bgFailed > 0]); // eslint-disable-line react-hooks/exhaustive-deps

  // On a desktop the whole sidebar folds away to a rail, and stays folded.
  const narrow = useMedia("(max-width:720px)");
  const [closed, setClosed] = useState(() => { try { return localStorage.getItem("bough:side-closed") === "1"; } catch { return false; } });
  const setSide = (c: boolean) => {
    setClosed(c);
    try { localStorage.setItem("bough:side-closed", c ? "1" : "0"); } catch { /* storage off */ }
  };
  // ⌘B (Ctrl+B) folds and unfolds, never inside a text field; the palette sends the same event.
  const closedRef = useRef(closed);
  closedRef.current = closed;
  useEffect(() => {
    const flipSide = () => setSide(!closedRef.current);
    const onKey = (e: KeyboardEvent) => {
      if (!(e.metaKey || e.ctrlKey) || e.altKey || e.shiftKey || e.key.toLowerCase() !== "b") return;
      const t = e.target as HTMLElement | null;
      if (t && (t.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(t.tagName))) return;
      e.preventDefault();
      flipSide();
    };
    window.addEventListener("keydown", onKey);
    window.addEventListener("bough:toggle-side", flipSide);
    return () => { window.removeEventListener("keydown", onKey); window.removeEventListener("bough:toggle-side", flipSide); };
  }, []);

  // Search sits behind the toolbar; a filter in force keeps it open.
  const [searching, setSearching] = useState(false);
  const searchRef = useRef<HTMLInputElement>(null);
  const showSearch = searching || Boolean(query);
  useEffect(() => { if (searching) searchRef.current?.focus(); }, [searching]);

  // A session's turn log, one line a turn, opens under its row when asked,
  // one at a time: the transcript already shows the open session's turns.
  const [expanded, setExpanded] = useState<Set<string>>(() => new Set([...readSet("bough:turns-open")].slice(-1)));
  const setOpen = useCallback((id: string, open: boolean) => setExpanded((cur) => {
    if (cur.has(id) === open) return cur;
    const next = new Set(open ? [id] : []);
    writeSet("bough:turns-open", next);
    return next;
  }), []);

  // Fetched on first open, kept per session, and read again once the
  // session has changed since.
  const [logs, setLogs] = useState<Record<string, { at: string; lines?: TurnLine[] }>>({});
  const fetching = useRef(new Set<string>());
  useEffect(() => {
    for (const r of rows) {
      if (!r.turns || !expanded.has(r.id) || logs[r.id]?.at === r.modified || fetching.current.has(r.id)) continue;
      const at = r.modified;
      fetching.current.add(r.id);
      api.turns(r.id)
        .then((lines) => setLogs((m) => ({ ...m, [r.id]: { at, lines } })))
        // No lines at this version says it failed; Retry forgets it.
        .catch(() => setLogs((m) => ({ ...m, [r.id]: { at } })))
        .finally(() => fetching.current.delete(r.id));
    }
  }, [rows, expanded, logs]);

  // What a session is about, on hover or focus: the title names it, the
  // summary says where it stands. One card for the whole list, fixed to
  // the viewport beside the row, because the list scrolls and would clip
  // anything hung off a row. It waits a beat so skimming does not flash it.
  const [card, setCard] = useState<{ id: string; top: number; left: number } | null>(null);
  const peekTimer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  const quiet = useRef(false);
  const peek = (r: Row, el: HTMLElement) => {
    clearTimeout(peekTimer.current);
    if (quiet.current) { setCard(null); return; }
    const rect = el.getBoundingClientRect();
    // Beside the row, so it never covers the turn log under it.
    peekTimer.current = setTimeout(() => setCard({
      id: r.id,
      top: Math.max(8, Math.min(rect.top, window.innerHeight - 200)),
      left: Math.min(rect.right + 8, window.innerWidth - 372),
    }), 300);
  };
  const unpeek = () => { clearTimeout(peekTimer.current); peekTimer.current = setTimeout(() => setCard(null), 150); };
  // A card for the session you just left is stale.
  useEffect(() => { clearTimeout(peekTimer.current); setCard(null); }, [selected]);

  // Workspaces fold too, kept across reloads; a search folds on its own
  // and forgets it when cleared, so 54 Background hits can be put away.
  const [wsFolded, setWsFolded] = useState<Set<string>>(() => readSet("bough:ws-folded"));
  const [searchFolds, setSearchFolds] = useState<Set<string>>(() => new Set());
  const searchOn = Boolean(query.trim());
  useEffect(() => { if (!searchOn) setSearchFolds(new Set()); }, [searchOn]);
  const flip = (set: Set<string>, key: string) => { const next = new Set(set); if (next.has(key)) next.delete(key); else next.add(key); return next; };
  const toggleWs = (key: string) => {
    if (searchOn) { setSearchFolds((cur) => flip(cur, key)); return; }
    setWsFolded((cur) => { const next = flip(cur, key); writeSet("bough:ws-folded", next); return next; });
  };

  const [archFolded, setArchFolded] = useState(false);
  // Showing archived from anywhere (the palette too) shows the section open.
  useEffect(() => { if (showArchived) setArchFolded(false); }, [showArchived]);

  // A first load says so only once it is slow enough to notice.
  const [slow, setSlow] = useState(false);
  useEffect(() => {
    if (loadedAt !== null) return;
    const t = setTimeout(() => setSlow(true), 200);
    return () => clearTimeout(t);
  }, [loadedAt]);
  const searchBtn = useRef<HTMLButtonElement>(null);

  // A long log shows its last turns; the rest wait behind one line.
  const [allTurns, setAllTurns] = useState<Set<string>>(() => new Set());

  // One tab stop in the tree: the item last focused, else the open row,
  // else the first. The arrows, Home and End move within it.
  const treeRef = useRef<HTMLDivElement>(null);
  const stop = useRef<HTMLElement | null>(null);
  useLayoutEffect(() => {
    const items = [...(treeRef.current?.querySelectorAll<HTMLElement>(TREE_ITEMS) ?? [])];
    const pick = (stop.current && items.includes(stop.current) ? stop.current : null)
      ?? items.find((el) => el.classList.contains("row-on")) ?? items[0];
    for (const el of items) el.tabIndex = el === pick ? 0 : -1;
    stop.current = pick ?? null;
  });

  // "/" opens search from anywhere that is not taking typing.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "/" || e.metaKey || e.ctrlKey || e.altKey) return;
      const t = e.target as HTMLElement | null;
      if (t && (t.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(t.tagName))) return;
      e.preventDefault();
      // A folded rail or a phone showing the thread has no search to focus: show the list first.
      setClosed(false);
      try { localStorage.setItem("bough:side-closed", "0"); } catch { /* storage off */ }
      showList.current?.();
      setSearching(true);
      requestAnimationFrame(() => searchRef.current?.focus());
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, []);
  const showList = useRef(onShowList);
  showList.current = onShowList;

  // Home points at a row: open the sidebar and the row's groups, clear the
  // search, and put focus on the row itself.
  useEffect(() => {
    if (!reveal) return;
    const r = rows.find((x) => x.id === reveal.id);
    if (!r) return;
    setSide(false);
    onQuery("");
    const path = r.repo || r.cwd;
    setWsFolded((cur) => { const next = new Set([...cur].filter((k) => !k.endsWith(":" + path))); writeSet("bough:ws-folded", next); return next; });
    const sec = r.background ? "background" : sessionSignal(r) < 2 || Date.now() - Date.parse(r.lastAt) < INACTIVE_MS ? "" : "inactive";
    if (sec) setUnfolded((cur) => { const next = new Set(cur).add(sec); writeSet("bough:unfolded", next); return next; });
    requestAnimationFrame(() => {
      const el = document.querySelector<HTMLElement>(`.sidebar button.row[data-id="${r.id}"]`);
      el?.scrollIntoView({ block: "nearest" });
      el?.focus();
    });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [reveal]);
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
    if (!["ArrowDown", "ArrowUp", "ArrowLeft", "ArrowRight", "Home", "End"].includes(e.key)) return;
    const items = [...e.currentTarget.querySelectorAll<HTMLElement>(TREE_ITEMS)];
    if (!items.length) return;
    const at = document.activeElement as HTMLElement | null;
    const cur = at?.closest(".row-wrap")?.querySelector<HTMLElement>("button.row") ?? at;
    const go = (el?: HTMLElement | null) => { if (el) { el.focus(); el.scrollIntoView({ block: "nearest" }); } };
    e.preventDefault();
    if (e.key === "Home" || e.key === "End") { go(items[e.key === "Home" ? 0 : items.length - 1]); return; }
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      const i = cur ? items.indexOf(cur) : -1;
      go(items[i < 0 ? (e.key === "ArrowDown" ? 0 : items.length - 1)
        : Math.max(0, Math.min(items.length - 1, i + (e.key === "ArrowDown" ? 1 : -1)))]);
      return;
    }
    if (!cur) return;
    const right = e.key === "ArrowRight";
    if (cur.classList.contains("sec-fold") || cur.classList.contains("ws-head")) {
      const open = cur.getAttribute("aria-expanded") === "true";
      // Right on an open group steps into its first child; left on a shut one steps out.
      // A section's → shares its click's one state: it opens, and never moves on.
      if (right && open && cur.classList.contains("sec-fold")) return;
      if (right && open) go(items[items.indexOf(cur) + 1]);
      else if (right || open) cur.click();
      else if (cur.classList.contains("ws-head")) go(cur.closest(".sec")?.querySelector<HTMLElement>("button.sec-fold"));
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
    else go(cur.closest(".ws")?.querySelector<HTMLElement>("button.ws-head") ?? cur.closest(".sec")?.querySelector<HTMLElement>("button.sec-fold"));
  };

  const q = query.trim().toLowerCase();
  // `twin`: a sibling row reads the same, so this one adds its id tail.
  // `child`: a background agent under the session that started it.
  const session = (r: Row, twin = false, child = false): React.ReactNode => {
    // A search hides the logs: they are not what matched.
    const open = Boolean(r.turns) && expanded.has(r.id) && !q;
    const log = logs[r.id];
    const name = plainTitle(r.title);
    const on = r.id === selected;
    // A recorded failure outranks the lifecycle: finished is not fine.
    const failed = r.trouble || (r.testsFailed ? "tests failed" : "") || (hasFailure(r) ? "failed" : "");
    // A failure and a pending ask are both news: say both.
    const asking = hasQuestion(r);
    const why = failed
      ? `${capital(failed)}${asking ? "; waiting for your answer" : failed === "tests failed" ? `; agent ${(STATUS[r.status]?.label ?? r.status).toLowerCase()}` : ""}`
      : STATUS[r.status]?.label ?? r.status;
    // Where the query hit, kept on screen: a late title hit shifts the
    // title, a hit elsewhere gets its own line centred on it.
    const hit = getSearchMatch(r, q);
    // A late hit starts its excerpt a few characters before it, so a narrow row still shows it.
    const title = displayTitle(r) || (child && r.status === "queued" ? "Queued agent" : "");
    const shown = hit?.field === "title" && hit.at > 24 ? excerpt(title, hit.at, 6).text : title;
    const reason = hit && hit.field !== "title" ? excerpt(hit.text, hit.at, 6) : undefined;
    // Done is what the check mark already says; the row keeps only the age then.
    // A background agent says its own lifecycle once, in its words; a parent says what its agents are doing.
    const life = child ? agentsFromRows({ ...r, id: r.spawnedBy ?? "" }, [r])[0]?.life ?? "unknown" : undefined;
    const label = child ? "" : failed ? capital(failed) + (asking ? "; waiting for you" : "") : r.status === "done" ? "" : STATUS[r.status]?.label ?? r.status;
    const lines = log?.lines && !allTurns.has(r.id) && log.lines.length > 3 ? log.lines.slice(-3) : log?.lines;
    const children = kids.get(r.id);
    const bg = children ? workCounts(agentsFromRows(r, children)) : null;
    // Only what is live or needs a look: the count of running agents, and failures.
    const bgText = bg ? [bg.running && `${bg.running} running`, bg.failed && `${bg.failed} failed`].filter(Boolean).join(" · ") : "";
    const stacked = Boolean(label || life || bgText);
    return (
      <Fragment key={r.id}>
      <div className={"session" + (child ? " session-child" : "")}>
        <div className={"row-wrap" + (open ? " row-open" : "")}>
          <button role="treeitem" onClick={() => onSelect(r.id)} data-id={r.id}
                  onMouseEnter={(e) => peek(r, e.currentTarget)} onMouseLeave={unpeek}
                  onBlur={unpeek}
                  aria-describedby={card?.id === r.id ? "row-card" : undefined}
                  aria-label={`${title || "Untitled session"}, ${life ? LIFE_WORD[life] : why}, ${ago(r.lastAt)} ago${r.branch ? `, branch ${r.branch}` : ""}${bgText ? `, background: ${bgText}` : ""}`}
                  className={"row" + (stacked ? " row-2" : "") + (on ? " row-on" : "") + (r.turns && !q ? " row-has-log" : "") + (r.trouble && onAck ? " row-has-ack" : "")}
                  aria-current={on ? "true" : undefined}
                  title={`${why} · ${ago(r.lastAt)} ago${r.branch ? ` · ${r.branch}` : ""}`}>
            {/* A failure you have not seen is a red mark; the reason is its label. */}
            <span className="row-mark">
              {failed
                ? <span className="status"><svg width={16} height={16} viewBox="0 0 24 24" fill="none" stroke="var(--red)" strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">{STATUS.error.glyph}</svg><span className="visually-hidden">{why}</span></span>
                : <StatusMark status={r.status} size={16} bare />}
            </span>
            {/* Status metadata goes under the title, so a chip never cuts the name. */}
            <span className={stacked ? "row-stack" : "row-line"}>
            <span className="row-name">
            {title
              ? <><span className="row-title" title={shown !== title ? title : undefined}>{marked(shown, q)}</span>{twin && <span className="mono row-id">{r.id.slice(-6)}</span>}</>
              // No title: the id tail alone tells rows apart.
              : <span className="row-title mono row-untitled">{r.id.slice(-6)}</span>}
            </span>
            {label && <span className={"num row-meta" + (failed ? " row-meta-bad" : asking ? " row-meta-ask" : "") + (failed || asking || r.status === "running" ? " row-meta-live" : "")} aria-hidden="true">
              <ModeChip row={r} />{label} · {ago(failed === "tests failed" && r.testsAt ? r.testsAt : r.lastAt)}
            </span>}
            {/* State and time, never cut: the title is what gives way. */}
            {life && <span className={"num row-meta row-meta-live" + (life === "failed" ? " row-meta-bad" : "")} aria-hidden="true">
              <ModeChip row={r} />{LIFE_WORD[life]} · {life === "queued" ? `waiting ${ago(r.lastAt)}` : ago(r.lastAt)}
            </span>}
            {bgText && <span className="bg-summary" aria-hidden="true">Background: {bgText}</span>}
            </span>
            {r.jobs && r.jobs.length > 0 && (
              <span className="num row-jobs" title={r.jobs.map((j) => j.cmd).join("\n")}>
                {r.jobs.length}<span className="visually-hidden"> {r.jobs.length === 1 ? "job" : "jobs"}</span>
              </span>
            )}
            {/* Touch has no hover card: a phone reads status and age off the row. */}
            {!label && !life && <span className={"num row-meta" + (failed ? " row-meta-bad" : asking ? " row-meta-ask" : "") + (failed || asking || r.status === "running" ? " row-meta-live" : "")} aria-hidden="true">
              <ModeChip row={r} />{ago(r.lastAt)}
            </span>}
          </button>
          {/* The disclosure and Seen are siblings of the row, not inside
              it: a button in a button is invalid and would open it too. */}
          {r.trouble && onAck && (
            <button className="btn row-ack" onClick={() => onAck(r.id)} aria-label={`Mark ${name || "session"} seen`}>Seen</button>
          )}
          {r.turns && !q ? (
            <button className="row-twist" tabIndex={-1} aria-expanded={open} aria-controls={`turns-${r.id}`}
                    aria-label={`${open ? "Hide" : "Show"} turn log of ${name || "session"}`}
                    onClick={() => setOpen(r.id, !open)}><Icon d={ICONS.chevron} size={14} /></button>
          ) : null}
        </div>
        {reason && <div className="row-why">{hit!.field}: {marked(reason.text, q)}</div>}
        {open && (
          <ol id={`turns-${r.id}`} className="turns" aria-label={`Turns of ${name || "session"}`}>
            {log?.lines && log.lines.length > 3 && (
              <li>
                <button className="turn-line turn-more" tabIndex={-1} aria-expanded={lines === log.lines}
                        onClick={() => setAllTurns((cur) => flip(cur, r.id))}>
                  <Icon d={ICONS.chevron} size={12} /><span className="turn-text">{lines === log.lines ? "Hide earlier turns" : `Earlier turns · ${log.lines.length - 3}`}</span>
                </button>
              </li>
            )}
            {lines ? lines.map((l) => (
              <li key={l.turn}>
                <button className="turn-line" tabIndex={-1} title={l.test ? `${l.test.cmd} exited ${l.test.exit}\n${l.text}` : l.text} onClick={() => onTurn?.(r.id, l.turn)}>
                  <span className="num turn-n">{l.turn}</span>
                  {/* A recorded test run says what happened first; narration only where nothing was recorded. */}
                  {l.test && <span className={"turn-result" + (l.test.exit ? " turn-result-bad" : "")}>{l.test.exit ? `Tests failed · exit ${l.test.exit}` : "Tests passed"}</span>}
                  {/* Narration is the agent's note, not a status: dimmed so it never reads as one. */}
                  <span className={"turn-text" + (l.test ? "" : " turn-note")}>{l.test ? l.test.cmd : l.text.replace(/^you (asked|said|wanted)( to| for| that)?\s+/i, "").replace(/^./, (c) => c.toUpperCase())}</span>
                </button>
              </li>
            )) : log ? (
              <li className="turn-wait">Couldn’t load turns · <button className="link" onClick={() => setLogs((m) => { const { [r.id]: _, ...rest } = m; return rest; })}>Retry</button></li>
            ) : <li className="turn-wait">Loading…</li>}
          </ol>
        )}
      </div>
      </Fragment>
    );
  };

  const workspaces = (groups: [string, Row[]][], sec: string) => groups.map(([ws, list]) => {
    const key = `${sec}:${list[0].repo || list[0].cwd}`;
    const open = !(searchOn ? searchFolds : wsFolded).has(key);
    const urgent = list.filter((r) => sessionSignal(r) === 0);
    const seen = new Map<string, number>();
    for (const r of list) seen.set(displayTitle(r), (seen.get(displayTitle(r)) ?? 0) + 1);
    return (
      <div key={key} className="ws">
        <button className="ws-head" role="treeitem" aria-expanded={open} onClick={() => toggleWs(key)} title={list[0].repo || list[0].cwd}>
          <Icon d={ICONS.chevron} size={12} /><Icon d={ICONS.folder} size={15} /><span className="ws-name">{ws}</span>
          {/* Folded, a group still says when something in it needs you. */}
          {!open && (urgent.length
            ? <span className={"num sec-count sec-count-" + (urgent.some(hasFailure) ? "trouble" : "needs-you")}>{urgent.length} need{urgent.length === 1 ? "s" : ""} you</span>
            : <span className="num sec-count">{list.length}</span>)}
        </button>
        {open && <div role="group">{list.map((r) => session(r, (seen.get(displayTitle(r)) ?? 0) > 1))}</div>}
      </div>
    );
  });

  // A section folds, during a search too, which starts with every match open.
  // `sub` is a second line under the heading ("1 running · 2 failed").
  const section = (key: string, label: string, open: boolean, toggle: () => void, count: React.ReactNode, body: React.ReactNode, alert?: "trouble" | "needs-you", sub?: string) => (
    <div className="sec">
      <button type="button" className={"sec-fold" + (sub ? " sec-fold-2" : "")} role="treeitem" aria-expanded={open} onClick={toggle} aria-controls={`sec-${key}`}>
        <Icon d={ICONS.chevron} size={12} />
        {sub ? <span className="sec-stack"><span>{label}</span><span className="bg-summary">{sub}</span></span> : <span>{label}</span>}
        <span className="ws-rule" />{count !== null && <span className={"num sec-count" + (alert ? " sec-count-" + alert : "")}>{count}</span>}
      </button>
      {open && <div className="sec-body" role="group" id={`sec-${key}`}>{body}</div>}
    </div>
  );

  // A phone has no room to fold the list into: there the list is the pane,
  // and its toolbar stays whole whatever a desktop left saved.
  const folded = closed && !narrow;
  const toolbar = (
    <div className="side-bar">
      <button className="side-icon side-collapse" onClick={() => setSide(!closed)} aria-expanded={!closed}
              aria-label={closed ? "Show sidebar" : "Hide sidebar"} title={`${closed ? "Show sidebar" : "Hide sidebar"} (${modKey()}B)`}>
        <Icon d={ICONS.panel} />
      </button>
      {folded ? <>
        {/* The rail keeps what the toolbar does; search is the palette, which leaves the rail folded. */}
        {onFind && <button className="side-icon" onClick={onFind} aria-label="Search sessions" title={`Search sessions (${modKey()}K)`}><Icon d={ICONS.search} /></button>}
        {onNew && <button className="side-new" onClick={onNew} aria-label="New session" title="New session"><Icon d={ICONS.compose} /></button>}
        <button className="side-icon" onClick={() => window.history.back()} aria-label="Back" title="Back"><Icon d={ICONS.back} /></button>
        <button className="side-icon" onClick={() => window.history.forward()} aria-label="Forward" title="Forward"><Icon d={ICONS.forward} /></button>
      </> : <>
        <button className="side-icon" onClick={() => window.history.back()} aria-label="Back" title="Back"><Icon d={ICONS.back} /></button>
        <button className="side-icon" onClick={() => window.history.forward()} aria-label="Forward" title="Forward"><Icon d={ICONS.forward} /></button>
        <button ref={searchBtn} className={"side-icon" + (showSearch ? " side-icon-on" : "")} aria-expanded={showSearch} aria-controls="q"
                onClick={() => { if (showSearch) { onQuery(""); setSearching(false); } else setSearching(true); }}
                aria-label="Search sessions" title="Search sessions (/)"><Icon d={ICONS.search} /></button>
        {onNew && (
          <button className="side-new" onClick={onNew} aria-label="New session" title="New session">
            <Icon d={ICONS.compose} />
          </button>
        )}
      </>}
    </div>
  );

  const nav = onView && <ViewNav view={view} onView={onView} wikiFlags={wikiFlags} icons={folded} />;
  if (folded) return <div className="sidebar sidebar-closed">{toolbar}<div className="rail-gap" />{nav}</div>;

  const total = recent.length + inactive.length + background.length + archived.length;
  // While something in Background needs you, its count says how many, not the total.
  const bgUrgent = background.filter((r) => sessionSignal(r) === 0);
  const bgAlert = background.some(hasFailure) ? "trouble" : bgUrgent.length ? "needs-you" : undefined;
  const bgSub = [bgRunning && `${bgRunning} running`, bgFailed && `${bgFailed} failed`].filter(Boolean).join(" · ");
  const foldOpen = (key: string, open: boolean) => (searchOn ? !searchFolds.has(key) : open);
  const foldToggle = (key: string, toggle: () => void) => () => (searchOn ? setSearchFolds((cur) => flip(cur, key)) : toggle());
  const cardRow = card ? rows.find((r) => r.id === card.id) : undefined;
  return (
    <div className="sidebar">
      {cardRow && card && (
        // Status from the recorded fields; the agent's own note is labelled
        // as that, so prose that went stale never reads as the state.
        <div id="row-card" className="row-card" role="tooltip" style={{ top: card.top, left: card.left }}>
          <div className="row-card-state">
            {cardRow.trouble || cardRow.testsFailed
              ? <span className="status head-trouble"><StatusMark status="error" bare />{capital(cardRow.trouble || "tests failed")}</span>
              : <StatusMark status={cardRow.status} />}
            <span className="num">{ago(cardRow.lastAt)} ago</span>
            {cardRow.branch && <span className="mono">{cardRow.branch}</span>}
          </div>
          {cardRow.summary && <p className="row-card-note"><span>Agent’s last note</span>{cardRow.summary}</p>}
        </div>
      )}
      {toolbar}
      {showSearch && (
        <div className="session-search">
          <label htmlFor="q" className="visually-hidden">Search sessions</label>
          <input id="q" ref={searchRef} className="field" autoComplete="off" value={query} placeholder="Search sessions"
                 onChange={(e) => onQuery(e.target.value)}
                 onKeyDown={(e) => {
                   if (e.key === "Escape") { e.preventDefault(); onQuery(""); setSearching(false); searchBtn.current?.focus(); return; }
                   // Down from the search lands on the first match.
                   if (e.key !== "ArrowDown") return;
                   const first = document.querySelector<HTMLElement>(".sidebar button.row");
                   if (first) { e.preventDefault(); first.focus(); }
                 }} />
        </div>
      )}
      {/* Only on trouble: a slow first load, or a list that stopped refreshing. */}
      {loadErr ? (
        <p className="side-fresh" role="status">
          {loadedAt === null ? "Sessions unavailable" : `Updates delayed · ${ago(new Date(loadedAt).toISOString())}`}
          {onRetry && <button className="link" onClick={onRetry}>Retry</button>}
        </p>
      ) : loadedAt === null && slow && <p className="side-fresh" role="status">Loading sessions…</p>}
      <div className="scroll" role="tree" aria-label="Sessions" ref={treeRef} onScroll={() => { clearTimeout(peekTimer.current); setCard(null); }} onKeyDown={walk}
           onFocus={(e) => {
             const t = e.target as HTMLElement;
             if (!t.matches(TREE_ITEMS)) return;
             // The tab stop follows focus now, not on the next render.
             if (stop.current && stop.current !== t) stop.current.tabIndex = -1;
             t.tabIndex = 0;
             stop.current = t;
           }}>
        {total === 0 && !(showArchived && archivedState !== "ready") && (
          <p className="list-none">{query ? `No sessions match “${query}”.` : "No sessions yet."}</p>
        )}
        {workspaces(recent, "recent")}
        {inactive.length > 0 && section("inactive", "Inactive · 72h+", foldOpen("inactive", unfolded.has("inactive")), foldToggle("inactive", () => toggleFold("inactive")),
          inactive.length, workspaces(byWorkspace(inactive), "inactive"))}
        {background.length > 0 && section("background", "Background", foldOpen("background", unfolded.has("background")), foldToggle("background", () => toggleFold("background")),
          bgUrgent.length ? <span title={`${background.length} in all`}>{bgUrgent.length} need you</span> : background.length, workspaces(byWorkspace(background), "background"), bgAlert, bgSub || undefined)}
        {/* Archived is not loaded until opened, so a search cannot have looked there. */}
        {section("archived", q && !showArchived ? "Archived not searched · Include" : "Archived", showArchived && !archFolded,
          // Once included, folding only hides the section; a search still covers it.
          () => (showArchived ? setArchFolded((v) => !v) : (setArchFolded(false), onToggleArchived())),
          showArchived && archivedState === "ready" ? archived.length : null,
          archivedState === "loading" ? <p className="list-none">Loading archived…</p>
          : archivedState === "failed" ? <p className="list-none">Couldn’t load archived · <button className="link" onClick={onRetryArchived}>Retry</button></p>
          : archived.length ? workspaces(byWorkspace(archived), "archived") : <p className="list-none">{q ? "No archived matches." : "Nothing archived."}</p>)}
      </div>
      {nav}
    </div>
  );
}

const NAV_HREF: Record<View, string> = { sessions: "#/", projects: "#/projects", hooks: "#/hooks", wiki: "#/wiki" };

/**
 * The pages beside sessions, as links: they have routes, so they open in a
 * new tab and copy as a URL. A plain click still goes through onView, which
 * pushes history the way the rest of the app does. `phone` adds Sessions:
 * there it is the shell's bottom bar, not the sidebar's foot.
 */
export function ViewNav({ view, onView, wikiFlags = 0, icons = false, phone = false }: {
  view: View; onView: (v: View) => void; wikiFlags?: number; icons?: boolean; phone?: boolean;
}) {
  const items = ([["sessions", "Sessions", ICONS.search], ["projects", "Projects", ICONS.projects], ["hooks", "Hooks", ICONS.hooks], ["wiki", "Wiki", ICONS.wiki]] as const)
    .filter(([v]) => phone || v !== "sessions");
  return (
    <nav className={phone ? "side-nav phone-nav" : "side-nav" + (icons ? " side-nav-rail" : "")} aria-label="Views">
      {items.map(([v, label, icon]) => (
        <a key={v} href={NAV_HREF[v]} className={"side-nav-item" + (view === v ? " side-nav-on" : "")}
           aria-current={view === v ? "page" : undefined} title={icons ? label : undefined}
           onClick={(e) => { if (e.metaKey || e.ctrlKey || e.shiftKey || e.button !== 0) return; e.preventDefault(); onView(v); }}>
          <Icon d={icon} size={20} />
          <span className={icons ? "visually-hidden" : undefined}>{label}</span>
          {v === "wiki" && wikiFlags > 0 && (
            <span className="nav-count" title={`${wikiFlags} claims to review`}>
              {wikiFlags}<span className="visually-hidden"> claims to review</span>
            </span>
          )}
        </a>
      ))}
    </nav>
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
  return text.split("\n").find((x) => x.trim()) ?? "";
}

export function ResultBlock({ line, nested }: { line: Line; nested?: boolean }) {
  // history.EntryText prepends a result's own code to its text (the
  // command is as memorable as its output). Here the code already has
  // its own block directly above, so showing it again doubles every
  // result. data.code is that prefix.
  const code = typeof line.data?.code === "string" ? (line.data.code as string) : "";
  const { text: body, note } = splitExecNote(code && line.text.startsWith(code) ? line.text.slice(code.length).trimStart() : line.text);
  const lines = body.split("\n");
  // The first line with a word in it: JSON output opens with a bare "[" or
  // "{", which made the row read "Result [".
  const head = lines.find((l) => /[\p{L}\p{N}]/u.test(l)) ?? lines.find((l) => l.trim()) ?? "";
  const notice = note && <ExecNote note={note} />;
  // Recorded, and empty: a line that says so, not an empty box to open.
  if (!body.trim()) return <><div className="tool-state"><span className="block-label">Result</span><span className="num">No output</span></div>{notice}</>;
  return (
    <>
    <details className="block thin">
      <summary>
        <span className="block-label">Result</span>
        <span className="mono block-detail">{head.slice(0, 90)}</span>
        <span className="num block-lines">{lineCount(lines.length)}</span>
        <CopyButton text={body} what="output" />
      </summary>
      <div className="block-body">
        <Code text={body} lang={resultLang(line)} />
      </div>
    </details>
    {notice}
    </>
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
 * A background job, status first: which job, what it ran, how it ended.
 * Its output was once a wall of running text; it now waits one click in.
 */
export function JobBlock({ line }: { line: Line }) {
  const ctx = useWork();
  const id = jobIdOf(line);
  // A notice that is not an outcome ("matched … while running") stays a quiet line.
  if (id === undefined) {
    const [head = "", ...rest] = (line.text || "").split("\n");
    const body = rest.join("\n").trim();
    return (
      <details className="block thin">
        <summary>
          <span className="block-label">Job</span>
          <span className="mono block-detail">{head.replace(/^job\s+/, "")}</span>
          <span className="num block-lines">{body ? lineCount(body.split("\n").length) : "No output"}</span>
        </summary>
        {body && <pre className="mono">{body}</pre>}
      </details>
    );
  }
  // One job is a typed start, a typed finish and a notice, merged by id: one row, where it first appears.
  if (ctx && ctx.jobFirst.get(String(id)) !== line.seq) return null;
  const w = ctx?.jobs.get(String(id)) ?? jobsFromLines([line], ctx?.session ?? "", false)[0];
  return w ? <JobRow w={w} /> : null;
}

export /** Entry data is JSON: read a field as a string without trusting it. */
function str(v: unknown): string {
  return typeof v === "string" ? v : "";
}

export function Entry({ line, codes, nested }: { line: Line; codes: string[]; nested?: boolean }) {
  const k = line.kind;
  if (k === "assistant" || k === "sub:assistant") {
    // The loop's "blocks dropped" marker is a notice about the reply, not part of it.
    const { text: said, note } = splitExecNote(line.text);
    const [body, program] = splitBareProgram(stripRunFences(said, codes));
    if (blank(body) && !program) return note ? <ExecNote note={note} /> : null; // the reply was only the program it ran
    // Inside a subagent card the rail and the card's own header
    // already say whose words these are; repeating "subagent" above
    // every paragraph of a five-step run is noise.
    return (
      <div className="say">
        {/* There is one assistant; naming it above every reply said nothing. */}
        {!nested && k !== "assistant" && <div className="say-who"><span className="sub-dot" /><span>subagent</span></div>}
        {!blank(body) && <Markdown text={body} />}
        {program && (
          // A program that lost its fence never ran: it is code, not prose.
          <details className="block thin">
            <summary>
              <span className="block-label">Program</span>
              <span className="tool-unrecorded"><WarnMark />Execution not recorded</span>
            </summary>
            <div className="block-body"><Code text={program} lang="javascript" /></div>
          </details>
        )}
        {note && <ExecNote note={note} />}
      </div>
    );
  }
  if (k === "nudge" && str(line.data?.retry)) {
    // The loop's retry note is bookkeeping: one line, the note inside.
    return (
      <details className="block thin">
        <summary>
          <span className="block-label">Retry {str(line.data?.retry)}</span>
          <span className="block-detail">{str(line.data?.why)}</span>
        </summary>
        <pre className="mono">{line.text}</pre>
      </details>
    );
  }
  if (k === "model-switch") {
    // A /model switch records the command and the loop's echo: one line, the record inside.
    const recs = (line.data?.lines ?? []) as string[];
    return (
      <details className="block thin">
        <summary>
          <span className="block-label">Model changed</span>
          <span className="block-detail">{line.text}</span>
        </summary>
        <pre className="mono">{recs.join("\n")}</pre>
      </details>
    );
  }
  if (k === "code" || k === "sub:code") return <CodeBlock line={line} />;
  if (k === "result" || k === "sub:result") return <ResultBlock line={line} nested={nested} />;
  if (k === "thinking") {
    // A column of rows all reading just "Thinking" says nothing about
    // which one is worth opening. Carry the same preview and line
    // count every other block has.
    const lines = (line.text || "").split("\n");
    // The first line with a word in it: JSON output opens with a bare "[" or
  // "{", which made the row read "Result [".
  const head = lines.find((l) => /[\p{L}\p{N}]/u.test(l)) ?? lines.find((l) => l.trim()) ?? "";
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
  if (k === "ask/answer" && line.data?.secret) return <div className="meta-line">secret stored</div>;
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

/** What a recognised step is doing, for a running card's activity line. */
const ING: Record<string, string> = {
  Ran: "Running", Wrote: "Writing", Patched: "Patching", Read: "Reading", Test: "Testing",
  Search: "Searching", Build: "Building", Fetch: "Fetching", Vet: "Vetting", Delegate: "Delegating",
};

/**
 * A card's worker when no thread index holds it (a story): rebuilt from
 * the card's own records, under the same rules the work index uses.
 */
function soloWorker(agent: SubAgent, live: boolean): Worker {
  const last = agent.lines.length ? agent.lines[agent.lines.length - 1].seq : agent.seq;
  const body: Line[] = [
    { seq: agent.seq, at: agent.from, kind: "sub:start", text: agent.task, data: { worker: agent.worker } },
    ...agent.lines,
    ...(agent.status ? [{ seq: last + 1, at: agent.to, kind: "sub:done", text: "", data: { worker: agent.worker, status: agent.status, steps: agent.steps } }] : []),
  ];
  return subagentsFromTurn({ seq: agent.seq, prompt: null, body, done: live ? null : { seq: last + 2, at: agent.to, kind: "done", text: "" } }, "", live)[0];
}

export function SubAgentView({ agent, live, worker, all }: {
  agent: SubAgent; live: boolean;
  /** Its entry in the thread's work index; a story without one derives it. */
  worker?: Worker;
  /** Collapse all / Expand all from the run's head; `at` makes a repeat count. */
  all?: { open: boolean; at: number } | null;
}) {
  const ctx = useWork();
  const w = worker ?? soloWorker(agent, live);
  const failed = w.life === "failed";
  // A failure is the one you opened the thread to read, so it opens
  // itself — once. A manual collapse stays collapsed, and nothing scrolls.
  const [open, setOpen] = useState(failed);
  const touched = useRef(false);
  useEffect(() => { if (failed && !touched.current) setOpen(true); }, [failed]);
  useEffect(() => { if (all) { touched.current = true; setOpen(all.open); } }, [all]);
  const now = useNow(w.life === "running");
  const steps = useRef<HTMLDetailsElement>(null);

  const codes = agent.lines.filter((l) => l.kind === "sub:code").map((l) => l.text);
  const task = firstLine(plainTitle(agent.task));
  const timing = [w.ms !== undefined ? duration(w.ms) : "", w.steps ? stepCount(w.steps) : ""].filter(Boolean).join(" · ");
  const line2 = [
    w.stepErrors ? `${w.stepErrors} step ${w.stepErrors === 1 ? "error" : "errors"}` : "",
    w.notRun ? `${w.notRun} later ${w.notRun === 1 ? "block" : "blocks"} skipped after a failure` : "",
  ].filter(Boolean).join(" · ");
  const aria = `Subagent ${agent.worker}, ${stateText(w)}${w.ms !== undefined ? `, ${spokenDuration(w.ms)}` : ""}${w.steps ? `, ${stepCount(w.steps)}` : ""}${task ? `: ${task.slice(0, 80)}` : ""}`;

  // Each step keeps its seq, so "View steps" can land on the first one that went wrong.
  const entry = (l: Line) => <div key={l.seq} data-step-seq={l.seq} style={{ display: "contents" }}><Entry line={l} codes={codes} nested /></div>;
  const recorded = w.recordedSteps ?? 0;
  const stepsDisclosure = (label?: string, lines = agent.lines) => lines.length ? (
    <details className="block thin" ref={steps} data-open-key={w.key + ":steps"}>
      <summary><span className="block-label">{label ?? (w.steps && w.steps > recorded ? `Steps · ${recorded} of ${w.steps} recorded` : `Steps · ${recorded || lines.length}`)}</span></summary>
      <div className="sub-body">{lines.map(entry)}</div>
    </details>
  ) : <p className="meta-line">No steps were recorded</p>;
  const taskDisclosure = agent.task ? (
    <details className="block thin" data-open-key={w.key + ":task"}>
      <summary><span className="block-label">Task</span></summary>
      <div className="sub-body"><Markdown text={agent.task} /></div>
    </details>
  ) : null;
  const resultSection = (
    <div className="sub-sec">
      <span className="block-label">Result</span>
      {w.result ? <Markdown text={w.result} /> : <p className="meta-line">Result not recorded</p>}
    </div>
  );
  const viewSteps = () => {
    const d = steps.current;
    if (!d) return;
    d.open = true;
    const first = agent.lines.find((l) => l.kind === "sub:error" || execNote(l.text));
    const at = first ? d.querySelector<HTMLElement>(`[data-step-seq="${first.seq}"]`)?.firstElementChild as HTMLElement | null : null;
    const el = at ?? d;
    el.scrollIntoView({ block: "nearest" });
    (el.querySelector<HTMLElement>("summary") ?? d.querySelector<HTMLElement>("summary"))?.focus({ preventScroll: true });
  };

  let body: React.ReactNode;
  if (w.life === "running") {
    // What it is doing now, from its latest recorded call; a verb only when the call names one.
    const lastCode = [...agent.lines].reverse().find((l) => l.kind === "sub:code");
    const op = lastCode ? parseCall(lastCode.text) : null;
    const since = `last activity ${duration(Math.max(0, now - Date.parse(agent.to)))} ago`;
    const latest = lastCode ? agent.lines.filter((l) => l.seq >= lastCode.seq) : [];
    const earlier = lastCode ? agent.lines.filter((l) => l.seq < lastCode.seq) : [];
    body = <>
      {op ? <p className="meta-line sub-activity">{ING[op.verb] ? `${ING[op.verb]} ${gistOf(op.gist)}` : "Latest recorded step"} · {since}</p>
        : <p className="meta-line">Waiting for the first recorded step.</p>}
      {latest.map(entry)}
      {taskDisclosure}
      {earlier.length > 0 && stepsDisclosure(`Earlier steps · ${earlier.length}`, earlier)}
    </>;
  } else if (w.life === "failed") {
    const errors = agent.lines.filter((l) => l.kind === "sub:error");
    body = <>
      <div className="sub-sec">
        <span className="block-label sub-fail-head">Failure</span>
        {w.error ? <pre className="mono sub-trace">{w.error}</pre> : <p className="meta-line">Failure details not recorded.</p>}
      </div>
      {errors.length > 1 && (
        <details className="block thin" data-open-key={w.key + ":errors"}>
          <summary><span className="block-label">Error details · {errors.length}</span></summary>
          <div className="sub-body">{errors.map((l) => <pre key={l.seq} className="mono sub-trace">{l.text}</pre>)}</div>
        </details>
      )}
      {w.result && resultSection}
      {taskDisclosure}
      {stepsDisclosure()}
    </>;
  } else if (w.life === "stopped") {
    const last = agent.lines.slice(-1);
    body = <>
      <p className="meta-line">Stopped</p>
      {last.map(entry)}
      {w.result && resultSection}
      {taskDisclosure}
      {stepsDisclosure()}
    </>;
  } else if (w.life === "finished") {
    body = <>{resultSection}{taskDisclosure}{stepsDisclosure()}</>;
  } else {
    body = <>
      <p className="meta-line">{w.life === "queued" ? "Waiting to start." : "The recorded history does not establish an outcome."}</p>
      {taskDisclosure}
      {stepsDisclosure()}
    </>;
  }

  return (
    <details className={"sub" + (failed ? " sub-failed" : "")} open={open} onToggle={(e) => setOpen(e.currentTarget.open)}
             data-work-key={w.key} data-open-key={w.key}>
      <summary aria-label={aria} onClick={() => {
        touched.current = true;
        // Opening onto a recorded result or error is reading it; a default-open failure is not.
        if (!open && (w.result || w.error)) ctx?.review.markReviewed(w);
      }}>
        <span className="num sub-tag">Subagent {agent.worker}</span>
        <span className="sub-task" title={agent.task || undefined}>{task}</span>
        <span className="sub-state"><WorkState w={w} /></span>
        <span className="num sub-steps">{timing}</span>
        {line2 && <span className="sub-line2">{line2}</span>}
      </summary>
      <div className="sub-body">
        {line2 && agent.lines.length > 0 && (
          <p className="exec-note">{line2} · <button type="button" className="link" onClick={viewSteps}>View steps</button></p>
        )}
        {body}
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
export function SubRun({ agents, live, seq, turn }: { agents: SubAgent[]; live: boolean; /** The run's seq and turn, to find its workers in the index. */ seq?: number; turn?: Turn }) {
  const ctx = useWork();
  const session = ctx?.session ?? "";
  const workers = useMemo(() => {
    const all = turn && seq !== undefined ? subagentsFromTurn(turn, session, live).filter((w) => w.subrunSeq === seq) : [];
    return agents.map((a) => all.find((w) => w.key.endsWith(`:${a.worker}:${a.seq}`)) ?? soloWorker(a, live));
  }, [agents, live, seq, turn, session]);
  const [all, setAll] = useState<{ open: boolean; at: number } | null>(null);
  const c = workCounts(workers);
  const parts = (["running", "finished", "failed", "stopped", "unknown"] as const).filter((k) => c[k]).map((k) => `${c[k]} ${k}`);
  // The parent waits only while this run is unresolved: on the members still running.
  const waiting = c.running;
  return (
    <div className="subrun">
      {(agents.length >= 2 || waiting > 0) && (
        <div className="subrun-head">
          <span>{agents.length} {agents.length === 1 ? "subagent" : "subagents"}{parts.length ? ` · ${parts.join(" · ")}` : ""}</span>
          {waiting > 0 && <span className="work-state" data-life="running">Parent waiting on {waiting} {waiting === 1 ? "subagent" : "subagents"}</span>}
          {agents.length >= 2 && (
            <button type="button" className="link" onClick={() => setAll({ open: !all?.open, at: Date.now() })}>
              {all?.open ? "Collapse all" : "Expand all"}
            </button>
          )}
        </div>
      )}
      {agents.map((a, i) => <SubAgentView key={a.worker + ":" + a.seq} agent={a} live={live} worker={workers[i]} all={all} />)}
    </div>
  );
}

/** A path cut from the left, keeping its last directory and filename: "…/serve/", "…/serve/app.tsx". */
function tailPath(p: string): string {
  if (/\s/.test(p) || !p.includes("/")) return p;
  const parts = p.split("/");
  return parts.length > 2 ? "…/" + parts.slice(-2).join("/") : p;
}

/** A command's first line; the line's own width truncates it, not a count. */
function gistOf(text: string): string {
  return firstLine(text);
}

/** One call's recorded facts, for its thin line and the hover list. */
interface CallFacts { verb: string; gist: string; cmd: string; exit?: number; ms?: number; failed: boolean; preview?: string }

function callFacts(code: Line, result?: Line): CallFacts {
  const call = parseCall(code.text);
  const out = result ? resultBody(result) : "";
  const exit = typeof result?.data?.exit === "number" ? (result.data.exit as number) : undefined;
  const ms = typeof result?.data?.ms === "number" ? (result.data.ms as number) : undefined;
  return { verb: call.verb, gist: gistOf(call.gist), cmd: gistOf(call.target || call.gist), exit, ms, failed: (exit !== undefined && exit !== 0) || /^error\b/i.test(out) };
}

const canHover = () => typeof window !== "undefined" && window.matchMedia?.("(hover: hover)").matches;

/**
 * Hover (or keyboard focus) on a thin line shows its details after a
 * beat, in a fixed layer the scrolling transcript cannot clip. Touch
 * screens get none: a tap opens the line instead. The pointer can move
 * onto it (it stays while hovered) to read or select the output.
 */
function useThinPop(rows: CallFacts[]) {
  const id = useId();
  const [at, setAt] = useState<DOMRect | null>(null);
  const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  const hide = useCallback(() => { clearTimeout(timer.current); setAt(null); }, []);
  const hideSoon = () => { clearTimeout(timer.current); timer.current = setTimeout(() => setAt(null), 150); };
  const keep = () => clearTimeout(timer.current);
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!at) return;
    // Escape closes the popover only; it must not also leave the thread.
    const key = (e: KeyboardEvent) => { if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); hide(); } };
    // The transcript scrolling moves the line away; the popover's own does not.
    const scroll = (e: Event) => { if (!ref.current?.contains(e.target as Node)) hide(); };
    window.addEventListener("keydown", key, true);
    window.addEventListener("scroll", scroll, true);
    // Moving to another session or view leaves nothing to describe.
    window.addEventListener("hashchange", hide);
    window.addEventListener("popstate", hide);
    return () => {
      window.removeEventListener("keydown", key, true); window.removeEventListener("scroll", scroll, true);
      window.removeEventListener("hashchange", hide); window.removeEventListener("popstate", hide);
    };
  }, [at, hide]);
  useEffect(() => () => clearTimeout(timer.current), []);
  const show = (e: React.SyntheticEvent<HTMLElement>) => {
    const el = e.currentTarget;
    if ((el.parentElement as HTMLDetailsElement | null)?.open) return;
    // A fine pointer only: focus walking the summaries never opens a preview.
    if (!canHover()) return;
    clearTimeout(timer.current);
    timer.current = setTimeout(() => setAt(el.getBoundingClientRect()), 300);
  };
  const handlers = {
    onMouseEnter: show, onMouseLeave: hideSoon, onBlur: hide, onClick: hide,
    "aria-describedby": at ? id : undefined,
  };
  // Placed from its own measured size: below the line if it fits, else
  // above, else pinned to the top (its max-height keeps it on screen).
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
      <div ref={ref} id={id} className="thin-pop" role="tooltip" style={{ top: 0, left: 0, visibility: "hidden" }}
           onMouseEnter={keep} onMouseLeave={hideSoon}>
        {rows.map((r, i) => (
          <div key={i} className={"thin-pop-row" + (r.failed ? " thin-pop-failed" : "")}>
            <span>{r.verb}</span>
            <span className="mono thin-pop-cmd">{r.cmd}</span>
            <span className="num">{r.exit !== undefined ? `exit ${r.exit}` : ""}</span>
            <span className="num">{r.ms !== undefined ? duration(r.ms) : ""}</span>
            {r.preview && <pre className="mono thin-pop-out">{r.preview}</pre>}
          </div>
        ))}
      </div>, document.body);
  }
  return { handlers, pop };
}

/** A pasted image in a prompt: a fixed box, so loading never moves the page. */
function Thumb({ path, n }: { path: string; n: number }) {
  const [state, setState] = useState<"loading" | "ready" | "error">("loading");
  return (
    <a href={api.attachmentURL(path)} target="_blank" rel="noreferrer" title={path}>
      {state === "error" ? "Image unavailable" : (
        <img src={api.attachmentURL(path)} alt={`Image #${n}`} onLoad={() => setState("ready")} onError={() => setState("error")}
             style={state === "loading" ? { opacity: 0 } : undefined} />
      )}
    </a>
  );
}

/** A result's text minus the code history prefixes onto it. */
function resultBody(l: Line): string {
  const code = str(l.data?.code);
  return code && l.text.startsWith(code) ? l.text.slice(code.length).trimStart() : l.text;
}

/** Verbs parseCall classifies; anything else is a group without an invented name. */
const KNOWN_VERBS = new Set(["Ran", "Wrote", "Patched", "Read", "Delegate", "Asked you", "Test", "Search", "Build", "Fetch", "Vet"]);

function WarnMark() {
  return (
    <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      <path d="M12 3 2 21h20L12 3z" /><path d="M12 10v5M12 18h.01" />
    </svg>
  );
}

/** A labelled copy button, for where an icon alone is not enough. */
function CopyText({ text, label }: { text: string; label: string }) {
  const [done, setDone] = useState(false);
  useEffect(() => {
    if (!done) return;
    const t = setTimeout(() => setDone(false), 1400);
    return () => clearTimeout(t);
  }, [done]);
  return (
    <button type="button" className="btn" onClick={() => navigator.clipboard?.writeText(text).then(() => setDone(true), () => {})}>
      {done ? "Copied" : label}
    </button>
  );
}

/**
 * A run of tool calls as one row: how many, the last thing it did, and
 * whether any failed. Opened, each call is its own block again.
 */
export function ToolRun({ lines, codes, live, stopped, failSeq }: { lines: Line[]; codes: string[]; live?: boolean; stopped?: boolean; /** The result seq of the failure the turn ended on: it opens itself. */ failSeq?: number }) {
  // Pair each call with the result recorded for it: one row per thing
  // done, not a "Ran" row and a "Result" row saying half each. A result
  // names its call in data.code, so notes in between never split the
  // pair; one without that record pairs only with the call right above.
  const rows: React.ReactNode[] = [];
  const facts: CallFacts[] = [];
  const used = new Set<number>();
  for (let i = 0; i < lines.length; i++) {
    const l = lines[i];
    if (used.has(i)) continue;
    if (l.kind === "code") {
      let result: Line | undefined;
      for (let j = i + 1; j < lines.length; j++) {
        const r = lines[j];
        if (r.kind !== "result" || used.has(j)) continue;
        const code = str(r.data?.code);
        if (code ? code.trim() === l.text.trim() : j === i + 1) { result = r; used.add(j); break; }
      }
      facts.push(callFacts(l, result));
      rows.push(<ToolCall key={l.seq} code={l} result={result} live={live} stopped={stopped} current={failSeq !== undefined && result?.seq === failSeq} />);
    } else if (l.kind === "job") {
      // Consecutive job rows share one head.
      let j = i;
      while (j < lines.length && lines[j].kind === "job") j++;
      rows.push(<JobLines key={l.seq} lines={lines.slice(i, j)} render={(x) => <JobBlock line={x} />} />);
      i = j - 1;
    } else {
      rows.push(<Entry key={l.seq} line={l} codes={codes} />);
    }
  }
  const { handlers, pop } = useThinPop(facts);
  const box = useRef<HTMLDetailsElement>(null);
  const calls = facts.length;
  const holdsFail = failSeq !== undefined && lines.some((l) => l.seq === failSeq);
  if (calls < 2) return <>{rows}</>;
  const failed = facts.filter((f) => f.failed).length;
  const timed = facts.every((f) => f.ms !== undefined);
  const totalMs = facts.reduce((n, f) => n + (f.ms ?? 0), 0);
  // One click from the count to the output that failed.
  const openFailed = (e: React.MouseEvent) => {
    e.preventDefault();
    e.stopPropagation();
    const run = box.current;
    const first = run?.querySelector<HTMLDetailsElement>(":scope>.toolrun-body>.block-failed");
    if (!run || !first) return;
    run.open = true;
    first.open = true;
    first.querySelector("summary")?.focus({ preventScroll: true });
    first.scrollIntoView({ block: "nearest" });
  };
  // The line names the work: what kind of calls, then what they were
  // aimed at. The count and time are secondary; the full targets sit in
  // the hover list and the expansion.
  // Only the failure count is red: one failed child does not make the work a failure.
  const verbs = [...new Set(facts.map((f) => f.verb))];
  const known = verbs.every((v) => KNOWN_VERBS.has(v));
  const label = !known || verbs.length > 2 ? "Tool group" : verbs.map((v, i) => (i ? v.toLowerCase() : v)).join(" and ");
  const targets = [...new Set(facts.map((f) => f.gist).filter(Boolean))];
  const fileish = verbs.every((v) => v === "Wrote" || v === "Patched" || v === "Read");
  // A path keeps its filename: the leading directories are what gets cut.
  // A program's gist carries its own " +N"; the row counts "N more" once, itself.
  const first = (targets[0] ?? "").replace(/ \+\d+$/, "");
  const target = first ? (fileish ? first.split("/").pop()! : first) : "";
  const more = targets.length - 1;
  return (
    <details className="block thin toolrun" ref={box} open={holdsFail || undefined} data-open-key={"tools:" + lines[0].seq}>
      <summary {...handlers}>
        <span className="block-label">{label}</span>
        {target && <span className="mono block-detail" title={targets[0]}>{target}</span>}
        {more > 0 && <span className="num tool-more">{more} more {fileish ? (more === 1 ? "file" : "files") : ""}</span>}
        <span className="num tool-meta">{calls} calls{timed ? ` · ${duration(totalMs)}` : ""}</span>
        {failed > 0 && <button type="button" className="link num toolrun-failed" onClick={openFailed}>{failed} failed</button>}
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
export function ToolCall({ code, result, live, stopped, current }: { code: Line; result?: Line; /** Its turn is still running. */ live?: boolean; stopped?: boolean; /** The failure its turn ended on: open, with the diagnosis. */ current?: boolean }) {
  const call = useMemo(() => parseCall(code.text), [code.text]);
  // The loop's "blocks not run" marker rides on the output; it is a notice, shown under the row.
  const { text: out, note } = splitExecNote(result ? resultBody(result) : "");
  // A job call reads as what it did; a wait that returned says it waited.
  const jobLabel = toolCallLabel(code.text);
  const label = jobLabel && result ? jobLabel.replace(/^Waiting for (Job \d+).*$/, "Waited for $1") : jobLabel;
  const continues = /^job (\d+) started in the background/.exec(out)?.[1];
  // Recorded evidence, when the loop stamped it: the block's own exit code
  // and how long it ran. Older results carry neither and show neither.
  const exit = typeof result?.data?.exit === "number" ? (result.data.exit as number) : undefined;
  const ms = typeof result?.data?.ms === "number" ? (result.data.ms as number) : undefined;
  const failed = (exit !== undefined && exit !== 0) || /^error\b/i.test(out);
  // A question nobody answered is an outcome, not an exception to parse.
  const timedOut = /ask: no answer after (\S+)/.exec(out);
  // A single call's line already says everything the hover list would,
  // so its hover shows what the line cannot: the first lines of output,
  // and the full first line of the call when the thin line had to cut it.
  const outAll = (out || "(no output)").split("\n");
  const shown = outAll.filter((l) => l.trim()).slice(0, 3);
  const more = outAll.length - shown.length;
  // The full call and its exit code live here, keeping the line itself short.
  const { handlers, pop } = useThinPop(timedOut ? [] : [{
    verb: result ? (failed ? "Failed" : "Output") : "no result yet", gist: call.gist, cmd: gistOf(call.target || call.gist), failed, exit, ms,
    preview: result ? shown.join("\n") + (more > 0 ? `\n+${lineCount(more)}` : "") : undefined,
  }]);
  // The recorded exit and time, success or not: "exit 0" is evidence too.
  const empty = Boolean(result) && !out.trim();
  const meta = [continues ? `Continues as Job ${continues}` : "", failed ? "Failed" : "", exit !== undefined ? `exit ${exit}` : "", empty ? "No output" : "", ms !== undefined ? `Command: ${duration(ms)}` : ""];
  const what = call.lang === "bash" ? "Command" : call.lang === "javascript" ? "Program" : "Content";
  // No result: still running, cut off by a stop, or never recorded. Each says which.
  const missing = result ? null : live ? "running" : stopped ? "Interrupted · result not recorded" : "Result not recorded";
  const [full, setFull] = useState(false);
  const phone = useMedia("(max-width:720px)");
  // The last lines of a failure are where the diagnosis is.
  const diag = failed ? out.split("\n").filter((l) => l.trim()).slice(-10) : [];
  const cmdText = call.lang === "bash" ? call.body : call.raw;
  return (
    <>
    <details className={"block thin toolcall" + (failed ? " block-failed" : "")} data-seq={result?.seq} open={current || undefined}>
      <summary {...handlers}>
        <span className="block-label">{timedOut ? "Question timed out" : label ?? call.verb}</span>
        {!label && <span className="mono block-detail" title={call.gist}>{timedOut ? timedOut[1] : phone ? tailPath(gistOf(call.gist)) : gistOf(call.gist)}</span>}
        {meta.some(Boolean) && (
          <span className={"num tool-meta" + (failed ? " tool-meta-failed" : "")}>{meta.filter(Boolean).join(" · ")}</span>
        )}
        {missing === "running" && (
          <span className="num tool-meta tool-running">
            <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2"
                 strokeLinecap="round" className="spin-mark" aria-hidden="true"><circle cx="12" cy="12" r="8.5" strokeDasharray="40 14" /></svg>
            Running
          </span>
        )}
        {missing && missing !== "running" && <span className="tool-unrecorded"><WarnMark />{missing}</span>}
        {!empty && <CopyButton text={out || call.body || call.raw} what={result ? "output" : call.verb.toLowerCase() + " block"} />}
      </summary>
      {pop}
      <div className="block-body">
        {/* Output first; the call that made it is one disclosure, once. */}
        {!result && call.body && <Code text={call.body} lang={call.lang} />}
        {failed && diag.length > 0 && (
          <div className="fail-diag">
            <pre className="mono fail-cmd">{cmdText}</pre>
            <pre className="mono">{diag.join("\n")}</pre>
            <div className="fail-actions">
              <CopyText text={cmdText} label="Copy command" />
              {diag.length < out.split("\n").length && (
                <button type="button" className="btn" aria-expanded={full} onClick={() => setFull((v) => !v)}>{full ? "Hide full output" : "Full output"}</button>
              )}
            </div>
          </div>
        )}
        {/* Output keeps its columns: a docker ps or a table wrapped at the
            block's edge scatters every row across three lines. */}
        {result && !empty && (!failed || full || !diag.length) && <div className="tool-output"><Code text={out} lang={resultLang(result)} /></div>}
        {result && failed && diag.length > 0 ? null : result ? (
          <details className="block-inner">
            <summary><span className="block-label">{what}</span></summary>
            <Code text={call.body || call.raw} lang={call.body ? call.lang : "javascript"} />
          </details>
        ) : call.body !== call.raw && (
          <details className="block-inner">
            <summary><span className="block-label">The call</span></summary>
            <Code text={call.raw} lang="javascript" />
          </details>
        )}
      </div>
    </details>
    {note && <ExecNote note={note} />}
    </>
  );
}

/**
 * Every hook and rule that fired in a turn, as one quiet row. They fire
 * on every tool call, so inline they drowned the transcript; folded,
 * the count says whether anything happened and the ledger is one click in.
 */
const DECIDED: Record<string, string> = { block: "blocked", deny: "denied", allow: "allowed", ask: "asked", rewrite: "rewritten", approve: "approved" };

export function TurnHooks({ lines, load, save }: { lines: Line[]; load?: Load; save?: Save }) {
  const fires = lines.filter((l) => l.kind === "hook");
  // A "hook <event>: notice" line a fire already carries is that fire,
  // said twice; one no fire carries is shown once, here.
  const carried = new Set(fires.map((l) => str(l.data?.notice)).filter(Boolean));
  const loose = lines.filter((l) => l.kind === "system" && !carried.has(l.text.replace(/^hook [^:]*:\s*/, "")));
  if (!fires.length && !loose.length) return null;
  // What the decisions were ("1 blocked"), not that there were some.
  const outcomes = new Map<string, number>();
  for (const l of fires) {
    const d = str(l.data?.decision);
    if (d) outcomes.set(d, (outcomes.get(d) ?? 0) + 1);
  }
  const errored = fires.filter((l) => str(l.data?.error)).length;
  const rules = new Set<string>();
  for (const l of fires) {
    const n = str(l.data?.notice);
    if (n.startsWith("applied ")) n.slice(8).split(", ").forEach((r) => rules.add(r));
  }
  const parts = [`${fires.length || loose.length} ${fires.length ? "fired" : "notices"}`];
  const bad: string[] = [];
  for (const [d, n] of outcomes) (d === "block" || d === "deny" ? bad : parts).push(`${n} ${DECIDED[d] ?? d}`);
  if (rules.size) parts.push(`${rules.size} ${rules.size === 1 ? "rule" : "rules"} applied`);
  return (
    <details className="block thin turn-hooks">
      <summary>
        <span className="block-label">Hooks</span>
        <span className="block-detail">{parts.join(" · ")}</span>
        {bad.length > 0 && <span className="num toolrun-failed">{bad.join(" · ")}</span>}
        {errored > 0 && <span className="num toolrun-failed">{errored} errored</span>}
      </summary>
      <div className="turn-hooks-body">
        {fires.map((l) => {
          const d = l.data ?? {};
          const err = str(d.error), decision = str(d.decision), notice = str(d.notice);
          return (
            <details key={l.seq} className="hook-invocation">
              <summary className="hook-line">
                <span className="mono">{str(d.name)}</span>{" · "}{str(d.event)}{" · "}
                <span className={err ? "hook-bad" : decision ? "hook-act" : "hook-why"}>
                  {err ? "errored" : decision || "passed"}
                </span>
                {notice && <span className="hook-why"> — {notice}</span>}
                {err && <span className="hook-why"> {err}</span>}
                {typeof d.ms === "number" && <span className="num hook-why"> · {d.ms} ms</span>}
                <span className="hook-why"> · input / output</span>
              </summary>
              <FireInspection fire={d as Partial<Fire>} load={load} save={save} />
            </details>
          );
        })}
        {loose.map((l) => <p key={l.seq} className="hook-line hook-why">{l.text}</p>)}
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
function TurnFooter({ turn, fail }: { turn: Turn; /** The result the turn's failure came from, when recorded. */ fail?: Line }) {
  const done = turn.done!;
  const u = usageOf(done);
  const files = Array.isArray(done.data?.files) ? (done.data!.files as string[]) : [];
  const exit = typeof done.data?.exit === "number" ? done.data.exit : fail?.data?.exit;
  const failed = typeof exit === "number" && exit !== 0;
  const facts: string[] = [];
  if (turn.prompt?.at) facts.push(`Turn: ${duration(Date.parse(done.at) - Date.parse(turn.prompt.at))}`);
  // What failed, said where the turn ends: a phone has no hover to read it from.
  const failCmd = fail ? gistOf(parseCall(str(fail.data?.code)).gist) : "";
  const failOut = fail ? resultBody(fail).split("\n").filter((l) => l.trim()).slice(-3) : [];
  const failName = fail ? /--- FAIL: (\S+)/.exec(resultBody(fail))?.[1] ?? failCmd : "";
  // The failed call already open on screen says it all; the footer then only points at it.
  const [shownOpen, setShownOpen] = useState(Boolean(fail));
  useEffect(() => {
    if (!fail) return;
    const check = () => {
      const el = document.querySelector<HTMLDetailsElement>(`details.block[data-seq="${fail.seq}"]`);
      let open = Boolean(el?.open);
      for (let d = el?.parentElement?.closest("details") ?? null; d && open; d = d.parentElement?.closest("details") ?? null) open = (d as HTMLDetailsElement).open;
      setShownOpen(open);
    };
    check();
    document.addEventListener("toggle", check, true);
    return () => document.removeEventListener("toggle", check, true);
  }, [fail]);
  const model = str(done.data?.model);
  const show = () => {
    const el = document.querySelector<HTMLElement>(`details.block[data-seq="${fail?.seq}"]`);
    if (!el) return;
    for (let d: HTMLElement | null = el; d; d = d.parentElement?.closest("details") ?? null) if (d instanceof HTMLDetailsElement) d.open = true;
    el.scrollIntoView({ block: "center" });
    el.querySelector<HTMLElement>("summary")?.focus({ preventScroll: true });
  };
  // The strip above owns session totals; a turn says what it took, with
  // its tokens on the price rather than as a third figure.
  if (u) facts.push(`${tokenCount(u.in)} in · ${tokenCount(u.out)} out`);
  if (u?.cost !== undefined) facts.push(money(u.cost));
  return (
    <div className="turn-foot">
      {failed && fail && shownOpen && !(turn.stopped || done.kind === "cancelled") ? (
        <span className="turn-fail-row">
          <span className="turn-failed">{failName || "Command"} failed · exit {exit}</span>
          <span aria-hidden="true">·</span>
          <button type="button" className="link turn-fail-jump" onClick={show}>Jump to command</button>
        </span>
      ) : (
        <span className={"turn-outcome" + (failed ? " turn-failed" : "")}>
          {turn.stopped || done.kind === "cancelled" ? "Stopped" : failed ? `Finished with a failed command · exit ${exit}` : "Finished"}
        </span>
      )}
      {model && <span className="mono">{model.split("/").pop()}</span>}
      {facts.map((f) => <span key={f} className="num">{f}</span>)}
      {failed && fail && !shownOpen && (
        <div className="turn-fail" role="note">
          <button type="button" className="link mono turn-fail-cmd" onClick={show}>{failCmd || "Show the failed command"}</button>
          {failOut.length > 0 && <pre className="mono">{failOut.join("\n")}</pre>}
        </div>
      )}
      {files.length > 0 && (
        <details className="turn-files">
          <summary>This turn: {files.length} {files.length === 1 ? "file" : "files"}</summary>
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
function RuntimeStrip({ row, lines, paused, onRetry, onContext, work }: { row: Row; lines: Line[]; paused?: number; onRetry?: () => void; onContext?: () => void; /** The Work button, after the metrics. */ work?: React.ReactNode }) {
  const [limits, setLimits] = useState<Record<string, number>>({});
  useEffect(() => {
    fetch("/api/models").then((r) => r.json()).then((c: { providers?: ProviderInfo[] }) => {
      const m: Record<string, number> = {};
      for (const p of c.providers ?? []) for (const x of p.models ?? []) if (x.context) m[x.id] = x.context;
      setLimits(m);
    }).catch(() => setLimits({}));
  }, []);
  const u = useMemo(() => sessionUsage(lines), [lines]);
  // Headroom is measured against the model that took the last input. A
  // /model after the last finished turn means the picker names a model
  // that has not read this context yet, so only the reading is shown.
  const switched = useMemo(() => {
    for (let i = lines.length - 1; i >= 0; i--) {
      if (lines[i].kind === "done" && usageOf(lines[i])) return false;
      if (/^\/model \S/.test(lines[i].text) && !/^\/model list\b/.test(lines[i].text)) return true;
    }
    return false;
  }, [lines]);
  const limit = row.model && !switched ? limits[row.model] : undefined;
  const pct = u && limit ? Math.min(100, Math.round((u.lastIn / limit) * 100)) : undefined;
  const strip = useRef<HTMLDivElement>(null);
  usePopovers(strip);
  // Work, cache and changes stand on their own: a session with no usage
  // recorded can still have a server running.
  return (
    <div className="runtime-strip" ref={strip}>
      {paused !== undefined && (
        <span className="rt-paused" role="status">
          Updates paused · last synced {clock(new Date(paused).toISOString())} · <button className="link" onClick={onRetry}>Retry</button>
        </span>
      )}
      {(() => {
        // The reading is the way into Context; there is no second button for
        // it, and it stays when nothing was reported so the way in does too.
        const tip = !u ? "No input tokens have been reported for this session"
          : limit ? `${u.lastIn.toLocaleString()} of ${limit.toLocaleString()} tokens, read by ${row.model}`
          : !row.model ? "No model is recorded for this session, so headroom is not known"
          : switched ? "The model changed after this input was read; headroom shows once the new model answers"
          : `${row.model} has no context window in the catalogue`;
        const body = <>
          <span className="rt-label">Context</span>
          <span className={"num rt-value" + (u ? "" : " rt-stale")}>{!u ? "Not reported" : `${tokenCount(u.lastIn)}${limit ? ` · ${tokenCount(Math.max(0, limit - u.lastIn))} left` : " · window unknown"}`}</span>
          {pct !== undefined && (
            <span className="rt-bar" role="meter" aria-label="Context used" aria-valuenow={pct} aria-valuemin={0} aria-valuemax={100}>
              <span className={pct >= 80 ? "rt-hot" : undefined} style={{ width: `${pct}%` }} />
            </span>
          )}
        </>;
        return onContext
          ? <button type="button" className="rt rt-link" title={`${tip} · open Context`} aria-label={`Context: ${tip}`} onClick={onContext}>{body}</button>
          : <Tip tip={tip}>{body}</Tip>;
      })()}
      {u?.cost === undefined && (
        <Tip tip="The provider has not reported a cost for this session">
          <span className="rt-label">Cost</span><span className="num rt-value rt-stale">Not reported</span>
        </Tip>
      )}
      {u?.cost !== undefined && (
        <Tip tip={`Session cost: ${u.in.toLocaleString()} tokens in · ${u.out.toLocaleString()} out`}>
          <span className="rt-label">Cost</span><span className="num rt-value">{money(u.cost)}</span>
        </Tip>
      )}
      <ChangesChip row={row} tick={lines.length} />
      <TestsChip lines={lines} running={row.status === "running"} />
      {row.cache && <CacheChip cache={row.cache} model={row.model} />}
      {work}
    </div>
  );
}

/**
 * The strip's disclosures behave as one menu: opening one shuts the
 * others, and Escape or a click elsewhere shuts it and gives focus back.
 */
function usePopovers(root: React.RefObject<HTMLElement | null>) {
  useEffect(() => {
    const el = root.current;
    if (!el) return;
    const open = () => [...el.querySelectorAll<HTMLDetailsElement>("details[open]")];
    const toggle = (e: Event) => {
      const d = e.target as HTMLDetailsElement;
      if (d.open) for (const o of open()) if (o !== d) o.open = false;
    };
    const key = (e: KeyboardEvent) => {
      const d = open()[0];
      if (e.key !== "Escape" || !d) return;
      e.preventDefault(); e.stopPropagation();
      d.open = false;
      d.querySelector("summary")?.focus();
    };
    const away = (e: MouseEvent) => { for (const d of open()) if (!d.contains(e.target as Node)) d.open = false; };
    el.addEventListener("toggle", toggle, true);
    window.addEventListener("keydown", key, true);
    document.addEventListener("mousedown", away);
    return () => { el.removeEventListener("toggle", toggle, true); window.removeEventListener("keydown", key, true); document.removeEventListener("mousedown", away); };
  }, [root]);
}

/**
 * What the session changed, scoped and named: its own edits by default
 * (from its checkpoints, so dirt already in the tree is not the agent's),
 * the working tree one tab away. A phone has no room for a popover and
 * goes to the full page, #/s/<id>/changes.
 */
function ChangesChip({ row, tick }: { row: Row; tick: number }) {
  const data = useChanges(row.id, tick);
  const phone = useMedia("(max-width:720px)");
  const [scope, setScope] = useState<Scope>("session");
  const c = countOf(data.session);
  const t = countOf(data.tree);
  const href = `#/s/${row.id}/changes`;
  const aria = `Session edits: ${c.text}${c.add !== undefined ? `, ${c.add} added, ${c.del} removed` : ""}. Working tree: ${t.text}${data.session.failed || data.tree.failed ? ", stale" : ""}`;
  const body = <>
    <span className="rt-label">Session edits</span>
    <span className={"num rt-value" + (c.quiet ? " rt-stale" : "")}>{c.text}{c.add !== undefined && <> <span className="rt-add">+{c.add}</span> <span className="rt-del">−{c.del}</span></>}</span>
  </>;
  if (phone && c.text === "None") return <span className="rt" aria-label={aria}>{body}</span>;
  if (phone) return <a className="rt rt-link" href={href} aria-label={aria}>{body}</a>;
  return (
    <details className="rt rt-jobs">
      <summary aria-label={aria}>{body}</summary>
      <div className="rt-pop rt-diff">
        <ChangesBody row={row} data={data} scope={scope} onScope={setScope} />
        <a className="link chg-full" href={href}>Open full view</a>
      </div>
    </details>
  );
}

/**
 * The last test run and how it ended, from the exit code the loop
 * recorded on its result — never from what the reply said about it.
 * Absent when the session ran no tests, or ran them before exit codes
 * were recorded.
 */
function TestsChip({ lines, running }: { lines: Line[]; running: boolean }) {
  const last = useMemo(() => lastTestRun(lines, running), [lines, running]);
  if (!last) return null;
  const failed = last.state === "failed";
  const word = { passed: "passed", failed: "failed", running: "running…", unrecorded: "not recorded" }[last.state];
  return (
    // The chip is a way to the evidence, not a second copy of it: it opens
    // the call in the transcript (and the run folding it) and lands there.
    <button className="rt rt-link" title={`${last.cmd}${last.exit !== undefined ? ` · exit ${last.exit}` : last.state === "unrecorded" ? " · no exit code was recorded" : ""} · ${new Date(last.at).toLocaleString()} · show in transcript`}
            onClick={() => {
              const el = document.querySelector<HTMLElement>(`details.block[data-seq="${last.seq}"]`);
              if (!el) return;
              for (let d: HTMLElement | null = el; d; d = d.parentElement?.closest("details") ?? null) if (d instanceof HTMLDetailsElement) d.open = true;
              el.scrollIntoView({ block: "center" });
              (el.querySelector<HTMLElement>("summary,button") ?? el).focus();
            }}>
      <span className="rt-label">Last tests</span>
      <span className={"num rt-value " + (failed ? "rt-del" : last.state === "passed" ? "rt-add" : "rt-stale")}>{failed && <span aria-hidden="true" className="rt-mark"><StatusMark status="error" bare /></span>}{word}</span>
      {(last.state === "passed" || last.state === "failed") && <span className="num rt-label">{ago(last.at)} ago</span>}
    </button>
  );
}

/** How long the running turn has taken, counted from its prompt. */
function RunClock({ since }: { since: string }) {
  const now = useNow(true);
  const t = Math.max(0, Math.round((now - Date.parse(since)) / 1000));
  return <span className="num head-clock">{Math.floor(t / 60)}:{String(t % 60).padStart(2, "0")}</span>;
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
  // An elapsed window is not a decision fact; only a warm one is.
  if (!left) return null;
  return (
    <Tip className="rt-cache-hot"
         tip={`Estimate: ${provider}'s documented cache window since the last turn ended, not a measured hit. Last turn read ${tokenCount(cache.read)} of ${tokenCount(cache.in)} input tokens from the cache (${hit}%), wrote ${tokenCount(cache.write)}`}>
      <span className="rt-label">Cache TTL</span>
      <span className="num rt-value">
        ~{Math.floor(left / 60)}:{String(left % 60).padStart(2, "0")}
      </span>
    </Tip>
  );
}

/**
 * A strip figure whose detail shows on hover after a beat, on keyboard
 * focus and on tap — not only in a title tooltip a phone never shows.
 */
function Tip({ tip, label, className, children }: { tip: string; /** The accessible name, when the visible words are too short to be one. */ label?: string; className?: string; children: React.ReactNode }) {
  const id = useId();
  // Hover and focus show it; a click or tap pins it; Escape shuts it.
  const [held, setHeld] = useState<"pin" | "shut" | null>(null);
  return (
    <button type="button" className={"rt rt-tip" + (className ? " " + className : "")} aria-label={label} aria-describedby={id}
            aria-expanded={held === "pin"} data-tip={held ?? undefined}
            onClick={() => setHeld((h) => h === "pin" ? "shut" : "pin")} onBlur={() => setHeld(null)} onMouseLeave={() => setHeld((h) => h === "shut" ? null : h)}
            onKeyDown={(e) => { if (e.key === "Escape" && held !== "shut") { e.stopPropagation(); setHeld("shut"); } }}>
      {children}
      <span className="rt-tip-body" role="tooltip" id={id}>{tip}</span>
    </button>
  );
}

/** A secret ask's own field: the value goes straight to the answer call and
 *  is never kept — no draft storage, no failure record holding it. */
function SecretAnswer({ askId, onAnswer }: { askId: string; onAnswer: (t: string, ask?: string) => Promise<string | null> | void }) {
  const [value, setValue] = useState("");
  const [state, setState] = useState<"" | "sending" | "failed">("");
  const submit = async (e: { preventDefault(): void }) => {
    e.preventDefault();
    if (!value || state === "sending") return;
    const t = value;
    setValue("");
    setState("sending");
    const error = await onAnswer(t, askId);
    setState(error ? "failed" : "");
  };
  return (
    <form className="ask-options" onSubmit={submit}>
      <input type="password" autoComplete="off" aria-label="Secret value" value={value}
             onChange={(e) => setValue(e.target.value)} disabled={state === "sending"} />
      <button className="btn" type="submit" disabled={!value || state === "sending"}>{state === "sending" ? "Submitting…" : "Submit"}</button>
      {state === "failed" && <span className="send-failed-text" role="alert">not sent, try again</span>}
    </form>
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
  const ctx = useWork();
  const codes = turn.body.filter((l) => l.kind === "code" || l.kind === "sub:code").map((l) => l.text);
  const hooks = useMemo(() => turn.body.filter(isHookLine), [turn.body]);
  const items = useMemo<Item[]>(
    () => groupTools(groupSubs(foldModelSwitch(foldRetries(turn.body.filter((l) => !isHookLine(l))))), codes),
    // codes is derived from turn.body on every render.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [turn.body]);
  // The loop appends "[skill: name]\n<SKILL.md>" blocks to the prompt a
  // skill was invoked from. What you typed is the part before them.
  // An @file is attached the same way, as "[file: path]\n<contents>":
  // pasted source is context, not the words of the prompt.
  const [raw, ...skills] = (turn.prompt?.text ?? "").split(/\n+(?=\[(?:skill|file): [^\]\n]+\]\n)/);
  // A pasted image rides as "[Image #N: path]": show the tag and the picture, not the path.
  const images = [...raw.matchAll(/\[Image #\d+: ([^\]\n]+)\]/g)].map((m) => m[1]);
  const said = raw.replace(/\[Image (#\d+): [^\]\n]+\]/g, "[Image $1]");
  // A turn that ended on a failed command opens that command, and only that one:
  // an earlier failure the agent went on to fix is history, still counted.
  const exit = turn.done?.data?.exit;
  const bad = (l?: Line) => typeof l?.data?.exit === "number" && l.data.exit !== 0;
  // A done that recorded no exit still ended on a failure when its last result failed.
  const lastResult = [...turn.body].reverse().find((l) => l.kind === "result");
  const fail = !turn.done || turn.stopped ? undefined
    : typeof exit === "number" ? (exit !== 0 ? [...turn.body].reverse().find((l) => l.kind === "result" && bad(l)) : undefined)
    : bad(lastResult) ? lastResult : undefined;
  const [full, setFull] = useState(false);
  const [opened, setOpened] = useState<number | null>(null);
  const long = said.length > 420 || said.split("\n").length > 4;
  // Each attachment is named once: as a chip where the prompt mentions
  // it (/exa, @go/serve.go), else in a strip below. Its contents open
  // under the prompt, one at a time.
  const atts = skills.map((s) => {
    const [head, ...body] = s.split("\n");
    const m = /^\[(skill|file): (.+)\]$/.exec(head.trim());
    const isFile = m?.[1] === "file";
    const token = (isFile ? "@" : "/") + (m?.[2] ?? head);
    let at = -1;
    for (let i = said.indexOf(token); i >= 0; i = said.indexOf(token, i + 1)) {
      if (i === 0 || /\s/.test(said[i - 1])) { at = i; break; }
    }
    return { isFile, token, body: body.join("\n"), at };
  });
  const chip = (i: number) => (
    <button key={"att" + i} type="button" className="mono prompt-chip" aria-expanded={opened === i}
            title={atts[i].isFile ? "Attached file" : "Skill"} onClick={() => setOpened((v) => (v === i ? null : i))}>
      {atts[i].token}
    </button>
  );
  const said2: React.ReactNode[] = [];
  let pos = 0;
  for (const i of atts.map((_, i) => i).filter((i) => atts[i].at >= 0).sort((a, b) => atts[a].at - atts[b].at)) {
    if (atts[i].at < pos) { atts[i].at = -1; continue; }
    said2.push(said.slice(pos, atts[i].at), chip(i));
    pos = atts[i].at + atts[i].token.length;
  }
  said2.push(said.slice(pos));
  const loose = atts.map((_, i) => i).filter((i) => atts[i].at < 0);
  return (
    <section className="turn" data-turn={n}>
      {turn.prompt && (
        <div className="prompt">
          <span className="mono prompt-mark">&gt;</span>
          <div className="prompt-text">
            {/* A long brief (pasted logs, a spec) is evidence, not the
                thing to navigate by: four lines, and one click for the rest. */}
            <p className={long && !full ? "prompt-clamp" : undefined}>{said2}</p>
            {long && (
              <button className="link prompt-more" onClick={() => setFull((v) => !v)}>
                {full ? "Show less" : "Show full prompt"}
              </button>
            )}
            {images.length > 0 && (
              <div className="prompt-images">
                {images.map((p, i) => (
                  <Thumb key={i} path={p} n={i + 1} />
                ))}
              </div>
            )}
            {loose.length > 0 && <div className="prompt-atts">{loose.map(chip)}</div>}
            {opened !== null && atts[opened] && (
              <pre className={"prompt-att" + (atts[opened].isFile ? " prompt-att-file" : "")}>{atts[opened].body}</pre>
            )}
          </div>
          <span className="num prompt-time">{clock(turn.prompt.at)}</span>
        </div>
      )}
      <div className="turn-body">
        {items.map((it, i) => {
          if (it.kind === "sub") return <SubRun key={"sub" + it.seq} agents={it.agents} seq={it.seq} turn={turn} live={(ctx?.live ?? true) && !turn.done && !turn.stopped} />;
          if (it.kind === "tools") return <ToolRun key={"tools" + it.seq} lines={it.lines} codes={codes} live={!turn.done && !turn.stopped} stopped={turn.stopped || turn.done?.kind === "cancelled"} failSeq={fail?.seq} />;
          if (it.line.kind === "job") {
            // Consecutive job rows share one head, rendered at the first of them.
            const prev = items[i - 1];
            if (prev?.kind === "line" && prev.line.kind === "job") return null;
            const run: Line[] = [];
            for (let j = i; j < items.length; j++) { const x = items[j]; if (x.kind !== "line" || x.line.kind !== "job") break; run.push(x.line); }
            return <JobLines key={"jobs" + it.seq} lines={run} render={(x) => <Entry line={x} codes={codes} />} />;
          }
          return <Entry key={it.seq} line={it.line} codes={codes} />;
        })}
        {tail}
        <TurnHooks lines={hooks} />
      </div>
      {turn.done && <TurnFooter turn={turn} fail={fail} />}
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
        // Folded like a recorded one: the line shows its latest sentence,
        // the essay is one click in.
        <details key={i} className={"block thin thinking" + (i === runs.length - 1 ? " stream-tip" : "")}>
          <summary>
            <span className="block-label">Thinking</span>
            <span className="block-detail">{plainTitle(r.text.split("\n").filter((l) => l.trim()).pop() ?? "").slice(-90)}</span>
          </summary>
          <div className="think-body"><Markdown text={r.text} /></div>
        </details>
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

// The keyboard shrinks the visual viewport, not 100dvh: the app follows it.
if (typeof window !== "undefined" && window.visualViewport) {
  const vv = window.visualViewport;
  const fit = () => document.documentElement.style.setProperty("--app-height", `${vv.height}px`);
  fit();
  vv.addEventListener("resize", fit);
}

export function Controls({ row, projects, onModel, onEffort, onAssign, only }: {
  row: Row; projects: Project[];
  onModel: (m: string) => Promise<boolean> | void; onEffort: (e: string) => Promise<boolean> | void; onAssign: (p: string) => void;
  /** Render just the model picker, or everything but it. */
  only?: "model" | "rest";
}) {
  const [cat, setCat] = useState<{ providers: ProviderInfo[]; efforts: string[] } | null>(null);
  const [catFailed, setCatFailed] = useState(false);
  const [nonce, setNonce] = useState(0);
  useEffect(() => {
    setCatFailed(false);
    fetch("/api/models").then((r) => r.json()).then(setCat).catch(() => { setCat(null); setCatFailed(true); });
  }, [nonce]);

  // A session that has not answered yet genuinely has no model to name;
  // one running a model the catalogue does not list still shows it.
  // "Default" can only be where a session starts: the supervisor has no
  // way back to it, so once a model is set it is not offered.
  const models: Option[] = row.model ? [] : [{ value: "", label: "Default model" }];
  for (const p of cat?.providers ?? []) {
    for (const m of p.models ?? []) {
      // The group already names the provider; the trigger drops it too.
      models.push({ value: m.id, label: m.id, short: m.id.split("/").pop(), group: p.plugin.replace(/^llm-/, ""),
                    detail: m.context ? contextSize(m.context) : undefined });
    }
  }
  if (row.model && !models.some((o) => o.value === row.model)) {
    models.push({ value: row.model, label: row.model, short: row.model.split("/").pop(), group: "In use" });
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
        <div className="ctl ctl-run" title="Model and effort for the next turn">
          <span className="ctl-label ctl-next">Next turn</span>
          <Select label="Next turn model" value={row.model ?? ""} options={models} searchable align="end" note="Applies to the next turn"
                  detailHeading="Context tokens"
                  footer={(o) => {
                    const m = o && cat?.providers.flatMap((p) => p.models ?? []).find((x) => x.id === o.value);
                    const price = (n?: number) => (n ? `$${+n.toFixed(2)}` : "Unavailable");
                    return m ? <>Input {price(m.input)} · Output {price(m.output)} <span className="sel-foot-unit">per 1M tokens</span></> : "Price unavailable";
                  }}
                  onChange={(v) => (v ? onModel(v) : undefined)} />
          {efforts.length > 0 && (
            <Select label="Next turn effort" value={row.effort ?? ""} align="end" note="Applies to the next turn" onChange={(v) => (v ? onEffort(v) : undefined)}
                    options={[...(row.effort ? [] : [{ value: "", label: "Provider default" }]), ...efforts.map((e) => ({ value: e, label: effortLabel(e) }))]} />
          )}
          {catFailed && <button className="btn" onClick={() => setNonce((n) => n + 1)}>Models unavailable · Retry</button>}
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

/**
 * Home: the sidebar is the one list of sessions, so Home lists none. It
 * says how many need you and points at the first such row there.
 */
function ControlOverview({ rows, onReveal, onOpenFailure, loadedAt, loadErr, onRetry }: {
  rows: Row[]; onReveal: (id: string) => void; loadedAt: number | null; loadErr: string | null; onRetry: () => void;
  /** Open the session at the failing call, when its transcript names one. */
  onOpenFailure: (id: string, seq?: number) => void;
}) {
  // The sidebar's own rule (sessionSignal): a failure outranks "done".
  const live = rows.filter((r) => !r.archived && !(r.empty && !r.live));
  const needs = live.filter((r) => sessionSignal(r) === 0).sort((a, b) => Date.parse(b.lastAt) - Date.parse(a.lastAt));
  const failed = needs.filter(hasFailure).slice(0, 5);
  const questions = needs.filter(hasQuestion);
  const running = live.filter((r) => !r.background && sessionSignal(r) === 1).length;
  // The evidence behind each failure: the failing test call and its exit,
  // read from the transcript. A failure with no test call says its trouble.
  const [evid, setEvid] = useState<Record<string, { cmd: string; exit?: number; seq: number; at: string }>>({});
  const key = failed.map((r) => r.id + r.lastAt).join();
  useEffect(() => {
    let on = true;
    for (const r of failed) {
      api.session(r.id).then((d) => {
        const t = lastTestRun(d.entries ?? [], false);
        if (on && t?.state === "failed") setEvid((m) => ({ ...m, [r.id]: { cmd: t.cmd, exit: t.exit, seq: t.seq, at: t.at } }));
      }).catch(() => {});
    }
    return () => { on = false; };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key]);
  return (
    <div className="ov">
      <header className="ov-head">
        <h1>Overview</h1>
        <span className="ov-hint">{modKey()}K to start a session</span>
      </header>
      <div className="scroll ov-body">
        {/* Lists are only as current as the last refresh, and say so. */}
        {loadErr && (
          <p className="ov-stale" role="status">
            {loadedAt === null ? "Status unavailable" : `Updates delayed · synced ${ago(new Date(loadedAt).toISOString())} ago`}
            <button className="link" onClick={onRetry}>Retry</button>
          </p>
        )}
        {needs.length > 0 ? (
          <>
            {failed.length > 0 && (
              <div className="ov-fails" role="list" aria-label="Unresolved failures">
                {failed.map((r) => {
                  const e = evid[r.id];
                  return (
                    <div key={r.id} className="ov-fail" role="listitem">
                      {/* The failing call when the transcript names one; else the only identity there is. */}
                      <span className="ov-fail-what" title={plainTitle(r.title) || r.id}>{e?.cmd ?? `${plainTitle(r.title) || untitled(r.id)} · ${r.trouble || (r.testsFailed ? "tests failed" : "failed")}`}</span>
                      {e?.exit !== undefined && <span className="num ov-fail-exit">exit {e.exit}</span>}
                      <span className="num">{r.repo?.split("/").pop()}</span>
                      <span className="num">{ago(e?.at ?? r.lastAt)} ago</span>
                      <button className="btn" onClick={() => onOpenFailure(r.id, e?.seq)}>Open failure</button>
                    </div>
                  );
                })}
              </div>
            )}
            {questions.length > 0 && (
              <p className="ov-none" role="status">
                <button className="link ov-point" onClick={() => onReveal(questions[0].id)}>{questions.length} waiting for you</button>
              </p>
            )}
          </>
        ) : !loadErr && (
          <p className="ov-none" role="status">{loadedAt === null ? "Loading sessions…" : `Nothing needs your attention.${running ? ` ${running} running.` : ""}`}</p>
        )}
      </div>
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

/**
 * Where each session was read, kept for this tab only: the scroll, which
 * cards and steps were open or shut, and the control focus came from.
 */
const scrollMemo = new Map<string, { top: number; follow: boolean; open?: string[]; shut?: string[]; focus?: string }>();
/** Where focus lands when a session is arrived at from elsewhere ("head": its title). */
const arrivals = new Map<string, string>();

/**
 * The way back from a background agent to the session that started it.
 * A parent the list does not hold is looked up; one that cannot be found
 * is said plainly, not offered as a link.
 */
function ParentLink({ id, rows, onOpen }: { id: string; rows: Row[]; onOpen?: (id: string) => void }) {
  const known = rows.find((r) => r.id === id);
  const [looked, setLooked] = useState<{ title: string } | "failed" | null>(null);
  useEffect(() => {
    if (known) return;
    let on = true;
    api.session(id).then((r) => { if (on) setLooked({ title: r.session.title }); }, () => { if (on) setLooked("failed"); });
    return () => { on = false; };
  }, [id, Boolean(known)]); // eslint-disable-line react-hooks/exhaustive-deps
  if (!known && looked === "failed") return <span className="child-parent-link">Parent unavailable</span>;
  const title = plainTitle(known?.title ?? (looked && looked !== "failed" ? looked.title : ""));
  return (
    <button type="button" className="child-parent-link" onClick={() => onOpen?.(id)}>
      <span aria-hidden="true">←</span>{title ? `Parent: ${title}` : "Parent session"}
    </button>
  );
}

type Pending = { id: string; text: string; after: number };

/** "503 Service Unavailable" or a bare "503" reads as what happened, code last. */
function sendError(e?: string) {
  const m = /^(\d{3})\b\s*(.*)$/.exec(e ?? "");
  if (!m) return e || "No response";
  const words: Record<string, string> = { "500": "Server error", "502": "Bad gateway", "503": "Service unavailable", "504": "Gateway timeout" };
  return `${m[2] || words[m[1]] || "Request failed"} (${m[1]})`;
}

export function Thread({ row, lines, loading = false, loadError, paused, onRetry, stream = [], activity = "", projects, onAck, onSend, onAnswer, onInterrupt, onArchive, onRename, onModel, onEffort, onAssign, onBack, onContext, busy, jump, sending = [], setSending = () => {}, onStopOrb, rows = [], onOpenSession, onStartProject, onNewProject }: {
  /** Loaded sessions: names the parent of a background agent and lists this session's agents. */
  rows?: Row[]; onOpenSession?: (id: string) => void;
  row: Row; lines: Line[]; loading?: boolean; stream?: DeltaRun[];
  /** The first read failed: there is no transcript to show. */
  loadError?: string;
  /** Catching up keeps failing: what is shown is as of this time. */
  paused?: number;
  onRetry?: () => void;
  /** The small model's live label for the running turn; never recorded. */
  activity?: string; projects: Project[]; busy: boolean; onBack?: () => void;
  /** Scroll to this turn (1-based) once it is on screen; `at` makes a repeat click count. */
  jump?: { turn: number; at: number; seq?: number } | null;
  onSend: (t: string) => Promise<string | null> | void; onAnswer: (t: string, ask?: string) => Promise<string | null> | void; onInterrupt: () => Promise<boolean> | void;
  onArchive: () => void; onRename: (t: string) => Promise<void>; onContext?: () => void; onAck?: () => void;
  /** Stop a project session's container; the child restarts it on its next command. */
  onStopOrb?: () => void;
  /** Start a project session in this project's orb, carrying the draft over unsent. */
  onStartProject?: (project: string, draft: string) => void;
  onNewProject?: () => void;
  onModel: (m: string) => Promise<boolean> | void; onEffort: (e: string) => Promise<boolean> | void; onAssign: (p: string) => void;
  /** This session's unrecorded sends, kept by the app across session switches. */
  sending?: Pending[]; setSending?: (f: (q: Pending[]) => Pending[]) => void;
}) {
  // One draft per session: switching away and back keeps what you were
  // typing there, and never carries it into another conversation.
  const draftKey = "bough:draft:" + row.id;
  const [draft, setDraft] = useState(() => { try { return sessionStorage.getItem(draftKey) ?? ""; } catch { return ""; } });
  useEffect(() => {
    try { draft ? sessionStorage.setItem(draftKey, draft) : sessionStorage.removeItem(draftKey); } catch { /* storage off */ }
  }, [draft, draftKey]);
  // Whether the draft is an answer is decided when you start it, keyed to
  // the question on screen then: a question arriving mid-draft must not
  // quietly turn a message into an answer, nor a newer one inherit it.
  // Saved with the draft, so a remount never re-decides it against a newer question.
  const askKey = "bough:draft-ask:" + row.id;
  const [draftAsk, setDraftAsk] = useState(() => { try { return sessionStorage.getItem(askKey) ?? row.ask?.id ?? ""; } catch { return row.ask?.id ?? ""; } });
  useEffect(() => {
    try { draft.trim() ? sessionStorage.setItem(askKey, draftAsk) : sessionStorage.removeItem(askKey); } catch { /* storage off */ }
  }, [draft, draftAsk, askKey]);
  const blank = !draft.trim();
  useEffect(() => { if (blank) setDraftAsk(row.ask?.id ?? ""); }, [blank, row.ask?.id]);
  const askChanged = !blank && draftAsk !== (row.ask?.id ?? "");
  const [trigger, setTrigger] = useState<Trigger | null>(null);
  // Project, rename and archive change rarely: they wait in a popover
  // under More rather than pushing the transcript down.
  const [more, setMore] = useState(false);
  const moreRef = useRef<HTMLDivElement>(null);
  // Opened from the keyboard, focus lands on the first control; opened by
  // pointer, on the dialog itself, so nothing looks preselected.
  const moreByKey = useRef(false);
  const closeMore = useCallback((refocus: boolean) => {
    setMore(false);
    if (refocus) moreRef.current?.querySelector<HTMLButtonElement>("button.more")?.focus();
  }, []);
  useEffect(() => {
    if (!more) return;
    // Escape closes the popover only; it must not also leave the thread.
    // A Select open inside it handles its own Escape first.
    const key = (e: KeyboardEvent) => {
      if (e.key !== "Escape" || moreRef.current?.querySelector(".sel-open")) return;
      e.preventDefault(); e.stopPropagation(); closeMore(true);
    };
    const away = (e: MouseEvent) => { if (!moreRef.current?.contains(e.target as Node)) setMore(false); };
    window.addEventListener("keydown", key, true);
    document.addEventListener("mousedown", away);
    return () => { window.removeEventListener("keydown", key, true); document.removeEventListener("mousedown", away); };
  }, [more, closeMore]);
  // Opening moves focus in, and the popover never runs under the keyboard.
  useLayoutEffect(() => {
    const pop = more ? moreRef.current?.querySelector<HTMLElement>(".head-pop") : null;
    if (!pop) return;
    const fit = () => {
      const vv = window.visualViewport;
      const bottom = vv ? vv.offsetTop + vv.height : innerHeight;
      pop.style.maxHeight = `${Math.max(0, bottom - pop.getBoundingClientRect().top - 12)}px`;
    };
    fit();
    (moreByKey.current ? pop.querySelector<HTMLElement>("button,input") ?? pop : pop).focus();
    window.visualViewport?.addEventListener("resize", fit);
    return () => window.visualViewport?.removeEventListener("resize", fit);
  }, [more]);
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
    if (!loading) scrollMemo.set(row.id, { top: el.scrollTop, follow: atBottom.current });
    // What was recorded when you left, so the button can say something new arrived.
    if (!atBottom.current && !away) awayAt.current = newest;
    setAway(!atBottom.current);
  };
  const awayAt = useRef(0);
  const toLatest = () => {
    atBottom.current = true; setAway(false);
    scrollMemo.delete(row.id);
    const still = window.matchMedia?.("(prefers-reduced-motion: reduce)").matches;
    end.current?.scrollIntoView({ block: "end", behavior: still ? "auto" : "smooth" });
  };
  // The block summaries are one tab stop: the current one is tabbable, and
  // so are its own controls; every other summary and its Copy wait for ↑/↓.
  const summaries = () => [...(scroller.current?.querySelectorAll<HTMLElement>("details.block > summary") ?? [])].filter((s) => s.offsetParent);
  const rovingAt = useRef<HTMLElement | null>(null);
  const rove = (all: HTMLElement[], on: HTMLElement) => {
    rovingAt.current = on;
    for (const s of all) {
      s.tabIndex = s === on ? 0 : -1;
      for (const b of s.querySelectorAll<HTMLElement>("button,a[href]")) {
        if (s === on) b.removeAttribute("tabindex"); else b.tabIndex = -1;
      }
    }
  };
  useEffect(() => {
    const all = summaries();
    if (all.length) rove(all, rovingAt.current && all.includes(rovingAt.current) ? rovingAt.current : all[0]);
  }); // eslint-disable-line react-hooks/exhaustive-deps
  // Cmd/Ctrl+End with focus in the transcript; elsewhere it is the page's.
  const latestKey = (e: React.KeyboardEvent) => {
    // ↑/↓ walk the block summaries, only when one has focus: never in text or output.
    const t = e.target as HTMLElement;
    if ((e.key === "ArrowDown" || e.key === "ArrowUp") && !e.altKey && t.matches("details.block > summary")) {
      const all = summaries();
      const i = all.indexOf(t) + (e.key === "ArrowDown" ? 1 : -1);
      if (i >= 0 && i < all.length) { e.preventDefault(); rove(all, all[i]); all[i].focus(); }
      return;
    }
    if (e.key !== "End" || !(e.metaKey || e.ctrlKey)) return;
    e.preventDefault();
    toLatest();
  };
  // Coming back to a session lands where you were reading. One that was
  // following its output (or is new) opens at the bottom, new events and all.
  const memo = scrollMemo.get(row.id);
  if (memo && !memo.follow) atBottom.current = false;
  const restoredAt = useRef(false);
  useEffect(() => {
    if (loading || restoredAt.current) return;
    restoredAt.current = true;
    const root = scroller.current;
    // Folds first, so the scroll lands on the layout that was left; then focus.
    const fold = (keys: string[] | undefined, on: boolean) => {
      for (const k of keys ?? []) { const d = root?.querySelector<HTMLDetailsElement>(`details[data-open-key="${CSS.escape(k)}"]`); if (d) d.open = on; }
    };
    fold(memo?.shut, false);
    fold(memo?.open, true);
    if (memo && !memo.follow && root) { root.scrollTop = memo.top; awayAt.current = newest; setAway(true); }
    const focus = arrivals.get(row.id) ?? memo?.focus;
    arrivals.delete(row.id);
    if (focus === "head") headRef.current?.focus({ preventScroll: true });
    else if (focus === "work") workBtn.current?.focus({ preventScroll: true });
    else if (focus) root?.querySelector<HTMLElement>(`[data-open-key="${CSS.escape(focus)}"] > summary`)?.focus({ preventScroll: true });
  }, [loading]); // eslint-disable-line react-hooks/exhaustive-deps
  useEffect(() => {
    if (atBottom.current) end.current?.scrollIntoView({ block: "end" });
  }, [lines.length, streamLen]);
  const turns = useMemo(() => groupTurns(lines), [lines]);

  // The session's Work: its jobs, its subagents and its direct background
  // agents, one index the transcript, the Work button and its dialog read.
  const kids = useChildren(row, rows, lines.length);
  const review = useReviewed(row.id);
  const reports = useMemo(() => agentReports(lines), [lines]);
  const workers = useMemo(() => workIndex({ session: row.id, lines, turns, row, rows, children: kids.children, live: row.live })
    // A child's report to this session is its result; nothing else carries one.
    .map((w) => (w.kind === "agent" && reports.has(w.id) ? { ...w, result: reports.get(w.id) } : w)), [row, lines, turns, rows, kids.children, reports]);
  const byKey = useMemo(() => new Map(workers.map((w) => [w.key, w])), [workers]);
  const { stops, requestStop } = useStopStore(byKey, review);
  const workCtx = useMemo<WorkCtx>(() => {
    const jobs = new Map(workers.filter((w) => w.kind === "job").map((w) => [w.id, w]));
    const jobFirst = new Map<string, number>();
    for (const l of lines) {
      const id = l.kind === "job" ? jobIdOf(l) : undefined;
      if (id !== undefined && !jobFirst.has(String(id))) jobFirst.set(String(id), l.seq);
    }
    return { session: row.id, live: row.live, workers, byKey, jobs, jobFirst, review, stops, requestStop };
  }, [row.id, row.live, workers, byKey, lines, review, stops, requestStop]);
  const counts = workCounts(workers, review.isNew);
  // Visible whenever there is work, and while background agents are still being looked up.
  const showWork = counts.total > 0 || kids.state === "loading" || kids.state === "error";
  const [workOpen, setWorkOpen] = useState(false);
  const [workTop, setWorkTop] = useState<number>();
  const workBtn = useRef<HTMLButtonElement>(null);
  const headRef = useRef<HTMLHeadingElement>(null);
  const threadRef = useRef<HTMLDivElement>(null);
  // Narrow is the pane, not the window: a thin pane beside a wide sidebar reads the same as a phone.
  const [paneNarrow, setPaneNarrow] = useState(false);
  useEffect(() => {
    const el = threadRef.current;
    if (!el || typeof ResizeObserver === "undefined") return;
    const ro = new ResizeObserver(() => setPaneNarrow(el.clientWidth < 760));
    ro.observe(el);
    return () => ro.disconnect();
  }, []);
  const sheet = useMedia("(max-width:480px)");
  const openWork = () => {
    const b = workBtn.current?.getBoundingClientRect(), t = threadRef.current?.getBoundingClientRect();
    if (b && t) setWorkTop(b.bottom - t.top + 4);
    setWorkOpen(true);
  };
  const closeWork = useCallback((refocus: boolean) => {
    setWorkOpen(false);
    if (refocus) requestAnimationFrame(() => workBtn.current?.focus());
  }, []);
  // Where focus came from when this session is left for another, restored on the way back.
  const origin = useRef<string | null>(null);
  const viewInTranscript = (w: Worker) => {
    setWorkOpen(false);
    // After the dialog is gone and the page is no longer inert.
    requestAnimationFrame(() => {
      const el = scroller.current?.querySelector<HTMLElement>(`[data-work-key="${CSS.escape(w.key)}"]`);
      if (!el) { workBtn.current?.focus(); return; }
      atBottom.current = false;
      // Opened for you, so not reviewed: only your own click on it is.
      for (let d: HTMLElement | null = el; d; d = d.parentElement?.closest("details") ?? null) if (d instanceof HTMLDetailsElement) d.open = true;
      el.scrollIntoView({ block: "center" });
      el.querySelector<HTMLElement>("summary")?.focus({ preventScroll: true });
    });
  };
  const openAgent = (w: Worker) => {
    setWorkOpen(false);
    origin.current = "work";
    arrivals.set(w.id, "head");
    onOpenSession?.(w.id);
  };
  const announce = useWorkAnnouncer(workers, !loading);
  // Leaving: remember which folds were open or shut, and the control focus was on.
  useLayoutEffect(() => {
    const root = scroller.current;
    return () => {
      if (!root) return;
      const folds = [...root.querySelectorAll<HTMLDetailsElement>("details[data-open-key]")];
      const a = document.activeElement as HTMLElement | null;
      const focus = origin.current ?? (a && root.contains(a) ? a.closest<HTMLElement>("[data-open-key]")?.dataset.openKey : undefined);
      const m = scrollMemo.get(row.id);
      scrollMemo.set(row.id, {
        top: m?.top ?? root.scrollTop, follow: m?.follow ?? atBottom.current,
        open: folds.filter((d) => d.open).map((d) => d.dataset.openKey!),
        shut: folds.filter((d) => !d.open && d.classList.contains("sub")).map((d) => d.dataset.openKey!),
        focus,
      });
    };
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  // A turn picked from the log: land on it once the transcript holds it,
  // flash it, and stop following the bottom so it stays put.
  const jumped = useRef(0);
  useEffect(() => {
    if (!jump || loading || jumped.current === jump.at) return;
    // A failure lands on its call, with every fold around it opened.
    const el = scroller.current?.querySelector<HTMLElement>(jump.seq ? `[data-seq="${jump.seq}"]` : `.turn[data-turn="${jump.turn}"]`);
    if (!el) return;
    jumped.current = jump.at;
    atBottom.current = false;
    for (let d: HTMLElement | null = el; d; d = d.parentElement?.closest("details") ?? null) if (d instanceof HTMLDetailsElement) d.open = true;
    el.scrollIntoView({ block: jump.seq ? "center" : "start" });
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

  const [multi, setMulti] = useState(false);
  const [narrow, setNarrow] = useState(false);
  // The textarea's width in the one-row layout, remembered for collapsing back.
  const composerRow = useRef(0);
  // A long paste should be visible, not a two-row porthole you have to
  // drag open. Grow to the text and stop at a third of the window.
  useEffect(() => {
    const el = composer.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = Math.min(el.scrollHeight, Math.round((window.visualViewport?.height ?? window.innerHeight) / 3)) + "px";
    // Two rows only once the text really wraps (measured, not counted);
    // staying multi until it fits again keeps the layout from flapping.
    const line = parseFloat(getComputedStyle(el).lineHeight) || 20;
    const pad = parseFloat(getComputedStyle(el).paddingTop) + parseFloat(getComputedStyle(el).paddingBottom);
    const wraps = draft.includes("\n") || el.scrollHeight - pad > line * 1.5;
    if (wraps !== multi) {
      if (wraps) setMulti(true);
      else if (!draft) setMulti(false);
      else {
        // Measure at single-row width before collapsing back.
        const probe = el.cloneNode() as HTMLTextAreaElement;
        probe.style.cssText = `position:absolute;visibility:hidden;height:auto;width:${composerRow.current}px`;
        probe.value = draft; el.parentElement?.appendChild(probe);
        if (probe.scrollHeight - pad <= line * 1.5) setMulti(false);
        probe.remove();
      }
    }
  }, [draft, multi]); // eslint-disable-line react-hooks/exhaustive-deps

  // Escape shuts a picker until the token it was over changes; the keyup
  // that follows the Escape would otherwise open it straight back.
  const dismissed = useRef("");
  useEffect(() => {
    const el = composer.current;
    if (!el || typeof ResizeObserver === "undefined") return;
    const box = el.parentElement!;
    // Width left for typing beside the buttons, measured: under 160px the buttons take their own row.
    const fit = () => {
      if (!box.classList.contains("composer-multi")) composerRow.current = el.clientWidth;
      const actions = box.querySelector<HTMLElement>(".composer-actions");
      setNarrow(box.clientWidth - (actions?.offsetWidth ?? 0) - 30 < 160);
    };
    const ro = new ResizeObserver(fit);
    ro.observe(el); ro.observe(box);
    const actions = box.querySelector(".composer-actions");
    if (actions) ro.observe(actions);
    window.visualViewport?.addEventListener("resize", fit);
    return () => { ro.disconnect(); window.visualViewport?.removeEventListener("resize", fit); };
  }, []);
  const [pickerOpen, setPickerOpen] = useState(false);
  const [activeOpt, setActiveOpt] = useState<string | undefined>();
  const caretTrigger = (el: HTMLTextAreaElement) => {
    const t = triggerAt(el.value, el.selectionStart ?? 0);
    const key = t ? `${t.from}:${t.kind}${t.token}` : "";
    if (key !== dismissed.current) dismissed.current = "";
    setTrigger(key && key === dismissed.current ? null : t);
  };

  // A message that did not go through is kept on its own, not folded back
  // into the draft: you may already be typing the next one, and switching
  // sessions must not lose it.
  const failedKey = "bough:failed:" + row.id;
  type Failure = { id?: string; at?: number; text: string; answer: boolean; ask?: string; error?: string };
  // Every send that did not go through is kept, each with its own cause.
  const [failures, setFailures] = useState<Failure[]>(() => {
    try { const v = JSON.parse(sessionStorage.getItem(failedKey) ?? "null"); return !v ? [] : Array.isArray(v) ? v : [v]; } catch { return []; }
  });
  useEffect(() => {
    try { failures.length ? sessionStorage.setItem(failedKey, JSON.stringify(failures)) : sessionStorage.removeItem(failedKey); } catch { /* storage off */ }
  }, [failures, failedKey]);
  const drop = (f: Failure) => setFailures((q) => q.filter((x) => x !== f));

  // What you just sent, shown the moment you send it. The recorded input
  // can take seconds to land (the child may be starting), and a message
  // that vanished from the composer with nothing in its place read as lost.
  // Each send is its own record, and each input recorded after it claims
  // exactly one, oldest first: a second send can no longer be "confirmed"
  // by the first one landing, nor replace its preview.
  const newest = lines.length ? lines[lines.length - 1].seq : 0;
  // While the transcript loads, the lines on hand may be another session's.
  const unlanded = loading ? sending : sending.filter((p, i) => lines.filter((l) => l.kind === "input" && l.seq > p.after).length <= i);
  const landedIds = sending.length - unlanded.length;
  useEffect(() => { if (landedIds) setSending((q) => q.slice(landedIds)); }, [landedIds]); // eslint-disable-line react-hooks/exhaustive-deps
  const [fullPending, setFullPending] = useState("");
  // Offered whenever the clamp actually hides something, whatever the length.
  const [clipped, setClipped] = useState<Record<string, boolean>>({});
  // Sending is something you did, so it is followed like new output is.
  useEffect(() => { if (atBottom.current) end.current?.scrollIntoView({ block: "end" }); }, [sending.length]);

  // The option being submitted, keyed to the question it answers.
  const [answering, setAnswering] = useState<{ ask: string; text: string } | null>(null);
  // `ask` is the question the answer was written for, captured when it was
  // written: the server refuses it once a newer question has replaced it.
  const deliver = async (t: string, answer: boolean, ask = row.ask?.id, retried?: Failure) => {
    if (retried) drop(retried);
    // An answer to a question that has since been replaced goes back to
    // the draft, unless you are already writing something newer there.
    if (answer && retried && ask !== row.ask?.id) {
      if (!draft.trim()) setDraft(t);
      else setFailures((q) => [...q, { ...retried, error: "That question expired" }]);
      return;
    }
    // A retry is the same request, so it keeps its id.
    const id = retried?.id ?? (typeof crypto.randomUUID === "function" ? crypto.randomUUID() : `${Date.now()}-${Math.random()}`);
    if (!answer) setSending((q) => [...q, { id, text: t, after: newest }]);
    else setAnswering({ ask: ask ?? "", text: t });
    const error = await (answer ? onAnswer(t, ask) : onSend(t));
    if (answer) setAnswering(null);
    if (error) setSending((q) => q.filter((p) => p.id !== id));
    // Each request is its own row; a retry that fails again replaces its own.
    if (error) setFailures((q) => [...q.filter((f) => f.id !== id), { id, at: Date.now(), text: t, answer, ask, error }]);
  };
  // Stop is asked once; the button says so until the ask is answered.
  const [stopping, setStopping] = useState<"" | "stopping" | "failed">("");
  useEffect(() => { if (!running) setStopping(""); }, [running]);
  const stop = async () => {
    setStopping("stopping");
    const ok = await onInterrupt();
    if (ok === false) setStopping("failed");
  };
  // Pastes too big to edit in place, and pasted images, sit in the draft
  // as "[Pasted text #N +L lines]" and "[Image #N]" tags, as in the TUI.
  // Send swaps them for the text and "[Image #N: path]"; a tag you
  // deleted drops its paste.
  const pastes = useRef<string[]>([]);
  const images = useRef<string[]>([]);
  // They are part of the draft: saved with it per session, so switching
  // away and back never leaves a bare tag that would be sent as text.
  const attsKey = "bough:draft-atts:" + row.id;
  const restored = useRef(false);
  if (!restored.current) {
    restored.current = true;
    try {
      const a = JSON.parse(sessionStorage.getItem(attsKey) ?? "null") as { pastes: string[]; images: string[] } | null;
      if (a) { pastes.current = a.pastes; images.current = a.images; }
    } catch { /* storage off */ }
  }
  useEffect(() => {
    try {
      if (draft && (pastes.current.length || images.current.length)) sessionStorage.setItem(attsKey, JSON.stringify({ pastes: pastes.current, images: images.current }));
      else sessionStorage.removeItem(attsKey);
    } catch { /* storage off */ }
  }, [draft, attsKey]);
  const [uploading, setUploading] = useState(0);
  const [attachErr, setAttachErr] = useState("");
  const insert = (s: string) => {
    const el = composer.current;
    const from = el?.selectionStart ?? draft.length, to = el?.selectionEnd ?? draft.length;
    setDraft((d) => d.slice(0, from) + s + d.slice(to));
    setTrigger(null);
    requestAnimationFrame(() => { el?.focus(); el?.setSelectionRange(from + s.length, from + s.length); });
  };
  const attach = async (files: File[]) => {
    setAttachErr("");
    // The tag lands at paste time, where the caret was; its path follows.
    // Until it does, the slot is "" and Send waits.
    // Images and other files share the slots; only the tag differs. A
    // non-image goes to the session's scratchpad and is sent as its path.
    const isImage = (f: File) => /^image\/(png|jpeg|gif|webp)$/.test(f.type);
    const tag = (f: File, i: number) => `${isImage(f) ? "Image" : "File"} #${i + 1}`;
    const slots = files.map(() => images.current.push("") - 1);
    insert(slots.map((i, k) => `[${tag(files[k], i)}] `).join(""));
    for (const [k, f] of files.entries()) {
      setUploading((n) => n + 1);
      try {
        images.current[slots[k]] = isImage(f) ? await api.attach(f) : await api.attachFile(row.id, f);
        try { sessionStorage.setItem(attsKey, JSON.stringify({ pastes: pastes.current, images: images.current })); } catch { /* storage off */ }
      } catch (err) {
        setAttachErr(`${tag(f, slots[k])} not attached: ${(err as Error).message}`);
      } finally {
        setUploading((n) => n - 1);
      }
    }
  };
  // A paste and a drop carry the same DataTransfer; a drop has no default
  // insert, so short text lands through insert() instead of the browser.
  const take = (e: { dataTransfer: DataTransfer; preventDefault(): void }, drop: boolean) => {
    pasted.current = true;
    const files = [...e.dataTransfer.files];
    if (files.length) { e.preventDefault(); void attach(files); return; }
    const text = e.dataTransfer.getData("text/plain").replace(/\r\n?/g, "\n");
    const n = text.split("\n").length;
    if (drop) e.preventDefault();
    if (text.length <= 800 && n <= 12) { if (drop && text) insert(text); return; }
    e.preventDefault();
    pastes.current.push(text);
    insert(`[Pasted text #${pastes.current.length} +${n} lines] `);
  };
  const expand = (t: string) => t
    .replace(/\[(Image|File) #(\d+)\]/g, (m, k, i) => (images.current[i - 1] ? `[${k} #${i}: ${images.current[i - 1]}]` : m))
    .replace(/\[Pasted text #(\d+) \+\d+ lines\]/g, (m, i) => pastes.current[i - 1] ?? m);

  const send = async () => {
    const t = draft.trim();
    // Enter reaches here even while the Send button is disabled.
    if (!t || busy || uploading || askChanged) return;
    // A tag whose content is gone is never sent as its placeholder.
    const lost = [...t.matchAll(/\[(?:Image|File) #(\d+)\]|\[Pasted text #(\d+) \+\d+ lines\]/g)]
      .filter((m) => m[1] ? !images.current[+m[1] - 1] : pastes.current[+m[2] - 1] === undefined);
    if (lost.length) { setAttachErr(`Attachment unavailable: remove ${lost.map((m) => m[0]).join(", ")}`); return; }
    setDraft("");
    const full = expand(t);
    pastes.current = []; images.current = [];
    await deliver(full, Boolean(draftAsk), draftAsk || undefined);
  };

  // Enter steers a running turn; ⌘/Ctrl+Enter holds the message until the
  // turn ends. The queue is per session and survives a reload.
  const queueKey = "bough:queue:" + row.id;
  const [queued, setQueued] = useState<{ id: string; text: string }[]>(() => {
    try { return JSON.parse(sessionStorage.getItem(queueKey) ?? "[]"); } catch { return []; }
  });
  useEffect(() => {
    try { queued.length ? sessionStorage.setItem(queueKey, JSON.stringify(queued)) : sessionStorage.removeItem(queueKey); } catch { /* storage off */ }
  }, [queued, queueKey]);
  const enqueue = () => {
    const t = draft.trim();
    if (!t || uploading || draftAsk || askChanged) return;
    setDraft("");
    const full = expand(t);
    pastes.current = []; images.current = [];
    setQueued((q) => [...q, { id: `${Date.now()}-${Math.random()}`, text: full }]);
  };
  // One queued message per ended turn: the next waits for the turn this one starts.
  // A send is done flushing when its turn starts or ends, or when it fails.
  const flushing = useRef(false);
  const doneTurns = turns.filter((t) => t.done).length;
  useEffect(() => { flushing.current = false; }, [running, doneTurns, failures.length]);
  useEffect(() => {
    if (running || loading || busy || row.ask || flushing.current || unlanded.length || !queued.length) return;
    flushing.current = true;
    const [next, ...rest] = queued;
    setQueued(rest);
    void deliver(next.text, false);
  }); // eslint-disable-line react-hooks/exhaustive-deps

  return (
    <WorkContext.Provider value={workCtx}>
    <div className="thread" ref={threadRef}>
      <header className="thread-head">
        <Back onBack={onBack} />
        <div className="head-main">
          {row.spawnedBy && <ParentLink id={row.spawnedBy} rows={rows} onOpen={onOpenSession} />}
          <h1 title={row.title} ref={headRef} tabIndex={-1}>{plainTitle(row.title) || untitled(row.id)}</h1>
          {(row.repo || row.branch) && (
            <span className="mono head-repo">
              {row.repo?.split("/").pop()}
              {row.branch && <span style={{ color: "var(--line-strong)" }}>/</span>}{row.branch}
            </span>
          )}
          {row.trouble && row.trouble !== "tests failed" ? (
            // One status: the reason replaces "Done".
            <span className="status head-trouble"><StatusMark status="error" bare />{capital(row.trouble)}</span>
          ) : row.status === "done" ? <span className="status head-idle">Run: Idle</span> : <StatusMark status={row.status} />}
          <ModeChip row={row} />
          {row.orb?.status === "running" && onStopOrb && <button className="btn head-ack" onClick={onStopOrb}>Stop orb</button>}
          {/* A test failure is the Tests chip's to say, once. */}
          {running && turns[turns.length - 1]?.prompt?.at && !turns[turns.length - 1]?.done && <RunClock since={turns[turns.length - 1].prompt!.at} />}
          {row.trouble && onAck && <button className="btn head-ack" onClick={onAck}>Mark seen</button>}
          {/* A phone keeps the run setting as words; its pickers are under Settings. */}
          <span className="head-run">{row.model?.split("/").pop() ?? "Default model"} · Effort: {row.effort ? effortLabel(row.effort) : "Default"}</span>
        </div>
        <div className="head-side">
          <Controls row={row} projects={projects} onModel={onModel} onEffort={onEffort} onAssign={onAssign} only="model" />
          <div className="head-more" ref={moreRef}>
            <button className="more" aria-label="Session settings" aria-expanded={more} aria-controls={"more-" + row.id}
                    onClick={(e) => { moreByKey.current = e.detail === 0; setMore((v) => !v); }}>
              <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.7"
                   strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                <path d="M4 7h10M18 7h2M4 17h4M12 17h8" /><circle cx="15" cy="7" r="2" /><circle cx="9" cy="17" r="2" />
              </svg>
            </button>
            {more && (
              <div className="head-pop" role="dialog" aria-label="Session settings" id={"more-" + row.id} tabIndex={-1} onKeyDown={(e) => {
                // Non-modal: tabbing past either end closes it, back on Settings.
                if (e.key !== "Tab") return;
                const all = [...e.currentTarget.querySelectorAll<HTMLElement>("button,input")].filter((el) => el.offsetParent);
                const first = all[0], last = all[all.length - 1];
                const here = document.activeElement;
                if ((e.shiftKey && (here === first || here === e.currentTarget)) || (!e.shiftKey && here === last)) { e.preventDefault(); closeMore(true); }
              }}>
                <div className="head-pop-run"><Controls row={row} projects={projects} onModel={onModel} onEffort={onEffort} onAssign={onAssign} only="model" /></div>
                <Controls row={row} projects={projects} onModel={onModel} onEffort={onEffort} onAssign={onAssign} only="rest" />
                <button className="head-pop-item" onClick={async () => {
                  closeMore(false);
                  // Empty is allowed: it hands the title back to the session.
                  await askText("Rename session", { initial: plainTitle(row.title), action: "Rename", allowEmpty: true, onSubmit: onRename });
                }}>Rename</button>
                <button className="head-pop-item" onClick={() => { closeMore(true); onArchive(); }}>{row.archived ? "Unarchive" : "Archive"}</button>
              </div>
            )}
          </div>
        </div>
        <RuntimeStrip row={row} lines={lines} paused={paused} onRetry={onRetry} onContext={onContext}
          work={showWork && (
            <WorkButton btnRef={workBtn} counts={counts} loading={kids.state === "loading"} unavailable={kids.state === "error"}
                        paused={paused !== undefined} narrow={paneNarrow || sheet} expanded={workOpen}
                        onClick={() => (workOpen ? closeWork(true) : openWork())} />
          )} />
      </header>

      <div className="scroll transcript" ref={scroller} onScroll={onScroll} onKeyDown={latestKey}
           onFocus={(e) => { const t = e.target as HTMLElement; if (t.matches("details.block > summary") && t !== rovingAt.current) rove(summaries(), t); }}
           tabIndex={0} role="region" aria-label="Transcript">
        {loading && loadError && (
          <p className="meta-line transcript-state transcript-retry" role="alert">
            Couldn't load transcript <button className="link" onClick={onRetry}>Retry</button>
          </p>
        )}
        {loading && !loadError && slow && <p className="meta-line transcript-state" role="status">Loading transcript…</p>}
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
        {unlanded.map((p) => (
          <section key={p.id} className="turn turn-sending">
            <div className="prompt">
              <span className="mono prompt-mark">&gt;</span>
              <div className="prompt-text">
                <p className={fullPending === p.id ? "" : "prompt-clamp"} ref={(el) => {
                  if (el && !clipped[p.id] && el.scrollHeight > el.clientHeight + 1) setClipped((m) => ({ ...m, [p.id]: true }));
                }}>{p.text}</p>
                {clipped[p.id] && (
                  <button className="link" onClick={() => setFullPending((v) => (v === p.id ? "" : p.id))}>
                    {fullPending === p.id ? "Show less" : "Show full prompt"}
                  </button>
                )}
              </div>
              <span className="num prompt-time turn-sending-state" role="status">{running ? "Steer pending…" : "Sending…"}</span>
            </div>
          </section>
        ))}
        {running && (turns.length === 0 || turns[turns.length - 1].done) && !row.ask && (
          <div className="turn"><div className="turn-body"><StreamView runs={stream} /><Working label={activity || "Working"} /></div></div>
        )}
        {row.ask && (
          <div className="ask" ref={ask}>
            <StatusMark status="needs-you" size={16} />
            {/* Questions carry paths and commands in backticks; raw, they read as noise. */}
            <div className="ask-q"><Markdown text={row.ask.text} /></div>
            {row.ask.secret && <SecretAnswer key={row.ask.id} askId={row.ask.id} onAnswer={onAnswer} />}
            {!row.ask.secret && row.ask.options.length > 0 && (
              <div className="ask-options">
                {/* Equal alternatives, so none of them is dressed as the primary action. */}
                {row.ask.options.map((o) => (
                  <button key={o} className="btn ask-option" disabled={busy || answering !== null}
                          onClick={() => deliver(o, true)}>
                    {answering?.ask === row.ask?.id && answering?.text === o ? "Submitting…" : o}
                  </button>
                ))}
              </div>
            )}
          </div>
        )}
        <div ref={end} />
      </div>

      <div className="composer-wrap">
        {row.ask && !askSeen && (
          <div className="ask-bar">
            <p><strong>Needs your answer</strong></p>
            <button className="btn" onClick={() => {
              // Land on the answer, not just near it: the first option if
              // there are any, otherwise the composer the answer is typed in.
              ask.current?.scrollIntoView({ block: "center" });
              (ask.current?.querySelector("button") ?? composer.current)?.focus({ preventScroll: true });
            }}>
              Review question
            </button>
          </div>
        )}
        {askChanged && (
          <div className="send-failed" role="alert">
            <span className="send-failed-line">
              <strong>Question changed</strong>
              <span className="send-failed-text">{draftAsk ? "the one this answer was for is gone" : "this draft was written as a message"}</span>
            </span>
            {/* Nothing changes until you choose; each choice is one small link. */}
            <span className="send-failed-actions">
              {row.ask && <button className="link" onClick={() => { ask.current?.scrollIntoView({ block: "center" }); setDraftAsk(row.ask?.id ?? ""); composer.current?.focus({ preventScroll: true }); }}>Use for this question</button>}
              <button className="link" onClick={() => { setDraftAsk(""); composer.current?.focus({ preventScroll: true }); }}>Keep as message</button>
            </span>
          </div>
        )}
        {failures.map((failed, i) => (
          <div key={failed.id ?? i} className="send-failed" role="alert">
            {/* What failed and why on one line; Retry and Edit need no expanding. */}
            <details>
              <summary>
                <svg className="send-failed-chev" width="12" height="12" viewBox="0 0 12 12" aria-hidden="true"><path d="M4.5 2.5 8 6l-3.5 3.5" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round" /></svg>
                <strong>{failed.answer ? "Answer not sent" : "Not sent"}</strong>
                <span className="send-failed-text">{sendError(failed.error)}<span className="send-failed-gist"> · {failed.text}</span></span>
              </summary>
              <div className="send-failed-body">
                <p className="send-failed-prompt">{failed.text}</p>
                <span className="send-failed-meta">
                  {failed.at && <span className="num">Tried {new Date(failed.at).toLocaleTimeString([], { hour: "numeric", minute: "2-digit", second: "2-digit" })}</span>}
                  <button className="link" onClick={() => drop(failed)}>Discard</button>
                </span>
              </div>
            </details>
            <span className="send-failed-actions">
              <button className="btn" disabled={busy} onClick={() => deliver(failed.text, failed.answer, failed.ask, failed)}>Retry</button>
              {/* Edit never lands on a newer draft: two prompts glued together is a third nobody wrote. */}
              <button className="btn" disabled={Boolean(draft.trim())} title={draft.trim() ? "Send or clear the current draft first" : undefined}
                onClick={() => { setDraft(failed.text); setDraftAsk(failed.answer ? failed.ask ?? "" : ""); drop(failed); composer.current?.focus(); }}>Edit</button>
            </span>
          </div>
        ))}
        {queued.length > 0 && (
          <ol className="queued" aria-label="Queued until the turn ends">
            {queued.map((m) => (
              <li key={m.id} className="queued-row">
                <span className="queued-tag">Queued</span>
                <span className="queued-text">{m.text}</span>
                {/* Edit never lands on a newer draft, as with a failed send. */}
                <button className="link" disabled={Boolean(draft.trim())} title={draft.trim() ? "Send or clear the current draft first" : undefined}
                  onClick={() => { setDraft(m.text); setQueued((q) => q.filter((x) => x.id !== m.id)); composer.current?.focus(); }}>Edit</button>
                <button className="link" onClick={() => setQueued((q) => q.filter((x) => x.id !== m.id))}>Remove</button>
              </li>
            ))}
          </ol>
        )}
        <div className={"composer" + (multi || narrow ? " composer-multi" : "")}
             onDragOver={(e) => e.preventDefault()}
             onDrop={(e) => { composer.current?.focus(); take(e, true); }}>
          <Mentions trigger={trigger} session={row.id}
            onPick={(t, value) => {
              // Replace the token being typed, and leave a trailing
              // space so the next word is not glued to it.
              // The whole token, including any of it after the caret.
              // One following space at most: a newline after the token is yours.
              const tail = /^\S* ?/.exec(draft.slice(t.to))![0];
              const next = draft.slice(0, t.from) + t.kind + value + " " + draft.slice(t.to + tail.length);
              setDraft(next);
              setTrigger(null);
              const el = composer.current;
              const caret = t.from + value.length + 2;
              requestAnimationFrame(() => { el?.focus(); el?.setSelectionRange(caret, caret); });
            }}
            onClose={() => { if (trigger) dismissed.current = `${trigger.from}:${trigger.kind}${trigger.token}`; setTrigger(null); }}
            onOpen={setPickerOpen} onActive={setActiveOpt} />
          <textarea id="composer" ref={composer} value={draft} rows={1}
            aria-label={draftAsk || (blank && row.ask) ? "Answer to agent question" : "Message"}
            aria-controls={pickerOpen ? "mention-list" : undefined}
            aria-activedescendant={pickerOpen ? activeOpt : undefined}
            disabled={Boolean(row.ask?.secret)}
            placeholder={row.ask?.secret ? "Answer in the secret field" : row.ask && !askChanged ? "Answer…" : running ? "Steer the running turn…" : row.spawnedBy ? "Message this background agent…" : "Next turn…"}
            onPaste={(e) => take({ dataTransfer: e.clipboardData, preventDefault: () => e.preventDefault() }, false)}
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
            onBlur={(e) => {
              pasted.current = false;
              // Focus moving into the picker (its Retry) keeps it open.
              if (e.relatedTarget instanceof Node && e.currentTarget.parentElement?.querySelector(".mention")?.contains(e.relatedTarget)) return;
              setTrigger(null);
            }}
            onKeyDown={(e) => {
              // The paste chord's own keydown comes before the paste, so
              // the next keydown after one is a genuine later keystroke.
              if (!(e.metaKey || e.ctrlKey)) pasted.current = false;
              // An open picker takes its keys before this runs (capture),
              // including Enter in its empty, loading and error states.
              // Enter that confirms an IME composition is not a send.
              if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
                e.preventDefault();
                if (running && (e.metaKey || e.ctrlKey)) enqueue(); else send();
              }
            }} />
          <div className="composer-bar">
            {uploading > 0 && <span className="attach-note">Attaching image…</span>}
            {attachErr && <span className="attach-note attach-err" role="alert">{attachErr}</span>}
            <div className="composer-actions">
              {/* Beside Send, so it never covers what you are reading. */}
              {away && (
                <button className="btn jump-latest" onClick={toLatest} title={modKey() + "End"}
                        aria-label={newest > awayAt.current ? "New activity, jump to latest" : "Jump to latest"}>
                  <span aria-hidden="true">↓</span>
                  <span className="jump-word">{newest > awayAt.current ? "New activity" : "Latest"}</span>
                </button>
              )}
              <SkillPicker onPick={(name, known) => {
                // A skill runs only as the lead word, so a pick replaces a
                // skill already there, never a leading path like /tmp/x.
                setDraft((d) => {
                  const rest = d.trimStart(), lead = /^\/(\S+)\s*/.exec(rest);
                  return `/${name} ${lead && known.includes(lead[1]) ? rest.slice(lead[0].length) : rest}`;
                });
                document.getElementById("composer")?.focus();
              }} />
              {running && (
                <button className="btn" disabled={stopping === "stopping"} onClick={stop}>
                  {stopping === "stopping" ? "Stopping…" : stopping === "failed" ? "Couldn’t stop · Retry" : "Stop"}
                </button>
              )}
              {running && !draftAsk && (
                <button className="btn" onClick={enqueue} disabled={uploading > 0 || blank || askChanged}
                        title={modKey() + "Enter: send when this turn ends"}>Queue</button>
              )}
              <button className="btn btn-primary" onClick={send} disabled={busy || uploading > 0 || blank || askChanged}
                      title={running && !draftAsk ? "Enter: steer the running turn" : undefined}>
                {(blank ? row.ask : draftAsk) ? "Answer" : running ? "Steer" : "Send"}
              </button>
            </div>
          </div>
        </div>
        <div className="composer-foot">
          {row.mode !== "project" && (
            <span className="composer-local">
              <span className="mode-local" title="A local session reads your files but cannot change them">Local · read-only</span>
              {onStartProject && projects.some((p) => p.slug) ? (
                <Select label="Start project session" value="" placeholder="Start project session…" align="start"
                        note="Your draft moves to the new session, unsent"
                        options={projects.filter((p) => p.slug).map((p) => ({ value: p.id, label: p.name }))}
                        onChange={(id) => onStartProject(id, expand(draft))} />
              ) : onNewProject && (
                // No project to run in yet: the palette's project flow; the draft stays here.
                <button type="button" className="btn composer-start" onClick={onNewProject}>Start project session…</button>
              )}
            </span>
          )}
          {!pickerOpen && (
            <span className="hint composer-hint">
              {running && !draftAsk ? `Enter steer · ${modKey()}Enter queue · Shift+Enter newline` : "Enter send · Shift+Enter newline"} · / commands · @ files
            </span>
          )}
        </div>
      </div>
      {workOpen && (
        <WorkDialog workers={workers} sheet={sheet} top={workTop} childState={kids.state} onRetryChildren={kids.retry}
                    paused={paused !== undefined} parent={row.id} onClose={closeWork} onView={viewInTranscript} onOpenAgent={openAgent} />
      )}
      {/* Transitions after the first load, batched: never a clock ticking. */}
      <span className="visually-hidden" role="status" aria-live="polite">{announce}</span>
    </div>
    </WorkContext.Provider>
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
  // The open transcript's read state: a first read that failed, and since
  // when catching up has been failing. Retry re-reads or catches up now.
  const [loadFail, setLoadFail] = useState<string | null>(null);
  // Only an authoritative 404 says a session is not here; other failures can be retried.
  const [missing, setMissing] = useState(false);
  const [paused, setPaused] = useState<number | undefined>(undefined);
  const [loadTry, setLoadTry] = useState(0);
  const retryRef = useRef<() => void>(() => {});
  useEffect(() => { lastSeq.current = lines.length ? lines[lines.length - 1].seq : 0; }, [lines]);
  // Live fragments of the reply being written, newest last. Never
  // merged into `lines`: these carry no history seq and the recorded
  // entry always supersedes them.
  const [stream, setStream] = useState<DeltaRun[]>([]);
  const [activity, setActivity] = useState("");
  const [query, setQuery] = useState("");
  const [archived, setArchived] = useState(false);
  // Whether the rows on hand were fetched with archived ones included.
  const [rowsAll, setRowsAll] = useState(false);
  const [busy, setBusy] = useState(false);
  // Two kinds of failure. A refresh that failed is stale data and heals on
  // the next poll; a send or action that failed is something you did that
  // did not happen, so it stays until you dismiss it — the 4s poll used to
  // clear it before it could be read.
  // What failed, named, with the call that failed so Retry repeats it on the same target.
  const [err, setErr] = useState<{ label: string; msg: string; retry: () => void } | null>(null);
  const [loadErr, setLoadErr] = useState<string | null>(null);
  // When the list last refreshed; null until the first read lands.
  const [loadedAt, setLoadedAt] = useState<number | null>(null);
  const [looked, setLooked] = useState<Row | null>(null);
  const [view, setView] = useState<View>("sessions");
  // Where the next new conversation runs; local unless someone picks a project.
  const [newMode, setNewMode] = useState<ModeValue>({ mode: "local" });
  const [orbOpen, setOrbOpen] = useState<string>();
  const [projects, setProjects] = useState<Project[]>([]);
  // Only a narrow window reads this (see the 720px media query): a
  // phone shows the list or the thread, never both.
  const [pane, setPane] = useState<"list" | "thread">("list");
  const [reveal, setReveal] = useState<{ id: string; at: number } | null>(null);
  // The Context panel takes over the thread pane for the open session,
  // and closes when a different one is opened.
  // A session's sub-page: its context inspector or its changes review.
  const [sub, setSub] = useState<"context" | "changes" | null>(null);
  const context = sub === "context";
  const [wikiRoute, setWikiRoute] = useState<WikiRoute>({ at: "index" });
  // The review count on the nav item. Polled slowly: it changes when an
  // ingest lands, which is minutes apart at the fastest.
  const [wikiFlags, setWikiFlags] = useState(0);
  useEffect(() => {
    const load = () => wikiApi.index()
      .then((ix) => setWikiFlags(ix.health.unsupported + ix.health.superseded + ix.health.uncited))
      // A failed read is not "nothing to review": keep the last count.
      .catch(() => {});
    load();
    const t = setInterval(load, 60_000);
    return () => clearInterval(t);
  }, []);

  const refresh = useCallback(async () => {
    // Each read lands on its own: a projects outage must not freeze the
    // fleet. Only the fleet's freshness is reported, in one place.
    api.projects().then(setProjects, () => {});
    try {
      const rs = await api.sessions(archived);
      setRows(rs); setLoadErr(null); setRowsAll(archived); setLoadedAt(Date.now());
    } catch (e) { setLoadErr(e instanceof Error ? e.message : String(e)); }
  }, [archived]);

  useEffect(() => { refresh(); const t = setInterval(refresh, POLL_MS); return () => clearInterval(t); }, [refresh]);

  useEffect(() => {
    if (!selected) return;
    let live = true;
    // Never show one session's transcript under another's header while loading.
    setLines([]);
    setLoadedFor(null);
    lastSeq.current = 0; // the cursor belongs to the session just left
    setLoadFail(null); setPaused(undefined); setMissing(false);
    // Its failure is the transcript's own state, with a retry, not a toast.
    api.session(selected).then((r) => {
      if (!live) return;
      setLines(r.entries);
      setLoadedFor(selected);
      setLooked(r.session);
      setRows((prev) => prev.map((x) => (x.id === r.session.id ? r.session : x)));
    }).catch((e) => { if (live) { setLoadFail(e instanceof Error ? e.message : String(e)); setMissing((e as { status?: number }).status === 404); } });

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
    let synced = Date.now();
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
        synced = Date.now();
        setPaused(undefined);
        setRows((rs) => rs.map((x) => (x.id === r.session.id ? r.session : x)));
        setLooked(r.session);
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
        // The transcript stays; the header says it is no longer current.
        setPaused((p) => p ?? synced);
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
    retryRef.current = () => { clearTimeout(timer); backoff = 4000; catchUp(); };
    return () => { live = false; clearTimeout(timer); stop(); };
  }, [selected, loadTry]);

  const visible = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return rows;
    // The id is searchable too: an untitled session shows only its id tail.
    return rows.filter((r) => getSearchMatch(r, q));
  }, [rows, query]);

  // A linked session the list does not hold (archived, not yet listed) is
  // still the session: its own lookup fills in.
  const row = rows.find((r) => r.id === selected) ?? (looked?.id === selected ? looked : null);

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
      if (h === "hooks" || h === "projects") { setView(h); setSub(null); setPane("thread"); if (h === "projects") setOrbOpen(undefined); return; }
      const po = /^projects\/([^/]+)\/orb$/.exec(h);
      if (po) { setView("projects"); setOrbOpen(po[1]); setSub(null); setPane("thread"); return; }
      const wr = parseWikiHash(h);
      if (wr) { setView("wiki"); setWikiRoute(wr); setSub(null); setPane("thread"); return; }
      const m = /^s\/([^/]+)\/?(context|changes)?$/.exec(h);
      if (m) {
        setView("sessions"); setSelected(m[1]); setSub((m[2] as "context" | "changes" | undefined) ?? null); setPane("thread");
      } else if (h === "") {
        // No session named: on a phone that is the list. The thread pane
        // held only "Choose a session", with no list and no way back to it.
        setView("sessions"); setSelected(null); setSub(null); setPane("list");
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
      : view === "projects" ? (orbOpen ? `#/projects/${orbOpen}/orb` : "#/projects")
      : view === "wiki" ? `#/${wikiHash(wikiRoute)}`
      : selected ? `#/s/${selected}${sub ? `/${sub}` : ""}`
      : "#/";
    if (window.location.hash !== want) {
      window.history.replaceState(null, "", want);
    }
  }, [view, selected, sub, wikiRoute]);

  // Moving around the wiki pushes, like opening a conversation: Back
  // from a cited entry returns to the page, and from a page to the index.
  const goWiki = useCallback((r: WikiRoute) => {
    setWikiRoute(r); setView("wiki"); setSub(null); setPane("thread");
    const want = `#/${wikiHash(r)}`;
    if (window.location.hash !== want) window.history.pushState(null, "", want);
  }, []);

  usePaletteKey(useCallback(() => setPalette(true), []));
  const narrow = useMedia("(max-width:720px)");
  const onView = (v: View) => {
    if (v === "wiki") goWiki({ at: "index" });
    else if (v === "sessions") { if (narrow) goList(); else { setView(v); setSub(null); } }
    else { setView(v); setSub(null); setPane("thread"); if (window.location.hash !== NAV_HREF[v]) window.history.pushState(null, "", NAV_HREF[v]); }
  };

  // Back on a phone goes to the list and says so in the URL: it only
  // swapped panes, so a reload landed back in the thread it had left.
  const goList = useCallback(() => {
    setSelected(null); setSub(null); setView("sessions"); setPane("list");
    if (window.location.hash !== "#/") window.history.pushState(null, "", "#/");
  }, []);

  // Arriving at Home on a desktop opens the session that most needs you,
  // else the most recent, once: an empty page beside the list said nothing.
  // Opening does not mark anything seen.
  const arrived = useRef(false);
  useEffect(() => {
    if (arrived.current || loadedAt === null) return;
    arrived.current = true;
    if (window.location.hash.replace(/^#\/?/, "") !== "" || window.matchMedia?.("(max-width:720px)").matches) return;
    const top = rows.filter((r) => !r.archived && !r.empty)
      .sort((a, b) => sessionSignal(a) - sessionSignal(b) || Number(Boolean(a.background)) - Number(Boolean(b.background)) || Date.parse(b.lastAt) - Date.parse(a.lastAt))[0];
    if (top) { setSelected(top.id); setPane("thread"); }
  }, [loadedAt, rows]);

  // The session last opened, so the list comes back with your place in it.
  const [lastId, setLastId] = useState<string | null>(null);
  // A turn picked from a session's log, for its thread to scroll to.
  const [jump, setJump] = useState<{ id: string; turn: number; at: number; seq?: number } | null>(null);
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
      if (sub) setSub(null); else goList();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [palette, view, selected, sub, goList]);

  // F6 / Shift+F6 cycle the regions: sidebar, header, transcript, composer.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "F6" || e.defaultPrevented) return;
      const regions = [".sidebar", ".thread-head", ".transcript", ".composer-wrap"]
        .map((s) => document.querySelector<HTMLElement>(s)).filter((el): el is HTMLElement => Boolean(el?.offsetParent));
      if (!regions.length) return;
      e.preventDefault();
      const here = regions.findIndex((r) => r.contains(document.activeElement));
      const next = regions[(here < 0 ? 0 : here + (e.shiftKey ? regions.length - 1 : 1)) % regions.length];
      focusRegion(next);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const openSession = useCallback((id: string) => {
    setSelected(id); setSub(null); setView("sessions"); setPane("thread");
    // A push, so Back returns to where you were rather than leaving.
    if (window.location.hash !== `#/s/${id}`) {
      window.history.pushState(null, "", `#/s/${id}`);
    }
  }, []);

  /** A send or answer: its failure is shown beside the composer it came from, not as a toast too. */
  // Locked per session: a slow send in one never blocks typing into another.
  const [locked, setLocked] = useState<Record<string, boolean>>({});
  const deliverTo = async (id: string, fn: () => Promise<unknown>): Promise<string | null> => {
    setLocked((m) => ({ ...m, [id]: true }));
    try { await fn(); return null; }
    catch (e) { return e instanceof Error ? e.message : String(e); }
    finally { setLocked((m) => ({ ...m, [id]: false })); await refresh(); }
  };
  // Sends not yet recorded, per session and outside the thread, so
  // switching away and back still shows what is on its way.
  const [pending, setPending] = useState<Record<string, Pending[]>>({});

  // `label` finishes "Couldn’t …". fn closes over its own session id, so a
  // Retry after navigating away still addresses the session that failed.
  const act = async (fn: () => Promise<unknown>, label = "do that"): Promise<boolean> => {
    setBusy(true);
    try { await fn(); setErr(null); return true; }
    catch (e) { setErr({ label, msg: e instanceof Error ? e.message : String(e), retry: () => { void act(fn, label); } }); return false; }
    finally { setBusy(false); await refresh(); }
  };
  // Archiving names the session and waits for a yes; Cancel is where focus starts.
  // A queued agent has not started, so calling it running overstates what
  // stopping would interrupt.
  const agentWords = (running: number, queued: number) => {
    const noun = running + queued === 1 ? "agent" : "agents";
    if (!queued) return `${running} running ${noun}`;
    if (!running) return `${queued} queued ${noun}`;
    return `${running} running and ${queued} queued ${noun}`;
  };

  // Archiving a session leaves its background agents running unless you
  // say otherwise: they may be doing work you still want, so ask.
  const archiveRow = async (r: Row) => {
    if (r.archived) return act(() => api.unarchive(r.id), "unarchive");
    const n = (r.agents?.running ?? 0) + (r.agents?.queued ?? 0);
    if (n === 0) {
      const ok = await askConfirm("Archive this session?", `“${plainTitle(r.title) || "Untitled session"}” leaves the list. It stays under Archived and can be restored.`,
        { action: "Archive", safe: true });
      return ok ? act(() => api.archive(r.id), "archive") : false;
    }
    const pick = await askChoice(`Archive ${plainTitle(r.title) || untitled(r.id)}?`,
      `Stop its ${agentWords(r.agents?.running ?? 0, r.agents?.queued ?? 0)} too?`, ["Stop and archive", "Archive only"]);
    if (!pick) return false;
    return act(() => api.archive(r.id, { stopChildren: pick === "Stop and archive" }), "archive");
  };

  // What the chrome can do, the keyboard can do. Session-scoped
  // commands only appear when one is open, so the list never offers
  // something that would fail.
  const start = (cwd: string, prompt: string, mode?: ModeValue) => act(async () => {
    const created = await api.create(cwd, prompt, mode?.mode, mode?.project);
    openSession(created.id);
  }, "start a session");
  // New starts where the open session works, and the new session's header
  // shows that folder before anything is sent. With none open, the palette
  // asks where: never a silent Home.
  const newSession = () => { if (row?.cwd) void start(row.cwd, ""); else setPalette(true); };
  const folderName = (p: string) => p.replace(/\/+$/, "").split("/").pop() || p;

  // The newest call whose recorded exit was not zero, in the open session.
  let latestFail: number | undefined;
  for (let i = lines.length - 1; row && i >= 0; i--) {
    const x = lines[i].data?.exit;
    if (lines[i].kind === "result" && typeof x === "number" && x !== 0) { latestFail = lines[i].seq; break; }
  }
  const commands: Command[] = [
    ...(home && row?.cwd && row.cwd !== home ? [{
      id: "new:cwd", group: "Start", label: `New session in ${folderName(row.cwd)}`, suggest: true,
      hint: "Folder: " + shortPath(row.cwd, home),
      run: () => start(row.cwd, ""),
    }] : []),
    ...(home ? [{
      id: "new:here", group: "Start", label: "New session in home", suggest: !row?.cwd || row.cwd === home,
      hint: "Folder: " + shortPath(home, home),
      run: () => start(home, ""),
    }] : []),
    // A project session runs in that project's orb; home is only where serve records it.
    ...(home ? projects.filter((p) => p.slug).map((p) => ({
      id: `new:orb:${p.id}`, group: "Start", label: `New session in ${p.name}`,
      hint: "orb", run: () => start(home, "", { mode: "project" as const, project: p.id }),
    })) : []),
    { id: "new:project", group: "Start", label: "New project…",
      run: async () => {
        const n = await askText("New project", { placeholder: "What is this work?", action: "Create" });
        if (n) act(() => api.newProject(n));
      } },
    { id: "go:sessions", group: "Navigation", label: "Sessions", run: () => goList() },
    { id: "go:projects", group: "Navigation", label: "Projects",
      run: () => { setView("projects"); setPane("thread"); } },
    { id: "go:hooks", group: "Navigation", label: "Hooks",
      run: () => { setView("hooks"); setPane("thread"); } },
    { id: "go:wiki", group: "Navigation", label: "Wiki", run: () => goWiki({ at: "index" }) },
    { id: "go:side", group: "Navigation", label: "Toggle sidebar", hint: `${modKey()}B`,
      run: () => window.dispatchEvent(new Event("bough:toggle-side")) },
    { id: "wiki:review", group: "Wiki", label: "Review flagged claims",
      hint: wikiFlags ? `${wikiFlags} flagged` : undefined, run: () => goWiki({ at: "review" }) },
    { id: "wiki:activity", group: "Wiki", label: "Wiki activity", run: () => goWiki({ at: "activity" }) },
    { id: "wiki:ingest", group: "Wiki", label: "Ingest now",
      hint: "compiles finished sessions into the wiki",
      run: () => act(() => wikiApi.ingest(), "start an ingest").then((ok) => { if (ok) goWiki({ at: "activity" }); }) },
    // Draining a backlog one "Mark seen" at a time is a chore; this is the
    // once-a-week sweep, kept off the screen because it is rare.
    ...(rows.some((r) => r.trouble) ? [{
      id: "ack:all", group: "Start", label: `Mark every failure seen (${rows.filter((r) => r.trouble).length})`,
      run: () => act(() => Promise.all(rows.filter((r) => r.trouble).map((r) => api.ack(r.id))), "mark failures seen"),
    }] : []),
    { id: "go:archived", group: "Navigation",
      label: archived ? "Hide archived sessions" : "Show archived sessions",
      run: () => setArchived((v) => !v) },
    ...(row ? [
      { id: "s:changes", group: "This session", label: "Review changes", suggest: true,
        hint: "Session edits", run: () => { setSub("changes"); setPane("thread"); } },
      { id: "s:context", group: "This session", label: "Inspect context", suggest: true,
        hint: "Context", run: () => { setSub("context"); setPane("thread"); } },
      // Searchable only, and it asks first: never one Enter away from an empty box.
      { id: "s:archive", group: "This session",
        label: row.archived ? "Unarchive this session" : "Archive this session",
        run: () => { void archiveRow(row); } },
      ...(latestFail !== undefined ? [{
        id: "s:fail", group: "This session", label: "Jump to latest failed call", suggest: true,
        run: () => { setView("sessions"); setSub(null); setPane("thread"); setJump({ id: row.id, turn: 0, seq: latestFail, at: Date.now() }); },
      }] : []),
      ...(row.status === "running" ? [{
        id: "s:stop", group: "This session", label: "Stop this turn", suggest: true,
        run: () => act(() => api.interrupt(row.id), "stop the turn"),
      }] : []),
    ] : []),
  ];

  return (
    <div className="app" data-pane={pane}>
      {row && !sub && view === "sessions" && (
        <div className="skip-links">
          <button className="skip-link" onClick={() => { const t = document.querySelector<HTMLElement>(".transcript"); if (t) focusRegion(t); }}>Skip to transcript</button>
          <button className="skip-link" onClick={() => document.getElementById("composer")?.focus()}>Skip to composer</button>
        </div>
      )}
      <Palette open={palette} onClose={() => { setPalette(false); setPalQuery(""); }} rows={rows}
               commands={commands} onOpenSession={openSession} initialQuery={palQuery} current={selected}
               onOpenWikiPage={(path) => goWiki({ at: "page", path })}
               onStart={home ? (text) => start(home, text) : undefined} />
      <Sidebar rows={visible} selected={selected ?? lastId} active={pane === "list"}
               onSelect={openSession} query={query} onQuery={setQuery}
               onTurn={(id, turn) => { if (id !== selected || view !== "sessions" || sub) openSession(id); else setPane("thread"); setJump({ id, turn, at: Date.now() }); }}
               view={view} wikiFlags={wikiFlags} onNew={newSession} onFind={() => setPalette(true)}
               onView={onView}
               showArchived={archived} onToggleArchived={() => setArchived((v) => !v)}
               archivedState={!archived || rowsAll ? "ready" : loadErr ? "failed" : "loading"} onRetryArchived={refresh}
               onAck={(id) => act(() => api.ack(id), "mark it seen")}
               onShowList={() => setPane("list")} reveal={reveal}
               loadedAt={loadedAt} loadErr={loadErr} onRetry={refresh} />
      {view === "wiki" ? (
        <WikiPage route={wikiRoute} onRoute={goWiki} onBack={goList} onOpenSession={openSession}
                  onSearch={(text) => { setPalQuery(text.replace(/\s+/g, " ").slice(0, 60)); setPalette(true); }} />
      ) : view === "hooks" ? (
        <HooksPage onBack={goList} rows={rows} />
      ) : view === "projects" ? (
        <ProjectsView
          projects={projects} rows={rows}
          onOpen={openSession}
          onBack={goList}
          onAssign={(id, p) => act(() => api.assign(id, p), "move the session")}
          onCreate={async (name) => { const p = await api.newProject(name); await refresh(); return p; }}
          onRename={async (id, name) => { await api.renameProject(id, name); await refresh(); }}
          onAssignMany={async (ids, p) => {
            // Per session: what moved is done, what did not stays selected there.
            const out = await Promise.allSettled(ids.map((id) => api.assign(id, p)));
            await refresh();
            return ids.filter((_, i) => out[i].status === "rejected");
          }}
          onDelete={(id) => act(() => api.deleteProject(id), "delete the project")}
          orbOpen={orbOpen} onOrbOpen={setOrbOpen} onOrbChanged={refresh} />
      ) : row && sub === "changes" ? (
        <ChangesPage row={row} tick={lines.length} onBack={() => setSub(null)} />
      ) : row && context ? (
        <ContextPage session={row.id} model={row.model} used={loadedFor === row.id ? sessionUsage(lines)?.lastIn : undefined} onBack={() => setSub(null)} />
      ) : row ? (
        <Thread key={row.id} row={row} lines={lines} jump={jump?.id === row.id ? jump : null} loading={loadedFor !== row.id} loadError={loadFail ?? undefined} paused={paused}
          onRetry={() => (loadedFor === row.id ? retryRef.current() : setLoadTry((n) => n + 1))} stream={stream} activity={activity} projects={projects} busy={busy || Boolean(locked[row.id])} onBack={goList}
          sending={pending[row.id] ?? []}
          setSending={(f) => setPending((m) => ({ ...m, [row.id]: f(m[row.id] ?? []) }))}
          onSend={(t) => deliverTo(row.id, () => api.prompt(row.id, t))}
          onAnswer={(t, ask) => deliverTo(row.id, () => api.answer(row.id, t, ask))}
          onInterrupt={() => act(() => api.interrupt(row.id), "stop the turn")}
          onArchive={() => archiveRow(row)}
          rows={rows} onOpenSession={openSession}
          onRename={async (t) => { await api.rename(row.id, t); await refresh(); }}
          onModel={(m) => act(() => api.model(row.id, m), "change model")}
          onEffort={(e) => act(() => api.effort(row.id, e), "change effort")}
          onAssign={(p) => act(() => api.assign(row.id, p), "move the session")}
          onContext={() => setSub("context")}
          onAck={() => act(() => api.ack(row.id), "mark it seen")}
          onStopOrb={() => act(() => api.stopOrb(row.id), "stop the orb")}
          onNewProject={() => { setPalQuery("New project"); setPalette(true); }}
          onStartProject={home ? (project, draft) => act(async () => {
            // The draft moves, unsent: the new session's composer holds it.
            const created = await api.create(home, "", "project", project);
            try {
              if (draft.trim()) sessionStorage.setItem("bough:draft:" + created.id, draft);
              sessionStorage.removeItem("bough:draft:" + row.id);
              sessionStorage.removeItem("bough:draft-atts:" + row.id);
            } catch { /* storage off */ }
            openSession(created.id);
          }, "start a project session") : undefined} />
      ) : (
        <div className={"thread" + (selected ? " empty" : "")}>
          {!selected ? (<>
            {home && (
              <div className="controls mode-start">
                <ModePicker projects={projects} value={newMode} onChange={setNewMode} />
                <button className="btn" onClick={() => { void start(home, "", newMode); }}>New session</button>
              </div>
            )}
            <ControlOverview rows={rows} onOpenFailure={(id, seq) => { openSession(id); if (seq) setJump({ id, turn: 0, seq, at: Date.now() }); }} onReveal={(id) => { setPane("list"); setQuery(""); setReveal({ id, at: Date.now() }); }} loadedAt={loadedAt} loadErr={loadErr} onRetry={refresh} />
          </>) : (
            // A link to a session the list does not hold: looked up on its
            // own, so an empty or slow list never leaves a blank pane.
            <div className="lookup" role={missing ? undefined : "status"}>
              {missing ? (<>
                <h1 className="lookup-title">Session not found</h1>
                <p className="lookup-body">This session isn’t on this server. It may be archived or deleted.</p>
                <div className="lookup-actions">
                  <button className="btn" onClick={goList}>Show all sessions</button>
                  <button className="btn" onClick={() => { setArchived(true); setPane("list"); setPalQuery(""); setPalette(true); }}>Search archived</button>
                </div>
              </>) : loadFail ? (<>
                <h1 className="lookup-title">Couldn’t load this session</h1>
                <p className="lookup-body">{loadFail}</p>
                <div className="lookup-actions">
                  <button className="btn" onClick={() => setLoadTry((n) => n + 1)}>Retry</button>
                  <button className="btn" onClick={goList}>Show all sessions</button>
                </div>
              </>) : <p className="lookup-body">Loading session…</p>}
            </div>
          )}
        </div>
      )}
      <DialogHost />
      {narrow && (pane === "list" || view !== "sessions") && (
        <ViewNav phone view={pane === "list" ? "sessions" : view} onView={onView} wikiFlags={wikiFlags} />
      )}
      {err ? (
        // Stays until dismissed or a later action succeeds; the buttons are the only controls.
        <div className="toast" role="alert">
          <p className="toast-msg">Couldn’t {err.label} — {err.msg}</p>
          <div className="toast-actions">
            <button className="btn" disabled={busy} onClick={err.retry}>Retry</button>
            <button className="btn" onClick={() => setErr(null)}>Dismiss</button>
          </div>
        </div>
      ) : null}
    </div>
  );
}
