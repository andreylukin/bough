import { Fragment, createContext, memo, useCallback, useContext, useEffect, useId, useLayoutEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { api, subscribe, watchBuild, type Change, type Scope, type TurnLine } from "./api";
import type { Event as LiveEvent, Line, Project, Row } from "./types";
import { MARKED, STATUS, StatusMark, UnseenDot, Working, hasFailure, hasQuestion, isUnseen, orbWord, rowNote, sessionSignal, statusWord } from "./status";
import { ProjectsView } from "./projects";
import { DRAG_SESSION, ProjectView, StartThreadCtx, byThread, projectOf } from "./project";
import { ModeChip, ModePicker, OrbUp, orbsUp, orbsUpLabel, type ModeValue } from "./mode";
import { OrbFailureBody, confirmFailedBuild, confirmStopOrb, orbUp, type OrbFailureLog } from "./orb";
import { Select, type Option } from "./select";
import { DialogHost, askChoice, askConfirm, askText, showShortcuts } from "./dialog";
import { focusComposerKey, isMac, newSessionKey, sheetKey, switchKey, treeKey } from "./keys";
import { Welcome, welcomeDismissed } from "./welcome";
import { clampToViewport } from "./popover";
import { Markdown, programRan, codeLabel, callVerb, callFailed, callRunning, callsHeadline, callStep, isCall, isNativeCall, presentTense, groupSubs, groupTools, groupTurns, isHookLine, isQuiet, untitled, blank, sessionUsage, usageOf, tokenCount, money, duration, plainTitle, stepCount, stripRunFences, splitBareProgram, foldRetries, foldModelSwitch, splitWork, thrownError, cleanError, isAgentNotice, storedNotices, workHeadline, type Segment, sessionTitle, titleKey, hasOwnTitle, type Item, type SubAgent, type Turn, lineCount } from "./render";
import { Code, parseCall, langForPath, toolCallLabel } from "./code";
import { lastTestRun } from "./runs";
import { agentWakeNotes, agentsFromRows, jobWakeNotes, jobsFromLines, subagentsFromTurn, useReviewed, workCounts, workIndex, type Worker } from "./work";
import { ExecNote, JobLines, JobRow, LIFE_WORD, WorkButton, WorkContext, WorkDialog, WorkGlyph, WorkState, agentReports, jobIdOf, spokenDuration, splitExecNote, stateText, useChildren, useStopStore, useWork, useWorkAnnouncer, type WorkCtx } from "./work-ui";
import { SkillPicker } from "./skills";
import { AttChip, PromptWords, foldPastes, lostTags, parsePrompt, promptNodes, wrapPaste } from "./prompt";
import { Mentions, triggerAt, type Trigger } from "./mention";
import { FireInspection, HooksPage, type Fire, type Load, type Save } from "./hooks";
import { MeView } from "./me";
import { ContextPage } from "./context";
import { ChangesBody, ChangesPage, EditDiff, FileEdit, callEdits, countOf, nativeEdits, outputParts, useChanges } from "./changes";
import { Palette, idTail, isTypingTarget, startFolders, useFullText, usePaletteKey, visit, type Command } from "./palette";
import { WikiPage, parseWikiHash, wikiApi, wikiHash, type WikiRoute } from "./wiki";
import { Elapsed, EmptyState, ErrorNote, ErrorToast, InlineFail, Pending, RawDetails, Spinner, StartingStatus, StateIcon, ago, elapsed, humanError, providerError } from "./loading";
import { PortalPane } from "./portal";

export type View = "sessions" | "me" | "projects" | "project" | "hooks" | "wiki";

// The welcome's start goes through act, which puts the reason in the
// toast and resolves false instead of throwing: passed straight through, a
// refused start read as a started one there, wrote bough:welcome-done and
// left "Starting…" up for good. Its own callout points at the toast.
const START_REFUSED = "The notice at the bottom says why.";
const POLL_MS = 4000; // sessions we are not streaming still change status

const clock = (iso: string) => new Date(iso).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
/** Local time of day for today; an older moment carries its date, so it never reads as later today. */
export const when = (iso: string, now = new Date()) => {
  const d = new Date(iso);
  if (d.toDateString() === now.toDateString()) return clock(iso);
  const day = d.toLocaleDateString([], { month: "short", day: "numeric", ...(d.getFullYear() !== now.getFullYear() ? { year: "numeric" } : {}) });
  return `${day}, ${clock(iso)}`;
};


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

// `ago` lives in loading.tsx beside `duration`, because orb.tsx needs it and
// this module already imports orb.tsx: importing back would close a cycle. The
// re-export keeps every `from "./app"` caller working.
export { ago };

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
/**
 * What Home opens by itself on arrival: the session that most needs you,
 * else the most recent — but only a person's own, and only from the last
 * day. A week-old interrupted background thread used to win on signal
 * alone and greet you every morning; the overview is the better greeting.
 */
export function arrivalPick(rows: Row[], now: number): Row | undefined {
  return rows.filter((r) => !r.archived && !r.empty && !r.background && now - Date.parse(r.lastAt) < ARRIVAL_MS)
    .sort((a, b) => sessionSignal(a) - sessionSignal(b) || Date.parse(b.lastAt) - Date.parse(a.lastAt))[0];
}
const ARRIVAL_MS = 24 * 3_600_000;

/** However old, a group shows at least this many rows before "N older". */
const FRESH_MIN = 3;
/** Quiet for 72h and nothing waiting on you: folded under its group. */
const isOld = (r: Row, now: number) => sessionSignal(r) >= 2 && now - Date.parse(r.lastAt) >= INACTIVE_MS;
/**
 * Folded under its group's foot: old, or a project thread nobody typed
 * into (the page's Empty group, which starts closed there too). Only a
 * project keeps its empty rows at all; elsewhere they are not listed.
 */
const isTucked = (r: Row, now: number, selected: string | null) =>
  isOld(r, now) || (sessionSignal(r) >= 2 && Boolean(r.empty) && !r.live && r.id !== selected);

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
export function displayTitle(r: Row): string {
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
/** A group key: the project a session belongs to, else its checkout. */
const groupKey = (r: Row) => (r.project ? `project:${r.project}` : r.repo || r.cwd);

/**
 * What dropping a session on a sidebar group does: the slug to move it
 * into, "" to take it out of its project, or undefined when the group is
 * no place for it. A project session lives in its project's orb and cannot
 * move (serve refuses). A folder group takes a project's session only when
 * it is the folder the session would list under once out: dropped on any
 * other folder it would not have landed where it was put.
 */
export function dropProject(r: Row, group: Row[], key: string): string | undefined {
  if (r.mode === "project") return undefined;
  const to = group[0]?.project;
  if (to) return r.project === to ? undefined : to;
  return r.project && (r.repo || r.cwd) === key ? "" : undefined;
}

function byWorkspace(rows: Row[], projectNames: Map<string, string> = new Map()): [string, Row[], string][] {
  // A session in a project groups under the project, whichever repo it ran
  // in; the rest group by the whole path, so two checkouts named alike stay
  // apart; only then does a name take its parent to tell them apart.
  const out = new Map<string, Row[]>();
  for (const r of rows) {
    const k = groupKey(r);
    if (!out.has(k)) out.set(k, []);
    out.get(k)!.push(r);
  }
  const names = new Map<string, number>();
  for (const list of out.values()) if (!list[0].project) names.set(workspaceOf(list[0]), (names.get(workspaceOf(list[0])) ?? 0) + 1);
  const label = (r: Row) => {
    if (r.project) return projectNames.get(r.project) ?? workspaceOf(r);
    const name = workspaceOf(r);
    if ((names.get(name) ?? 0) < 2) return name;
    return (r.repo || r.cwd).split("/").filter(Boolean).slice(-2).join("/");
  };
  const latest = (list: Row[]) => Math.max(...list.map((r) => Date.parse(r.lastAt)));
  const signal = (list: Row[]) => Math.min(...list.map(sessionSignal));
  return [...out.entries()]
    // A project's group is ordered as its page orders its threads.
    .map(([k, list]) => [label(list[0]), list.sort(list[0].project ? byThread : byUrgency), k] as [string, Row[], string])
    .sort((a, b) => signal(a[1]) - signal(b[1]) || latest(b[1]) - latest(a[1]));
}

/** What the sidebar's arrows walk; one of them at a time is the tab stop. */
const TREE_ITEMS = "button.sec-fold, button.ws-head, button.row, button.ws-older, button.turn-line";

/** The query's first match in text, marked. */
function marked(text: string, q: string): React.ReactNode {
  const i = q ? text.toLowerCase().indexOf(q) : -1;
  if (i < 0) return text;
  return <>{text.slice(0, i)}<mark className="hit">{text.slice(i, i + q.length)}</mark>{text.slice(i + q.length)}</>;
}

/** Highlights the first match of q inside el (CSS Custom Highlight API); returns the undo. */
function markMatch(el: HTMLElement, q: string): (() => void) | undefined {
  const reg = (globalThis as { CSS?: { highlights?: Map<string, unknown> } }).CSS?.highlights;
  const H = (globalThis as { Highlight?: new (...r: Range[]) => unknown }).Highlight;
  if (!reg || !H) return undefined;
  const needle = q.toLowerCase();
  const walker = document.createTreeWalker(el, NodeFilter.SHOW_TEXT);
  for (let n = walker.nextNode(); n; n = walker.nextNode()) {
    const i = (n.textContent ?? "").toLowerCase().indexOf(needle);
    if (i < 0) continue;
    const range = document.createRange();
    range.setStart(n, i);
    range.setEnd(n, i + needle.length);
    if (!document.getElementById("jump-hit-style")) {
      const style = document.createElement("style");
      style.id = "jump-hit-style";
      style.textContent = "::highlight(jump-hit){background:#354b3b;color:#d7e9dc}";
      document.head.appendChild(style);
    }
    reg.set("jump-hit", new H(range));
    return () => { reg.delete("jump-hit"); };
  }
  return undefined;
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
  me: <><circle cx="12" cy="8" r="3.5" /><path d="M5 20a7 7 0 0 1 14 0" /></>,
  wiki: <><path d="M4.5 5.5A1.5 1.5 0 0 1 6 4h13.5v14H6a1.5 1.5 0 0 0-1.5 1.5z" /><path d="M4.5 19.5A1.5 1.5 0 0 0 6 21h13.5v-3" /><path d="M9 8.5h6" /></>,
  chevron: <path d="M9 6l6 6-6 6" />,
  filter: <path d="M4 6.5h16M7 12h10M10 17.5h4" />,
};

/** A route change puts focus on .app, so the next Tab reaches the skip links; a field or dialog in use keeps it. */
export function focusAppOnRoute(doc: Document) {
  const a = doc.activeElement as HTMLElement | null;
  if (a && (/^(INPUT|TEXTAREA|SELECT)$/.test(a.tagName ?? "") || a.closest?.("[role=dialog], dialog"))) return;
  doc.querySelector<HTMLElement>(".app")?.focus({ preventScroll: true });
}

/** Back and forward keep their slot with no history, so the icons after them never shift. */
export function HistoryArrows({ back, forward }: { back: boolean; forward: boolean }) {
  const hide = !back && !forward;
  return <>
    <button className={"side-icon" + (hide ? " side-icon-reserved" : "")} onClick={() => window.history.back()} disabled={!back} aria-hidden={hide || undefined} tabIndex={hide ? -1 : undefined} aria-label="Back" title="Back"><Icon d={ICONS.back} /></button>
    <button className={"side-icon" + (hide ? " side-icon-reserved" : "")} onClick={() => window.history.forward()} disabled={!forward} aria-hidden={hide || undefined} tabIndex={hide ? -1 : undefined} aria-label="Forward" title="Forward"><Icon d={ICONS.forward} /></button>
  </>;
}

/** Whether history can go back or forward from here, where the browser says (the Navigation API); else both stay on. */
function useHistoryNav(): { back: boolean; forward: boolean } {
  type Nav = EventTarget & { canGoBack: boolean; canGoForward: boolean };
  const nav = (window as unknown as { navigation?: Nav }).navigation;
  const read = () => ({ back: nav ? nav.canGoBack : true, forward: nav ? nav.canGoForward : true });
  const [state, setState] = useState(read);
  useEffect(() => {
    if (!nav) return;
    const on = () => setState(read());
    nav.addEventListener("currententrychange", on);
    window.addEventListener("popstate", on);
    return () => { nav.removeEventListener("currententrychange", on); window.removeEventListener("popstate", on); };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  return state;
}

/** The sidebar row to mark: none while a page other than a session is on screen, or the address is lost. */
export function sidebarSelected(view: View, lost: string | null, selected: string | null, lastId: string | null): string | null {
  return view !== "sessions" || lost !== null ? null : selected ?? lastId;
}

export function Sidebar({ rows, projects = [], selected, onSelect, onTurn, query, onQuery, said, saidElsewhere, showArchived, onToggleArchived, archivedState = "ready", onRetryArchived, view = "sessions", onView, wikiFlags = 0, onNew, onAck, active = true, onShowList, reveal, loadedAt = 0, loadErr = null, onRetry, onOpenProject, onMove }: {
  rows: Row[]; selected: string | null; onSelect: (id: string) => void;
  /** Project labels, so a project group is headed by its name. */
  projects?: Project[];
  /** The fleet's freshness: when the list last loaded (null before), and why the last refresh failed. */
  loadedAt?: number | null; loadErr?: string | null; onRetry?: () => void;
  /** Whether the rows hold archived sessions yet, once the section is open. */
  archivedState?: "loading" | "failed" | "ready"; onRetryArchived?: () => void;
  /** Open a session at one turn of its log. */
  onTurn?: (id: string, turn: number) => void;
  /** showArchived is the Archived section being open: the rows then include archived ones. */
  query: string; onQuery: (q: string) => void; showArchived: boolean; onToggleArchived: () => void;
  /** The first transcript line the query matched, per session, from the full-text search. */
  said?: Map<string, { seq: number; text: string }>;
  /** Transcript matches in sessions the list does not hold (archived); opens them in ⌘K. */
  saidElsewhere?: { count: number; open: () => void };
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
  /** Open a project's page: what a project group's name does. */
  onOpenProject?: (slug: string) => void;
  /** Move a session into a project ("" takes it out): what dropping a row on a group does. */
  onMove?: (id: string, project: string) => void;
}) {
  // Status lives in the glyphs and the order; the sections are only
  // where a session ran, and whether it is still recent.
  // Runs nobody started by hand always fold into Background — the person
  // chose that; one that needs attention lights the section header instead.
  const projectNames = useMemo(() => new Map(projects.map((p) => [p.slug, p.name])), [projects]);
  const { recent, needIds, lifted, recentAll, background, archived, kids } = useMemo(() => {
    const recent: Row[] = [], background: Row[] = [], archived: Row[] = [];
    // A background agent is not a row of its own: its parent's row counts
    // the running ones, and the parent's Work panel lists them. Only an
    // agent whose parent is gone from the list keeps a row, so none is lost.
    const byId = new Map(rows.map((r) => [r.id, r]));
    const kids = new Map<string, Row[]>();
    for (const r of rows) {
      // A project's thread is listed in its project's group, whoever
      // started it and however empty: the project page lists the same set.
      const member = projectOf(r);
      const parent = !member && r.spawnedBy ? byId.get(r.spawnedBy) : undefined;
      if (parent) {
        kids.set(parent.id, [...(kids.get(parent.id) ?? []), r]);
        continue;
      }
      // A session opened and never sent a message holds nothing to go back
      // to. It shows while it is open, while its child is up (one just made
      // with New), or when a search asks for it.
      // A project session whose orb failed to start is empty and dead too,
      // but hiding it would swallow the failure and the prompt it lost.
      // An archived one always lists: it was archived on purpose, and hiding it
      // left it reachable only by URL.
      if (!member && r.empty && !r.live && !r.archived && r.orb?.status !== "failed" && r.id !== selected && !query) continue;
      if (r.archived) archived.push(r);
      else if (r.background && !member) background.push(r);
      // Old sessions stay in their project's group, folded under "older".
      else recent.push(r);
    }
    // Each session lists once: what needs you pins on top and leaves its
    // group; the group's head counts what went up. A project's thread is
    // pinned and stays too: its group is the project page's list, and an
    // errored thread missing from it was the two disagreeing.
    const needIds = new Set(query ? [] : [...recent, ...background].filter((r) => sessionSignal(r) === 0).map((r) => r.id));
    const lifted = new Map<string, number>();
    const groups = byWorkspace(recent, projectNames).flatMap(([ws, list, gk]): [string, Row[], string][] => {
      const rest = list.filter((r) => !needIds.has(r.id) || projectOf(r));
      if (rest.length < list.length) lifted.set(gk, list.length - rest.length);
      return rest.length ? [[ws, rest, gk]] : [];
    });
    return { recent: groups, needIds, lifted, recentAll: recent, background, archived, kids };
  }, [rows, selected, query, projectNames]);

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
  // Moving from row to row swaps the card at once, never showing the last row's content at the new spot.
  const cardAt = useRef(0);
  const peek = (r: Row, el: HTMLElement) => {
    clearTimeout(peekTimer.current);
    if (quiet.current) { setCard(null); return; }
    const rect = el.getBoundingClientRect();
    // Beside the row, so it never covers the turn log under it.
    const next = {
      id: r.id,
      top: Math.max(8, Math.min(rect.top, window.innerHeight - 200)),
      left: Math.min(rect.right + 8, window.innerWidth - 372),
    };
    if (Date.now() - cardAt.current < 250) { cardAt.current = Date.now(); setCard(next); return; }
    peekTimer.current = setTimeout(() => { cardAt.current = Date.now(); setCard(next); }, 300);
  };
  const unpeek = () => {
    clearTimeout(peekTimer.current);
    setCard((c) => { if (c) cardAt.current = Date.now(); return null; });
  };
  // Any click or scroll elsewhere closes it too: a card never lingers over the thread.
  useEffect(() => {
    if (!card) return;
    const close = () => { clearTimeout(peekTimer.current); setCard(null); };
    document.addEventListener("pointerdown", close, true);
    window.addEventListener("scroll", close, true);
    window.addEventListener("blur", close);
    return () => { document.removeEventListener("pointerdown", close, true); window.removeEventListener("scroll", close, true); window.removeEventListener("blur", close); };
  }, [card]);
  // A card for the session you just left is stale, and so is one over a view you left.
  useEffect(() => { clearTimeout(peekTimer.current); setCard(null); }, [selected, view]);
  useEffect(() => {
    const close = () => { clearTimeout(peekTimer.current); setCard(null); };
    window.addEventListener("hashchange", close);
    return () => window.removeEventListener("hashchange", close);
  }, []);

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
  // Archived included from a filter ("Include") lasts as long as that filter.
  const archFromFilter = useRef(false);
  useEffect(() => {
    if (searchOn || !archFromFilter.current) return;
    archFromFilter.current = false;
    if (showArchived) onToggleArchived();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [searchOn]);

  // A first load says so only once it is slow enough to notice.
  const [slow, setSlow] = useState(false);
  useEffect(() => {
    if (loadedAt !== null) return;
    const t = setTimeout(() => setSlow(true), 200);
    return () => clearTimeout(t);
  }, [loadedAt]);
  const searchBtn = useRef<HTMLButtonElement>(null);
  const hist = useHistoryNav();
  // A hairline under the toolbar once the list has scrolled under it.
  const [scrolled, setScrolled] = useState(false);

  // The row being dragged onto a group, and the group it is over.
  const [dragging, setDragging] = useState<Row | null>(null);
  const [dropAt, setDropAt] = useState<string | null>(null);

  // A long log shows its last turns; the rest wait behind one line.
  const [allTurns, setAllTurns] = useState<Set<string>>(() => new Set());

  // One tab stop in the tree: the item last focused, else the open row,
  // else the first. The arrows, Home and End move within it.
  const treeRef = useRef<HTMLDivElement>(null);
  const stop = useRef<HTMLElement | null>(null);
  const focusedStop = useRef(false);
  useLayoutEffect(() => {
    const items = [...(treeRef.current?.querySelectorAll<HTMLElement>(TREE_ITEMS) ?? [])];
    // Only a stop someone focused is kept: one picked while the list was
    // still loading (Archived alone) must not outlive the rows arriving.
    const pick = (stop.current && focusedStop.current && items.includes(stop.current) ? stop.current : null)
      ?? items.find((el) => el.classList.contains("row-on")) ?? items[0];
    for (const el of items) el.tabIndex = el === pick ? 0 : -1;
    stop.current = pick ?? null;
  });

  // "/" opens search from anywhere that is not taking typing.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "/" || e.metaKey || e.ctrlKey || e.altKey) return;
      if (isTypingTarget(e.target)) return;
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
    // An old session sits folded under its group's "older" line.
    const sec = r.background && !projectOf(r) ? "background" : isTucked(r, Date.now(), null) ? `older:recent:${groupKey(r)}` : "";
    if (sec) setUnfolded((cur) => { const next = new Set(cur).add(sec); writeSet("bough:unfolded", next); return next; });
    requestAnimationFrame(() => {
      const el = document.querySelector<HTMLElement>(`.sidebar button.row[data-id="${r.id}"]`);
      el?.scrollIntoView({ block: "nearest" });
      el?.focus();
    });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [reveal]);
  useEffect(() => () => clearTimeout(peekTimer.current), []);
  // A deep link or palette jump lands on a row that may be scrolled away.
  useEffect(() => {
    if (!selected) return;
    requestAnimationFrame(() => document.querySelector<HTMLElement>(`.sidebar button.row[data-id="${selected}"]`)?.scrollIntoView({ block: "nearest" }));
  }, [selected]);

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
    // Escape in the tree drops a filter in force; focus stays where it is.
    if (e.key === "Escape" && query) { e.preventDefault(); onQuery(""); setSearching(false); return; }
    if (e.altKey || e.metaKey || e.ctrlKey || isTypingTarget(e.target)) return;
    const key = treeKey(e.key);
    if (!["ArrowDown", "ArrowUp", "ArrowLeft", "ArrowRight", "Home", "End"].includes(key)) return;
    const items = [...e.currentTarget.querySelectorAll<HTMLElement>(TREE_ITEMS)];
    if (!items.length) return;
    const at = document.activeElement as HTMLElement | null;
    const cur = at?.closest(".row-wrap")?.querySelector<HTMLElement>("button.row") ?? at;
    const go = (el?: HTMLElement | null) => { if (el) { el.focus(); el.scrollIntoView({ block: "nearest" }); } };
    e.preventDefault();
    if (key === "Home" || key === "End") { go(items[key === "Home" ? 0 : items.length - 1]); return; }
    if (key === "ArrowDown" || key === "ArrowUp") {
      const i = cur ? items.indexOf(cur) : -1;
      go(items[i < 0 ? (key === "ArrowDown" ? 0 : items.length - 1)
        : Math.max(0, Math.min(items.length - 1, i + (key === "ArrowDown" ? 1 : -1)))]);
      return;
    }
    if (!cur) return;
    const right = key === "ArrowRight";
    if (cur.classList.contains("sec-fold") || cur.classList.contains("ws-head")) {
      const open = cur.getAttribute("aria-expanded") === "true";
      // Right on an open group steps into its first child; left on a shut one steps out.
      // A section's → shares its click's one state: it opens, and never moves on.
      if (right && open && cur.classList.contains("sec-fold")) return;
      if (right && open) go(items[items.indexOf(cur) + 1]);
      else if (right || open) (cur.querySelector<HTMLElement>(".ws-fold") ?? cur).click();
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
  // `pin`: the copy in Needs you, a link to the same session with no log of its own.
  const session = (r: Row, twin = false, child = false, pin = false): React.ReactNode => {
    // A search hides the logs: they are not what matched.
    const open = Boolean(r.turns) && expanded.has(r.id) && !q && !pin;
    const log = logs[r.id];
    const name = plainTitle(r.title);
    const on = r.id === selected;
    // A failure and a pending ask are both news: say both.
    const note = rowNote(r);
    const { failed, asking } = note;
    const why = failed
      ? `${capital(failed)}${asking ? "; waiting for your answer" : failed === "tests failed" ? `; agent ${(STATUS[r.status]?.label ?? r.status).toLowerCase()}` : ""}`
      : (STATUS[r.status]?.label ?? r.status) + (isUnseen(r) ? ", not seen yet" : "");
    // Where the query hit, kept on screen: a late title hit shifts the
    // title, a hit elsewhere gets its own line centred on it.
    const hit = getSearchMatch(r, q);
    // A late hit starts its excerpt a few characters before it, so a narrow row still shows it.
    const own = displayTitle(r) || (child && r.status === "queued" ? "Queued agent" : "");
    // No title: the shared fallback name, with the id tail as a secondary chip.
    const title = own || sessionTitle(r);
    const chip = twin || !hasOwnTitle(r) && !own;
    const shown = hit?.field === "title" && hit.at > 24 ? excerpt(title, hit.at, 6).text : title;
    const reason = hit && hit.field !== "title" ? excerpt(hit.text, hit.at, 6) : undefined;
    // Only the transcript held it: the line that did, so the row says why it is here.
    const saidLine = !hit && q ? said?.get(r.id)?.text.replace(/\s+/g, " ").trim() : undefined;
    const saidShown = saidLine ? excerpt(saidLine, Math.max(0, saidLine.toLowerCase().indexOf(q)), 6) : undefined;
    // Done is what the check mark already says; the row keeps only the age then.
    // A background agent says its own lifecycle once, in its words; a parent says what its agents are doing.
    const life = child ? agentsFromRows({ ...r, id: r.spawnedBy ?? "" }, [r])[0]?.life ?? "unknown" : undefined;
    const label = child ? "" : note.label;
    const lines = log?.lines && !allTurns.has(r.id) && log.lines.length > 3 ? log.lines.slice(-3) : log?.lines;
    const children = kids.get(r.id);
    const bg = children ? workCounts(agentsFromRows(r, children)) : null;
    // Only what is live or needs a look: the count of running agents, and failures.
    const bgText = bg ? [bg.running && `${bg.running} running`, bg.failed && `${bg.failed} failed`].filter(Boolean).join(" · ") : "";
    // The project's environment failed to set up: its own indicator, always shown, never the session's status.
    const setup = r.mode === "project" && r.orb?.status === "failed" ? projectNames.get(r.orb.project) ?? r.orb.project : "";
    // A second line only when it says more than the glyph: a failure's
    // reason, a question, a background life or a setup failure. Plain
    // running, waiting and done rows stay one line.
    const plain = note.plain;
    // A child's glyph says running, finished and stopped; only a failure or a wait takes a second line.
    const stacked = Boolean((label && !plain) || life === "failed" || life === "queued" || setup);
    const movable = Boolean(onMove) && r.mode !== "project";
    return (
      <Fragment key={r.id}>
      <div className={"session" + (child ? " session-child" : "")}>
        <div className={"row-wrap" + (open ? " row-open" : "")}>
          {/* data-live: a process holds the session. Nothing draws it, but a
              done row with a child and one without read the same, and the
              restart model test must tell them apart (a view or a
              reconnect must never give a session a child). */}
          <button role="treeitem" onClick={() => onSelect(r.id)} data-id={r.id} data-live={r.live ? "" : undefined}
                  draggable={movable || undefined}
                  onDragStart={movable ? (e) => { e.dataTransfer.setData(DRAG_SESSION, r.id); e.dataTransfer.effectAllowed = "move"; unpeek(); setDragging(r); } : undefined}
                  onDragEnd={movable ? () => { setDragging(null); setDropAt(null); } : undefined}
                  onMouseEnter={(e) => peek(r, e.currentTarget)} onMouseLeave={unpeek}
                  onBlur={unpeek}
                  aria-describedby={card?.id === r.id ? "row-card" : undefined}
                  aria-label={`${title}, ${life ? LIFE_WORD[life] : why}, ${ago(r.lastAt)} ago${r.branch ? `, branch ${r.branch}` : ""}${bgText ? `, background: ${bgText}` : ""}${setup ? `, ${setup}: setup failed` : ""}`}
                  className={"row" + (stacked ? " row-2" : "") + (on ? " row-on" : "") + (r.turns && !q && !pin ? " row-has-log" : "") + (r.trouble && onAck ? " row-has-ack" : "")}
                  aria-current={on ? "true" : undefined}
                  title={`${title}\n${why} · ${ago(r.lastAt)} ago${r.branch ? ` · ${r.branch}` : ""}${setup ? `\n${setup}: setup failed` : ""}`}>
            {/* A failure you have not seen is a red mark; the reason is its label. */}
            <span className="row-mark">
              {life
                ? (life === "finished" || life === "stopped" || life === "unknown" ? null : <WorkGlyph life={life} />)
                : failed
                ? <span className="status"><svg width={16} height={16} viewBox="0 0 24 24" fill="none" stroke="var(--red)" strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">{STATUS.error.glyph}</svg><span className="visually-hidden">{why}</span></span>
                : MARKED.has(r.status) ? <StatusMark status={r.status} size={16} bare />
                // The open row is being looked at; its ack is already on the way.
                : isUnseen(r) && !on ? <UnseenDot /> : null}
            </span>
            {/* Status metadata goes under the title, so a chip never cuts the name. */}
            <span className={stacked ? "row-stack" : "row-line"}>
            <span className="row-name">
            <span className={"row-title" + (own ? "" : " row-untitled")}>{marked(shown, q)}</span>{chip && <span className="mono row-id" title={`Session id ending ${idTail(r.id)}`}><span className="visually-hidden">session id </span>{idTail(r.id)}</span>}
            {/* The age, on every row at the same right edge; the title is what gives way. */}
            <span className="row-when" aria-hidden="true">{!stacked && <ModeChip row={r} bare />}{bg && (bg.running > 0 || bg.failed > 0) && (
              <span className="row-bg" data-running={bg.running ? "" : undefined}>
                {bg.failed > 0 && <span className="work-dot" />}
                {bg.running > 0 && <><WorkGlyph life="running" size={10} />{bg.running}</>}
              </span>
            )}{!stacked && r.mode === "project" && r.orb && r.orb.status !== "failed" && r.orb.status !== "stopped" && r.orb.status !== "" && <span className="row-when-sep" aria-hidden="true">·</span>}<span className="row-age">{ago(failed === "tests failed" && r.testsAt ? r.testsAt : r.lastAt)}</span></span>
            </span>
            {stacked && (label || setup) && !life && <span className={"num row-meta" + (failed ? " row-meta-bad" : asking ? " row-meta-ask" : r.status === "running" ? " row-meta-run" : "") + (failed || asking || setup || r.status === "running" ? " row-meta-live" : "")} aria-hidden="true">
              <ModeChip row={r} bare name={setup} />{label}
            </span>}
            {life && <span className={"num row-meta row-meta-live" + (life === "failed" ? " row-meta-bad" : life === "running" ? " row-meta-run" : "")} aria-hidden="true">
              <ModeChip row={r} bare name={setup} />{life === "queued" ? `${LIFE_WORD[life]} · waiting` : <span className="row-life-word">{LIFE_WORD[life]}</span>}
            </span>}
            </span>
            {r.jobs && r.jobs.length > 0 && (
              <span className="num row-jobs" title={r.jobs.map((j) => j.cmd).join("\n")}>
                <svg width={10} height={10} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.6" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true"><path d="M4 17l6-5-6-5M12 19h8" /></svg>{r.jobs.length}<span className="visually-hidden"> {r.jobs.length === 1 ? "job" : "jobs"}</span>
              </span>
            )}
          </button>
          {/* The disclosure and Seen are siblings of the row, not inside
              it: a button in a button is invalid and would open it too.
              A pin carries Seen when it is the row's only copy: a local
              session is lifted out of its folder, a project's thread is not. */}
          {r.trouble && onAck && (!pin || !projectOf(r)) && (
            <button className="btn row-ack" onClick={() => onAck(r.id)} aria-label={`Mark ${name || "session"} seen`}>Seen</button>
          )}
          {r.turns && !q && !pin ? (
            <button className="row-twist" tabIndex={-1} aria-hidden="true" aria-expanded={open} aria-controls={`turns-${r.id}`}
                    aria-label={`${open ? "Hide" : "Show"} turn log of ${name || "session"}`}
                    onClick={() => setOpen(r.id, !open)}><Icon d={ICONS.chevron} size={14} /></button>
          ) : null}
        </div>
        {reason && <div className="row-why">{hit!.field}: {marked(reason.text, q)}</div>}
        {saidShown && <div className="row-why">said: {marked(saidShown.text, q)}</div>}
        {open && (
          <ol id={`turns-${r.id}`} className="turns" role="group" aria-label={`Turns of ${name || "session"}`}>
            {log?.lines && log.lines.length > 3 && (
              <li role="none">
                <button className="turn-line turn-more" role="treeitem" tabIndex={-1} aria-expanded={lines === log.lines}
                        onClick={() => setAllTurns((cur) => flip(cur, r.id))}>
                  <Icon d={ICONS.chevron} size={12} /><span className="turn-text">{lines === log.lines ? "Hide earlier turns" : `Earlier turns · ${log.lines.length - 3}`}</span>
                </button>
              </li>
            )}
            {lines ? lines.map((l) => (
              <li key={l.turn} role="none">
                <button className="turn-line" role="treeitem" tabIndex={-1} title={l.test ? `${l.test.cmd} exited ${l.test.exit}\n${l.text}` : l.text} onClick={() => onTurn?.(r.id, l.turn)}>
                  <span className="num turn-n">{l.turn}</span>
                  {/* A recorded test run says what happened first; narration only where nothing was recorded. */}
                  {l.test && <span className={"turn-result" + (l.test.exit ? " turn-result-bad" : "")}>{l.test.exit ? `Tests failed · exit ${l.test.exit}` : "Tests passed"}</span>}
                  {/* Narration is the agent's note, not a status: dimmed so it never reads as one. */}
                  <span className={"turn-text" + (l.test ? "" : " turn-note")}>{l.test ? l.test.cmd : l.text.replace(/^you (asked|said|wanted)( to| for| that)?\s+/i, "").replace(/^./, (c) => c.toUpperCase())}</span>
                </button>
              </li>
            )) : log ? (
              <li className="turn-wait"><InlineFail what="Couldn’t load turns" onRetry={() => setLogs((m) => { const { [r.id]: _, ...rest } = m; return rest; })} /></li>
            ) : <li className="turn-wait">Loading…</li>}
          </ol>
        )}
      </div>
      </Fragment>
    );
  };

  const workspaces = (groups: [string, Row[], string][], sec: string) => groups.map(([ws, list, gk]) => {
    const key = `${sec}:${gk}`;
    const open = !(searchOn ? searchFolds : wsFolded).has(key);
    const urgent = list.filter((r) => sessionSignal(r) === 0);
    const up = list[0].project ? orbsUp(list) : 0;
    const seen = new Map<string, number>();
    const nameKey = (r: Row) => titleKey(displayTitle(r) || sessionTitle(r));
    for (const r of list) seen.set(nameKey(r), (seen.get(nameKey(r)) ?? 0) + 1);
    const to = dragging && onMove ? dropProject(dragging, list, gk) : undefined;
    const drop = to === undefined ? {} : {
      "data-drop": dropAt === key ? "over" : "ok",
      onDragOver: (e: React.DragEvent) => { e.preventDefault(); e.dataTransfer.dropEffect = "move"; setDropAt(key); },
      onDragLeave: (e: React.DragEvent<HTMLDivElement>) => { if (!e.currentTarget.contains(e.relatedTarget as Node | null)) setDropAt((k) => (k === key ? null : k)); },
      onDrop: (e: React.DragEvent) => { e.preventDefault(); const id = dragging!.id; setDragging(null); setDropAt(null); onMove!(id, to); },
    };
    return (
      <div key={key} className="ws" {...drop}>
        {/* A project's name opens its page and its chevron folds the group,
            like every project sidebar; a folder's whole head folds, since a
            folder has no page. The arrow keys fold through .ws-fold. */}
        <button className="ws-head" role="treeitem" aria-expanded={open} data-project={list[0].project || undefined}
                onClick={() => { if (list[0].project && onOpenProject) onOpenProject(list[0].project); else toggleWs(key); }}
                aria-label={`${ws}${open ? "" : urgent.length ? `, ${urgent.length} need${urgent.length === 1 ? "s" : ""} you` : `, ${list.length}`}${!open && up ? `, ${orbsUpLabel(up)}` : ""}`} title={list[0].project ? `Open project ${ws}` : `${list[0].repo || list[0].cwd}\nNot a project: sessions grouped by where they ran`}>
          <span className="ws-fold" role="presentation" onClick={(e) => { if (list[0].project && onOpenProject) { e.stopPropagation(); toggleWs(key); } }}><Icon d={ICONS.chevron} size={12} /></span>
          {/* The eyebrow is text only: small caps and colour already say "group"; an icon and a mark put the name off the title column. */}
          <span className={"ws-name" + (list[0].project ? " ws-project" : "")}>{ws}</span>
          {/* After the name, so project and folder eyebrows share one left edge and a project still reads as one. */}
          {list[0].project && <Icon d={ICONS.projects} size={12} />}
          {/* Folded, a group still says when something in it needs you, in that state's colour. */}
          {!open && urgent.length > 0 && <span className={"count " + (urgent.some(hasFailure) ? "is-failed" : "is-waiting")} aria-hidden="true">{urgent.length}</span>}
          {/* Folded, a project group still says how many of its orbs are up. Open, each
              running row already carries its own chip. */}
          {!open && <OrbUp n={up} quiet />}
          {/* Rows pinned to Needs you leave the group; its head says where they went. */}
          {sec === "recent" && lifted.get(gk) ? <span className="ws-lifted" title="Listed under Needs you">{lifted.get(gk)} need{lifted.get(gk) === 1 ? "s" : ""} you ↑</span> : null}
        </button>
        {/* Over a group, say what the drop does: taking a session out of its project is not obvious from a folder. */}
        {to !== undefined && dropAt === key && <p className="ws-drop-hint">{to ? `Move to ${ws}` : "Take it out of its project"}</p>}
        {/* Folded, what needs you stays in view. */}
        {!open && urgent.length > 0 && <div role="group">{urgent.map((r) => session(r, (seen.get(nameKey(r)) ?? 0) > 1))}</div>}
        {open && (() => {
          // Sessions quiet for 72h fold under one line at the group's foot;
          // a search shows them all.
          const now = Date.now();
          const fresh = list.filter((r) => !isTucked(r, now, selected)), older = list.filter((r) => isTucked(r, now, selected));
          // A group leads with a few rows whatever their age: on a laptop
          // where last week's work is the work, one row and "57 older"
          // per group left the sidebar saying nothing. An empty thread is
          // not a row worth leading with.
          while (fresh.length < FRESH_MIN) {
            const i = older.findIndex((r) => !r.empty);
            if (i < 0) break;
            fresh.push(...older.splice(i, 1));
          }
          // Recent empty threads are not older, only folded.
          const more = older.every((r) => isOld(r, now)) ? "older" : "more";
          const olderKey = `older:${key}`;
          const olderOpen = foldOpen(olderKey, unfolded.has(olderKey));
          const dup = (r: Row) => (seen.get(nameKey(r)) ?? 0) > 1;
          return (
            <div role="group">
              {fresh.map((r) => session(r, dup(r)))}
              {older.length > 0 && (
                <button type="button" className="ws-older" role="treeitem" aria-expanded={olderOpen}
                        onClick={foldToggle(olderKey, () => toggleFold(olderKey))}>
                  <Icon d={ICONS.chevron} size={12} />{olderOpen ? `Hide ${older.length} ${more}` : `${older.length} ${more}`}
                </button>
              )}
              {olderOpen && older.map((r) => session(r, dup(r)))}
              {/* A long expansion folds from its foot too, so the way back is never a scroll away. */}
              {olderOpen && older.length > 8 && (
                <button type="button" className="ws-older" role="treeitem" aria-expanded={olderOpen}
                        onClick={foldToggle(olderKey, () => toggleFold(olderKey))}>
                  <Icon d={ICONS.chevron} size={12} />{`Hide ${more}`}
                </button>
              )}
            </div>
          );
        })()}
      </div>
    );
  });

  // A section folds, during a search too, which starts with every match open.
  const section = (key: string, label: React.ReactNode, open: boolean, toggle: () => void, count: React.ReactNode, body: React.ReactNode, alert?: "trouble" | "needs-you") => (
    <div className="sec">
      <button type="button" className="sec-fold" role="treeitem" aria-expanded={open} onClick={toggle} aria-controls={`sec-${key}`}>
        <Icon d={ICONS.chevron} size={12} /><span>{label}</span>
        {count !== null && <span className={alert ? "count " + (alert === "trouble" ? "is-failed" : "is-waiting") : "num sec-count"}><span className="visually-hidden">, </span>{count}</span>}
      </button>
      {open && <div className="sec-body" role="group" id={`sec-${key}`}>{body}</div>}
    </div>
  );

  // A phone has no room to fold the list into: there the list is the pane,
  // and its toolbar stays whole whatever a desktop left saved.
  const folded = closed && !narrow;
  const toolbar = (
    <div className={"side-bar" + (scrolled && !folded ? " side-bar-scrolled" : "")}>
      <button className="side-icon side-collapse" onClick={() => setSide(!closed)} aria-expanded={!closed}
              aria-label={closed ? "Show sidebar" : "Hide sidebar"} title={`${closed ? "Show sidebar" : "Hide sidebar"} (${modKey()}B)`}>
        <Icon d={ICONS.panel} />
      </button>
      {/* A phone's list is the root: a title, no history arrows. */}
      {narrow && <h2 className="side-title">Sessions</h2>}
      {/* One order folded or not: back, forward, find, new; the arrows hold their slot before there is history. */}
      {!narrow && <HistoryArrows back={hist.back} forward={hist.forward} />}
      {folded
        // The same slot does the same thing folded: it unfolds the list with its filter open.
        ? <button className="side-icon" onClick={() => { setSide(false); setSearching(true); }}
                  aria-label="Filter the list" title={`Filter the list (/) · search everything with ${modKey()}K`}><Icon d={ICONS.filter} /></button>
        : <button ref={searchBtn} className={"side-icon" + (showSearch ? " side-icon-on" : "")} aria-expanded={showSearch} aria-controls="q"
                onClick={() => { if (showSearch) { onQuery(""); setSearching(false); } else setSearching(true); }}
                aria-label="Filter the list" title={`Filter the list (/) · search everything with ${modKey()}K`}><Icon d={ICONS.filter} /></button>}
      {onNew && <button className={"side-new" + (folded ? "" : " side-new-label")} onClick={onNew} aria-label="New session" title="New session"><Icon d={ICONS.compose} />{!folded && <span>New session</span>}</button>}
    </div>
  );

  const nav = onView && <ViewNav view={view} onView={onView} wikiFlags={wikiFlags} icons={folded} />;
  if (folded) return <div className="sidebar sidebar-closed">{toolbar}<div className="rail-gap" />{nav}</div>;

  const total = recentAll.length + background.length + archived.length;
  // While something in Background needs you, its count says how many, not the total.
  // A failure already shows as "N failed" in the subline, so it isn't counted again here.
  const bgUrgent = background.filter((r) => sessionSignal(r) === 0 && !hasFailure(r));
  const bgAlert = background.some(hasFailure) ? "trouble" : bgUrgent.length ? "needs-you" : undefined;
  // Pinned on top while non-empty: links to the same sessions, which stay in their groups.
  const needsYou = [...recentAll, ...background].filter((r) => needIds.has(r.id));
  const foldOpen = (key: string, open: boolean) => (searchOn ? !searchFolds.has(key) : open);
  const foldToggle = (key: string, toggle: () => void) => () => (searchOn ? setSearchFolds((cur) => flip(cur, key)) : toggle());
  const cardRow = card ? rows.find((r) => r.id === card.id) : undefined;
  return (
    <div className="sidebar">
      {cardRow && card && (
        // Status from the recorded fields; the agent's own note is labelled
        // as that, so prose that went stale never reads as the state.
        <div id="row-card" key={cardRow.id} className="row-card" role="tooltip" style={{ top: card.top, left: card.left }}>
          <p className="row-card-title">{sessionTitle(cardRow)}</p>
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
          <label htmlFor="q" className="visually-hidden">Filter sessions</label>
          <input id="q" ref={searchRef} className="field" autoComplete="off" value={query} placeholder="Filter sessions"
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
          {loadedAt === null ? "Sessions unavailable" : `Updates delayed · synced ${ago(new Date(loadedAt).toISOString())} ago`}
          {onRetry && <button className="link" onClick={onRetry}>Retry</button>}
        </p>
      ) : loadedAt === null && slow && <p className="side-fresh" role="status">Loading sessions…</p>}
      <div className="scroll" role="tree" aria-label="Sessions" ref={treeRef} onScroll={(e) => { clearTimeout(peekTimer.current); setCard(null); setScrolled(e.currentTarget.scrollTop > 0); }} onKeyDown={walk}
           onFocus={(e) => {
             const t = e.target as HTMLElement;
             if (!t.matches(TREE_ITEMS)) return;
             // The tab stop follows focus now, not on the next render.
             if (stop.current && stop.current !== t) stop.current.tabIndex = -1;
             t.tabIndex = 0;
             stop.current = t;
             focusedStop.current = true;
           }}>
        {/* Empty only once a load has answered: before that it is loading or unavailable, said above. */}
        {total === 0 && loadedAt !== null && !loadErr && !(showArchived && archivedState !== "ready") && (
          <p className="list-none">{query ? `No sessions match “${query}”.` : "No sessions yet."}</p>
        )}
        {q && saidElsewhere && saidElsewhere.count > 0 && (
          <p className="list-none"><button className="link" onClick={saidElsewhere.open}>{saidElsewhere.count} more found in the conversation · {modKey()}K</button></p>
        )}
        {needsYou.length > 0 && (
          <div className="needs">
            <p className="eyebrow" id="needs-you" role="presentation">Needs you<span className="num needs-count">{needsYou.length}</span><span className="needs-note">across projects</span></p>
            <div role="group" aria-labelledby="needs-you">{needsYou.map((r) => session(r, false, false, true))}</div>
          </div>
        )}
        {workspaces(recent, "recent")}
        {background.length > 0 && section("background", "Background", foldOpen("background", unfolded.has("background")), foldToggle("background", () => toggleFold("background")),
          bgFailed ? <span title={`${bgFailed} failed of ${background.length}`}>{bgFailed}</span> : bgUrgent.length ? <span title={`${bgUrgent.length} need you of ${background.length}`}>{bgUrgent.length}</span> : background.length,
          workspaces(byWorkspace(background.filter((r) => !needIds.has(r.id)), projectNames), "background"), bgAlert)}
        {/* Archived is not loaded until opened, so a search cannot have looked there. */}
        {/* Opened and empty, the fold has nothing to offer. */}
        {!(showArchived && archivedState === "ready" && !archived.length && !q) && section("archived", q && !showArchived ? <>Archived not searched · <span className="sec-include">Include</span></> : "Archived", showArchived && !archFolded,
          // Once included, folding only hides the section; a search still covers it.
          () => (showArchived ? setArchFolded((v) => !v) : (setArchFolded(false), archFromFilter.current = Boolean(q), onToggleArchived())),
          showArchived && archivedState === "ready" ? archived.length : null,
          archivedState === "loading" ? <div className="list-none"><Pending what="Archived" inline onRetry={onRetryArchived} /></div>
          : archivedState === "failed" ? <p className="list-none"><InlineFail what="Couldn’t load archived" onRetry={onRetryArchived} /></p>
          : archived.length ? workspaces(byWorkspace(archived, projectNames), "archived") : <p className="list-none">{q ? "No archived matches." : "Nothing archived."}</p>)}
      </div>
      {nav}
    </div>
  );
}

const NAV_HREF: Record<View, string> = { sessions: "#/", me: "#/me", projects: "#/projects", project: "#/projects", hooks: "#/hooks", wiki: "#/wiki" };

/** A label id from before projects were keyed by slug: a UUID, which the
 *  slug pattern also accepts, so the shape has to be tested outright. */
const OLD_PROJECT_ID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

/** What projectdef.ValidSlug accepts. A route that is not one names no project. */
const SLUG = /^[a-z0-9][a-z0-9-]{0,62}$/;

/** Why a project session has no Project picker. One line, on hover. */
const PROJECT_SESSION = "A project session lives in its project's orb; start a thread in the other project instead.";

/**
 * The pages beside sessions, as links: they have routes, so they open in a
 * new tab and copy as a URL. A plain click still goes through onView, which
 * pushes history the way the rest of the app does. `phone` adds Sessions:
 * there it is the shell's bottom bar, not the sidebar's foot.
 */
export function ViewNav({ view, onView, wikiFlags = 0, icons = false, phone = false }: {
  view: View; onView: (v: View) => void; wikiFlags?: number; icons?: boolean; phone?: boolean;
}) {
  const items = ([["sessions", "Sessions", ICONS.search], ["me", "Me", ICONS.me], ["projects", "Projects", ICONS.projects], ["hooks", "Hooks", ICONS.hooks], ["wiki", "Wiki", ICONS.wiki]] as const)
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
            <summary><span className="block-label">Program</span></summary>
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

const GUTTER = /^\s*\d+\s*[|│]\s?/;

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
  // A file view's line-number gutter ("12|", "12│") is not the content.
  const head = (lines.find((l) => /[\p{L}\p{N}]/u.test(l.replace(GUTTER, ""))) ?? lines.find((l) => l.trim()) ?? "").replace(GUTTER, "");
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
  // A background agent's finish note: a notice row, its report one click in.
  if (id === undefined && isAgentNotice(line)) {
    const m = /^\[agent (.*?)(?: · [0-9a-f-]+)? (finished|failed|stopped)\]\s*([\s\S]*)$/.exec(line.text)!;
    // serve leads a failure with its reason: it belongs in the label, not one click in.
    const why = m[2] === "failed" ? /^Background agent failed: (.*)\n?([\s\S]*)$/.exec(m[3]) : null;
    const body = (why ? why[2] : m[3]).trim();
    // The agent's name is often its whole prompt: the row shows its first clause, in prose, the rest on hover.
    const name = m[1].trim();
    const short = name.split(/(?<=[.!?:])\s|\n/)[0].split(/\s+/).slice(0, 10).join(" ");
    return (
      <details className="block thin agent-notice">
        <summary>
          <span className="block-label">Background agent {m[2]}{why ? `: ${why[1]}` : ""}</span>
          <span className="block-detail agent-notice-name" title={name}>{short.length < name.length ? short.replace(/[.!?:]$/, "") + "…" : short}</span>
        </summary>
        {body && <div className="block-body"><Markdown text={body} /></div>}
      </details>
    );
  }
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

/**
 * A run of todo/* records as one collapsed row. Each add, done and clear
 * used to be its own line ("todo/done" three times, bare item texts),
 * which buried the reply around it; opened, it lists what changed.
 */
export function TodoRun({ lines, live }: { lines: Line[]; live?: boolean }) {
  const text = new Map<number, string>();
  for (const l of lines) if (l.kind === "todo/add") text.set(Number(l.data?.id), String(l.data?.text ?? l.text ?? ""));
  // The plan as it stands after the run: add/done/clear folded into one item set.
  const items = new Map<number, boolean>();
  for (const l of lines) {
    const id = Number(l.data?.id);
    if (l.kind === "todo/clear") items.clear();
    else if (l.kind === "todo/add") items.set(id, items.get(id) ?? false);
    else if (l.kind === "todo/done") items.set(id, true);
  }
  const plan = [...items].sort((a, b) => a[0] - b[0]);
  const done = plan.filter(([, d]) => d).length;
  const current = live ? plan.find(([, d]) => !d)?.[0] : undefined;
  const head = plan.length ? `${done} of ${plan.length} done` : "Cleared";
  return (
    <details className="block thin todo-run todo-plan">
      <summary>
        <span className="block-label">Plan</span>
        <span className="block-detail num">{head}</span>
        {plan.length > 0 && <span className="todo-bar" aria-hidden="true"><span style={{ width: `${(100 * done) / plan.length}%` }} /></span>}
      </summary>
      {plan.length > 0 && (
        <ul className="todo-plan-list">
          {plan.map(([id, d]) => (
            <li key={id} data-done={d ? "" : undefined} data-current={id === current ? "" : undefined}>
              <span className="todo-mark" aria-hidden="true">{d ? "✓" : id === current ? <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2"
                strokeLinecap="round" className="spin-mark"><circle cx="12" cy="12" r="8.5" strokeDasharray="40 14" /></svg> : "○"}</span>
              {d ? <s>{text.get(id) || `#${id}`}</s> : <span>{text.get(id) || `#${id}`}</span>}
            </li>
          ))}
        </ul>
      )}
      <details className="block thin todo-history">
        <summary><span className="block-label">History · {lines.length}</span></summary>
      <ul className="todo-run-list">
        {lines.map((l) => {
          const id = Number(l.data?.id);
          if (l.kind === "todo/clear") return <li key={l.seq} className="meta-line">Cleared the list</li>;
          const t = text.get(id) || (l.kind === "todo/add" ? l.text : "") || `#${id}`;
          return <li key={l.seq}><span aria-hidden="true">{l.kind === "todo/done" ? "✓ " : "+ "}</span>{l.kind === "todo/done" ? <s>{t}</s> : t}</li>;
        })}
      </ul>
      </details>
    </details>
  );
}

// Splits a reply on the loop's sentinel lines, skipping any quoted inside a fenced block.
function splitGuessed(text: string): string[] {
  const parts: string[] = [];
  let cur: string[] = [], fence = "";
  for (const l of text.split("\n")) {
    const f = /^[ \t]*(`{3,}|~{3,})/.exec(l);
    if (f) fence = !fence ? f[1] : l.trim().startsWith(fence) ? "" : fence;
    if (!fence && !f && l.trim() === "[guessed output omitted]") { parts.push(cur.join("\n")); cur = []; continue; }
    cur.push(l);
  }
  parts.push(cur.join("\n"));
  return parts;
}

/** A provider refusing the key (401/403), which no retry of the same model fixes. */
export function authError(text: string): boolean {
  return /\b(HTTP|status) 40[13]\b|\b40[13] (unauthori[sz]ed|forbidden)\b|authentication_error|invalid[ _-]?(x-)?api[ _-]?key/i.test(text);
}

/**
 * The hash the router writes back, or null to leave it. Its first run sees
 * the state from before the hash was read ("#/"), and a link's ?turn= was
 * lost to it; a sub-page's query (?turn=, ?file=) is the page's own.
 */
export function hashToReplace(current: string, want: string, sub: string | null, first: boolean): string | null {
  if (first || current === want || (sub && current.startsWith(want + "?"))) return null;
  return want;
}

/** R4-C, MB-ERR: a failed turn says what broke in one short line, the provider's message under it, the raw text behind Show details. Retry and Switch model are both secondary. */
const RetryTurn = createContext<(() => unknown) | null>(null);
function ErrorCard({ text }: { text: string }) {
  const retry = useContext(RetryTurn);
  const editPrompt = useContext(EditPrompt);
  const { title, body } = providerError(text);
  const [retrying, setRetrying] = useState(false);
  const retryBtn = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    // Only the latest card, only when nothing else holds focus and the composer is empty.
    const el = retryBtn.current, cards = document.querySelectorAll(".err-card");
    if (el && !editPrompt?.busy && cards[cards.length - 1]?.contains(el) && (document.activeElement === document.body || !document.activeElement)) el.focus({ preventScroll: true });
  }, []); // eslint-disable-line react-hooks/exhaustive-deps
  return (
    <div className="err err-card">
      <StateIcon kind="alert" />
      <p className="err-head" role="alert">{title}</p>
      {body && <p className="err-msg">{body}</p>}
      <RawDetails raw={text.trim()} className="err-raw" />
      <div className="err-actions">
        <button type="button" ref={retryBtn} className="btn" disabled={retrying} aria-busy={retrying || undefined} onClick={async () => {
          if (!retry) return;
          setRetrying(true);
          try { await retry(); } finally { setRetrying(false); }
        }}>{retrying ? <><Spinner /> Retrying…</> : "Retry"}</button>
        <button type="button" className="btn err-action" onClick={() => {
          const pick = [...document.querySelectorAll<HTMLButtonElement>('.composer-tools button[aria-label^="Next turn model"]')].find((b) => b.offsetParent);
          pick?.scrollIntoView({ block: "nearest" });
          pick?.click();
        }}>Switch model</button>
      </div>
    </div>
  );
}

/** The finished turn's answer whose actions sit on the turn's footer line, not under the prose. */
const FootAnswer = createContext<number | undefined>(undefined);
const answerBody = (text: string, codes: string[]) => splitBareProgram(stripRunFences(splitExecNote(text).text, codes))[0];
function AnswerActs({ line, body }: { line: Line; body: string }) {
  return (
    <div className="msg-acts">
      <span className="num say-time" title={new Date(line.at).toLocaleString()}>{when(line.at)}</span>
      <CopyButton text={body} what="answer as Markdown" />
    </div>
  );
}

export function Entry({ line, codes, nested, until }: { line: Line; codes: string[]; nested?: boolean; /** When the next entry landed: a thinking block's end. */ until?: string }) {
  const footSeq = useContext(FootAnswer);
  const k = line.kind;
  if (isNativeCall(line)) return <NativeCall line={line} />;
  if (isCall(line)) return <CallRows calls={[line]} nested />;
  if (k === "assistant" || k === "sub:assistant") {
    // The loop's "blocks dropped" marker is a notice about the reply, not part of it.
    const { text: said, note } = splitExecNote(line.text);
    const [body, bare] = splitBareProgram(stripRunFences(said, codes));
    const program = programRan(bare, codes) ? "" : bare; // the recorded Program row already shows it
    if (blank(body) && !program) return note ? <ExecNote note={note} /> : null; // the reply was only the program it ran
    // The loop replaced the whole reply as model-guessed output: say so quietly, not as the answer.
    if (!program && body.trim() === "[guessed output omitted]") return <p className="exec-note exec-note-quiet">The reply was only output the model guessed, so it was removed</p>;
    // Inside a subagent card the rail and the card's own header
    // already say whose words these are; repeating "subagent" above
    // every paragraph of a five-step run is noise.
    return (
      <div className="say">
        {/* There is one assistant; naming it above every reply said nothing. */}
        {!nested && k !== "assistant" && <div className="say-who"><span className="sub-dot" /><span>subagent</span></div>}
        {!blank(body) && splitGuessed(body).map((part, i) => (
          // The loop swapped a fenced block of invented output for a sentinel line: a quiet note, not prose.
          <Fragment key={i}>
            {i > 0 && <p className="exec-note exec-note-quiet">A code block with guessed output was removed</p>}
            {!blank(part) && <Markdown text={part} />}
          </Fragment>
        ))}
        {!nested && k === "assistant" && !blank(body) && line.seq !== footSeq && <AnswerActs line={line} body={body} />}
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
  if (k === "input") {
    // R3-C: a steer the running turn took, inside that turn.
    // Yours, so it sits where your prompts do: a bubble on the right, labelled.
    return (
      <div className="steer-note">
        <span className="steer-word" title="Sent while the turn ran" aria-label="Steer">
          <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" aria-hidden="true">
            <circle cx="12" cy="12" r="9" /><circle cx="12" cy="12" r="2.5" />
            <path d="M12 3v6.5M12 14.5V21M3 12h6.5M14.5 12H21" />
          </svg>
        </span>
        <p className="prompt-bubble steer-bubble"><PromptWords text={line.text} /><SentImages text={line.text} /></p>
      </div>
    );
  }
  if (k === "model-switch") {
    // A /model switch records the command and the loop's echo: one line, the record inside.
    const recs = (line.data?.lines ?? []) as string[];
    return (
      <details className="block thin">
        <summary>
          <span className="block-label">Model changed</span>{" "}
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
          <span className="block-label">{(() => {
            // Recorded reasoning is over: past tense, with how long it took when both ends are known.
            const ms = until && line.at ? Date.parse(until) - Date.parse(line.at) : NaN;
            return Number.isFinite(ms) && ms >= 1000 ? `Thought for ${duration(ms)}` : "Thought";
          })()}</span>
          <span className="block-detail">{plainTitle(head).slice(0, 90)}</span>
        </summary>
        {/* Reasoning is markdown like any other reply: left raw it
            shows its own backticks and list markers as punctuation. */}
        <div className="think-body"><Markdown text={line.text} /></div>
      </details>
    );
  }
  if (k === "error" || k === "sub:error") {
    if (k === "sub:error") return <div className="err">{line.text}</div>;
    return <ErrorCard text={line.text} />;
  }
  if (k === "ask") return null; // the live ask renders as its own card below
  if (k === "ask/answer" && line.data?.secret) return <div className="meta-line">secret stored</div>;
  // The answer also comes back as the ask's Result row: labelled, not a second bare copy.
  if (k === "ask/answer") return <div className="meta-line">You answered: {line.text}</div>;
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

  const codes = agent.lines.filter((l) => l.kind === "sub:code").map((l) => l.text);
  const task = firstLine(plainTitle(agent.task));
  // One step count for the header and the body: the loop's reported total, or what was recorded when that is more.
  const recorded = w.recordedSteps ?? 0;
  const totalSteps = Math.max(w.steps ?? 0, recorded || agent.lines.length);
  // A running row ticks its elapsed time; an ended one says its recorded duration.
  const ran = w.ms !== undefined ? duration(w.ms) : w.life === "running" && w.startedAt ? elapsed(Math.max(0, now - Date.parse(w.startedAt))) : "";
  const timing = [ran, totalSteps ? stepCount(totalSteps) : ""].filter(Boolean).join(" · ");
  const line2 = [
    w.stepErrors ? `${w.stepErrors} step ${w.stepErrors === 1 ? "error" : "errors"}` : "",
    w.notRun ? `${w.notRun} later ${w.notRun === 1 ? "block" : "blocks"} skipped after a failure` : "",
  ].filter(Boolean).join(" · ");
  const aria = `Subagent ${agent.worker}, ${stateText(w)}${w.ms !== undefined ? `, ${spokenDuration(w.ms)}` : ""}${totalSteps ? `, ${stepCount(totalSteps)}` : ""}${task ? `: ${task.slice(0, 80)}` : ""}`;

  // Each step keeps its seq, so "View steps" can land on the first one that went wrong.
  const entry = (l: Line) => <div key={l.seq} data-step-seq={l.seq} style={{ display: "contents" }}><Entry line={l} codes={codes} nested /></div>;
  const stepsDisclosure = (label?: string, lines = agent.lines) => lines.length ? (
    <details className="block thin" data-open-key={w.key + ":steps"}>
      <summary><span className="block-label">{label ?? (recorded && totalSteps > recorded ? `Steps · ${recorded} of ${totalSteps} recorded` : `Steps · ${totalSteps}`)}</span></summary>
      <div className="sub-body">{lines.map(entry)}</div>
    </details>
  ) : <p className="meta-line">No steps were recorded</p>;
  const taskDisclosure = agent.task ? (
    <details className="block thin" data-open-key={w.key + ":task"}>
      <summary><span className="block-label">Task</span></summary>
      <div className="sub-body"><Markdown text={agent.task} /></div>
    </details>
  ) : null;
  // Said once: a missing result is one quiet line, not a labelled empty section.
  const resultSection = w.result ? (
    <div className="sub-sec">
      <span className="block-label">Result</span>
      <Markdown text={w.result} />
    </div>
  ) : <p className="meta-line">Result not recorded</p>;

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
        <span className="num sub-tag" title={`Subagent ${agent.worker}`}>{agent.worker}</span>
        <span className={"sub-task" + (task ? "" : " exec-note-quiet")} title={agent.task || undefined}>{task || "Untitled subagent · task not recorded"}</span>
        <span className="sub-state"><WorkState w={w} /></span>
        <span className="num sub-steps">{timing}</span>
        {line2 && <span className="sub-line2">{line2}</span>}
      </summary>
      <div className="sub-body">
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

/**
 * R4-B: what a failed program's failure is called: a go test FAIL name, else
 * the sub-command of its last shell call that stopped the chain ("go test"
 * from "gofmt -w x && go test ./..."), never a file it edited or its first call.
 */
export function failNameOf(code: string, out: string): string {
  const test = /--- FAIL: (\S+)/.exec(out)?.[1];
  if (test) return test;
  const calls = [...code.matchAll(/tools\.bash\(\s*(["'`])((?:\\.|(?!\1)[^\\])*)\1/g)].map((m) => m[2]);
  const cmd = calls.at(-1);
  if (!cmd) return "";
  const parts = cmd.split(/\s*(?:&&|\|\||;)\s*/).map((c) => c.trim()).filter(Boolean);
  const short = (c: string) => {
    const w = c.split(/\s+/);
    return w.length > 1 && /^[a-z][\w-]*$/.test(w[1]) ? `${w[0]} ${w[1]}` : w[0];
  };
  // An earlier sub-command that complained under its own name stopped the chain there.
  const named = parts.slice(0, -1).find((c) => new RegExp(`^${short(c).split(" ")[0].replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}:`, "m").test(out));
  return short(named ?? parts.at(-1) ?? cmd);
}

/** R4-B: a multi-file read's output cut at each file, when every file restarts its numbering at 1. */
export function readChunks(code: string, out: string): { path: string; text: string }[] | null {
  if (!/^\s*tools\.view|console\.log\(\s*tools\.view/m.test(code) || /tools\.(?!view)\w+\(/.test(code)) return null;
  const paths = [...code.matchAll(/tools\.view\(\s*(["'`])([^"'`]+)\1/g)].map((m) => m[2]);
  if (paths.length < 2) return null;
  const chunks: string[][] = [];
  for (const l of out.split("\n")) {
    if (/^\s*1\s*[|│]/.test(l) || !chunks.length) chunks.push([]);
    chunks.at(-1)!.push(l);
  }
  if (chunks.length !== paths.length) return null;
  return chunks.map((c, i) => ({ path: paths[i], text: c.join("\n").replace(/\n+$/, "") }));
}

/** One call's recorded facts, for its thin line and the hover list. */
interface CallFacts { verb: string; gist: string; cmd: string; exit?: number; ms?: number; failed: boolean; preview?: string }

/**
 * A mixed run named by what it did, edits first: "Edited math.ts,
 * index.ts" with the lines changed, then how many reads and commands.
 * Null when the programs used none of those tools.
 */
export function runSummary(codes: string[], facts: CallFacts[], turnEdits?: Change[] | null, /** The calls that did not fail; all of them when omitted. */ okCodes = codes,
  /** An engine's native calls in the run: counted from their records, as there is no source to read. */ native: Line[] = []): { label: string; add: number; del: number; rest: string } | null {
  const all = codes.join("\n"), ok = okCodes.join("\n");
  const count = (name: string, text = all) => [...text.matchAll(new RegExp(`tools\\.${name}\\s*\\(`, "g"))].length;
  const okNative = native.filter((l) => !callRunning(l) && !callFailed(l));
  const tools = (name: string, ls = native) => ls.filter((l) => l.data?.tool === name).length;
  const nEdits = nativeEdits(okNative);
  // A failed call's edits are not counted as done.
  const edited = [...new Set([...[...ok.matchAll(/tools\.(?:patch|write)\s*\(\s*(["'`])([^"'`]+)\1/g)].map((m) => m[2]), ...nEdits.map((f) => f.path)].map((p) => p.split("/").pop()!))];
  const reads = count("view") + tools("view"), runs = count("bash") + tools("bash"), edits = count("patch", ok) + count("write", ok) + tools("patch", okNative) + tools("write", okNative);
  if (!reads && !runs && !edits && !turnEdits?.length) return null;
  // Counted from the calls, so a write nobody printed counts too (all additions).
  let add = 0, del = 0;
  for (const f of [...callEdits(ok), ...nEdits]) { add += f.add; del += f.del; }
  const rest = [reads ? `read ${reads} ${reads === 1 ? "file" : "files"}` : "", runs ? `ran ${runs} ${runs === 1 ? "command" : "commands"}` : ""].filter(Boolean);
  if (turnEdits?.length) {
    // The checkpoint diff is what the header and footer read: shell edits count too.
    const names = turnEdits.map((f) => f.path.split("/").pop()!);
    add = turnEdits.reduce((n, f) => n + Math.max(0, f.add), 0); del = turnEdits.reduce((n, f) => n + Math.max(0, f.del), 0);
    return { label: `Edited ${names.length <= 2 ? names.join(", ") : `${names.length} files`}`, add, del, rest: rest.join(" · ") };
  }
  if (!edits) return { label: rest.join(" · ").replace(/^./, (c) => c.toUpperCase()), add: 0, del: 0, rest: "" };
  const names = edited.length && edited.length <= 2 ? edited.join(", ") : `${edited.length || edits} files`;
  return { label: `Edited ${names}`, add, del, rest: rest.join(" · ") };
}

function callFacts(code: Line, result?: Line, calls: Line[] = []): CallFacts {
  const call = parseCall(code.text);
  const exit = typeof result?.data?.exit === "number" ? (result.data.exit as number) : undefined;
  const ms = typeof result?.data?.ms === "number" ? (result.data.ms as number) : undefined;
  const failed = (exit !== undefined && exit !== 0) || Boolean(thrownError(result));
  // Recorded calls name the block by what it actually did; the first one
  // leads, and " +N" says how many more there were (the gist convention).
  if (calls.length) {
    const first = calls[0];
    const gist = first.text + (calls.length > 1 ? ` +${calls.length - 1}` : "");
    return { verb: callVerb(String(first.data?.tool ?? "")), gist, cmd: first.text, exit, ms, failed };
  }
  return { verb: call.verb, gist: gistOf(call.gist), cmd: gistOf(call.target || call.gist), exit, ms, failed };
}

/** "+3 −1 · 40ms · exit 1": one call row's evidence, from its record. */
function CallMeta({ line }: { line: Line }) {
  const d = line.data ?? {};
  const ms = typeof d.ms === "number" ? d.ms : undefined;
  const add = typeof d.add === "number" ? d.add : 0, del = typeof d.del === "number" ? d.del : 0;
  const exit = typeof d.exit === "number" ? d.exit : undefined;
  return (
    <span className="num call-meta">
      {add > 0 && <span className="rt-add">+{add}</span>}{del > 0 && <span className="rt-del">−{del}</span>}
      {ms !== undefined && <span>{ms < 1000 ? "<1s" : duration(ms)}</span>}
      {exit !== undefined && exit !== 0 && <span className="tool-meta-failed">exit {exit}</span>}
    </span>
  );
}

/**
 * The calls a block made, one row each, as they were recorded: what ran,
 * what it cost, and for the one that failed, why. While the block runs
 * the call in flight sits last with a spinner, so progress streams in a
 * step at a time instead of appearing all at once with the result.
 */
export function CallRows({ calls, running, nested }: { calls: Line[]; running?: RunningCall | null; nested?: boolean }) {
  if (!calls.length && !running) return null;
  return (
    <ol className={"call-rows" + (nested ? " call-rows-nested" : "")}>
      {calls.map((l) => {
        const failed = callFailed(l);
        const why = typeof l.data?.error === "string" ? firstLine(cleanError(l.data.error)) : "";
        return (
          <li key={l.seq} className={"call-row" + (failed ? " call-row-failed" : "")}>
            <span className="call-mark" aria-hidden="true">{failed ? "✕" : "✓"}</span>
            <span className="call-verb">{callVerb(String(l.data?.tool ?? ""))}</span>
            <span className="mono call-detail" title={l.text}>{l.text}</span>
            {why && <span className="call-why" title={why}>{why}</span>}
            <CallMeta line={l} />
          </li>
        );
      })}
      {running && (
        <li className="call-row call-row-running">
          <span className="call-mark" aria-hidden="true">
            <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" className="spin-mark"><circle cx="12" cy="12" r="8.5" strokeDasharray="40 14" /></svg>
          </span>
          <span className="call-verb">{presentTense(callVerb(running.tool))}</span>
          <span className="mono call-detail" title={running.detail}>{running.detail}</span>
          <span className="num call-meta"><Elapsed since={running.at} title="Running" /></span>
        </li>
      )}
    </ol>
  );
}

/** A native call's facts for its run's header and hover list, from its own record. */
function nativeFacts(l: Line): CallFacts {
  const d = l.data ?? {};
  const out = str(d.output).split("\n").filter((x) => x.trim());
  return {
    verb: callVerb(str(d.tool)), gist: l.text, cmd: str(d.cmd) || l.text,
    exit: typeof d.exit === "number" ? d.exit : undefined, ms: typeof d.ms === "number" ? d.ms : undefined,
    failed: !callRunning(l) && callFailed(l), preview: out.slice(0, 3).join("\n") || undefined,
  };
}

/** The last lines of a running call's live output: enough to see it is alive, and what it says now. */
const tailLines = (s: string, n = 3) => s.split("\n").filter((l) => l.trim()).slice(-n);

/**
 * An engine session's call, as one row: what it did and what it cost,
 * opening onto what it printed. There is no program around it to show,
 * so the row is the call itself. While it runs it spins and keeps its
 * last three lines of output in view; a call that outlived its turn says
 * which job it became, and one whose result reached the model after the
 * model had moved on says "late".
 */
export function NativeCall({ line, current }: { line: Line; /** The failure its turn ended on: open. */ current?: boolean }) {
  const d = line.data ?? {};
  const tool = str(d.tool);
  const running = callRunning(line);
  const canceled = d.canceled === true;
  const failed = !running && !canceled && callFailed(line);
  const why = failed && typeof d.error === "string" ? firstLine(cleanError(d.error)) : "";
  const output = str(d.output);
  const tail = running ? tailLines(str(d.tail)) : [];
  const job = typeof d.job === "number" ? d.job : undefined;
  // The summary shows one clipped line of the command; the open row
  // shows all of it, so a long one-liner can actually be read.
  const cmd = str(d.cmd) || line.text;
  return (
    <details className={"block thin toolcall call-native" + (failed ? " block-failed" : "")} data-seq={running ? undefined : line.seq}
             open={current || (running && tail.length > 0) || undefined}>
      <summary role="button">
        <span className="block-label">{running ? presentTense(callVerb(tool)) : callVerb(tool)}</span>
        {line.text && <span className="mono block-detail" title={str(d.cmd) || line.text}>{line.text}</span>}
        {why && <span className="tool-thrown" title={why}>{why}</span>}
        {failed && <FailMark />}
        {canceled && <span className="tool-unrecorded tool-stopped"><StopMark />Cancelled</span>}
        {job !== undefined && <span className="num call-badge" title="It outlived its turn and ran on as a background job">Job {job}</span>}
        {d.late === true && <span className="num call-badge" title="The model moved on while this ran; the result reached it later">Late</span>}
        {running ? (
          <span className="num tool-meta tool-running">
            <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2"
                 strokeLinecap="round" className="spin-mark" aria-hidden="true"><circle cx="12" cy="12" r="8.5" strokeDasharray="40 14" /></svg>
            <Elapsed since={line.at} title="Running" />
          </span>
        ) : <CallMeta line={line} />}
        {output && <CopyButton text={output} what="output" />}
      </summary>
      <div className="block-body">
        {cmd && <pre className="mono call-cmd">{cmd}</pre>}
        {running ? (tail.length > 0 && <pre className="mono call-tail" aria-live="off">{tail.join("\n")}</pre>)
          : output.trim() ? <div className="tool-output"><Code text={output} lang={tool === "view" ? langForPath(line.text) : ""} /></div>
          : <p className="tool-noresult">No output.</p>}
        {d.truncated === true && <p className="exec-note exec-note-quiet">Output shortened here: the head and tail are kept</p>}
      </div>
    </details>
  );
}

/** Fired when a turn starts or output streams in; open output cards close. */
const TRANSCRIPT_GREW = "bough:transcript-grew";

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
  const anchor = useRef<HTMLElement | null>(null);
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
    // A new turn or streamed output: the card would cover what is arriving.
    window.addEventListener(TRANSCRIPT_GREW, hide);
    // The line moved without a scroll event (content above it grew): it no longer points at it.
    let frame = requestAnimationFrame(function watch() {
      if (!anchor.current?.isConnected || Math.abs(anchor.current.getBoundingClientRect().top - at.top) > 1) { hide(); return; }
      frame = requestAnimationFrame(watch);
    });
    return () => {
      window.removeEventListener("keydown", key, true); window.removeEventListener("scroll", scroll, true);
      window.removeEventListener("hashchange", hide); window.removeEventListener("popstate", hide);
      window.removeEventListener(TRANSCRIPT_GREW, hide); cancelAnimationFrame(frame);
    };
  }, [at, hide]);
  useEffect(() => () => clearTimeout(timer.current), []);
  const show = (e: React.SyntheticEvent<HTMLElement>) => {
    const el = e.currentTarget;
    if ((el.parentElement as HTMLDetailsElement | null)?.open) return;
    // A fine pointer only: focus walking the summaries never opens a preview.
    if (!canHover()) return;
    clearTimeout(timer.current);
    timer.current = setTimeout(() => { anchor.current = el; setAt(el.getBoundingClientRect()); }, 300);
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

/** The pictures a message carries, as its recorded prompt shows them: a
 *  steer, a send still on its way and a queued message would otherwise
 *  show a bare "[Image #N]", which reads as an image that never went. */
function SentImages({ text }: { text: string }) {
  const { images } = parsePrompt(text);
  return images.length ? <span className="prompt-images">{images.map((p, i) => <Thumb key={i} path={p} n={i + 1} />)}</span> : null;
}

/** A result's text minus the code history prefixes onto it. */
function resultBody(l: Line): string {
  const code = str(l.data?.code);
  return code && l.text.startsWith(code) ? l.text.slice(code.length).trimStart() : l.text;
}

/** Verbs parseCall classifies; anything else is a group without an invented name. */
const KNOWN_VERBS = new Set(["Ran", "Wrote", "Patched", "Read", "Delegate", "Asked you", "Test", "Search", "Build", "Fetch", "Vet"]);

/** MB-TR: a failure's one red glyph, a 12px ×-circle. */
function FailMark() {
  return (
    <svg className="fail-mark" width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" aria-hidden="true">
      <circle cx="12" cy="12" r="9" /><path d="m9 9 6 6M15 9l-6 6" />
    </svg>
  );
}

/** MB-TR: a stop is neither error nor warning: a small neutral square. */
export function StopMark() {
  return <span className="stop-mark" aria-hidden="true" />;
}

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
export function ToolRun({ lines, codes, live, stopped, failSeq, spawned, turnEdits }: { lines: Line[]; codes: string[]; live?: boolean; stopped?: boolean;
  /** The turn's checkpoint diff, when this is its one group: the counts the header and footer show, shell edits included. */ turnEdits?: Change[] | null; /** The subagent card that follows the run, when it has a result. */ spawned?: Worker; /** The result seq of the failure the turn ended on: it opens itself. */ failSeq?: number }) {
  // Pair each call with the result recorded for it: one row per thing
  // done, not a "Ran" row and a "Result" row saying half each. A result
  // names its call in data.code, so notes in between never split the
  // pair; one without that record pairs only with the call right above.
  const rows: React.ReactNode[] = [];
  const facts: CallFacts[] = [];
  /** Calls that did not fail: a failed edit is not counted as done. */
  const okCodes: string[] = [];
  const used = new Set<number>();
  for (let i = 0; i < lines.length; i++) {
    const l = lines[i];
    if (used.has(i)) continue;
    if (l.kind === "code") {
      let result: Line | undefined;
      // The block's own calls: every call row recorded after it, up to its result.
      const calls: Line[] = [];
      for (let j = i + 1; j < lines.length; j++) {
        const r = lines[j];
        // A native call recorded while a run_js block ran is not that block's own.
        if (r.kind === "call" && !isNativeCall(r) && !used.has(j)) { calls.push(r); used.add(j); continue; }
        if (r.kind === "code") break;
        if (r.kind !== "result" || used.has(j)) continue;
        const code = str(r.data?.code);
        if (code ? code.trim() === l.text.trim() : j === i + 1) { result = r; used.add(j); break; }
      }
      const f = callFacts(l, result, calls);
      facts.push(f);
      if (!f.failed) okCodes.push(l.text);
      rows.push(<ToolCall key={l.seq} code={l} result={result} calls={calls} live={live} stopped={stopped} current={failSeq !== undefined && result?.seq === failSeq} spawned={spawned} />);
    } else if (isNativeCall(l)) {
      const f = nativeFacts(l);
      facts.push(f);
      rows.push(<NativeCall key={l.seq} line={l} current={failSeq === l.seq} />);
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
  // Mixed work is named from counts, edits first; never after its first command.
  const mixed = !known || verbs.length > 2;
  const summary = mixed ? runSummary(lines.filter((l) => l.kind === "code").map((l) => l.text), facts, turnEdits, okCodes, lines.filter(isNativeCall)) : null;
  const label = summary ? summary.label : mixed ? "Tool group" : verbs.map((v, i) => (i ? v.toLowerCase() : v)).join(" and ");
  const targets = [...new Set(facts.map((f) => f.gist).filter(Boolean))];
  const fileish = verbs.every((v) => v === "Wrote" || v === "Patched" || v === "Read");
  // A path keeps its filename: the leading directories are what gets cut.
  // A program's gist carries its own " +N"; the row counts "N more" once, itself.
  const first = (targets[0] ?? "").replace(/ \+\d+$/, "");
  const target = first ? (fileish ? first.split("/").pop()! : first) : "";
  const more = targets.length - 1;
  return (
    <div className={"toolrun-wrap" + (failed > 0 ? " toolrun-has-failed" : "")}>
    <details className="block thin toolrun" ref={box} open={holdsFail || undefined} data-open-key={"tools:" + lines[0].seq}>
      <summary role="button" {...handlers}>
        <span className="block-label">{label}</span>{" "}
        {summary ? <>
          {summary.add > 0 && <span className="num rt-add">+{summary.add}</span>}{" "}
          {summary.del > 0 && <span className="num rt-del">−{summary.del}</span>}{" "}
          {summary.rest && <span className="num tool-more">{summary.rest}</span>}{" "}
        </> : <>
          {target && <span className="mono block-detail" title={targets[0]}>{target}</span>}{" "}
          {more > 0 && <span className="num tool-more">{more} more {fileish ? (more === 1 ? "file" : "files") : ""}</span>}{" "}
        </>}
        <span className="num tool-meta">{calls} calls{timed ? ` · ${totalMs < 1000 ? "<1s" : duration(totalMs)}` : ""}</span>
      </summary>
      {pop}
      <div className="toolrun-body">{rows}</div>
    </details>
    {/* R3-F: the button that opens the failure sits beside the summary, not inside it, pinned to the row's end so it never wraps alone. */}
    {failed > 0 && <button type="button" className="link num toolrun-failed" onClick={openFailed}><FailMark />{failed} failed</button>}
    </div>
  );
}

/**
 * One call and what came back, as a single row. The summary says what
 * was done and how much it printed; opened, the program and its output
 * sit together, with the raw call one level further in.
 */
export function ToolCall({ code, result, calls = [], live, stopped, current, spawned }: { code: Line; result?: Line; /** The block's recorded calls, in order. */ calls?: Line[]; /** Its turn is still running. */ live?: boolean; stopped?: boolean; /** The failure its turn ended on: open, with the diagnosis. */ current?: boolean; /** The subagent card it started, when that has a result. */ spawned?: Worker }) {
  const call = useMemo(() => parseCall(code.text), [code.text]);
  // The call in flight belongs to the block still waiting on its result (a child's call shows under its own card).
  const inFlight = useContext(RunningCallCtx);
  const running = live && !result && inFlight && inFlight.kind === "call" ? inFlight : null;
  // The block is named by its recorded calls when it has them: one call
  // is that call, several are counted; the regex over the source is the
  // fallback for older sessions that recorded none.
  const recorded = calls.length > 0 || Boolean(running);
  const headline = running ? { label: presentTense(callVerb(running.tool)), detail: running.detail }
    : calls.length === 1 ? { label: callVerb(String(calls[0].data?.tool ?? "")), detail: calls[0].text }
    : calls.length > 1 ? { label: callsHeadline(calls), detail: "" } : null;
  // A block that only edited keeps the edit row (files and counts); any
  // other recorded block is named by its calls, so a failed command
  // after an edit never reads "Edit failed".
  const editsOnly = !running && calls.length > 0 && calls.every((l) => l.data?.tool === "write" || l.data?.tool === "patch");
  // The files it patched or wrote, one row each, read from the call itself.
  const edits = useMemo(() => callEdits(code.text), [code.text]);
  const editAdd = edits.reduce((n, f) => n + f.add, 0), editDel = edits.reduce((n, f) => n + f.del, 0);
  const turnSeq = useContext(TurnSeq);
  const session = useWork()?.session;
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
  // A block that threw failed, whatever exit its bash calls had.
  const rawThrown = thrownError(result);
  const failed = (exit !== undefined && exit !== 0) || Boolean(rawThrown);
  const thrown = rawThrown && cleanError(rawThrown);
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
  // "exit N" is a bash exit; a block that threw says what it threw instead.
  // R3-F: a failed call says so once, quietly, at the end of the row: never in the meta as well.
  const meta = [continues ? `Continues as Job ${continues}` : "", empty ? "No output" : "", ms !== undefined ? (ms < 1000 ? "<1s" : duration(ms)) : ""];
  const what = call.lang === "bash" ? "Command" : call.lang === "javascript" ? "Program" : "Content";
  // No result: still running, cut off by a stop, or never recorded. Each says which.
  const card = !result && spawned && /tools\.spawn(All)?\(/.test(code.text) ? spawned : undefined;
  const missing = result || card ? null : live ? "running" : stopped ? "Interrupted" : "Result not recorded";
  // MB-TR: "exit 0" is noise; a non-zero exit is the one red fact in the meta.
  const exitBad = exit !== undefined && exit !== 0 && !thrown;
  const [full, setFull] = useState(false);
  // File rows fetch their numbered diffs only once the call is opened.
  const [opened, setOpened] = useState(Boolean(current));
  const phone = useMedia("(max-width:720px)");
  // The last lines of a failure are where the diagnosis is.
  // R4-B: an edit shown as a rendered diff below is not repeated here as -/+ text.
  const diag = failed ? cleanError(outputParts(out).filter((p) => p.kind !== "edit" || !edits.some((f) => f.path === p.path)).map((p) => (p.kind === "text" ? p.text : "")).join("\n")).split("\n").filter((l) => l.trim()).slice(-10) : [];
  const cmdText = call.lang === "bash" ? call.body : call.raw;
  return (
    <>
    <details className={"block thin toolcall" + (failed ? " block-failed" : "")} data-seq={result?.seq} open={current || (recorded && !result && live) || undefined} onToggle={(e) => setOpened(e.currentTarget.open)}>
      <summary role="button" {...handlers}>
        <span className="block-label">{timedOut ? "Question timed out" : label ?? (headline && !editsOnly ? headline.label : edits.length ? (failed ? "Edit failed" : "Edited") : call.verb)}</span>
        {!label && headline && !editsOnly && !timedOut ? (headline.detail ? <span className="mono block-detail" title={headline.detail}>{phone ? tailPath(headline.detail) : headline.detail}</span> : null)
        : !label && edits.length > 0 && !timedOut ? <>
          <span className="mono block-detail edit-detail" title={edits.map((f) => f.path).join("\n")}>{edits.length === 1 ? edits[0].path.split("/").pop() : `${edits.length} files`}</span>
          {!failed && <span className="num edit-counts">{editAdd > 0 && <span className="rt-add">+{editAdd}</span>}{editDel > 0 && <span className="rt-del">−{editDel}</span>}</span>}
        </> : !label && <span className="mono block-detail" title={call.gist}>{timedOut ? timedOut[1] : phone ? tailPath(gistOf(call.gist)) : gistOf(call.gist)}</span>}
        {thrown && <span className="tool-thrown" title={thrown}>{firstLine(thrown)}</span>}
        {failed && <FailMark />}
        {(exitBad || meta.some(Boolean)) && (
          <span className="num tool-meta">{exitBad && <span className="tool-meta-failed">exit {exit}</span>}{exitBad && meta.some(Boolean) ? " · " : ""}{meta.filter(Boolean).join(" · ")}</span>
        )}
        {missing === "running" && (
          <span className="num tool-meta tool-running">
            <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2"
                 strokeLinecap="round" className="spin-mark" aria-hidden="true"><circle cx="12" cy="12" r="8.5" strokeDasharray="40 14" /></svg>
            <Elapsed since={code.at} title="Running" />
          </span>
        )}
        {card && <WorkState w={card} />}
        {missing && missing !== "running" && <span className={"tool-unrecorded" + (stopped ? " tool-stopped" : "")}>{stopped ? <StopMark /> : <WarnMark />}{missing}</span>}
        {!empty && <CopyButton text={out || call.body || call.raw} what={result ? "output" : call.verb.toLowerCase() + " block"} />}
      </summary>
      {pop}
      <div className="block-body">
        {/* What the block did, step by step, before what it printed. */}
        {/* One recorded call is the summary line itself; rows earn their place from the second call, or one in flight. */}
        {(calls.length > 1 || running) && <CallRows calls={calls} running={running} />}
        {/* Output first; the call that made it is one disclosure, once. */}
        {result && !empty && !(failed && diag.length > 0) && <span className="body-copy"><CopyButton text={out} what="output" /></span>}
        {stopped && !result && !card && <p className="tool-noresult">No result was recorded.</p>}
        {!result && call.body && <Code text={call.body} lang={call.lang} />}
        {failed && diag.length > 0 && (
          <div className="fail-diag">
            <div className="fail-head"><span className="fail-lead">Failed</span> <span className="fail-first">{diag.find((l) => /^\s*(error|Error|FAIL|panic)/.test(l)) ?? diag.at(-1)}</span></div>
            <pre className="mono fail-cmd">{cmdText}</pre>
            <pre className="mono fail-out">{diag.map((l, i) => <span key={i} className={/^\s*(error:|Error)/.test(l) ? "fail-line fail-line-err" : "fail-line"}>{l}</span>)}</pre>
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
        {result && edits.length > 0 && (
          <div className="edit-files">{edits.map((f) => <FileEdit key={f.path} edit={f} session={opened ? session : undefined} turn={turnSeq} />)}</div>
        )}
        {result && !empty && (!failed || full || !diag.length) && (() => {
          // Edits the tools printed read as diffs; everything else keeps its columns.
          // An edit already a file row above is not said twice.
          const parts = outputParts(out).filter((p) => p.kind !== "edit" || !edits.some((f) => f.path === p.path));
          if (!parts.some((p) => p.kind === "edit")) {
            const text = parts.map((p) => (p.kind === "text" ? p.text : "")).join("\n").replace(/^\n+|\n+$/g, "");
            const files = !edits.length && readChunks(code.text, out);
            if (files) return <div className="tool-output">{files.map((f) => <Fragment key={f.path}><div className="mono read-file-head">{f.path}</div><Code text={f.text} lang={langForPath(f.path)} /></Fragment>)}</div>;
            return text.trim() ? <div className="tool-output"><Code text={edits.length ? text : out} lang={resultLang(result)} /></div> : null;
          }
          return <div className="tool-output">{parts.map((p, i) => p.kind === "edit" ? <EditDiff key={i} part={p} />
            : p.text.trim() ? <Code key={i} text={p.text.replace(/^\n+|\n+$/g, "")} lang={resultLang(result)} /> : null)}</div>;
        })()}
        {result && failed && diag.length > 0 ? null : result ? (
          <details className="block-inner">
            <summary><span className="block-label">{what}</span></summary>
            <Code text={call.body || call.raw} lang={call.body ? call.lang : "javascript"} />
          </details>
        ) : call.body !== call.raw && (
          <details className="block-inner">
            <summary><span className="block-label">Program</span></summary>
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

/** "Stop hooks" when every fire is one event, else "Hooks": the row says which part of the turn it belongs to. */
function hookEventLabel(fires: Line[]): string {
  const events = new Set(fires.map((l) => str(l.data?.event)).filter(Boolean));
  const [only] = events;
  return events.size === 1 && only ? `${only[0].toUpperCase()}${only.slice(1)} hooks` : "Hooks";
}

export function TurnHooks({ lines, load, save }: { lines: Line[]; load?: Load; save?: Save }) {
  // A fire that decided nothing, changed nothing and said nothing is not
  // news: the built-in rules hook runs after every result, so every turn
  // carried "1 fired" about a hook that did nothing. Those stay in the
  // Hooks view's ledger, which keeps every invocation.
  const fires = lines.filter((l) => l.kind === "hook" &&
    (str(l.data?.decision) || str(l.data?.error) || str(l.data?.notice) || (l.data?.output !== null && l.data?.output !== undefined)));
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
        <span className="block-label">{hookEventLabel(fires)}</span>{" "}
        <span className="block-detail">{parts.join(" · ")}</span>{" "}
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
function TurnFooter({ turn, fail, longest = 0, failedWork = 0, unknownSubs = 0, extra, edits, acts }: {
  turn: Turn; /** The turn's checkpoint diff, once read. */ edits?: Change[] | null; /** The result the turn's failure came from, when recorded. */ fail?: Line;
  /** The longest recorded run of the turn's jobs and subagents, so wall time never reads shorter than its work. */ longest?: number;
  failedWork?: number; unknownSubs?: number;
  /** The turn's own controls (Expand all), before the usage on the right. */ extra?: React.ReactNode;
  /** The answer's Copy and time, last on the line. */ acts?: React.ReactNode;
}) {
  const done = turn.done!;
  const u = usageOf(done);
  const files = Array.isArray(done.data?.files) ? (done.data!.files as string[]) : [];
  const exit = typeof done.data?.exit === "number" ? done.data.exit : fail?.data?.exit;
  const failed = typeof exit === "number" && exit !== 0;
  // A turn whose last word was an error (a 401, a retry that failed again) failed, whatever its exit.
  const errored = turn.body.filter((l) => !["system", "usage", "job", "hook", "meta"].includes(l.kind)).at(-1)?.kind === "error";
  const facts: string[] = [];
  const worked = turn.prompt?.at ? Math.max(Date.parse(done.at) - Date.parse(turn.prompt.at), longest) : 0;
  // Under a second is not a fact worth a slot ("Worked for 0s").
  if (worked >= 1000) facts.push("Worked for " + duration(worked));
  // What failed, said where the turn ends: a phone has no hover to read it from.
  // A native call is its own record: the command, and the output it kept.
  const native = fail && isNativeCall(fail);
  const failCmd = !fail ? "" : native ? str(fail.data?.cmd) || fail.text : gistOf(parseCall(str(fail.data?.code)).gist);
  const failBody = !fail ? "" : native ? str(fail.data?.output) : resultBody(fail);
  const failOut = failBody.split("\n").filter((l) => l.trim()).slice(-3);
  const failName = !fail ? "" : native ? (fail.data?.tool === "bash" ? failNameOf(`tools.bash(${JSON.stringify(failCmd)})`, failBody) : callVerb(str(fail.data?.tool))) || failCmd
    : failNameOf(str(fail.data?.code), resultBody(fail)) || failCmd;
  // The engine closed the turn with calls still running: they ran on as jobs, in Work.
  const stillRunning = typeof done.data?.running === "number" ? Math.max(0, done.data.running - (turn.settled ?? 0)) : 0;
  const work = useWork();
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
  // The engine's done names no model: its replies carry their own provenance.
  const model = str(done.data?.model) || str([...turn.body].reverse().find((l) => l.kind === "assistant" && str(l.data?.model))?.data?.model);
  const show = () => {
    const el = document.querySelector<HTMLElement>(`details.block[data-seq="${fail?.seq}"]`);
    if (!el) return;
    for (let d: HTMLElement | null = el; d; d = d.parentElement?.closest("details") ?? null) if (d instanceof HTMLDetailsElement) d.open = true;
    const head = el.querySelector<HTMLElement>("summary") ?? el;
    head.scrollIntoView({ block: "start" });
    head.focus({ preventScroll: true });
    head.classList.remove("turn-flash");
    void head.offsetWidth;
    head.classList.add("turn-flash");
    setTimeout(() => head.classList.remove("turn-flash"), 1700);
  };
  // The strip above owns session totals; a turn says what it took, with
  // its tokens on the price rather than as a third figure.
  // An older transcript recorded usage as its own line; the footer says it instead.
  const legacy = u ? "" : turn.body.find((l) => l.kind === "usage")?.text.replace(/^usage · /, "").replace(", ", " / ") ?? "";
  // Tokens ride on the cost's title; the cost alone sits muted on the right.
  const tokens = u ? `${tokenCount(u.in)} in / ${tokenCount(u.out)} out` : legacy;
  if (unknownSubs) facts.push(`${unknownSubs} ${unknownSubs === 1 ? "subagent" : "subagents"} unknown`);
  return (
    <div className="turn-foot">
      {failed && fail && shownOpen && !(turn.stopped || done.kind === "cancelled") ? (
        <span className="turn-fail-row">
          <span className="turn-failed">{failName || "Command"} failed · exit {exit}</span>
          <span aria-hidden="true">·</span>
          <button type="button" className="link turn-fail-jump" onClick={show}>Jump to command</button>
        </span>
      ) : (errored && !(turn.stopped || done.kind === "cancelled")) || cutAfterProse(turn) ? null : (
        // R3-F: a turn that ended on an error says so in the error itself, not again under it.
        <span className={"turn-outcome" + (turn.stopped || done.kind === "cancelled" ? " turn-stopped" : failed || failedWork ? " turn-failed" : "")}>
          {(turn.stopped || done.kind === "cancelled") && <StopMark />}
          {turn.stopped || done.kind === "cancelled" ? statusWord("stopped") : failed ? `${statusWord("done")} with a failed command · exit ${exit}` : statusWord("done") + (failedWork ? ` · ${failedWork} failed` : "")}
        </span>
      )}
      {facts.map((f) => <span key={f} className="num">{f}</span>)}
      {files.length > 0 && <TurnFiles files={files} turn={turn} edits={edits} />}
      {stillRunning > 0 && (
        <button type="button" className="link num turn-running" onClick={() => work?.openWork?.()} disabled={!work?.openWork}>
          {stillRunning === 1 ? "1 call still running" : `${stillRunning} calls still running`}
        </button>
      )}
      {extra}
      {(u?.cost !== undefined || tokens || model) && <span className="turn-foot-right">
        {u?.cost !== undefined ? <span className="num" title={tokens}>{money(u.cost)}</span> : tokens && <span className="num">{tokens}</span>}
        {model && <span className="mono" title={model}>{(u?.cost !== undefined || tokens) ? " · " : ""}{model.split("/").pop()}</span>}
      </span>}
      {failed && fail && !shownOpen && (
        <div className="turn-fail" role="note">
          <button type="button" className="link mono turn-fail-cmd" onClick={show}>{failCmd || "Show the failed command"}</button>
          {failOut.length > 0 && <pre className="mono">{failOut.join("\n")}</pre>}
        </div>
      )}
      {acts}
    </div>
  );
}


/** The input seq of the turn being drawn: a turn's edits are diffed from its own checkpoint. */
const TurnSeq = createContext<number | undefined>(undefined);

/** An engine call in flight: its live start event and the output streamed since (call-delta). */
export type NativeRun = { id: string; kind: string; tool: string; detail: string; at: string; tail: string; worker?: string };

/** The running native calls after one live event: a start adds its row, a call-delta extends its tail. */
export function liveNative(m: Map<string, NativeRun>, ev: LiveEvent): Map<string, NativeRun> {
  const id = typeof ev.extra?.id === "string" ? ev.extra.id : "";
  if (ev.kind === "call-delta") {
    const c = m.get(id);
    // Only the tail is ever shown; the record carries the whole output.
    return c ? new Map(m).set(id, { ...c, tail: (c.tail + ev.text).slice(-4000) }) : m;
  }
  if ((ev.kind === "call" || ev.kind === "sub:call") && id && ev.extra?.phase === "start") {
    const worker = typeof ev.extra.worker === "string" ? ev.extra.worker : undefined;
    return new Map(m).set(id, { id, kind: ev.kind, tool: String(ev.extra.tool ?? ""), detail: ev.text, at: ev.at, tail: "", worker });
  }
  // A call still running when its turn closed runs on as a job, in Work, not as a row of a finished turn.
  if (ev.kind === "done" || ev.kind === "cancelled") return m.size ? new Map() : m;
  return m;
}

/**
 * The transcript with an engine's running calls appended as unrecorded
 * call lines (phase "start"), so they take their place in the live turn
 * the way a recorded call does, and give way to that record by id.
 */
export function withRunningCalls(lines: Line[], running: Map<string, NativeRun>): Line[] {
  if (!running.size) return lines;
  const recorded = new Set(lines.filter(isNativeCall).map((l) => l.data!.id as string));
  const last = lines.length ? lines[lines.length - 1].seq : 0;
  const extra = [...running.values()].filter((c) => !recorded.has(c.id)).map((c, i): Line => ({
    // Between the last record and the next: never a seq a record can take.
    seq: last + (i + 1) / 1000, at: c.at, kind: c.kind, text: c.detail,
    data: { id: c.id, tool: c.tool, phase: "start", tail: c.tail, ...(c.worker ? { worker: c.worker } : {}) },
  }));
  return extra.length ? [...lines, ...extra] : lines;
}

/** The call in flight: the tools plugin's live "call" start event (never recorded), until its end or the block's result lands. */
export type RunningCall = { kind: string; tool: string; detail: string; at: string };
export const RunningCallCtx = createContext<RunningCall | null>(null);

/** The thread's one read of its edits and working tree, shared by the header chip and every turn's files. */
const unread = { files: null, repo: true, failed: false };
export const SessionChanges = createContext<ReturnType<typeof useChanges> | null>(null);

/**
 * "3 files +7 −2": the turn's changed files, opening the Changes review
 * on this turn's own edits. The counts are the turn's calls (a write is
 * all additions), the review's are its checkpoints.
 */
export function TurnFiles({ files, turn, edits: diff }: { files: string[]; turn: Turn; /** The turn's checkpoint diff: the same counts as the header, shell edits included. */ edits?: Change[] | null }) {
  const id = useWork()?.session ?? "";
  const calls = useMemo(() => [...callEdits(turn.body.filter((l) => l.kind === "code").map((l) => l.text).join("\n")), ...nativeEdits(turn.body)], [turn.body]);
  const edits = diff?.length ? diff : calls;
  const add = edits.reduce((n, f) => n + Math.max(0, f.add), 0), del = edits.reduce((n, f) => n + Math.max(0, f.del), 0);
  const text = <>
    {files.length} {files.length === 1 ? "file" : "files"}
    {add > 0 && <span className="num rt-add">+{add}</span>}
    {del > 0 && <span className="num rt-del">−{del}</span>}
  </>;
  if (!id || !turn.prompt) return <span className="turn-files">{text}</span>;
  const href = `#/s/${id}/changes?turn=${turn.prompt.seq}`;
  return <button type="button" className="link turn-files" data-href={href} title={files.join("\n")} onClick={() => { location.hash = href.slice(1); }}>{text}</button>;
}

/**
 * The session's running budget, always in view: what the context holds
 * against the model's window, what the session has spent, and the model
 * actually answering. The window is only named when the session records
 * its model; a default model is not guessed at.
 */
/**
 * The Portal button, carrying what the pane behind it would say. A live portal
 * is a running thing, so it gets the accent dot and the port. No live portal is
 * the resting state: the word alone, no dot, no count.
 */
export function PortalButton({ ports, onClick }: { ports: number[]; onClick: () => void }) {
  const n = ports.length;
  const label = n === 0 ? null : n === 1 ? String(ports[0]) : `${n} ports`;
  return (
    <button className="btn head-ack head-portal" onClick={onClick}
            title={n === 0 ? "Show a server running inside this session’s orb. Nothing is listening yet."
              : n === 1 ? `A server is listening on port ${ports[0]} in this session’s orb`
              : `${n} servers are listening in this session’s orb`}
            aria-label={n === 0 ? "Portal — nothing is listening yet"
              : n === 1 ? `Portal — port ${ports[0]} is live`
              : `Portal — ${n} ports are live`}>
      {n > 0 && <span className="work-dot work-dot-accent" aria-hidden="true" />}
      <span>Portal</span>
      {label && <><span className="work-sep" aria-hidden="true"> · </span><span className="head-portal-port mono">{label}</span></>}
    </button>
  );
}

function RuntimeStrip({ row, lines, paused, onRetry, onContext, work, actions, loading, failed, cat }: { row: Row; /** MB-ERR: the transcript failed to load, so no loading chrome. */ failed?: boolean; lines: Line[]; paused?: number; onRetry?: () => void; onContext?: () => void; /** The Work button, after the metrics. */ work?: React.ReactNode; /** Header actions (Stop orb, Mark seen), after Work. */ actions?: React.ReactNode; loading?: boolean; /** The thread's model catalogue, read once for the strip and the pickers. */ cat: Catalogue | null }) {
  const limits = useMemo(() => {
    const m: Record<string, number> = {};
    for (const p of cat?.providers ?? []) for (const x of p.models ?? []) if (x.context) m[x.id] = x.context;
    return m;
  }, [cat]);
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
  // R3-G: the metrics fold into one "…", cache first, then tests, cost,
  // edits and context, until the title group (status and all) and the strip
  // each fit their room. A phone folds them all. A new pane width starts over.
  const [fold, setFold] = useState(0);
  // The chip's tab lives here: a fold (a phone, a narrow pane) moves the
  // chip under "Details" and back, which mounts it anew each time.
  const [chgScope, setChgScope] = useState<Scope>("session");
  const recheck = useRef<() => void>(() => {});
  // A fold that frees no width fires no resize, so each fold checks again.
  useEffect(() => { const id = requestAnimationFrame(() => recheck.current()); return () => cancelAnimationFrame(id); }, [fold]);
  useEffect(() => {
    const el = strip.current, head = el?.parentElement, main = head?.querySelector<HTMLElement>(".head-main");
    if (!el || !head || !main || typeof ResizeObserver === "undefined") return;
    const check = () => {
      if (window.matchMedia?.("(max-width:720px)").matches) return setFold(5);
      // An open popover's content widens the strip, and a fold would move
      // its chip under "Details", shutting what the person is reading.
      // Its close checks again (the toggle listener below).
      if (el.querySelector("details[open]")) return;
      const h1 = main.querySelector("h1");
      // R4-F: the metrics row does not wrap, so it can run under the actions
      // without the strip itself overflowing; that counts as over too.
      const metrics = el.querySelector<HTMLElement>(".rt-metrics"), acts = el.querySelector<HTMLElement>(".rt-actions");
      const last = metrics?.lastElementChild?.getBoundingClientRect();
      const collide = !!(metrics && (metrics.scrollWidth > metrics.clientWidth + 1 ||
        (last && acts && acts.offsetTop === metrics.offsetTop && last.right > acts.getBoundingClientRect().left)));
      const over = collide || main.scrollWidth > main.clientWidth + 1 || el.scrollWidth > el.clientWidth + 1 || (h1 && h1.clientWidth < Math.min(h1.scrollWidth, 160));
      if (over) setFold((f) => Math.min(f + 1, 5));
    };
    recheck.current = check;
    let w = head.clientWidth;
    const ro = new ResizeObserver(() => {
      if (head.clientWidth !== w) { w = head.clientWidth; setFold(0); }
      requestAnimationFrame(check);
    });
    ro.observe(head); ro.observe(main); ro.observe(el);
    el.querySelectorAll(":scope>.rt-metrics,:scope>.rt-actions").forEach((n) => ro.observe(n));
    const toggled = () => requestAnimationFrame(check);
    el.addEventListener("toggle", toggled, true);
    return () => { ro.disconnect(); el.removeEventListener("toggle", toggled, true); };
  }, []);
  const cache = row.cache && <CacheChip key="cache" cache={row.cache} model={row.model} />;
  const tests = <TestsChip key="tests" lines={lines} running={row.status === "running"} />;
  const edits = failed ? null : <ChangesChip key="edits" row={row} scope={chgScope} onScope={setChgScope} />;
  // Cache, changes and tests stand on their own: a session with no usage
  // recorded can still have a server running.
  const context = (() => {
    // The reading is the way into Context; there is no second button for
    // it, and it stays when nothing was reported so the way in does too.
    const tip = !u ? "No input tokens have been reported for this session"
      : limit ? `${u.lastIn.toLocaleString()} of ${limit.toLocaleString()} tokens, read by ${row.model}`
      : !row.model ? "No model is recorded for this session, so headroom is not known"
      : switched ? "The model changed after this input was read; headroom shows once the new model answers"
      : `${row.model} has no context window in the catalogue`;
    // Until the transcript arrives there is nothing to report yet; after,
    // a session that never reports shows one quiet dash, not a label.
    if (failed) return null;
    if (loading && !u) return <span key="context" className="rt rt-loading" aria-label="Loading usage"><span className="rt-label">Context</span><span className="rt-skel" /></span>;
    // No value, no chip: the Context view (palette, settings sheet) still names why.
    if (!u) return null;
    const body = <>
      <span className="rt-label">Context</span>
      <span className={"num rt-value" + (u ? "" : " rt-stale")}>{!u ? "—" :`${tokenCount(u.lastIn)}${limit ? ` · ${tokenCount(Math.max(0, limit - u.lastIn))} left` : ""}`}</span>
      {pct !== undefined && (
        <span className="rt-bar" role="meter" aria-label="Context used" aria-valuenow={pct} aria-valuemin={0} aria-valuemax={100}>
          <span className={pct >= 80 ? "rt-hot" : pct >= 60 ? "rt-warm" : undefined} style={{ width: `${pct}%` }} />
        </span>
      )}
    </>;
    return onContext
      ? <button key="context" type="button" className="rt rt-link" title={`${tip} · open Context`} aria-label={`Context: ${tip}`} onClick={onContext}>{body}</button>
      : <Tip key="context" tip={tip}>{body}</Tip>;
  })();
  const cost = u?.cost !== undefined && (
    <Tip key="cost" tip={`Session cost ${money(u.cost)} · ${u.in.toLocaleString()} tokens in · ${u.out.toLocaleString()} out`}>
      <span className="rt-label">Cost</span><span className="num rt-value">{money(u.cost)}</span>
    </Tip>
  );
  const folded = [fold >= 5 && context, fold >= 4 && edits, fold >= 3 && cost, fold >= 2 && tests, fold >= 1 && cache].filter(Boolean);
  // Two groups on the right: the metrics (folding behind one "…"), then the
  // actions, which never fold.
  return (
    <div className="runtime-strip" ref={strip}>
      {paused !== undefined && (
        <span className="rt-paused" role="status">
          <span className="rt-paused-word">Updates paused</span> · last synced {clock(new Date(paused).toISOString())} · <button className="link" onClick={onRetry}>Retry</button>
        </span>
      )}
      <div className="rt-metrics">
        {fold < 5 && context}
        {fold < 3 && cost}
        {fold < 4 && edits}
        {fold < 2 && tests}
        {fold < 1 && cache}
        {folded.length > 0 && (
          <details className="rt rt-jobs rt-more">
            <summary aria-label="More session details">
              <svg width="16" height="16" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true"><circle cx="5" cy="12" r="1.8" /><circle cx="12" cy="12" r="1.8" /><circle cx="19" cy="12" r="1.8" /></svg>
              <span className="rt-more-word">Details</span>
            </summary>
            <div className="rt-pop">{folded}</div>
          </details>
        )}
      </div>
      {(work || actions) && <div className="rt-actions">{work}{actions}</div>}
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
      if (d.open) for (const o of open()) if (o !== d && !o.contains(d)) o.open = false;
      if (d.open) clampToViewport(d.querySelector<HTMLElement>(":scope>.rt-pop"));
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
export function ChangesChip({ row, scope, onScope }: { row: Row; scope: Scope; onScope: (s: Scope) => void }) {
  const data = useContext(SessionChanges) ?? { session: unread, tree: unread, turn: undefined, turnSeq: undefined, retry: () => {} };
  const phone = useMedia("(max-width:720px)");
  // The chip is the one place that names a missing repository; the body's tabs show a dash.
  const noRepo = (r: typeof data.session) => (r.files !== null && !r.repo ? { text: "No Git repository", quiet: true } : null);
  const c: ReturnType<typeof countOf> = noRepo(data.session) ?? countOf(data.session);
  const t: ReturnType<typeof countOf> = noRepo(data.tree) ?? countOf(data.tree);
  const href = `#/s/${row.id}/changes`;
  // R4-F: nothing to count reads as one phrase, not "Session edits None".
  const none = c.text === "None";
  // A failed refresh keeps the last list: say so with no edits too.
  const stale = data.session.failed || data.tree.failed ? ", stale" : "";
  const aria = none ? `No edits. Working tree: ${t.text}${stale}` : `Session edits: ${c.text}${c.add !== undefined ? `, ${c.add} added, ${c.del} removed` : ""}. Working tree: ${t.text}${stale}`;
  // A local session outside a checkout has no number to show: two words, not a sentence at value weight.
  const body = c.quiet && c.text === "No Git repository" ? <span className="rt-label" title="No Git repository">No repo</span> : none ? <span className="rt-label">No edits</span> : <>
    <span className="rt-label">Edits</span>
    <span className={"num rt-value" + (c.quiet ? " rt-stale" : "")}>{c.text}{c.add !== undefined && <> <span className="rt-add">+{c.add}</span> <span className={"rt-del" + (c.del ? "" : " rt-zero")}>−{c.del}</span></>}</span>
  </>;
  // A failed read is no value: no chip, rather than "Unavailable".
  if (data.session.files === null && data.session.failed) return null;
  if (phone && none) return <span className="rt" aria-label={aria}>{body}</span>;
  if (phone) return <a className="rt rt-link" href={href} aria-label={aria}>{body}</a>;
  return (
    <details className="rt rt-jobs">
      <summary aria-label={aria}>{body}</summary>
      <div className="rt-pop rt-diff">
        <ChangesBody row={row} data={data} scope={scope} onScope={onScope} />
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
  if (!last || last.state === "unrecorded") return null;
  const failed = last.state === "failed";
  const word = { passed: "passed", failed: "failed", running: "running…", unrecorded: "not recorded" }[last.state];
  return (
    // The chip is a way to the evidence, not a second copy of it: it opens
    // the call in the transcript (and the run folding it) and lands there.
    <button className="rt rt-link" title={`${last.cmd}${last.exit !== undefined ? ` · exit ${last.exit}` : ""} · ${new Date(last.at).toLocaleString()} · show in transcript`}
            onClick={() => {
              const el = document.querySelector<HTMLElement>(`details.block[data-seq="${last.seq}"]`);
              if (!el) return;
              for (let d: HTMLElement | null = el; d; d = d.parentElement?.closest("details") ?? null) if (d instanceof HTMLDetailsElement) d.open = true;
              el.scrollIntoView({ block: "center" });
              (el.querySelector<HTMLElement>("summary,button") ?? el).focus();
            }}>
      <span className="rt-label">Tests</span>
      <span className={"num rt-value " + (failed ? "rt-test-failed" : last.state === "passed" ? "rt-test-passed" : "rt-stale")}>{failed && <span aria-hidden="true" className="rt-mark"><StatusMark status="error" bare /></span>}{last.state === "passed" && <span aria-hidden="true" className="rt-mark">✓</span>}{word}</span>
      {(last.state === "passed" || last.state === "failed") && <span className="num rt-label">· {ago(last.at)} ago</span>}
    </button>
  );
}

/** How long the running turn has taken, counted from its prompt. */
function RunClock({ since }: { since: string }) {
  const now = useNow(true);
  return <span className="num head-clock">{elapsed(Math.max(0, now - Date.parse(since)))}</span>;
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
      <span className="rt-label">Cache</span>
      <span className="num rt-value"><span className="rt-warm-dot" aria-hidden="true" />
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
  const body = useRef<HTMLSpanElement>(null);
  // Kept inside the viewport: shifted left or right by what would overflow, 8px from each edge.
  const clamp = () => requestAnimationFrame(() => {
    const b = body.current;
    if (!b) return;
    b.style.translate = "";
    const r = b.getBoundingClientRect();
    if (!r.width) return;
    const shift = r.right > innerWidth - 8 ? innerWidth - 8 - r.right : r.left < 8 ? 8 - r.left : 0;
    if (shift) b.style.translate = `${Math.round(shift)}px 0`;
  });
  return (
    <button type="button" className={"rt rt-tip" + (className ? " " + className : "")} aria-label={label} aria-describedby={id}
            aria-expanded={held === "pin"} data-tip={held ?? undefined} onMouseEnter={clamp} onFocus={clamp}
            onClick={() => { setHeld((h) => h === "pin" ? "shut" : "pin"); clamp(); }} onBlur={() => setHeld(null)} onMouseLeave={() => setHeld((h) => h === "shut" ? null : h)}
            onKeyDown={(e) => { if (e.key === "Escape" && held !== "shut") { e.stopPropagation(); setHeld("shut"); } }}>
      {children}
      <span className="rt-tip-body" role="tooltip" id={id} ref={body}>{tip}</span>
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

/**
 * Which work segments are open, per session and segment first seq, for
 * as long as the page is. A "show work expanded" preference would be the
 * default this falls back to: WorkSegmentRow's `defaultOpen` ORed with it.
 */
const segOpen = new Map<string, boolean>();

/**
 * One stretch of work between replies as one row. Opened, it holds exactly
 * the rows it folded. It is a native details element, so a jump that opens
 * every details around its target opens this one too.
 */
/** MB-TR: "+12 −3" on a folded segment, from the edits its own calls made; a zero side is left out. */
function WorkEdits({ seg }: { seg: Extract<Segment, { kind: "work" }> }) {
  const lines = seg.items.flatMap((it) => it.kind === "tools" ? it.lines : it.kind === "line" ? [it.line] : []);
  const code = lines.filter((l) => l.kind === "code").map((l) => l.text).join("\n");
  // Native calls carry their own counts; keyed by seq, as the lines are a fresh array every render.
  const nativeKey = lines.filter(isNativeCall).map((l) => l.seq).join(",");
  const edits = useMemo(() => [...callEdits(code), ...nativeEdits(lines)], [code, nativeKey]); // eslint-disable-line react-hooks/exhaustive-deps
  const add = edits.reduce((n, f) => n + f.add, 0), del = edits.reduce((n, f) => n + f.del, 0);
  if (!add && !del) return null;
  return <span className="num work-seg-edits">{add > 0 && <span className="rt-add">+{add}</span>}{del > 0 && <span className="rt-del">−{del}</span>}</span>;
}

function WorkSegmentRow({ seg, session, defaultOpen, running, since, step, all, children }: {
  seg: Extract<Segment, { kind: "work" }>; session: string; defaultOpen: boolean;
  /** The last segment of a live turn: a spinner, the turn's timer and the current step. */
  running?: boolean; since?: string; step?: string;
  all?: { open: boolean; at: number } | null; children: React.ReactNode;
}) {
  const key = session + ":" + seg.seq;
  const [open, setOpen] = useState(() => segOpen.get(key) ?? defaultOpen);
  const box = useRef<HTMLDetailsElement>(null);
  useEffect(() => {
    if (!all) return;
    segOpen.set(key, all.open); setOpen(all.open);
    // Expand all reaches the grouped calls inside too, so a failing sub-command shows without another click.
    box.current?.querySelectorAll<HTMLDetailsElement>(".work-seg-body details.toolrun").forEach((d) => { d.open = all.open; });
  }, [all]); // eslint-disable-line react-hooks/exhaustive-deps
  return (
    <details ref={box} className={"block thin work-seg" + (running ? " work-seg-live" : "")} open={open} data-open-key={"seg:" + seg.seq}
             onToggle={(e) => { if (e.target !== e.currentTarget) return; const o = e.currentTarget.open; segOpen.set(key, o); setOpen(o); }}>
      <summary role="button" aria-expanded={open}>
        {running && (
          <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2"
               strokeLinecap="round" className="spin-mark" aria-hidden="true"><circle cx="12" cy="12" r="8.5" strokeDasharray="40 14" /></svg>
        )}
        <span className="block-label">{running ? "Working" : workHeadline(seg)}</span>
        {running && since && <span className="num work-seg-time"><Elapsed since={since} /></span>}
        {running && step && <span className="mono block-detail work-seg-step" title={step}>{step}</span>}
        {!running && <WorkEdits seg={seg} />}
        {seg.failed > 0 && <span className="num toolrun-failed"><FailMark />{seg.failed} failed</span>}
      </summary>
      <div className="work-seg-body">{children}</div>
    </details>
  );
}

/** What woke the engine, said as what happened: its own input is an instruction to the model, not something you typed. */
export function wakeLabel(prompt: Line): string {
  const d = prompt.data ?? {};
  if (d.reason === "heartbeat") return "The agent checked on its running calls";
  if (d.reason === "notice") return "A background job finished while the agent was idle";
  const n = Array.isArray(d.calls) ? d.calls.length : typeof d.calls === "number" ? d.calls : 1;
  return n > 1 ? `${n} background calls finished` : "A background call finished";
}

export function TurnView({ turn, tail, n, working, superseded }: { turn: Turn; tail?: React.ReactNode; /** 1-based position, so the turn log can land on it. */ n?: number;
  /** A later turn has ended, so this one can no longer be running. */ superseded?: boolean;
  /** The live turn's activity ("Thinking", "Running go test"): its working row says it, once. */ working?: string }) {
  const ctx = useWork();
  const editPrompt = useContext(EditPrompt);
  const codes = turn.body.filter((l) => l.kind === "code" || l.kind === "sub:code").map((l) => l.text);
  const hooks = useMemo(() => turn.body.filter(isHookLine), [turn.body]);
  const items = useMemo<Item[]>(
    // A finished turn's usage line is the footer's to say, on its one line.
    // An engine's spawn call that went fine is told by the subagent card it made; its own row would say the task twice.
    () => groupTools(groupSubs(foldModelSwitch(foldRetries(turn.body.filter((l) => !isHookLine(l) && !(turn.done && l.kind === "usage")
      && !(isNativeCall(l) && l.data?.tool === "spawn" && !callFailed(l) && !callRunning(l) && turn.body.some((s) => s.kind === "sub:start")))))), codes),
    // codes is derived from turn.body on every render.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [turn.body]);
  // Skills, @files and big pastes ride along with the prompt: each is a chip (see parsePrompt).
  const { raw, said, plain, images, atts } = parsePrompt(turn.prompt?.text ?? "");
  // A turn a finished background job started is not something you typed:
  // its prompt is the loop's instruction to the model plus the job notes.
  const wakeJobs = useMemo(() => {
    const notes = turn.prompt ? jobWakeNotes(raw) : null;
    if (!notes?.length || !turn.prompt) return null;
    const at = turn.prompt.at;
    return jobsFromLines(notes.map((text, i) => ({ seq: turn.prompt!.seq * 1000 + i, at, kind: "job", text })), "", false);
  }, [turn.prompt, raw]);
  // The same wake-up, started by a background agent's finish note.
  const wakeAgents = useMemo(() => {
    const notes = turn.prompt ? agentWakeNotes(raw) : null;
    return notes?.length ? notes.map((text, i): Line => ({ seq: turn.prompt!.seq * 1000 + 500 + i, at: turn.prompt!.at, kind: "job", text })) : null;
  }, [turn.prompt, raw]);
  // A turn that ended on a failed command opens that command, and only that one:
  // an earlier failure the agent went on to fix is history, still counted.
  const exit = turn.done?.data?.exit;
  const bad = (l?: Line) => typeof l?.data?.exit === "number" && l.data.exit !== 0;
  // A done that recorded no exit still ended on a failure when its last result failed.
  const lastResult = [...turn.body].reverse().find((l) => l.kind === "result");
  const resultFail = !turn.done || turn.stopped ? undefined
    : typeof exit === "number" ? (exit !== 0 ? [...turn.body].reverse().find((l) => l.kind === "result" && bad(l)) : undefined)
    : bad(lastResult) ? lastResult : undefined;
  // On the engine a call is the unit: the turn ended on the last native call that failed
  // (a bash one when the done names an exit). A cancelled call is a stop, not a failure.
  const nativeLast = !turn.done || turn.stopped ? undefined
    : typeof exit === "number" ? (exit !== 0 ? [...turn.body].reverse().find((l) => isNativeCall(l) && bad(l)) : undefined)
    : [...turn.body].reverse().find(isNativeCall);
  const fail = resultFail ?? (nativeLast && callFailed(nativeLast) && nativeLast.data?.canceled !== true ? nativeLast : undefined);
  // A turn the engine opened itself: a background call finished, or it checked on its running calls.
  const wake = turn.prompt?.data?.wake === true;
  const [full, setFull] = useState(false);
  // R3-F: the turn's checkpoint diff, read once it is done, so its group and footer count what the header counts.
  const [turnEdits, setTurnEdits] = useState<Change[] | null>(null);
  const hasFiles = Array.isArray(turn.done?.data?.files) && (turn.done!.data!.files as string[]).length > 0;
  useEffect(() => {
    if (!hasFiles || !ctx?.session || !turn.prompt) return;
    let live = true;
    api.edits(ctx.session, turn.prompt.seq).then((r) => { if (live) setTurnEdits(r.files); }, () => {});
    return () => { live = false; };
  }, [hasFiles, ctx?.session, turn.prompt?.seq]); // eslint-disable-line react-hooks/exhaustive-deps
  const long = plain.length > 420 || plain.split("\n").length > 4;
  const { nodes: said2, loose } = promptNodes(said, atts);
  // No done and nothing live to write one (the session stopped, or a later
  // turn began): the turn was cut off, and its open rows stop ticking.
  // A slash command (/model) writes no done: it was never a turn to cut off.
  const commandOnly = !turn.prompt && turn.body.every((l) => l.kind === "command" || l.kind === "system");
  const cut = !turn.done && !commandOnly && Boolean(superseded || (ctx && !ctx.live));
  // An ended turn's answer keeps its actions on the footer line, so no blank band sits under the prose.
  const answer = turn.done || cut ? [...turn.body].reverse().find((l) => l.kind === "assistant" && !blank(answerBody(l.text, codes))) : undefined;
  const answerActs = answer && <AnswerActs line={answer} body={answerBody(answer.text, codes)} />;
  const live = !turn.done && !turn.stopped && !cut;
  const segs = useMemo(() => splitWork(items, codes, live && (ctx?.live ?? true)),
    // codes is derived from items' turn.body.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [items, live, ctx?.live]);
  const works = segs.filter((sg): sg is Extract<Segment, { kind: "work" }> => sg.kind === "work");
  // The turn's working indicator is its last stretch of work, when nothing was said after it.
  const lastWork = works[works.length - 1];
  const runningSeg = live && lastWork?.last ? lastWork : undefined;
  const [allSegs, setAllSegs] = useState<{ open: boolean; at: number } | null>(null);
  const folds = works.filter((sg) => sg.rows >= 2).length;
  const renderItem = (it: Item, i: number, list: Item[]): React.ReactNode => {
    if (it.kind === "sub") return <SubRun key={"sub" + it.seq} agents={it.agents} seq={it.seq} turn={turn} live={(ctx?.live ?? true) && live} />;
    if (it.kind === "tools") {
      // A spawn's result is its card below: the program reads as that card's state.
      const next = list[i + 1];
      const spawned = next?.kind === "sub" ? subagentsFromTurn(turn, "", live).find((w) => w.subrunSeq === next.seq && w.result) : undefined;
      // Only the turn's one group can own its whole diff.
      const sole = items.filter((x) => x.kind === "tools").length === 1;
      return <ToolRun key={"tools" + it.seq} lines={it.lines} codes={codes} live={live} spawned={spawned} turnEdits={sole ? turnEdits : undefined} stopped={turn.stopped || turn.done?.kind === "cancelled" || cut} failSeq={fail?.seq} />;
    }
    if (it.line.kind.startsWith("todo/")) {
      // Consecutive todo records fold into one row, rendered at the first.
      const prev = list[i - 1];
      if (prev?.kind === "line" && prev.line.kind.startsWith("todo/")) return null;
      const run: Line[] = [];
      for (let j = i; j < list.length; j++) { const x = list[j]; if (x.kind !== "line" || !x.line.kind.startsWith("todo/")) break; run.push(x.line); }
      return <TodoRun key={"todo" + it.seq} lines={run} live={(ctx?.live ?? true) && live} />;
    }
    if (it.line.kind === "job") {
      // Consecutive job rows share one head, rendered at the first of them.
      const prev = list[i - 1];
      if (prev?.kind === "line" && prev.line.kind === "job") return null;
      const run: Line[] = [];
      for (let j = i; j < list.length; j++) { const x = list[j]; if (x.kind !== "line" || x.line.kind !== "job") break; run.push(x.line); }
      return <JobLines key={"jobs" + it.seq} lines={run} render={(x) => <Entry line={x} codes={codes} />} />;
    }
    return <Entry key={it.seq} line={it.line} codes={codes}
                  until={it.line.kind === "thinking" ? turn.body[turn.body.findIndex((l) => l.seq === it.line.seq) + 1]?.at ?? turn.done?.at : undefined} />;
  };
  return (
    <section className="turn" data-turn={n}>
      {turn.prompt && wakeAgents && (
        <div className="job-wake" role="note">
          <p className="meta-line">
            {wakeAgents.length === 1 ? "A background agent finished" : `${wakeAgents.length} background agents finished`} while the agent was idle · {when(turn.prompt.at)}
          </p>
          {wakeAgents.map((l) => <Entry key={l.seq} line={l} codes={codes} />)}
        </div>
      )}
      {turn.prompt && wakeJobs && (
        <div className="job-wake" role="note">
          <p className="meta-line">
            {wakeJobs.length === 1 ? "A background job finished" : `${wakeJobs.length} background jobs finished`} while the agent was idle · {when(turn.prompt.at)}
          </p>
          {wakeJobs.map((w) => <JobRow key={w.key} w={w} />)}
        </div>
      )}
      {turn.prompt && wake && !wakeJobs && !wakeAgents && (
        <div className="job-wake call-wake" role="note">
          <p className="meta-line"><span aria-hidden="true">↻ </span>{wakeLabel(turn.prompt)} · {when(turn.prompt.at)}</p>
        </div>
      )}
      {turn.prompt && !wake && !wakeJobs && !wakeAgents && (
        <div className="prompt">
          <div className="prompt-text prompt-bubble">
            {/* A long brief (pasted logs, a spec) is evidence, not the
                thing to navigate by: four lines, and one click for the rest. */}
            {said.trim() || images.length || atts.length
              ? <p className={long && !full ? "prompt-clamp" : undefined}>{said2}</p>
              : <p className="meta-line">(empty message)</p>}
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
            {loose.length > 0 && <div className="prompt-atts">{loose.map((a, i) => <AttChip key={i} att={a} />)}</div>}
          </div>
          <div className="msg-acts prompt-acts">
            <span className="num prompt-time" title={new Date(turn.prompt.at).toLocaleString()}>{when(turn.prompt.at)}</span>
            <CopyButton text={raw} what="prompt" />
            <button type="button" className="copy-btn prompt-edit" disabled={!editPrompt || editPrompt.busy}
                    aria-label="Edit into composer"
                    title={editPrompt?.busy ? "Send or clear the current draft first" : "Edit into composer"}
                    onClick={() => editPrompt?.edit(raw)}><svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true"><path d="M4 20h4L18.5 9.5a2.83 2.83 0 0 0-4-4L4 16v4z" /><path d="M13.5 6.5l4 4" /></svg></button>
          </div>
        </div>
      )}
      <RetryTurn.Provider value={turn.prompt && editPrompt?.retry ? () => editPrompt.retry?.(turn.prompt!.text) : null}>
      <FootAnswer.Provider value={answer?.seq}>
      <TurnSeq.Provider value={turn.prompt?.seq}>
      <div className="turn-body">
        {segs.map((sg) => {
          if (sg.kind !== "work") return <Fragment key={"i" + sg.item.seq}>{renderItem(sg.item, 0, [sg.item])}</Fragment>;
          const rows = sg.items.map((it, i) => <Fragment key={"i" + it.seq}>{renderItem(it, i, sg.items)}</Fragment>);
          // Only while the thread says the turn is working: a turn waiting on your answer is not.
          const running = live && working !== undefined && sg === runningSeg;
          if (!running && sg.rows < 2) return rows;
          // R4-D: once the last call's result landed its row is done; a lagging activity label must not say it still runs.
          const tip = sg.items.at(-1);
          const done = (l?: Line) => l?.kind === "result" || Boolean(l && isNativeCall(l) && !callRunning(l));
          const settled = tip?.kind === "tools" ? done(tip.lines.at(-1)) : tip?.kind === "line" && done(tip.line);
          const step = working && working !== "Working" && working !== WAITING_MODEL && working !== "Thinking" && !settled ? working : sg.step;
          return (
            <WorkSegmentRow key={"seg" + sg.seq} seg={sg} session={ctx?.session ?? ""} all={allSegs}
                            defaultOpen={Boolean(fail && sg.last && (sg.seqs.includes(fail.seq) || sg.failed > 0))}
                            running={running} since={turn.prompt?.at} step={step}>
              {rows}
            </WorkSegmentRow>
          );
        })}
        {tail}
        {/* R4-C: a stopped turn marks the cut right where its prose ends, not only in the footer. */}
        {cutAfterProse(turn) && (
          <p className="turn-interrupted">
            <StopMark />
            Interrupted
          </p>
        )}
        {/* No stretch of work to carry it (the turn opened on a reply, or has said nothing yet). */}
        {working === WAITING_MODEL && !runningSeg && live ? <WaitingModel since={turn.prompt?.at} /> : working !== undefined && !runningSeg && live && <Working label={working === "Thinking" ? working : "Working"}>{turn.prompt?.at && <Elapsed since={turn.prompt.at} />}</Working>}
        <TurnHooks lines={hooks} />
      </div>
      </TurnSeq.Provider>
      </FootAnswer.Provider>
      </RetryTurn.Provider>
      {turn.done && (() => {
        // The turn's own workers: what ran inside its span of entries.
        const seqs = turn.body.map((l) => l.seq);
        const lo = Math.min(...seqs), hi = Math.max(...seqs);
        const mine = (ctx?.workers ?? []).filter((w) => (w.subrunSeq ?? w.seq) >= lo && (w.subrunSeq ?? w.seq) <= hi);
        return <TurnFooter turn={turn} fail={fail} edits={turnEdits} longest={Math.max(0, ...mine.map((w) => w.ms ?? 0))}
                           failedWork={mine.filter((w) => w.life === "failed").length}
                           unknownSubs={mine.filter((w) => w.kind !== "job" && w.life === "unknown").length}
                           acts={answerActs}
                           extra={folds >= 2 && (
                             <button type="button" className="link turn-seg-all" onClick={() => setAllSegs({ open: !allSegs?.open, at: Date.now() })}>
                               {allSegs?.open ? "Collapse all" : "Expand all"}
                             </button>
                           )} />;
      })()}
      {/* No done and nothing live to write one: the turn was cut off, and says so where it ends, like Stopped. */}
      {cut && (
        <div className="turn-foot">
          {/* R3-C: one stop word and one duration phrase, as a stopped turn's footer has. */}
          <span className="turn-outcome turn-stopped"><StopMark />{statusWord("stopped")}</span>
          {turn.prompt?.at && (() => {
            const end = turn.body[turn.body.length - 1]?.at ?? turn.prompt.at;
            const ms = Date.parse(end) - Date.parse(turn.prompt.at);
            return ms >= 1000 && <span className="num">Worked for {duration(ms)}</span>;
          })()}
          {answerActs}
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

/**
 * The live reply after one delta event: a fragment extends the run of its
 * kind or starts the next; the engine's delta-reset (a retry, or a newer
 * request replacing the one that streamed) clears it, since what was shown
 * was never said. The first `sealed` runs belong to an entry already
 * recorded and go when its refetch lands, so a fragment never extends
 * one: it would go with them.
 */
export function streamAfter(prev: DeltaRun[], ev: { kind: string; text: string }, sealed = 0): DeltaRun[] {
  if (ev.kind === "delta-reset") return prev.length ? [] : prev;
  const kind = ev.kind === "thinking-delta" ? "thinking" : "assistant";
  const n = prev.length;
  if (n > sealed && prev[n - 1].kind === kind) {
    const next = prev.slice();
    next[n - 1] = { kind, text: next[n - 1].text + ev.text };
    return next;
  }
  return [...prev, { kind, text: ev.text }];
}

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
          <Markdown text={r.text} live={i === runs.length - 1} />
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

type Catalogue = { providers: ProviderInfo[]; efforts: string[]; /** Serve's own llm row; a session's is its row's `configured`. */ default?: { plugin: string; model: string; effort?: string } };

/** GET /api/models, once per caller; `enabled` false reads nothing (the caller was handed one). */
function useCatalogue(enabled = true) {
  const [cat, setCat] = useState<Catalogue | null>(null);
  const [failed, setFailed] = useState(false);
  const [nonce, setNonce] = useState(0);
  useEffect(() => {
    if (!enabled) return;
    setFailed(false);
    fetch("/api/models").then((r) => r.json()).then(setCat).catch(() => { setCat(null); setFailed(true); });
  }, [nonce, enabled]);
  const retry = useCallback(() => setNonce((n) => n + 1), []);
  return useMemo(() => ({ cat, failed, retry }), [cat, failed, retry]);
}

export function Controls({ row, projects, onModel, onEffort, onAssign, only, catalogue, disabled = false }: {
  row: Row; projects: Project[];
  onModel: (m: string, plugin?: string) => Promise<boolean> | void; onEffort: (e: string) => Promise<boolean> | void; onAssign: (p: string) => void;
  /** Render just the model picker, or everything but it. */
  only?: "model" | "rest";
  /** A catalogue already read by the thread; without one, Controls reads its own. */
  catalogue?: ReturnType<typeof useCatalogue>;
  /** The transcript did not load, so the picker waits for it. */
  disabled?: boolean;
}) {
  const own = useCatalogue(!catalogue);
  const { cat, failed: catFailed, retry: retryCat } = catalogue ?? own;

  // A session that has not answered yet genuinely has no model to name;
  // one running a model the catalogue does not list still shows it.
  // "Default" can only be where a session starts: the supervisor has no
  // way back to it, so once a model is set it is not offered.
  // The configured model is named, not called "Default": that word named
  // nothing, and a session that has not answered yet still runs as something.
  // It is the session's own llm row, from the row: /api/models' default
  // is serve's config, and a session in a repo with its own bough.yml (or
  // a child started with --set llm.plugin) runs something else. A
  // provider whose config names no model is named by its plugin.
  const conf = row.configured;
  const confName = conf ? conf.model || conf.plugin : "";
  const value = conf ? "" : row.model ?? "";
  // Until the catalogue loads the picker names nothing, as the effort
  // select says nothing: it has no list to name a choice from.
  const models: Option[] = !cat ? [] : conf
    ? [{ value: "", label: confName, short: confName.split("/").pop(), group: "Configured", detail: "default" }]
    : row.model ? [] : [{ value: "", label: "Default model" }];
  for (const p of cat?.providers ?? []) {
    for (const m of p.models ?? []) {
      // The group already names the provider; the trigger drops it too.
      models.push({ value: m.id, label: m.id, short: m.id.split("/").pop(), group: capital(p.plugin.replace(/^llm-/, "")),
                    detail: m.context ? contextSize(m.context) : undefined });
    }
  }
  if (cat && value && !models.some((o) => o.value === value)) {
    models.push({ value, label: value, short: value.split("/").pop(), group: "In use" });
  }

  // Effort is offered for what the chosen model supports; a model the
  // catalogue does not describe falls back to every level it knows.
  const runsAs = conf ? conf.model : row.model;
  const chosen = cat?.providers.flatMap((p) => p.models ?? []).find((m) => m.id === runsAs);
  const efforts = chosen?.efforts?.length ? chosen.efforts : (cat?.efforts ?? []);
  // Likewise the effort: the configured level, or the provider's own when the config sets none.
  const effortDefault: Option = conf?.effort ? { value: "", label: `${effortLabel(conf.effort)} · default`, short: effortLabel(conf.effort) }
    : conf ? { value: "", label: "Provider default", short: "Default" } : { value: "", label: "Default effort" };

  return (
    <div className="controls">
      {only !== "rest" && (
        // What the next turn runs as is one setting: the model and how hard
        // it thinks, side by side, on every screen.
        <div className="ctl ctl-run" title="Model and effort for the next turn">
          <span className="ctl-label ctl-next">Next turn</span>
          <span className="ctl-label ctl-field">Model</span>
          <Select label="Next turn model" value={cat ? value : ""} options={models} searchable align="end" disabled={disabled || !cat} currentGroup="Next turn"
                  placeholder={catFailed ? "Unavailable" : cat ? "Choose" : "Loading"}
                  suffix={row.effort}
                  footer={(o) => {
                    const m = o && cat?.providers.flatMap((p) => p.models ?? []).find((x) => x.id === o.value);
                    // Input / output per 1M tokens; the highlighted row already names the model.
                    return m?.input && m?.output ? <>${+m.input.toFixed(2)} / ${+m.output.toFixed(2)} <span className="sel-foot-unit">per 1M</span></> : cat ? "Price unavailable" : null;
                  }}
                  // The provider that lists the model runs it; a bare id would stay on the current one.
                  onChange={(v) => (v ? onModel(v, cat?.providers.find((p) => p.models?.some((m) => m.id === v))?.plugin) : undefined)} />
          {/* Always present, so Settings keeps one shape: disabled with the reason until levels are known. */}
          <span className="ctl-label ctl-field ctl-effort">Effort</span>
          <Select label="Next turn effort" value={row.effort ?? ""} align="end" onChange={(v) => (v ? onEffort(v) : undefined)}
                  disabled={efforts.length === 0} placeholder={catFailed ? "Unavailable" : cat ? "Not offered" : "Loading"}
                  options={efforts.length ? [...(row.effort ? [] : [effortDefault]), ...efforts.map((e) => ({ value: e, label: effortLabel(e) }))] : []} />
          {catFailed && <button className="btn" onClick={retryCat}>Models unavailable · Retry</button>}
        </div>
      )}
      {only !== "model" && (
        <div className="ctl">
          <span className="ctl-label">Project</span>
          {/* A project session's project is in its history and its orb: there is nothing to move it to. */}
          {row.mode === "project"
            ? <span className="ctl-fixed" title={PROJECT_SESSION}>{projects.find((p) => p.slug === row.project)?.name ?? row.project}</span>
            : <Select label="Project" value={row.project ?? ""} align="end" onChange={onAssign}
                      options={[{ value: "", label: "Unassigned" }, ...projects.map((p) => ({ value: p.slug, label: p.name }))]} />}
        </div>
      )}
      {/* A running child keeps the MEMORY.md it started with: serve gives it
          the project's directory only at a start. Without this, a move read
          as applying to the next turn. */}
      {only !== "model" && row.mode !== "project" && row.live && (() => {
        const name = (slug: string) => projects.find((p) => p.slug === slug)?.name ?? slug;
        const had = row.startedIn ?? "", now = row.project ?? "";
        const next = had === now ? "" : now ? ` ${name(now)}’s applies from its next start.` : " Taking it out applies from its next start.";
        return <p className="ctl-brief" role="note">Running with {had ? `${name(had)}’s` : "no project"} MEMORY.md.{next}</p>;
      })()}
    </div>
  );
}

/**
 * Home: the sidebar is the one list of sessions, so Home lists none. It
 * says how many need you and points at the first such row there.
 */
export function ControlOverview({ rows, onReveal, onOpenFailure, loadedAt, loadErr, onRetry, actions }: {
  /** Header controls: where a new session runs, and New session. */
  actions?: React.ReactNode;
  rows: Row[]; onReveal: (id: string) => void; loadedAt: number | null; loadErr: string | null; onRetry: () => void;
  /** Open the session at the failing call, when its transcript names one. */
  onOpenFailure: (id: string, seq?: number) => void;
}) {
  // The sidebar's own rule (sessionSignal): a failure outranks "done".
  // An empty session whose orb failed to set up stays, as in the sidebar:
  // hiding it would say "nothing needs you" under a red row.
  const setupFailed = (r: Row) => r.orb?.status === "failed";
  const live = rows.filter((r) => !r.archived && !(r.empty && !r.live && !setupFailed(r)));
  const needs = live.filter((r) => sessionSignal(r) === 0 || setupFailed(r)).sort((a, b) => Date.parse(b.lastAt) - Date.parse(a.lastAt));
  const failed = needs.filter((r) => hasFailure(r) || setupFailed(r)).slice(0, 5);
  const questions = needs.filter(hasQuestion);
  const runningRows = live.filter((r) => !r.background && sessionSignal(r) === 1);
  const running = runningRows.length;
  const runningRow = runningRows[0];
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
  const project = (r: Row) => r.repo?.split("/").pop();
  return (
    <div className="ov">
      <header className="ov-head">
        <h1>Overview</h1>
        <span className="ov-hint"><kbd>{modKey()}K</kbd> to search or start a session</span>
        {actions && <div className="ov-actions">{actions}</div>}
      </header>
      <div className="scroll ov-body">
        {/* Lists are only as current as the last refresh, and say so. */}
        {loadErr && (
          <p className="ov-stale" role="status">
            {loadedAt === null ? "Status unavailable" : `Updates delayed · synced ${ago(new Date(loadedAt).toISOString())} ago`}
            <button className="link" onClick={onRetry}>Retry</button>
          </p>
        )}
        {needs.length > 0 && (failed.length > 0 || questions.length > 0) ? (
          <>
            <section className="ov-card" aria-label="Needs you">
              <div className="ov-card-head"><span className="eyebrow">Needs you</span><span className="num ov-card-count">{failed.length + questions.length}</span></div>
              {failed.length > 0 && (
                <div className="ov-fails" role="list" aria-label="Unresolved failures">
                  {failed.map((r) => {
                    const e = evid[r.id];
                    return (
                      <div key={r.id} className="ov-fail" role="listitem">
                        <StatusMark status="error" size={16} bare />
                        <span className="ov-fail-main">
                          <span className="ov-fail-what" title={plainTitle(r.title) || r.id}>{sessionTitle(r)}</span>
                          <span className="ov-state is-failed">{r.trouble ? r.trouble[0].toUpperCase() + r.trouble.slice(1) : r.testsFailed ? "Tests failed" : setupFailed(r) ? orbWord("failed") : "Failed"}</span>
                          {/* The failing call when the transcript names one. */}
                          {e?.cmd && <span className="mono ov-fail-cmd" title={e.cmd}>{e.cmd}{e.exit !== undefined && ` · exit ${e.exit}`}</span>}
                        </span>
                        <span className="ov-meta">{project(r)}</span>
                        <span className="num ov-meta">{ago(e?.at ?? r.lastAt)} ago</span>
                        <button className="btn btn-sm" onClick={() => onOpenFailure(r.id, e?.seq)} aria-label={`Open failure in ${sessionTitle(r)}`}>Open</button>
                      </div>
                    );
                  })}
                </div>
              )}
              {questions.length > 0 && (
                <div className="ov-wait-list" role="group" aria-label="Waiting for you">
                  {questions.map((r) => (
                    <button key={r.id} className="ov-wait" onClick={() => onReveal(r.id)}>
                      <StatusMark status="needs-you" size={16} bare />
                      <span className="ov-fail-main">
                        <span className="ov-wait-line"><span className="ov-wait-title">{sessionTitle(r)}</span><span className="ov-state is-waiting">Waiting</span></span>
                        {r.ask?.text && <span className="ov-wait-q">{r.ask.text}</span>}
                      </span>
                      <span className="ov-meta">{project(r)}</span>
                      <span className="num ov-meta">{ago(r.lastAt)} ago</span>
                    </button>
                  ))}
                </div>
              )}
            </section>
            {runningRow && (
              <p className="ov-empty-sub"><button className="link ov-point" onClick={() => onReveal(runningRow.id)}>{running} running</button></p>
            )}
          </>
        ) : !loadErr && (
          loadedAt === null ? <p className="ov-none" role="status">Loading sessions…</p> : (
            <div className="ov-empty" role="status">
              {/* Something running is not "nothing": say what is going on, and link to it. */}
              <p className="ov-empty-title">Nothing needs your attention</p>
              {runningRow && (
                <p className="ov-empty-sub">{running} session{running === 1 ? "" : "s"} running · <button className="link ov-point" onClick={() => onReveal(runningRow.id)}>open</button></p>
              )}
              {/* One tip, not a toolbar: the header already says the rest. */}
              <p className="ov-empty-keys"><span className="ov-key"><kbd>{modKey()}K</kbd> Search or start a session</span><span className="ov-key"><kbd>?</kbd> All shortcuts</span></p>
            </div>
          )
        )}
      </div>
    </div>
  );
}

/* ---------------- thread ---------------- */

export function Back({ onBack, label = "Back to sessions" }: { onBack?: () => void; label?: string }) {
  if (!onBack) return null;
  return (
    <button className="back" onClick={onBack} aria-label={label}>
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

/** steer: whether a turn was running when you sent it. Read off the live
 *  status instead, the message itself flipped the session to running before
 *  the transcript showed it, so every new turn read "Steer pending…". */
/** `seen`: seq|at keys of inputs on hand at send time; a prompt resent after a
 *  stop must not count its earlier copy as landed. */
type Pending = { id: string; text: string; after: number; steer?: boolean; seen?: string[]; /** The server took it; the turn has not started yet. */ accepted?: boolean; /** When it was sent, for the prompt time. */ at?: string };

/** The composer's status word: a send not yet recorded, a turn with no
 *  output yet, then output arriving. Derived from the same render as the
 *  transcript, so the word never runs ahead of what is shown. */
export function composerStatus({ sending, accepted = false, running, streamed, activity, stopping = false }: { sending: boolean; /** The send was taken, the turn not yet started. */ accepted?: boolean; running: boolean; streamed: boolean; activity: string; /** Stop was asked and the server has not recorded it yet. */ stopping?: boolean }): "" | "Sending" | "Waiting" | "Working" | "Stopping" {
  if (stopping && (sending || running)) return "Stopping";
  if (sending) return accepted ? "Waiting" : "Sending";
  if (!running) return "";
  // R4-D: one word per phase; what it is doing is detail, never a second status word.
  return streamed || activity ? "Working" : "Waiting";
}

/** A stopped turn whose last word was prose: the cut is marked under that prose, and the footer does not repeat it. */
function cutAfterProse(turn: Turn) {
  return Boolean((turn.stopped || turn.done?.kind === "cancelled") && [...turn.body].reverse().find((l) => !isHookLine(l) && l.kind !== "cancelled" && l.kind !== "done" && l.kind !== "usage")?.kind === "assistant");
}

/** Shown until the model's first output: a breathing dot, nothing to read. */
function WaitingDot() {
  return <div className="waiting-dot" aria-hidden="true"><i /></div>;
}

const WAITING_MODEL = "Model is thinking";
/** The activity the engine sends as a model request starts (internal/unreal/session/actor.go). */
const ENGINE_WAITING = "model is thinking";
const WAITING_WHY = "Nothing has come back yet. A reasoning model can think for 10–25 s before its first word, and some models return no reasoning summary to stream meanwhile.";

/** R2-B: the send was taken and nothing has come back yet. MB-STREAM: the wait is timed from 3s. */
export function WaitingModel({ since }: { since?: string | number }) {
  const [mounted] = useState(() => Date.now());
  return <p className="waiting-model" role="status" title={WAITING_WHY}><i className="breath-dot" aria-hidden="true" /><span>{WAITING_MODEL}</span><Elapsed since={since ?? mounted} from={3} /></p>;
}

function SendingPrompt({ p, accepted = false, clamp = true, onClip, clipped, onToggle, children }: { p: Pending; /** The turn it started is running: no longer on its way. */ accepted?: boolean; clamp?: boolean; onClip?: (el: HTMLParagraphElement) => void; clipped?: boolean; onToggle?: () => void;
  /** The running turn's body: inside the same section, as a recorded turn has it, so no separator appears and then goes. */ children?: React.ReactNode }) {
  return (
    <section className={"turn" + (accepted ? "" : " turn-sending")}>
      <div className="prompt">
        <div className="prompt-text prompt-bubble">
          <p className={clamp ? "prompt-clamp" : ""} ref={(el) => { if (el) onClip?.(el); }}><PromptWords text={p.text} /></p>
          <SentImages text={p.text} />
          {clipped && <button className="link" onClick={onToggle}>{clamp ? "Show full prompt" : "Show less"}</button>}
        </div>
        {/* The recorded prompt's action row keeps its height here, so the body does not drop when it lands. */}
        <div className="msg-acts prompt-acts">
          {!accepted && !p.accepted ? <span className="num prompt-time turn-sending-state" role="status">{p.steer ? "Steer pending…" : "Sending…"}</span>
            : p.at && <span className="num prompt-time" title={new Date(p.at).toLocaleString()}>{when(p.at)}</span>}
          <CopyButton text={p.text} what="prompt" />
        </div>
      </div>
      {children}
    </section>
  );
}

/** A session just started with a prompt, before its row is in the list:
 *  the prompt in place, not a "Loading session…" swap. */
/** A session that exists but has not written its first line: its container is being prepared. */
export function StartingThread() {
  return (
    <div className="thread">
      <div className="lookup" role="status"><p className="lookup-body">Starting the session… its container is being prepared.</p><WaitingDot /></div>
    </div>
  );
}

export function PendingThread({ sending }: { sending: Pending[] }) {
  return (
    <div className="thread">
      <div className="scroll transcript" role="region" aria-label="Transcript">
        {sending.map((p) => <SendingPrompt key={p.id} p={p} />)}
        <WaitingDot />
      </div>
    </div>
  );
}

/** Rows a Stop swallowed: unsent when Stop was pressed, and a done or
 * cancel was recorded after them with no input of theirs. */
export function swallowedByStop(unlanded: Pending[], stopped: Set<string>, lines: Line[]): Pending[] {
  return unlanded.filter((p) => stopped.has(p.id)
    && lines.some((l) => (l.kind === "done" || l.kind === "cancelled") && l.seq > p.after)
    && !lines.some((l) => l.kind === "input" && l.seq > p.after));
}

/** Esc in the composer stops a running turn; an open picker or an IME composition takes it first. */
export function escStops(e: { key: string; running: boolean; pickerOpen: boolean; composing?: boolean }): boolean {
  return e.key === "Escape" && e.running && !e.pickerOpen && !e.composing;
}

/** The prompt of a turn that was stopped before any reply, to put back in the composer; "" otherwise. */
export function stoppedPrompt(lines: Line[]): string {
  let i = lines.length - 1;
  while (i >= 0 && (lines[i].kind !== "input" || lines[i].data?.steer)) i--;
  if (i < 0) return "";
  const after = lines.slice(i + 1);
  if (!after.some((l) => l.kind === "cancelled") || after.some((l) => l.kind === "assistant")) return "";
  return lines[i].text ?? "";
}

/** "503 Service Unavailable" or a bare "503" reads as what happened, code last. */
function sendError(e?: string) {
  const m = /^(\d{3})\b\s*(.*)$/.exec(e ?? "");
  if (!m) return e || "No response";
  const words: Record<string, string> = { "500": "Server error", "502": "Bad gateway", "503": "Service unavailable", "504": "Gateway timeout" };
  return `${m[2] || words[m[1]] || "Request failed"} (${m[1]})`;
}

// The composer's draft is Thread state, so every keystroke re-rendered the
// whole transcript (markdown, highlighting): typing lagged in long sessions.
// A turn re-renders only when its own props change; only the open turn's
// tail does while you type.
const TurnViewMemo = memo(TurnView);
/** Puts a sent prompt back in the composer; busy while a draft is there, so it never glues two prompts together. */
const EditPrompt = createContext<{ busy: boolean; edit: (t: string) => void; /** R4-C: resend a prompt through the composer's send path. */ retry?: (t: string) => void } | null>(null);

/**
 * The image build a session waits on, live: polls the build log once a
 * second from where it left off and follows the end unless you scrolled up.
 * A session opened mid-build used to show only "building" for minutes.
 */
function OrbBuildLog({ id, project, onRebuild, rebuildErr }: { id: string; project: string; onRebuild?: () => Promise<void>; rebuildErr?: string }) {
  const [text, setText] = useState("");
  const [err, setErr] = useState("");
  const [done, setDone] = useState("");
  const [run, setRun] = useState(0); // bumped by Rebuild: poll the new build from the start
  // The timer: build.json's start, and its end once the build stops.
  const [span, setSpan] = useState<{ start?: number; end?: number }>({});
  const now = useNow(!done && span.start !== undefined);
  const pre = useRef<HTMLPreElement>(null);
  const follow = useRef(true);
  useEffect(() => {
    let off = 0, stop = false;
    setText(""); setDone(""); setSpan({});
    const tick = async () => {
      try {
        const r = await api.sessionBuildLog(id, off);
        if (stop) return;
        setErr("");
        setSpan({ start: r.startedAt ? Date.parse(r.startedAt) : undefined, end: r.endedAt ? Date.parse(r.endedAt) : undefined });
        // A smaller offset means a new build truncated the log.
        if (r.offset < off) setText(r.text); else if (r.text) setText((t) => t + r.text);
        off = r.offset;
        // The project's build state ends the poll: a rebuild started from
        // here never marks this session "building".
        if (r.state && r.state !== "building") { setDone(r.state); return; }
      } catch (e) {
        if (!stop) setErr((e as Error).message);
      }
      if (!stop) setTimeout(tick, 1000);
    };
    tick();
    return () => { stop = true; };
  }, [id, run]);
  useLayoutEffect(() => {
    const el = pre.current;
    if (el && follow.current) el.scrollTop = el.scrollHeight;
  }, [text]);
  const lines = text.split("\n");
  const tail = lines.length > 2000 ? lines.slice(-2000).join("\n") : text;
  return (
    <div className="block orb-failure" role="region" aria-label={`${project} image build log`}>
      <p className="meta-line">
        {done ? `Build ${done === "ok" ? "finished" : done} · new sessions use this image` : `Building the ${project} image…`}
        {span.start !== undefined && <span className="num" role="timer" aria-label="Build time"> · {duration(Math.max(0, (done && span.end ? span.end : now) - span.start))}</span>}
      </p>
      {err && <p className="send-failed-text" role="alert">Couldn’t read the build log: {err}</p>}
      {rebuildErr && <p className="send-failed-text" role="alert">Couldn’t start a rebuild: {rebuildErr}</p>}
      {done && onRebuild && <button className="link orb-rebuild" onClick={async () => { await onRebuild(); setRun((n) => n + 1); }}>Rebuild</button>}
      {tail ? <pre className="mono" ref={pre} aria-live="off"
                   onScroll={(e) => { const el = e.currentTarget; follow.current = el.scrollTop + el.clientHeight >= el.scrollHeight - 8; }}>{tail}</pre>
        : <p className="meta-line">Waiting for build output…</p>}
    </div>
  );
}

/**
 * A failed orb's "why": the orb chip only said "failed", with nothing to
 * open. The error and the tail of resume.log load when you open it, and
 * again on Refresh, so a fixed definition can be checked from here.
 */
function OrbFailure({ id, project, name, onRebuild, onRetry, rebuildErr }: { id: string; project?: string; name: string; onRebuild?: () => Promise<void>; onRetry?: () => Promise<void>; rebuildErr?: string }) {
  const [log, setLog] = useState<OrbFailureLog | null>(null);
  const [err, setErr] = useState("");
  const [retried, setRetried] = useState(false);
  const [retryErr, setRetryErr] = useState("");
  const load = () => {
    setErr("");
    api.sessionOrbLog(id).then((l) => setLog(l), (e) => setErr((e as Error).message));
  };
  useEffect(load, [id]); // eslint-disable-line react-hooks/exhaustive-deps
  // A sibling of the header, not a child: the header is a fixed-height grid,
  // and inside it the log wrapped into a 200px-wide, 6000px-tall sliver.
  return (
    <div className="block orb-failure" role="region" aria-label={`${name} setup failure`}>
      {err ? <p className="send-failed-text" role="alert">Couldn’t load the log: {err}</p>
        : !log ? <p className="meta-line">Loading…</p>
        : <OrbFailureBody log={log} projectSlug={project} name={name} onRebuild={onRebuild}
            onRetry={onRetry && (async () => { try { await onRetry(); setRetried(true); } catch (e) { if ((e as Error).message !== "cancelled") setRetryErr((e as Error).message); } })} />}
      {retried && <p className="meta-line" role="status">Stopped. The orb restarts and reruns resume.sh on the session’s next command.</p>}
      {retryErr && <p className="send-failed-text" role="alert">Couldn’t stop the orb: {retryErr}</p>}
      {rebuildErr && <p className="send-failed-text" role="alert">Couldn’t start a rebuild: {rebuildErr}</p>}
      <button className="link" onClick={load}>Refresh</button>
    </div>
  );
}

const noLines: Line[] = [];

/** A composer upload in flight, by session: see Thread's follow. */
type Upload = { slot: number; tag: string; done: Promise<string> };
const uploads = new Map<string, Set<Upload>>();

export function Thread({ row, lines: given, loading = false, loadError, paused, onRetry, stream = [], activity = "", projects, onAck, onSend, onAnswer, onInterrupt, onArchive, onRename, onModel, onEffort, onAssign, onBack, onContext, onPortal, busy, jump, sending = [], setSending = () => {}, onStopOrb, rows = [], onOpenSession, onStartProject, onNewProject }: {
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
  jump?: { turn: number; at: number; seq?: number; q?: string } | null;
  onSend: (t: string) => Promise<string | null> | void; onAnswer: (t: string, ask?: string) => Promise<string | null> | void; onInterrupt: () => Promise<boolean> | void;
  onArchive: () => void; onRename: (t: string) => Promise<void>; onContext?: () => void; onPortal?: () => void; onAck?: () => void;
  /** Stop a project session's container; the child restarts it on its next command. */
  onStopOrb?: () => void;
  /** Start a project session in this project's orb, carrying the draft over unsent. */
  onStartProject?: (project: string, draft: string) => void;
  onNewProject?: () => void;
  onModel: (m: string, plugin?: string) => Promise<boolean> | void; onEffort: (e: string) => Promise<boolean> | void; onAssign: (p: string) => void;
  /** This session's unrecorded sends, kept by the app across session switches. */
  sending?: Pending[]; setSending?: (f: (q: Pending[]) => Pending[]) => void;
}) {
  // The parent still holds the session just left for a render: never show it under this title.
  const lines = loading ? noLines : given;
  // Read once the transcript is: id and length change in the same render, so a switch reads each once.
  const changesRead = useChanges(loading ? "" : row.id, lines.length);
  const catalogue = useCatalogue();
  const changes = useMemo(() => changesRead, [changesRead.session, changesRead.tree]); // eslint-disable-line react-hooks/exhaustive-deps
  // One draft per session: switching away and back keeps what you were
  // typing there, and never carries it into another conversation.
  const draftKey = "bough:draft:" + row.id;
  const [draft, setDraft] = useState(() => { try { return localStorage.getItem(draftKey) ?? ""; } catch { return ""; } });
  useEffect(() => {
    try { draft ? localStorage.setItem(draftKey, draft) : localStorage.removeItem(draftKey); } catch { /* storage off */ }
  }, [draft, draftKey]);
  // Whether the draft is an answer is decided when you start it, keyed to
  // the question on screen then: a question arriving mid-draft must not
  // quietly turn a message into an answer, nor a newer one inherit it.
  // Saved with the draft, so a remount never re-decides it against a newer question.
  const askKey = "bough:draft-ask:" + row.id;
  const [draftAsk, setDraftAsk] = useState(() => { try { return localStorage.getItem(askKey) ?? row.ask?.id ?? ""; } catch { return row.ask?.id ?? ""; } });
  useEffect(() => {
    try { draft.trim() ? localStorage.setItem(askKey, draftAsk) : localStorage.removeItem(askKey); } catch { /* storage off */ }
  }, [draft, draftAsk, askKey]);
  const blank = !draft.trim();
  const deliverRef = useRef<(t: string) => void>(() => {});
  const editPrompt = useMemo(() => ({ busy: !blank, edit: (t: string) => { toDraft(t); setDraftAsk(""); composer.current?.focus(); }, retry: (t: string) => deliverRef.current(t) }), [blank]);
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
    // A screen or more down: a long read gets a way back to the start too.
    setDown(el.scrollTop > 40 && el.scrollHeight > el.clientHeight);
  };
  const [down, setDown] = useState(false);
  const toStart = () => {
    atBottom.current = false;
    const still = window.matchMedia?.("(prefers-reduced-motion: reduce)").matches;
    scroller.current?.scrollTo({ top: 0, behavior: still ? "auto" : "smooth" });
  };
  const awayAt = useRef(0);
  const toLatest = (instant = false) => {
    atBottom.current = true; setAway(false);
    scrollMemo.delete(row.id);
    const still = instant || window.matchMedia?.("(prefers-reduced-motion: reduce)").matches;
    end.current?.scrollIntoView({ block: "end", behavior: still ? "auto" : "smooth" });
  };
  // More than a screen from the end: a smooth scroll would crawl, so jump.
  const far = () => { const el = scroller.current; return !!el && el.scrollHeight - el.scrollTop - el.clientHeight > el.clientHeight; };
  // The block summaries are one tab stop: the current one is tabbable, and
  // so are its own controls; every other summary and its Copy wait for ↑/↓.
  // A closed <details> hides its content without clearing offsetParent: skip rows inside one.
  const summaries = () => [...(scroller.current?.querySelectorAll<HTMLElement>("details.block > summary") ?? [])].filter((s) => {
    if (!s.offsetParent) return false;
    for (let d = s.parentElement?.parentElement?.closest("details"); d; d = d.parentElement?.closest("details")) if (!d.open) return false;
    return true;
  });
  const rovingAt = useRef<HTMLElement | null>(null);
  const rove = (all: HTMLElement[], on: HTMLElement) => {
    rovingAt.current = on;
    for (const s of all) {
      // A "Worked for" fold is a toggle of its own, always one Tab away; ↑/↓ still pass through it.
      // R3-F: a tool group's summary is also its own Tab stop, so the calls inside are reachable without ↑/↓.
      s.tabIndex = s === on || s.parentElement!.classList.contains("work-seg") || s.parentElement!.classList.contains("toolrun") ? 0 : -1;
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
    if (e.key === "End" && (e.metaKey || e.ctrlKey)) { e.preventDefault(); toLatest(); return; }
    // The page keys scroll the transcript itself, from anywhere in it but a
    // field, output that scrolls on its own, or a control that owns the key.
    const el = scroller.current;
    if (!el || e.altKey || e.metaKey || e.ctrlKey || e.defaultPrevented) return;
    if (t.closest("input, textarea, select, [contenteditable], pre, .md-table-scroll")) return;
    const arrow = e.key === "ArrowDown" || e.key === "ArrowUp";
    if (arrow && t !== el) return;
    const page = el.clientHeight * 0.9, line = 40;
    const by: Record<string, number> = { ArrowDown: line, ArrowUp: -line, PageDown: page, PageUp: -page, " ": e.shiftKey ? -page : page };
    if (e.key === " " && t !== el) return;
    if (e.key === "Home") { e.preventDefault(); toStart(); return; }
    if (e.key === "End") { e.preventDefault(); toLatest(); return; }
    if (by[e.key] === undefined) return;
    e.preventDefault();
    el.scrollBy({ top: by[e.key] });
  };
  // Coming back to a session lands where you were reading. One that was
  // following its output (or is new) opens at the bottom, new events and all.
  const memo = scrollMemo.get(row.id);
  if (memo && !memo.follow) atBottom.current = false;
  const restoredAt = useRef(false);
  // Layout effects, both: a long transcript painted at the top first and jumped to the end a frame later.
  useLayoutEffect(() => {
    if (loading || restoredAt.current) return;
    restoredAt.current = true;
    const root = scroller.current;
    // Folds first, so the scroll lands on the layout that was left; then focus.
    const fold = (keys: string[] | undefined, on: boolean) => {
      for (const k of keys ?? []) { const d = root?.querySelector<HTMLDetailsElement>(`details[data-open-key="${CSS.escape(k)}"]`); if (d) d.open = on; }
    };
    fold(memo?.shut, false);
    fold(memo?.open, true);
    if (memo && !memo.follow && root) { root.scrollTop = memo.top; awayAt.current = newest; setAway(root.scrollHeight - root.scrollTop - root.clientHeight >= 40); }
    const focus = arrivals.get(row.id) ?? memo?.focus;
    arrivals.delete(row.id);
    if (focus === "head") headRef.current?.focus({ preventScroll: true });
    else if (focus === "work") workBtn.current?.focus({ preventScroll: true });
    else if (focus) root?.querySelector<HTMLElement>(`[data-open-key="${CSS.escape(focus)}"] > summary`)?.focus({ preventScroll: true });
  }, [loading]); // eslint-disable-line react-hooks/exhaustive-deps
  useLayoutEffect(() => {
    if (atBottom.current) end.current?.scrollIntoView({ block: "end" });
  }, [lines.length, streamLen]);
  const turns = useMemo(() => groupTurns(lines), [lines]);
  const stored = useMemo(() => storedNotices(lines), [lines]);
  // R4-B: the last finished turn ended on a failed command: the header says Failed, as its footer does, never a checked Done.
  const lastFail = useMemo(() => {
    const t = [...turns].reverse().find((u) => u.done);
    if (!t?.done || t.stopped || t.done.kind === "cancelled") return null;
    const exit = t.done.data?.exit;
    const r = [...t.body].reverse().find((l) => l.kind === "result" && typeof l.data?.exit === "number" && l.data.exit !== 0);
    if (typeof exit === "number" ? exit === 0 : !r || r !== [...t.body].reverse().find((l) => l.kind === "result")) return null;
    return r ? failNameOf(str(r.data?.code), resultBody(r)) || "Command" : "Command";
  }, [turns]);
  // Numbered by prompt, as the turn log counts; once per transcript, not a rescan per turn per render.
  const turnNums = useMemo(() => { let n = 0; return turns.map((t) => (t.prompt ? ++n : undefined)); }, [turns]);

  // The session's Work: its jobs, its subagents and its direct background
  // agents, one index the transcript, the Work button and its dialog read.
  const kids = useChildren(row, rows, lines.length);
  const review = useReviewed(row.id);
  const reports = useMemo(() => agentReports(lines), [lines]);
  const workers = useMemo(() => workIndex({ session: row.id, lines, turns, row, rows, children: kids.children, live: row.live })
    // A child's report to this session is its result; nothing else carries one.
    .map((w) => (w.kind === "agent" && reports.has(w.id) ? { ...w, result: reports.get(w.id) } : w)), [row, lines, turns, rows, kids.children, reports]);
  const byKey = useMemo(() => new Map(workers.map((w) => [w.key, w])), [workers]);
  const { stops, requestStop } = useStopStore(byKey, review, kids.refresh);
  const workCtx = useMemo<WorkCtx>(() => {
    const jobs = new Map(workers.filter((w) => w.kind === "job").map((w) => [w.id, w]));
    const jobFirst = new Map<string, number>();
    for (const l of lines) {
      const id = l.kind === "job" ? jobIdOf(l) : undefined;
      if (id !== undefined && !jobFirst.has(String(id))) jobFirst.set(String(id), l.seq);
    }
    return { session: row.id, live: row.live, workers, byKey, jobs, jobFirst, review, stops, requestStop, openWork: () => setWorkOpen(true) };
  }, [row.id, row.live, workers, byKey, lines, review, stops, requestStop]);
  const counts = workCounts(workers, review.isNew);
  // Visible whenever there is work, and while background agents are still being looked up.
  const showWork = counts.total > 0 || kids.state === "loading" || kids.state === "error";
  const [workOpen, setWorkOpen] = useState(false);
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
  const openWork = () => setWorkOpen(true);
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
    // A search landing marks what matched, without touching the DOM React owns.
    const hl = jump.q ? markMatch(el, jump.q) : undefined;
    const t = setTimeout(() => el.classList.remove("turn-flash"), 1600);
    return () => { clearTimeout(t); hl?.(); };
  }, [jump, loading, turns.length]);
  // A fast read shows nothing at all; only a slow one earns a word.
  const [slow, setSlow] = useState(false);
  useEffect(() => {
    if (!loading) { setSlow(false); return; }
    const t = setTimeout(() => setSlow(true), 200);
    return () => clearTimeout(t);
  }, [loading]);
  const running = row.status === "running";
  // Which orb panel is open under the header: the failure's why, or the live
  // build log (while building, or after you pressed Rebuild).
  const [orbView, setOrbView] = useState<"" | "why" | "build">("");
  const [rebuildErr, setRebuildErr] = useState("");
  const rebuild = async () => {
    if (!row.project) return;
    setRebuildErr("");
    try {
      await api.buildOrb(row.project);
    } catch (e) {
      // 409: a build is already running; the log shows it.
      if ((e as { status?: number }).status !== 409) { setRebuildErr((e as Error).message); return; }
    }
    setOrbView("build");
  };

  // A long paste should be visible, not a two-row porthole you have to
  // drag open. Grow to the text and stop at a third of the window.
  useEffect(() => {
    const el = composer.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = Math.min(el.scrollHeight, Math.round((window.visualViewport?.height ?? window.innerHeight) / 3)) + "px";
  }, [draft]);

  // Escape shuts a picker until the token it was over changes; the keyup
  // that follows the Escape would otherwise open it straight back.
  const dismissed = useRef("");
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
  // An input with the same text also lands it: `after` is the newest seq
  // on hand at send time, and when those lines were stale (a session just
  // created or switched to) the recorded prompt showed at the top while
  // its "Sending…" copy stayed stuck at the bottom.
  const inputs = lines.filter((l) => l.kind === "input");
  const sameText = (p: Pending) => { const want = p.text.trim().slice(0, 200); return inputs.slice(-(sending.length + 3)).some((l) => !p.seen?.includes(`${l.seq}|${l.at}`) && (l.text ?? "").trim().startsWith(want)); };
  // R3-D: a local command (/model, /think) records a command line, never an
  // input: that record is what lands it, or its row said "Sending…" forever.
  const isCmd = (p: Pending) => /^\/[a-z]/i.test(p.text.trim());
  const cmdLanded = (p: Pending) => { const verb = p.text.trim().split(/\s+/)[0]; return lines.some((l) => l.kind === "command" && l.seq > p.after && (l.text ?? "").trim().split(/\s+/)[0] === verb); };
  const prompts = sending.filter((p) => !isCmd(p));
  const unlanded = loading ? sending : sending.filter((p) => isCmd(p) ? !cmdLanded(p) : inputs.filter((l) => l.seq > p.after).length <= prompts.indexOf(p) && !sameText(p));
  // R3-C: a turn is live from the moment its prompt is sent, not only once
  // the row says running: Esc in that gap must stop it, and a message sent
  // then steers it.
  const live = running || unlanded.some((p) => !p.steer);
  // Sends still on their way to the server: a stop waits for them, or it
  // would reach a session with nothing to stop and the prompt would run.
  const inflight = useRef(new Set<Promise<unknown>>());
  // A running turn whose prompt has not landed yet: its stream sits in the
  // prompt's own section, with the prompt time, as the recorded turn will.
  // Built after `status` is known (it is declared below), hence a thunk.
  // A trailing /model (or other command) section never gets a done: a
  // prompt sent after it is still the turn that runs, not that section.
  const tailTurn = turns[turns.length - 1];
  const openTail = Boolean(tailTurn && !tailTurn.done && !(tailTurn.prompt === null && tailTurn.body.every((l) => isQuiet(l.kind)) && unlanded.some((p) => !p.steer)));
  const liveBodyFn = running && !openTail && !row.ask
    ? () => <div className="turn-body"><StreamView runs={stream} />{status === "Waiting" ? <WaitingModel since={unlanded.find((p) => !p.steer)?.at} /> : <Working label={stream.at(-1)?.kind === "thinking" ? "Thinking" : "Working"}>{unlanded.find((p) => !p.steer)?.at && <Elapsed since={unlanded.find((p) => !p.steer)!.at!} />}</Working>}</div> : null;
  const liveHost = liveBodyFn ? unlanded.filter((p) => !p.steer).at(-1) : undefined;
  useEffect(() => { window.dispatchEvent(new Event(TRANSCRIPT_GREW)); }, [stream, lines.length, sending.length]);
  const landedIds = sending.filter((p) => !unlanded.includes(p)).map((p) => p.id).join(" ");
  useEffect(() => { if (landedIds) { const ids = new Set(landedIds.split(" ")); setSending((q) => q.filter((p) => !ids.has(p.id))); } }, [landedIds]); // eslint-disable-line react-hooks/exhaustive-deps
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
    if (!answer) setSending((q) => [...q, { id, text: t, after: newest, steer: live, seen: inputs.map((l) => `${l.seq}|${l.at}`), at: new Date().toISOString() }]);
    else setAnswering({ ask: ask ?? "", text: t });
    const req = Promise.resolve(answer ? onAnswer(t, ask) : onSend(t));
    inflight.current.add(req);
    const error = await req.finally(() => inflight.current.delete(req));
    if (answer) setAnswering(null);
    if (error) setSending((q) => q.filter((p) => p.id !== id));
    else if (!answer) setSending((q) => q.map((p) => (p.id === id ? { ...p, accepted: true } : p)));
    // Each request is its own row; a retry that fails again replaces its own.
    if (error) setFailures((q) => [...q.filter((f) => f.id !== id), { id, at: Date.now(), text: t, answer, ask, error }]);
  };
  // Stop is asked once; the button says so until the server records it.
  const [stopping, setStopping] = useState<"" | "stopping" | "failed">("");
  // R3-D: the server's error status wins over a send it never started.
  const status = row.status === "error" ? "" : composerStatus({ sending: !running && unlanded.some((p) => !p.steer), accepted: unlanded.some((p) => !p.steer && p.accepted), running, streamed: stream.length > 0, activity, stopping: stopping === "stopping" });
  // MB-STREAM: a status row (Waiting, Working) that appears between sends is followed like new output.
  useLayoutEffect(() => { if (atBottom.current) end.current?.scrollIntoView({ block: "end" }); }, [status]);
  const failedLoad = loading && Boolean(loadError);
  // MB-ERR: a toast sits above the composer, not over Send.
  const composerWrap = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const el = composerWrap.current, root = document.documentElement;
    if (!el || typeof ResizeObserver === "undefined") return;
    const ro = new ResizeObserver(() => root.style.setProperty("--composer-h", `${el.offsetHeight + 16}px`));
    ro.observe(el);
    return () => { ro.disconnect(); root.style.removeProperty("--composer-h"); };
  }, []);
  useEffect(() => { if (!live) setStopping(""); }, [live]);
  useEffect(() => {
    if (!restoreOnStop.current || running || loading) return;
    const t = stoppedPrompt(lines);
    if (!t) return;
    restoreOnStop.current = false;
    if (!draft.trim()) toDraft(t);
  }, [running, loading, newest]); // eslint-disable-line react-hooks/exhaustive-deps
  // Stopped before any reply: the prompt comes back to an empty composer.
  const restoreOnStop = useRef(false);
  const stop = async () => {
    restoreOnStop.current = true;
    setStopping("stopping");
    await Promise.allSettled([...inflight.current]);
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
      const a = JSON.parse(localStorage.getItem(attsKey) ?? "null") as { pastes: string[]; images: string[] } | null;
      if (a) { pastes.current = a.pastes; images.current = a.images; }
    } catch { /* storage off */ }
  }
  useEffect(() => {
    try {
      if (draft && (pastes.current.length || images.current.length)) localStorage.setItem(attsKey, JSON.stringify({ pastes: pastes.current, images: images.current }));
      else localStorage.removeItem(attsKey);
    } catch { /* storage off */ }
  }, [draft, attsKey]);
  const [uploading, setUploading] = useState(0);
  // Main's Start thread button and what it last said (see StartThreadCtx).
  const startThread = useContext(StartThreadCtx);
  const [threadNote, setThreadNote] = useState("");
  // Back from another session, a tag whose upload failed meanwhile (or
  // before) still says so: the alert that said it died with the old Thread.
  const [attachErr, setAttachErr] = useState(() => {
    if (uploads.get(row.id)?.size) return "";
    const lost = lostTags(draft.trim(), images.current, pastes.current);
    return lost.length ? `Attachment unavailable: remove ${lost.join(", ")}` : "";
  });
  // An upload is followed by whichever Thread shows its session: the one
  // that started it, or the one mounted on the way back.
  const alive = useRef(true);
  const follow = async (u: Upload) => {
    setUploading((n) => n + 1);
    try {
      const path = await u.done;
      // Stored slot by slot, so an unmounted Thread's stale refs never
      // overwrite what the mounted one has since.
      try {
        const a = JSON.parse(localStorage.getItem(attsKey) ?? "null") as { pastes: string[]; images: string[] } | null;
        if (a && a.images[u.slot] === "") { a.images[u.slot] = path; localStorage.setItem(attsKey, JSON.stringify(a)); }
      } catch { /* storage off */ }
      if (alive.current) images.current[u.slot] = path;
    } catch (err) {
      if (alive.current) setAttachErr(`${u.tag} not attached: ${(err as Error).message}`);
    } finally {
      if (alive.current) setUploading((n) => n - 1);
    }
  };
  useEffect(() => {
    alive.current = true;
    uploads.get(row.id)?.forEach((u) => void follow(u));
    return () => { alive.current = false; };
  }, []); // eslint-disable-line react-hooks/exhaustive-deps
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
      const u: Upload = { slot: slots[k], tag: tag(f, slots[k]), done: isImage(f) ? api.attach(f) : api.attachFile(row.id, f) };
      const set = uploads.get(row.id) ?? new Set<Upload>();
      uploads.set(row.id, set.add(u));
      const followed = follow(u);
      void u.done.catch(() => {}).finally(() => set.delete(u));
      await followed;
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
    .replace(/\[Pasted text #(\d+) \+(\d+) lines\]/g, (m, i, n) => (pastes.current[i - 1] === undefined ? m : wrapPaste(pastes.current[i - 1], +n)));
  // A sent message back in the composer: its pastes are tags again, as when they were pasted.
  const toDraft = (t: string) => setDraft(foldPastes(t, (body) => pastes.current.push(body)));

  deliverRef.current = (t: string) => { void deliver(t, false); };
  const send = async () => {
    const t = draft.trim();
    // Enter reaches here even while the Send button is disabled.
    // Nor before the transcript is read: what landed is judged against it.
    if (!t || loading || busy || uploading || askChanged || row.archived) return;
    // A tag whose content is gone is never sent as its placeholder.
    const lost = lostTags(t, images.current, pastes.current);
    if (lost.length) { setAttachErr(`Attachment unavailable: remove ${lost.join(", ")}`); return; }
    setDraft("");
    const full = expand(t);
    pastes.current = []; images.current = [];
    // What you send is followed into view, even from far up the history.
    if (!atBottom.current) toLatest(far());
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
    // Held to send's rule: queued, a lost tag would go out as its placeholder.
    const lost = lostTags(t, images.current, pastes.current);
    if (lost.length) { setAttachErr(`Attachment unavailable: remove ${lost.join(", ")}`); return; }
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
  // A Stop can swallow a line already written to the child: no input is
  // ever recorded for it, and its row said "Sending…" until a reload lost
  // it. Once the stop has settled with rows still unlanded, a message goes
  // back to the front of the queue and a steer says it was dropped.
  // Only rows unsent when Stop was pressed qualify, and only once the
  // transcript shows the stopped turn ended after them.
  const stoppedIds = useRef<Set<string>>(new Set());
  useEffect(() => { if (stopping === "stopping") stoppedIds.current = new Set(unlanded.map((p) => p.id)); }, [stopping]); // eslint-disable-line react-hooks/exhaustive-deps
  const unlandedIds = unlanded.map((p) => p.id).join(" ");
  useEffect(() => {
    if (!stoppedIds.current.size || running || loading || busy) return;
    const lost = swallowedByStop(unlanded, stoppedIds.current, lines);
    if (!lost.length) { if (!unlanded.some((p) => stoppedIds.current.has(p.id))) stoppedIds.current = new Set(); return; }
    const timer = setTimeout(() => {
      stoppedIds.current = new Set();
      const ids = new Set(lost.map((p) => p.id));
      setSending((q) => q.filter((p) => !ids.has(p.id)));
      const steers = lost.filter((p) => p.steer);
      if (steers.length) setFailures((q) => [...q, ...steers.map((p) => ({ id: p.id, at: Date.now(), text: p.text, answer: false, error: "Steer dropped by Stop" }))]);
      const msgs = lost.filter((p) => !p.steer);
      if (msgs.length) { flushing.current = false; setQueued((q) => [...msgs.map((p) => ({ id: p.id, text: p.text })), ...q]); }
    }, 4000);
    return () => clearTimeout(timer);
  }, [unlandedIds, running, loading, busy, newest]); // eslint-disable-line react-hooks/exhaustive-deps

  return (
    <WorkContext.Provider value={workCtx}>
    <SessionChanges.Provider value={changes}>
    <div className="thread" ref={threadRef}>
      <header className="thread-head">
        {/* A breadcrumb row of its own above the title: inside .head-main it wrapped and pushed the title off the side controls. */}
        {row.spawnedBy && <ParentLink id={row.spawnedBy} rows={rows} onOpen={onOpenSession} />}
        <Back onBack={onBack} />
        <div className="head-main">
          <h1 title={row.title} ref={headRef} tabIndex={-1}>{sessionTitle(row)}</h1>
          {/* Until the transcript is read the header names no status: "Done" became "Done · 11 failed" a moment later. */}
          {loading ? null : (status === "Sending" || status === "Waiting" || row.status === "running") ? (
            // R4-D: while a turn runs the header says the transcript's word, Working, not the list's Running.
            // R2-B: a send on its way is work, never the last turn's Done.
            <span className={"status head-live" + (status === "Stopping" ? " head-stopping" : "")}>{status === "Stopping" ? <i className="head-stop-mark" aria-hidden="true" /> : <StatusMark status="running" bare />}{status || "Working"}</span>
          ) : row.trouble && row.trouble !== "tests failed" ? (
            // One status: the reason replaces "Done".
            <span className="status head-trouble"><StatusMark status="error" bare />{capital(row.trouble)}</span>
          ) : row.mode === "project" && row.orb?.status === "failed" ? null /* Setup failed says it; "Done" beside it contradicted it. */
            // Worst outcome first: a finished session whose work failed does not read as a bare Done.
            : row.status === "done" && counts.failed > 0
              // The mark carries the red and the Work button the count, once: "12 failed" twice on one line named no subject.
              ? <span className="status head-trouble" title={`${counts.failed} ${counts.failed === 1 ? "worker" : "workers"} failed`}><StatusMark status="error" bare />{statusWord("done")}<span className="visually-hidden">, {counts.failed} {counts.failed === 1 ? "worker" : "workers"} failed</span></span>
              : row.status === "done" && lastFail
                ? <span className="status head-failed" title={`${lastFail} failed`}><WarnMark />Failed</span>
                : <StatusMark status={row.status} />}
          {/* MB-HDR: the settings popover has no "Where" line, so the repo stays here, quiet, after the status. */}
          {(row.repo || row.branch) && (
            <span className="mono head-repo">
              {row.repo?.split("/").pop()}
              {row.branch && <span style={{ color: "var(--line-strong)" }}>/</span>}{row.branch}
            </span>
          )}
          <ModeChip row={row} phases name={projects.find((p) => p.slug === row.orb?.project)?.name} />
          {/* A short link beside the chip: a full button pushed the title row
              past its 32px and covered the strip below. */}
          {row.orb?.status === "failed" && (
            <button className="link" aria-expanded={orbView === "why"} aria-label={orbView === "why" ? "Hide why setup failed" : "Why did setup fail?"}
                    onClick={() => setOrbView((v) => (v === "why" ? "" : "why"))}>
              {orbView === "why" ? "Hide" : "Why?"}
            </button>
          )}
          {row.orb?.status === "building" && (
            <button className="link" aria-expanded={orbView === "build"} aria-label={orbView === "build" ? "Hide build log" : "Show the live build log"}
                    onClick={() => setOrbView((v) => (v === "build" ? "" : "build"))}>
              {orbView === "build" ? "Hide" : "Log"}
            </button>
          )}
          {/* A test failure is the Tests chip's to say, once. */}
          {running && turns[turns.length - 1]?.prompt?.at && !turns[turns.length - 1]?.done && <RunClock since={turns[turns.length - 1].prompt!.at} />}
        </div>
        <RuntimeStrip cat={catalogue.cat} row={row} lines={lines} paused={paused} onRetry={onRetry} onContext={onContext} loading={loading} failed={failedLoad}
          actions={<>
            {/* A portal is worth a button: it was reachable only from the
                palette, and only on a project session, so on a machine
                whose sessions are nearly all local it was invisible. */}
            {row.mode === "project" && onPortal && (
              <PortalButton ports={row.orb?.portals ?? []} onClick={onPortal} />
            )}
            {orbUp(row.orb) && onStopOrb && <button className="btn head-ack head-stop" onClick={onStopOrb}>Stop orb</button>}
            {row.trouble && onAck && <button className="btn head-ack" onClick={onAck}>Mark seen</button>}
          </>}
          work={showWork && (
            <WorkButton btnRef={workBtn} counts={counts} loading={kids.state === "loading"} unavailable={kids.state === "error"}
                        paused={paused !== undefined} narrow={paneNarrow || sheet} expanded={workOpen}
                        onClick={() => (workOpen ? closeWork(true) : openWork())} />
          )} />
        {/* After the strip, so Tab follows the visual order: Work, Context, Cost, then Settings. */}
        {/* The model and effort pickers live in the composer toolbar; a phone's are under Settings. */}
        <div className="head-side">
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
                <div className="head-pop-run"><Controls row={row} projects={projects} onModel={onModel} onEffort={onEffort} onAssign={onAssign} only="model" catalogue={catalogue} disabled={failedLoad} /></div>
                <Controls row={row} projects={projects} onModel={onModel} onEffort={onEffort} onAssign={onAssign} only="rest" catalogue={catalogue} />
                <button className="head-pop-item" onClick={async () => {
                  closeMore(false);
                  // Empty is allowed: it hands the title back to the session.
                  await askText("Rename session", { initial: plainTitle(row.title), action: "Rename", allowEmpty: true, onSubmit: onRename });
                }}>
                  <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                    <path d="M4 20h4L19 9l-4-4L4 16z" />
                  </svg>
                  Rename…
                </button>
                <button className={"head-pop-item" + (row.archived ? "" : " head-pop-danger")} onClick={() => { closeMore(true); onArchive(); }}>
                  <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                    <path d="M4 5h16v4H4zM6 9v10h12V9M10 13h4" />
                  </svg>
                  {row.archived ? "Unarchive" : "Archive…"}
                </button>
              </div>
            )}
          </div>
        </div>
      </header>
      {orbView === "why" && row.orb?.status === "failed" && <OrbFailure key={row.id} id={row.id} project={row.project} name={projects.find((p) => p.slug === row.project)?.name ?? row.orb.project} onRebuild={row.project ? rebuild : undefined} onRetry={onStopOrb ? async () => { if (!(await confirmStopOrb(row.jobs))) throw new Error("cancelled"); await api.stopOrb(row.id); } : undefined} rebuildErr={rebuildErr} />}
      {orbView === "build" && row.orb && <OrbBuildLog key={row.id} id={row.id} project={row.orb.project} onRebuild={row.project ? rebuild : undefined} rebuildErr={rebuildErr} />}

      <div className="scroll transcript" ref={scroller} onScroll={onScroll} onKeyDown={latestKey}
           onFocus={(e) => { const t = e.target as HTMLElement; if (t.matches("details.block > summary") && t !== rovingAt.current) rove(summaries(), t); }}
           tabIndex={0} role="region" aria-label="Transcript">
        {loading && loadError && (
          <ErrorNote className="transcript-state" err={loadError}
            title={/taking too long|timed? ?out|deadline/i.test(loadError!) ? "This session is taking too long" : "Couldn’t load this session"}
            action={onRetry && { label: "Retry", onClick: onRetry }} secondary={onBack && { label: "Show all sessions", onClick: onBack }}>
            {/taking too long|timed? ?out|deadline/i.test(loadError!) ? "The server has not answered yet." : undefined}
          </ErrorNote>
        )}
        {loading && !loadError && slow && <p className="meta-line transcript-state" role="status">Loading transcript…</p>}
        {!loading && turns.length === 0 && !stored.length && !running && !row.ask && !unlanded.length && (
          <div className="transcript-state thread-empty">
            <h2>{row.spawnedBy ? "This background agent has not run yet" : "Nothing here yet"}</h2>
            <p>Describe the next task below, or press / for skills.</p>
          </div>
        )}
        <EditPrompt.Provider value={editPrompt}>
        {turns.map((t, i) => (
          // Numbered by prompt, as the turn log counts: a leading /model
          // section has no prompt and no number.
          <TurnViewMemo key={t.seq} turn={t} n={turnNums[i]} superseded={t.prompt !== null && turns.slice(i + 1).some((u) => u.done)}
            // The preview belongs to the turn that is still open, so it
            // sits where the recorded entry will appear and is replaced
            // in place rather than jumping up the page.
            tail={i === turns.length - 1 && openTail ? <StreamView runs={stream} /> : undefined}
            // The turn says it is working once: on its running row of work, or under the tail when there is none.
            working={i === turns.length - 1 && openTail && running && !row.ask
              ? (stream.length && stream[stream.length - 1].kind === "thinking" ? "Thinking" : activity || (stream.length ? "Working" : WAITING_MODEL)) : undefined} />
        ))}
        </EditPrompt.Provider>
        {stored.map((l) => <JobBlock key={l.seq} line={l} />)}
        {unlanded.map((p) => (
          <SendingPrompt key={p.id} p={p} accepted={running && !p.steer} clamp={fullPending !== p.id} clipped={clipped[p.id]}
            onClip={(el) => { if (!clipped[p.id] && el.scrollHeight > el.clientHeight + 1) setClipped((m) => ({ ...m, [p.id]: true })); }}
            onToggle={() => setFullPending((v) => (v === p.id ? "" : p.id))}>
            {p === liveHost && liveBodyFn?.()}
          </SendingPrompt>
        ))}
        {!running && unlanded.some((p) => !p.steer) && (status === "Waiting" ? <WaitingModel since={unlanded.find((p) => !p.steer)?.at} /> : <WaitingDot />)}
        {liveBodyFn && !liveHost && <div className="turn">{liveBodyFn()}</div>}
        {row.ask && (
          <div className="ask" ref={ask}>
            <StatusMark status="needs-you" size={16} />
            {/* Questions carry paths and commands in backticks; raw, they read as noise. */}
            <div className="ask-q"><Markdown text={row.ask.text} /></div>
            {row.ask.secret && <SecretAnswer key={row.ask.id} askId={row.ask.id} onAnswer={onAnswer} />}
            {!row.ask.secret && row.ask.options?.length ? (
              <div className="ask-options">
                {/* Equal alternatives, so none of them is dressed as the primary action. */}
                {row.ask.options.map((o) => (
                  <button key={o} className="btn ask-option" disabled={busy || answering !== null}
                          onClick={() => deliver(o, true)}>
                    {answering?.ask === row.ask?.id && answering?.text === o ? "Submitting…" : o}
                  </button>
                ))}
              </div>
            ) : null}
          </div>
        )}
        <div ref={end} />
      </div>

      <div className="composer-wrap" ref={composerWrap}>
        {/* Archived stays readable, but says so before anything is typed into it. */}
        {row.archived && (
          <p className="archived-note composer-note" role="status">
            <strong className="composer-note-lead">Archived</strong>
            <span className="composer-note-body">This session is read-only until unarchived.</span>
            <button className="btn" onClick={onArchive}>Unarchive</button>
          </p>
        )}
        {row.ask && !askSeen && (
          <div className="ask-bar composer-note">
            <p><span className="composer-note-dot" aria-hidden="true" /><strong>Needs your answer</strong></p>
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
          <div className="send-failed composer-note composer-note-err" role="alert">
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
          <div key={failed.id ?? i} className="send-failed composer-note composer-note-err" role="alert">
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
                  <button className="link" onClick={() => { drop(failed); composer.current?.focus(); }}>Discard</button>
                </span>
              </div>
            </details>
            <span className="send-failed-actions">
              {/* The row goes with the button that had focus: focus lands in the composer, never on the page. */}
              <button className="btn" disabled={busy} onClick={() => { void deliver(failed.text, failed.answer, failed.ask, failed); composer.current?.focus(); }}>Retry</button>
              {/* Edit never lands on a newer draft: two prompts glued together is a third nobody wrote. */}
              <button className="btn composer-edit" disabled={Boolean(draft.trim())} title={draft.trim() ? "Send or clear the current draft first" : undefined}
                onClick={() => { toDraft(failed.text); setDraftAsk(failed.answer ? failed.ask ?? "" : ""); drop(failed); composer.current?.focus(); }}><span className="edit-word">Edit</span>{draft.trim() && <span className="edit-why">Clear the draft to edit</span>}</button>
            </span>
          </div>
        ))}
        {queued.length > 0 && (
          <ol className="queued" aria-label="Queued until the turn ends">
            {queued.map((m) => (
              <li key={m.id} className="queued-row">
                <span className="queued-tag" title="Queued">
                  <svg width="12" height="12" viewBox="0 0 12 12" fill="none" stroke="currentColor" strokeWidth="1.3" aria-hidden="true"><circle cx="6" cy="6" r="4.75" /><path d="M6 3.5V6l1.75 1.25" strokeLinecap="round" /></svg>
                  <span className="visually-hidden">Queued</span>
                </span>
                <span className="queued-text"><PromptWords text={m.text} /></span>
                <SentImages text={m.text} />
                {/* Edit never lands on a newer draft, as with a failed send. */}
                <button className="link composer-edit" disabled={Boolean(draft.trim())} title={draft.trim() ? "Send or clear the current draft first" : undefined}
                  onClick={() => { toDraft(m.text); setQueued((q) => q.filter((x) => x.id !== m.id)); composer.current?.focus(); }}><span className="edit-word">Edit</span>{draft.trim() && <span className="edit-why">Clear the draft to edit</span>}</button>
                <button className="link" onClick={() => setQueued((q) => q.filter((x) => x.id !== m.id))}>Remove</button>
              </li>
            ))}
          </ol>
        )}
        {/* The input on its own row, then one toolbar under it. */}
        <div className="composer composer-multi"
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
            aria-label={(row.archived ? "Read-only" : failedLoad ? "Waiting for the session to load" : row.ask?.secret ? "Answer in the secret field above" : row.ask && !askChanged ? "Type your answer…" : running || status === "Waiting" ? "Steer the running turn…" : row.spawnedBy ? "Message this background agent…" : "Describe the next task…").replace(/…$/, "")}
            // The list exists only once it has rows; a loading or failed picker is a status line with no id to point at.
            aria-controls={pickerOpen && activeOpt ? "mention-list" : undefined}
            aria-activedescendant={pickerOpen ? activeOpt : undefined}
            disabled={Boolean(row.ask?.secret) || row.archived}
            placeholder={row.archived ? "Read-only" : failedLoad ? "Waiting for the session to load" : row.ask?.secret ? "Answer in the secret field above" : row.ask && !askChanged ? "Type your answer…" : running || status === "Waiting" ? "Steer the running turn…" : row.spawnedBy ? "Message this background agent…" : "Describe the next task…"}
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
              if (escStops({ key: e.key, running: live, pickerOpen, composing: e.nativeEvent.isComposing })) {
                e.preventDefault();
                if (stopping !== "stopping") stop();
                return;
              }
              if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
                e.preventDefault();
                if (running && (e.metaKey || e.ctrlKey)) enqueue(); else send();
              }
            }} />
          <div className="composer-bar">
            <div className="composer-tools">
              <SkillPicker session={row.id} disabled={failedLoad} onPick={(name, known) => {
                // A skill runs only as the lead word, so a pick replaces a
                // skill already there, never a leading path like /tmp/x.
                setDraft((d) => {
                  const rest = d.trimStart(), lead = /^\/(\S+)\s*/.exec(rest);
                  return `/${name} ${lead && known.includes(lead[1]) ? rest.slice(lead[0].length) : rest}`;
                });
                document.getElementById("composer")?.focus();
              }} />
              <span className="composer-sep" aria-hidden="true" />
              <Controls row={row} projects={projects} onModel={onModel} onEffort={onEffort} onAssign={onAssign} only="model" catalogue={catalogue} disabled={failedLoad} />
              {/* Only the project's main thread hands work out, so only its composer offers it: a thread of the project, on a task, that reports back here. */}
              {startThread && (
                <button type="button" className="btn btn-sm composer-start-thread" disabled={failedLoad} onClick={() => { void (async () => {
                  const t = await askText("Start a thread", { body: "A new thread of the project takes this task; its finish note comes back to this thread.", placeholder: "What should the thread do?", action: "Start" });
                  if (!t?.trim()) return;
                  try { await startThread(t.trim()); setThreadNote("Thread started"); }
                  catch (e) { setThreadNote("Couldn’t start the thread: " + (e instanceof Error ? e.message : String(e))); }
                })(); }}>Start thread</button>
              )}
              {row.mode !== "project" && row.writable && (
                <span className="mode-local mode-badge" title={`File edits are allowed only inside ${row.writable}. The shell runs as you.`}><svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true"><path d="M4 20h4L19 9l-4-4L4 16v4z" /></svg><span className="mode-word">Edits {row.writable.split("/").pop()}</span></span>
              )}
              {/* The footer's "Start project session…" already says this session cannot write; the badge stays only when there is no such offer. */}
              {row.mode !== "project" && !row.writable && !((onStartProject && projects.length > 0) || onNewProject) && (
                <span className="mode-local mode-badge" title="Runs on this machine. Can edit files only inside a git checkout; read-only elsewhere."><svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" aria-hidden="true"><rect x="5" y="11" width="14" height="10" rx="2" /><path d="M8 11V7a4 4 0 0 1 8 0v4" /></svg><span className="mode-word">Read-only</span></span>
              )}
            </div>
            {uploading > 0 && <span className="attach-note" role="status"><span className="attach-spin" aria-hidden="true" />Attaching image…</span>}
            {threadNote && <span className={"attach-note" + (threadNote.startsWith("Couldn") ? " attach-err" : "")} role="status">{threadNote}</span>}
            {attachErr && <span className="attach-note attach-err" role="alert"><span className="attach-bang" aria-hidden="true">!</span>{attachErr}</span>}
            <div className="composer-actions">
              {/* Beside Send, so it never covers what you are reading. */}
              {down && !away && (
                <button className="btn jump-latest" onClick={toStart} title="Home" aria-label="Top">
                  <span aria-hidden="true">↑ </span>
                  <span className="jump-word">Top</span>
                </button>
              )}
              {away && (
                <button className="btn jump-latest" onClick={() => toLatest()} title={modKey() + "End"}
                        aria-label={newest > awayAt.current ? "New activity, jump to latest" : "Jump to latest"}>
                  <span aria-hidden="true">↓ </span>
                  <span className="jump-word">{newest > awayAt.current ? "New activity" : "Latest"}</span>
                </button>
              )}
              {/* One filled control: Stop is a square icon, Queue shows once there is a draft to queue. */}
              {/* Not before the transcript is read: the header names no status until then, and a Stop beside it claimed a turn it could not show. */}
              {live && !loading && (stopping === "failed"
                ? <button className="btn composer-stop-retry" onClick={stop} title="Stop (Esc)" aria-keyshortcuts="Escape">Retry stop</button>
                : <button className="btn btn-ghost composer-stop" disabled={stopping === "stopping"} onClick={stop}
                          aria-label={stopping === "stopping" ? "Stopping" : "Stop"} title="Stop (Esc)" aria-keyshortcuts="Escape">
                    {stopping === "stopping" ? <span className="composer-spin" aria-hidden="true" />
                      : <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true"><rect width="10" height="10" rx="2" fill="currentColor" /></svg>}
                  </button>)}
              {running && !draftAsk && !blank && (
                <button className="btn composer-queue" onClick={enqueue} disabled={uploading > 0 || askChanged}
                        title={modKey() + "Enter"}>Queue</button>
              )}
              <button className="btn btn-primary" onClick={send} disabled={loading || busy || uploading > 0 || blank || askChanged || row.archived}
                      title={failedLoad ? "Transcript didn’t load" : running && !draftAsk ? "Enter" : undefined}
                      aria-label={(blank ? row.ask : draftAsk) ? "Answer" : running ? "Steer" : "Send"}>
                <span className="send-word">{(blank ? row.ask : draftAsk) ? "Answer" : running ? "Steer" : "Send"}</span>
                <kbd className="send-key" aria-hidden="true">↵</kbd>
                <svg className="send-arrow" width="20" height="20" viewBox="0 0 20 20" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true"><path d="M10 16V4M5 9l5-5 5 5" /></svg>
              </button>
            </div>
          </div>
        </div>
        <div className="composer-foot">
          {status && (
            <span className={"composer-status composer-status-" + status.toLowerCase()}>
              <span className="composer-dot" aria-hidden="true" />{status === "Waiting" ? WAITING_MODEL : status}
            </span>
          )}
          {/* A session that can already edit its checkout has no reason to move; the offer is for read-only ones. */}
          {row.mode !== "project" && !row.writable && !failedLoad && (
            <span className="composer-local">
              {onStartProject && projects.length > 0 ? (
                <Select label="Start project session" value="" placeholder="Start project session…" align="start"
                        note="This session is read-only outside a git checkout. Your draft moves with you, unsent"
                        options={projects.map((p) => ({ value: p.slug, label: p.name }))}
                        onChange={(id) => onStartProject(id, expand(draft))} />
              ) : onNewProject && (
                // No project to run in yet: the palette's project flow; the draft stays here.
                <button type="button" className="btn composer-start" onClick={onNewProject}>Start project session…</button>
              )}
            </span>
          )}
          {!pickerOpen && (
            <span className="hint composer-hint">
              {(running && !draftAsk
                ? [["↵", "steer"], [[modKey() === "\u2318" ? "\u2318" : "Ctrl", "↵"], "queue"], [[modKey() === "\u2318" ? "\u21e7" : "Shift", "↵"], "newline"], ["Esc", "stop"]]
                : live ? [["↵", "send"], [[modKey() === "\u2318" ? "\u21e7" : "Shift", "↵"], "newline"], ["Esc", "stop"], ["/", "commands"]]
                : [["↵", "send"], [[modKey() === "\u2318" ? "\u21e7" : "Shift", "↵"], "newline"], ["/", "commands"], ["@", "files"]]
              ).map(([k, w]) => <span key={w as string} className="composer-key">{Array.isArray(k) ? <span className="keys-combo">{k.map((c) => <kbd key={c}>{c}</kbd>)}</span> : <kbd>{k}</kbd>} {w}</span>)}
            </span>
          )}
        </div>
      </div>
      {workOpen && (
        <WorkDialog workers={workers} sheet={sheet} anchor={workBtn} childState={kids.state} onRetryChildren={kids.retry}
                    paused={paused !== undefined} parent={row.id} onClose={closeWork} onView={viewInTranscript} onOpenAgent={openAgent} />
      )}
      {/* Transitions after the first load, batched: never a clock ticking. */}
      <span className="visually-hidden" role="status" aria-live="polite">{announce}</span>
    </div>
    </SessionChanges.Provider>
    </WorkContext.Provider>
  );
}

export default function App() {
  const [rows, setRows] = useState<Row[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  // For the list poll, which is not re-made per selection.
  const openRef = useRef(selected);
  openRef.current = selected;
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
  // Sessions this page created, by when. A project thread is answered
  // before its child has written a line — its container is still being
  // prepared — so a 404 on one of these is "starting", not "not found",
  // and the read is tried again until it lands.
  const created = useRef(new Map<string, number>());
  const [starting, setStarting] = useState(false);
  const startingFor = useRef<string | null>(null);
  const STARTING_MS = 120_000;
  const [paused, setPaused] = useState<number | undefined>(undefined);
  const [loadTry, setLoadTry] = useState(0);
  // The session whose last read failed (not found, or could not load).
  // A failure is not an answer to keep: coming back to its link asks
  // again, where the same selection alone would not re-read it.
  const failedLookup = useRef<string | null>(null);
  const retryRef = useRef<() => void>(() => {});
  useEffect(() => { lastSeq.current = lines.length ? lines[lines.length - 1].seq : 0; }, [lines]);
  // Live fragments of the reply being written, newest last. Never
  // merged into `lines`: these carry no history seq and the recorded
  // entry always supersedes them.
  const [stream, setStream] = useState<DeltaRun[]>([]);
  const [activity, setActivity] = useState("");
  // The call in flight, from the tools plugin's live start event: what the turn is doing, from the runtime rather than a guess.
  const [runningCall, setRunningCall] = useState<RunningCall | null>(null);
  // An engine session's calls in flight, by call id, with their live output. Several run at
  // once, each is a row of its own, and none is recorded until it ends.
  const [nativeRunning, setNativeRunning] = useState<Map<string, NativeRun>>(() => new Map());
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
  // The toast stays mounted for its exit, marked data-leaving.
  const [toast, setToast] = useState<(NonNullable<typeof err> & { leaving?: boolean }) | null>(null);
  useEffect(() => {
    if (err) { setToast(err); return; }
    setToast((t) => (t ? { ...t, leaving: true } : t));
    const id = window.setTimeout(() => setToast(null), 130);
    return () => clearTimeout(id);
  }, [err]);
  const [loadErr, setLoadErr] = useState<string | null>(null);
  // When the list last refreshed; null until the first read lands.
  const [loadedAt, setLoadedAt] = useState<number | null>(null);
  const [looked, setLooked] = useState<Row | null>(null);
  const [view, setView] = useState<View>("sessions");
  // `bough update` restarts the control room, but this page keeps the UI
  // it loaded with. The server says which build answered; when that
  // changes, the page offers the reload rather than looking unfixed.
  const [updated, setUpdated] = useState(false);
  useEffect(() => { watchBuild(() => setUpdated(true)); }, []);
  // Where the next new conversation runs; local unless someone picks a project.
  const [newMode, setNewMode] = useState<ModeValue>({ mode: "local" });
  const [orbOpen, setOrbOpen] = useState<string>();
  // The project whose page is open (#/projects/<slug>).
  const [projectSlug, setProjectSlug] = useState("");
  // The thread the page was asked to open (#/projects/<slug>/t/<id>).
  const [projectFocus, setProjectFocus] = useState<{ id: string; at: number }>();
  const [projects, setProjects] = useState<Project[]>([]);
  // Only a narrow window reads this (see the 720px media query): a
  // phone shows the list or the thread, never both.
  const [pane, setPane] = useState<"list" | "thread">("list");
  const [reveal, setReveal] = useState<{ id: string; at: number } | null>(null);
  // The Context panel takes over the thread pane for the open session,
  // and closes when a different one is opened.
  // A session's sub-page: its context inspector or its changes review.
  const [sub, setSub] = useState<"context" | "changes" | "portal" | null>(null);
  const context = sub === "context";
  const [wikiRoute, setWikiRoute] = useState<WikiRoute>({ at: "index" });
  // A hash route nothing here knows; null on every known one.
  const [lost, setLost] = useState<string | null>(null);
  // The review count on the nav item. Polled slowly: it changes when an
  // ingest lands, which is minutes apart at the fastest.
  const [wikiFlags, setWikiFlags] = useState(0);
  useEffect(() => {
    const load = () => wikiApi.index()
      .then((ix) => setWikiFlags(ix.health.unsupported + ix.health.superseded + ix.health.uncited))
      // A failed read is not "nothing to review": keep the last count.
      .catch(() => {});
    load();
    const t = setInterval(() => { if (!document.hidden) load(); }, 60_000);
    return () => clearInterval(t);
  }, []);

  // Only the newest read lands: an answer for the list before Archived was
  // opened never overwrites the one that includes it.
  const readSeq = useRef(0);
  const projectsAt = useRef(0);
  // A poll never stacks on a read still out: one list request at a time.
  const inFlight = useRef(false);
  const refresh = useCallback(async (poll = false) => {
    if (poll && inFlight.current) return;
    // Each read lands on its own: a projects outage must not freeze the
    // fleet. Only the fleet's freshness is reported, in one place.
    // Projects change by hand, rarely: a poll reads them every 30s, an action at once.
    // An action's read is awaited whole: a dialog that closes on it must
    // hand focus back to the list as it now is, not the one it replaces.
    let projectsRead: Promise<void> | undefined;
    if (!poll || Date.now() - projectsAt.current > 30_000) { projectsAt.current = Date.now(); projectsRead = api.projects().then(setProjects, () => {}); }
    const seq = ++readSeq.current;
    inFlight.current = true;
    try {
      const rs = await api.sessions(archived);
      if (seq !== readSeq.current) return;
      setRows(rs); setLoadErr(null); setRowsAll(archived); setLoadedAt(Date.now());
      // The open session can leave the list (archiving it does) while it
      // stays on screen, and its row is then `looked`, which only its
      // event stream refreshed: an archived session sends none, so the
      // thread kept saying it was not archived and kept counting agents
      // that had stopped. Read its row with the list instead.
      const open = openRef.current;
      if (open && !rs.some((r) => r.id === open)) {
        api.session(open, lastSeq.current).then((r) => { if (openRef.current === open) setLooked(r.session); }, () => {});
      }
    } catch (e) { if (seq === readSeq.current) setLoadErr(e instanceof Error ? e.message : String(e)); }
    finally { if (seq === readSeq.current) inFlight.current = false; if (!poll) await projectsRead; }
  }, [archived]);

  // While a session's event stream is open it carries that session's
  // changes, so the list poll slows down.
  const streaming = selected !== null;
  const mountedRefresh = useRef(false);
  useEffect(() => {
    // The first read is the mount's; a later change (Archived) reads at once too.
    if (!mountedRefresh.current || !streaming) void refresh();
    mountedRefresh.current = true;
    // A hidden tab does not poll; coming back reads at once.
    const t = setInterval(() => { if (!document.hidden) void refresh(true); }, streaming ? POLL_MS * 3 : POLL_MS);
    const back = () => { if (!document.hidden) void refresh(true); };
    document.addEventListener("visibilitychange", back);
    return () => { clearInterval(t); document.removeEventListener("visibilitychange", back); };
  }, [refresh, streaming]);

  useEffect(() => {
    if (!selected) return;
    let live = true;
    // Never show one session's transcript under another's header while loading.
    setLines([]);
    setLoadedFor(null);
    lastSeq.current = 0; // the cursor belongs to the session just left
    // Starting holds across its own retries: a thread that is booting is
    // listed already, and clearing this on each try flashed its row's
    // "Loading transcript…" between them.
    setLoadFail(null); setPaused(undefined); setMissing(false);
    if (startingFor.current !== selected) setStarting(false);
    let retry: ReturnType<typeof setTimeout> | undefined;
    // Its failure is the transcript's own state, with a retry, not a toast.
    api.session(selected).then((r) => {
      if (!live) return;
      created.current.delete(selected);
      failedLookup.current = null;
      setStarting(false);
      setLines(r.entries);
      setLoadedFor(selected);
      setLooked(r.session);
      setRows((prev) => prev.map((x) => (x.id === r.session.id ? r.session : x)));
    }).catch((e) => {
      if (!live) return;
      const gone = (e as { status?: number }).status === 404;
      const since = created.current.get(selected);
      if (gone && since !== undefined && Date.now() - since < STARTING_MS) {
        startingFor.current = selected;
        setStarting(true);
        retry = setTimeout(() => setLoadTry((n) => n + 1), 1000);
        return;
      }
      setStarting(false);
      failedLookup.current = selected;
      setLoadFail(e instanceof Error ? e.message : String(e)); setMissing(gone);
    });

    setStream([]);
    setNativeRunning(new Map());
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
      // The engine's own "model is thinking" only says a request is out
      // and nothing came back: that is Waiting (worded the same on the
      // page), and as a label of work it turned the header to Working.
      if (ev.kind === "activity") { setActivity(ev.text === ENGINE_WAITING ? "" : ev.text); return; }
      // An engine's call carries the provider's call id (a string); the loop's per-block calls number theirs.
      const native = (ev.kind === "call" || ev.kind === "sub:call") && typeof ev.extra?.id === "string";
      // Live only, never refetched: a native call's start and its streamed output.
      if (ev.kind === "call-delta" || (native && ev.extra?.phase === "start")) {
        setNativeRunning((m) => liveNative(m, ev));
        return;
      }
      // A native call's end falls through: it is recorded, and the refetch below brings the row that replaces the running one.
      if (!native && (ev.kind === "call" || ev.kind === "sub:call")) {
        // A start is live only (nothing to catch up on); an end is recorded and refetched below.
        if (ev.extra?.phase === "start") { setRunningCall({ kind: ev.kind, tool: String(ev.extra.tool ?? ""), detail: ev.text, at: ev.at }); return; }
        setRunningCall(null);
      } else if (ev.kind !== "assistant-delta" && ev.kind !== "thinking-delta") setRunningCall(null); // the block moved on
      if (ev.kind === "assistant-delta" || ev.kind === "thinking-delta" || ev.kind === "delta-reset") {
        if (ev.kind !== "delta-reset") setActivity(""); // the program it named is over; its label must not come back
        // A reset leaves nothing on screen for the next record to supersede.
        else superseded = 0;
        const sealed = superseded;
        setStream((prev) => {
          const next = streamAfter(prev, ev, sealed);
          runs = next.length;
          return next;
        });
        return;
      }
      if (ev.kind === "done" || ev.kind === "cancelled") setNativeRunning((m) => liveNative(m, ev));
      superseded = runs;
      clearTimeout(timer);
      timer = setTimeout(catchUp, 120);
    });
    retryRef.current = () => { clearTimeout(timer); backoff = 4000; catchUp(); };
    return () => { live = false; clearTimeout(retry); clearTimeout(timer); stop(); };
  }, [selected, loadTry]);

  // The filter reads transcripts too, as ⌘K does: what you remember is
  // often something said ("bg-done"), which no title or branch holds.
  const { hits: textHits } = useFullText(query, true);
  const said = useMemo(() => new Map(textHits.filter((h) => h.lines.length).map((h) => [h.id, h.lines[0]])), [textHits]);
  const visible = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return rows;
    // The id is searchable too: an untitled session shows only its id tail.
    return rows.filter((r) => getSearchMatch(r, q) || said.has(r.id));
  }, [rows, query, said]);

  // A linked session the list does not hold (archived, not yet listed) is
  // still the session: its own lookup fills in.
  const row = rows.find((r) => r.id === selected) ?? (looked?.id === selected ? looked : null);

  // The tab says where you are.
  const rowName = row ? sessionTitle(row) : "";
  useEffect(() => {
    const page = lost !== null && view === "sessions" && !selected ? "Page not found"
      : view === "project" ? (projects.find((p) => p.slug === projectSlug)?.name ?? projectSlug)
      : view === "projects" ? "Projects"
      : view === "hooks" ? "Hooks"
      : view === "me" ? "Me"
      : view === "wiki" ? (wikiRoute.at === "page" ? `${wikiRoute.path.split("/").pop()!.replace(/\.md$/, "")} · Wiki`
        : wikiRoute.at === "review" ? "Review · Wiki" : wikiRoute.at === "activity" ? "Activity · Wiki" : "Wiki")
      : rowName;
    document.title = page ? `${page} · bough` : "bough";
  }, [lost, view, selected, wikiRoute, rowName, projectSlug, projects]);

  // A preview outlives its turn only if the entry it was previewing
  // never arrived. Once the session is no longer running there is
  // nothing left to be a preview of. The same goes for native calls'
  // running rows: done/cancelled clear them, but a child that died
  // mid-call (a crash, an archive kill, a serve restart) sends neither,
  // and its row spun on in an Interrupted session. Needs-you is not that:
  // the ask or secret call that put the row there is still running, and
  // dropping its row took the Ask block out of the transcript.
  const status = row?.status;
  useEffect(() => {
    if (!status || status === "running") return;
    setStream([]);
    setActivity("");
    if (status !== "needs-you") setNativeRunning((m) => (m.size ? new Map() : m));
  }, [status]);

  const [palette, setPalette] = useState(false);
  // What the palette opens with, when something other than ⌘K opened it
  // (Review's "Search history" hands it the claim).
  const [palQuery, setPalQuery] = useState("");
  // ⌘K is everything; ⌘P switches sessions; ⌥N starts one.
  const [palMode, setPalMode] = useState<"all" | "switch" | "new">("all");
  const [home, setHome] = useState("");
  useEffect(() => { api.home().then(setHome).catch(() => setHome("")); }, []);
  // Where serve was started: usually the repo someone ran `bough serve` in.
  // New sessions offer it first when it is a checkout they could edit;
  // home is a read-only start, which is not what a first New should be.
  const [startDir, setStartDir] = useState<{ path: string; checkout?: string } | null>(null);
  useEffect(() => { api.setup().then((s) => setStartDir(s.folder.exists ? s.folder : null)).catch(() => {}); }, []);

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
      if (h === "hooks" || h === "projects" || h === "me") { setLost(null); setView(h); setSub(null); setPane("thread"); if (h === "projects") setOrbOpen(undefined); return; }
      const po = /^projects\/([^/]+)\/orb$/.exec(h);
      if (po) {
        setLost(null); setView("projects"); setSub(null); setPane("thread");
        // A link minted when projects were labels names an id nothing
        // resolves; the project list is where it meant to go.
        setOrbOpen(OLD_PROJECT_ID.test(po[1]) ? undefined : po[1]);
        // In place: assigning location.hash pushed, so Back landed on the
        // old link and was redirected forward again.
        if (OLD_PROJECT_ID.test(po[1])) window.history.replaceState(null, "", "#/projects");
        return;
      }
      const ps = /^projects\/([^/]+)(?:\/t\/([^/]+))?$/.exec(h);
      if (ps) {
        // A label id from before the re-key, or anything that is not a
        // slug, names no project: the list is where that link meant to go.
        // Replaced, not pushed (see the orb link above); replaceState fires no hashchange, so read again.
        if (OLD_PROJECT_ID.test(ps[1]) || !SLUG.test(ps[1])) { window.history.replaceState(null, "", "#/projects"); read(); return; }
        // The page picks the session it shows (main, or a thread) once it
        // has read the project; whatever was open elsewhere is not it.
        setLost(null); setView("project"); setProjectSlug(ps[1]); setSelected(null); setSub(null); setPane("thread");
        setProjectFocus(ps[2] ? { id: ps[2], at: Date.now() } : undefined);
        return;
      }
      const wr = parseWikiHash(h);
      if (wr) { setLost(null); setView("wiki"); setWikiRoute(wr); setSub(null); setPane("thread"); return; }
      const m = /^s\/([^/]+)\/?(context|changes|portal)?(?:\?.*)?$/.exec(h);
      if (m) {
        if (failedLookup.current === m[1]) setLoadTry((n) => n + 1);
        setView("sessions"); setSelected(m[1]); setSub((m[2] as "context" | "changes" | "portal" | undefined) ?? null); setPane("thread");
      } else if (h === "") {
        // No session named: on a phone that is the list. The thread pane
        // held only "Choose a session", with no list and no way back to it.
        setView("sessions"); setSelected(null); setSub(null); setPane("list");
      } else {
        // A route nothing here knows is a page not found, never a session lookup.
        setView("sessions"); setSelected(null); setSub(null); setPane("thread"); setLost(h);
        return;
      }
      setLost(null);
    };
    read();
    // Any navigation of yours beats opening a session on arrival: going to
    // #/ while the first load was still out bounced into the latest session.
    const nav = () => { arrived.current = true; read(); requestAnimationFrame(() => focusAppOnRoute(document)); };
    window.addEventListener("popstate", nav);
    window.addEventListener("hashchange", nav);
    return () => {
      window.removeEventListener("popstate", nav);
      window.removeEventListener("hashchange", nav);
    };
  }, []);

  const routed = useRef(false);
  // Writing it back is replaceState, not push: every keystroke in the
  // sidebar filter would otherwise become a history entry to walk back
  // through. Opening a conversation pushes (see openSession).
  useEffect(() => {
    if (lost !== null) return; // the unknown route stays in the URL it came from
    const want = view === "hooks" ? "#/hooks"
      : view === "me" ? "#/me"
      // The session the page shows (ProjectView's onShow), not the one it
      // was opened on: from the focus, switching threads or '‹ All
      // threads' left the URL on the first thread, and a thread opened
      // from the project's home was in no URL, so a reload or a shared
      // link lost it.
      : view === "project" ? (selected ? `#/projects/${projectSlug}/t/${selected}` : `#/projects/${projectSlug}`)
      : view === "projects" ? (orbOpen ? `#/projects/${orbOpen}/orb` : "#/projects")
      : view === "wiki" ? `#/${wikiHash(wikiRoute)}`
      : selected ? `#/s/${selected}${sub ? `/${sub}` : ""}`
      : "#/";
    const first = !routed.current;
    routed.current = true;
    const next = hashToReplace(window.location.hash, want, sub, first);
    if (next !== null) window.history.replaceState(null, "", next);
  }, [view, selected, sub, wikiRoute, lost, projectSlug]);

  // Moving around the wiki pushes, like opening a conversation: Back
  // from a cited entry returns to the page, and from a page to the index.
  const goWiki = useCallback((r: WikiRoute) => {
    setLost(null); setWikiRoute(r); setView("wiki"); setSub(null); setPane("thread");
    const want = `#/${wikiHash(r)}`;
    if (window.location.hash !== want) window.history.pushState(null, "", want);
  }, []);

  usePaletteKey(useCallback(() => { setPalMode("all"); setPalette(true); }, []));
  const narrow = useMedia("(max-width:720px)");
  // A finish is seen once its transcript is on screen: the ack is the
  // same one "Mark seen" sends, so it survives a reload. A hidden tab has
  // seen nothing; loadedAt re-runs this on the read that coming back makes.
  const viewing = row && (view === "project" || (view === "sessions" && (!sub || sub === "portal"))) && (!narrow || pane === "thread") ? row.id : "";
  const viewingUnseen = Boolean(viewing && row?.unseen);
  useEffect(() => {
    if (!viewingUnseen || document.hidden) return;
    api.ack(viewing).then(() => refresh(), () => {});
  }, [viewing, viewingUnseen, loadedAt, refresh]);
  const onView = (v: View) => {
    setLost(null);
    if (v === "wiki") goWiki({ at: "index" });
    else if (v === "sessions") { if (narrow) goList(); else { setView(v); setSub(null); } }
    else { setView(v); setSub(null); setPane("thread"); if (window.location.hash !== NAV_HREF[v]) window.history.pushState(null, "", NAV_HREF[v]); }
  };

  // Back on a phone goes to the list and says so in the URL: it only
  // swapped panes, so a reload landed back in the thread it had left.
  const goList = useCallback(() => {
    setLost(null); setSelected(null); setSub(null); setView("sessions"); setPane("list");
    if (window.location.hash !== "#/") window.history.pushState(null, "", "#/");
  }, []);

  // Arriving at Home on a desktop opens the session that most needs you,
  // else the most recent, once: an empty page beside the list said nothing.
  // Opening does not mark anything seen.
  const arrived = useRef(false);
  const mountedAt = useRef(Date.now());
  useEffect(() => {
    if (arrived.current || loadedAt === null) return;
    arrived.current = true;
    // A list that answers late finds the overview already on screen: leave it there, never swap it away.
    if (loadedAt - mountedAt.current > 400) return;
    if (window.location.hash.replace(/^#\/?/, "") !== "" || window.matchMedia?.("(max-width:720px)").matches) return;
    const top = arrivalPick(rows, Date.now());
    if (top) { setSelected(top.id); setPane("thread"); }
  }, [loadedAt, rows]);

  // The session last opened, so the list comes back with your place in it.
  const [lastId, setLastId] = useState<string | null>(null);
  // A turn picked from a session's log, for its thread to scroll to.
  const [jump, setJump] = useState<{ id: string; turn: number; at: number; seq?: number; q?: string } | null>(null);
  useEffect(() => { if (selected) setLastId(selected); }, [selected]);
  // Sessions in the order you were last in them, for ⌘P.
  const [visited, setVisited] = useState<string[]>([]);
  useEffect(() => { if (selected) setVisited((v) => v[0] === selected ? v : visit(v, selected)); }, [selected]);

  // Esc closes the Context and Changes pages, unless something nearer owns it.
  // It never leaves the session: in the composer it stops a running turn.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape" || e.defaultPrevented || palette || view !== "sessions" || !selected || !sub) return;
      const a = document.activeElement as HTMLElement | null;
      if (a && (a.tagName === "TEXTAREA" || a.tagName === "INPUT" || a.isContentEditable)) return;
      if (document.querySelector(".dlg-scrim, .sel-pop, .skills-pop, .mention")) return;
      e.preventDefault();
      setSub(null);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [palette, view, selected, sub]);

  // ? shows every shortcut, from anywhere that is not taking typing.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.defaultPrevented || palette || !sheetKey(e)) return;
      e.preventDefault();
      showShortcuts(modKey());
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [palette]);

  // ⌘P switches session, ⌥N starts one, ⌥I goes to the composer: from anywhere, a text field included.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.defaultPrevented || document.querySelector(".dlg-scrim")) return;
      if (switchKey(e, isMac())) { e.preventDefault(); setPalCwd(""); setPalMode("switch"); setPalette(true); }
      else if (newSessionKey(e)) { e.preventDefault(); setPalCwd(""); setPalMode("new"); setPalette(true); }
      else if (focusComposerKey(e) && !palette) {
        const c = document.getElementById("composer");
        if (c) { e.preventDefault(); c.focus(); }
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [palette]);

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

  /**
   * Which session the project page shows. It is the App's `selected`, so
   * the transcript, the event stream and the composer are the ones the
   * sessions view uses — the page only says which thread they are for.
   */
  const showProjectSession = useCallback((id: string) => setSelected(id || null), []);

  // A project session belongs on its project's page, with the page's
  // main thread and the other threads beside it: opened as a plain
  // session it is a conversation with no room around it.
  const goProject = useCallback((slug: string, id?: string) => {
    setLost(null); setView("project"); setProjectSlug(slug); setSelected(null); setSub(null); setPane("thread");
    setProjectFocus(id ? { id, at: Date.now() } : undefined);
    const hash = id ? `#/projects/${slug}/t/${id}` : `#/projects/${slug}`;
    if (window.location.hash !== hash) window.history.pushState(null, "", hash);
  }, []);

  const openSession = useCallback((id: string) => {
    setLost(null); setSelected(id); setSub(null); setView("sessions"); setPane("thread");
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
    // The list shows the change as soon as the server takes it: a poll
    // already in flight could otherwise land stale after act's refresh.
    // The open session's own lookup too: once the refresh drops an
    // archived row from the list, the page shows that copy, and a session
    // with no child sends no event that would re-read it.
    const mark = (archived: boolean) => {
      setRows((rs) => rs.map((x) => x.id === r.id ? { ...x, archived } : x));
      setLooked((l) => l?.id === r.id ? { ...l, archived } : l);
    };
    if (r.archived) return act(async () => { await api.unarchive(r.id); mark(false); }, "unarchive");
    const n = (r.agents?.running ?? 0) + (r.agents?.queued ?? 0);
    if (n === 0) {
      const ok = await askConfirm("Archive this session?", `“${sessionTitle(r)}” leaves the list. It stays under Archived and can be restored.`,
        { action: "Archive", safe: true });
      return ok ? act(async () => { await api.archive(r.id); mark(true); }, "archive") : false;
    }
    const pick = await askChoice(`Archive ${sessionTitle(r)}?`,
      `Stop its ${agentWords(r.agents?.running ?? 0, r.agents?.queued ?? 0)} too?`, ["Stop and archive", "Archive only"]);
    if (!pick) return false;
    return act(async () => { await api.archive(r.id, { stopChildren: pick === "Stop and archive" }); mark(true); }, "archive");
  };

  // What the chrome can do, the keyboard can do. Session-scoped
  // commands only appear when one is open, so the list never offers
  // something that would fail.
  const markCreated = (id: string) => { created.current.set(id, Date.now()); };
  // A create answers only once its child has written history (up to
  // serve's 10 s create timeout). The palette has closed by then, and
  // with nothing on screen the page looked idle while a child was starting.
  const [creating, setCreating] = useState(false);
  const start = (cwd: string, prompt: string, mode?: ModeValue) => act(async () => {
    setCreating(true);
    try {
      const created = await api.create(cwd, prompt, mode?.mode, mode?.project);
      markCreated(created.id);
      // The prompt shows where it will land while the new row is fetched.
      if (prompt.trim()) setPending((m) => ({ ...m, [created.id]: [{ id: created.id, text: prompt, after: 0, at: new Date().toISOString() }] }));
      if (mode?.mode === "project" && mode.project) goProject(mode.project, created.id);
      else openSession(created.id);
      // The route moved by pushState, which fires no hashchange: a palette
      // opened while this was starting would stay up over the new session.
      closePalette();
    } finally { setCreating(false); }
  }, "start a session");
  // New starts where the open session works, and the new session's header
  // shows that folder before anything is sent. With none open, the palette
  // asks where: never a silent Home.
  // Nothing is created until the first prompt: New opens the palette aimed
  // at that folder, and typing a prompt there starts the session. A click
  // alone used to leave an empty session behind every time.
  const [palCwd, setPalCwd] = useState("");
  // One function for the life of the page: the palette resubscribes its
  // close-on-navigate listeners whenever onClose changes, and a fresh
  // (max-width:720px) query made after the viewport crossed it never
  // fires, so a resize past 720 px left the palette up.
  const closePalette = useCallback(() => { setPalette(false); setPalQuery(""); setPalCwd(""); setPalMode("all"); }, []);
  // A project session's cwd is its orb worktree: never a folder to start a host session in.
  const rowDir = row?.orb ? home : row?.cwd;
  const newSession = () => { setPalCwd(rowDir ?? ""); setPalMode("new"); setPalette(true); };
  // "auto" shows the welcome while the server has no sessions; "on" is the
  // palette asking for it again; "off" is skipped or already used.
  const [welcome, setWelcome] = useState<"auto" | "on" | "off">(() => (welcomeDismissed() ? "off" : "auto"));
  const showWelcome = welcome === "on" || (welcome === "auto" && loadedAt !== null && !rows.some((r) => !r.archived));
  // A phone opens on the list pane, which on an empty server is one line
  // of "No sessions yet."; the welcome lives in the thread pane.
  useEffect(() => { if (showWelcome) setPane("thread"); }, [showWelcome]);
  const folderName = (p: string) => p.replace(/\/+$/, "").split("/").pop() || p;

  // The newest call whose recorded exit was not zero, in the open session.
  let latestFail: number | undefined;
  for (let i = lines.length - 1; row && i >= 0; i--) {
    const x = lines[i].data?.exit;
    if (lines[i].kind === "result" && typeof x === "number" && x !== 0) { latestFail = lines[i].seq; break; }
  }
  const commands: Command[] = [
    ...(home && rowDir && rowDir !== home ? [{
      id: "new:cwd", group: "Start", label: `New session in ${folderName(rowDir)}`, suggest: true,
      hint: "Folder: " + shortPath(rowDir, home),
      run: () => start(rowDir, ""),
    }] : []),
    ...(home && startDir?.checkout && startDir.path !== home && startDir.path !== rowDir ? [{
      id: "new:start", group: "Start", label: `New session in ${folderName(startDir.path)}`, suggest: !rowDir || rowDir === home,
      hint: "Folder: " + shortPath(startDir.path, home) + " · can edit",
      run: () => start(startDir.path, ""),
    }] : []),
    ...(home ? [{
      id: "new:here", group: "Start", label: "New session in home", suggest: (!rowDir || rowDir === home) && !startDir?.checkout,
      hint: "Folder: " + shortPath(home, home),
      run: () => start(home, ""),
    }] : []),
    // Every folder sessions ran in, so New is never only home.
    ...(home ? startFolders(rows, home).filter((p) => p !== rowDir && p !== startDir?.path).map((p) => ({
      id: `new:dir:${p}`, group: "Start", label: `New session in ${folderName(p)}`,
      hint: "Folder: " + shortPath(p, home), run: () => start(p, ""),
    })) : []),
    // A project session runs in that project's orb; home is only where serve records it.
    ...(home ? projects.map((p) => ({
      id: `new:orb:${p.slug}`, group: "Start", label: `New session in ${p.name}`,
      hint: "orb", run: () => start(home, "", { mode: "project" as const, project: p.slug }),
    })) : []),
    ...(home ? [{
      id: "new:folder", group: "Start", label: "New session in a folder…", hint: "a git checkout can be edited",
      run: async () => {
        const p = await askText("Start in folder", { initial: shortPath(rowDir ?? home, home), placeholder: "~/code/your-repo", action: "Start" });
        if (p?.trim()) start(p.trim().replace(/^~(?=\/|$)/, home), "");
      },
    }] : []),
    { id: "help:welcome", group: "Navigation", label: "Show the welcome", hint: "connect a model, pick a folder",
      run: () => { setWelcome("on"); goList(); } },
    { id: "new:project", group: "Start", label: "New project…",
      run: async () => {
        const n = await askText("New project", { placeholder: "What is this work?", action: "Create" });
        if (n) act(() => api.newProject(n));
      } },
    { id: "go:sessions", group: "Navigation", label: "Sessions", run: () => goList() },
    // The nav links' own move, so they push: setting the view alone let
    // the write-back replace the entry, and Back skipped the page you left.
    { id: "go:projects", group: "Navigation", label: "Projects", run: () => onView("projects") },
    { id: "go:hooks", group: "Navigation", label: "Hooks", run: () => onView("hooks") },
    { id: "go:wiki", group: "Navigation", label: "Wiki", run: () => goWiki({ at: "index" }) },
    { id: "help:keys", group: "Navigation", label: "Keyboard shortcuts", hint: "?",
      run: () => showShortcuts(modKey()) },
    // ⌘P elsewhere: here it is this palette again, cleared, where titles are searched.
    { id: "go:switch", group: "Navigation", label: "Switch session", hint: `${modKey()}P`,
      run: () => requestAnimationFrame(() => { setPalMode("switch"); setPalette(true); }) },
    { id: "go:composer", group: "Navigation", label: "Focus the composer", hint: modKey() === "\u2318" ? "\u2325I" : "Alt+I",
      run: () => requestAnimationFrame(() => document.getElementById("composer")?.focus()) },
    { id: "go:side", group: "Navigation", label: "Toggle sidebar", hint: `${modKey()}B`,
      run: () => window.dispatchEvent(new Event("bough:toggle-side")) },
    // The composer's own model picker, opened: one place sets the next turn's model.
    ...(view === "sessions" ? [{ id: "go:model", group: "Navigation", label: "Switch model…", hint: "next turn",
      run: () => requestAnimationFrame(() => {
        const pick = [...document.querySelectorAll<HTMLButtonElement>('.composer-tools button[aria-label^="Next turn model"]')].find((b) => b.offsetParent);
        pick?.scrollIntoView({ block: "nearest" });
        pick?.click();
      }) }] : []),
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
    // Only while a session is on screen: on the wiki or hooks pages "this
    // session" was the last one visited, which nothing on screen showed.
    ...(row && view === "sessions" ? [
      { id: "s:changes", group: "This session", label: "Review changes", suggest: true,
        hint: "Session edits", run: () => { setSub("changes"); setPane("thread"); } },
      { id: "s:context", group: "This session", label: "Inspect context", suggest: true,
        hint: "Context", run: () => { setSub("context"); setPane("thread"); } },
      { id: "s:portal", group: "This session", label: "Open portal", suggest: row.mode === "project",
        hint: "Server in the orb", run: () => { setSub("portal"); setPane("thread"); } },
      // Searchable only, and it asks first: never one Enter away from an empty box.
      { id: "s:archive", group: "This session",
        label: row.archived ? "Unarchive this session" : "Archive this session",
        destructive: !row.archived,
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

  /**
   * The conversation, wherever it is shown. The sessions view gives it the
   * whole pane; the project page puts the same element in its middle
   * column, on the same stream and the same composer — a thread opened
   * there is not a second, lesser view of it.
   */
  // Running native calls sit after the recorded lines as rows of the live turn, until their own record lands.
  const shownLines = useMemo(() => withRunningCalls(lines, nativeRunning), [lines, nativeRunning]);
  const threadFor = (r: Row) => (
    <RunningCallCtx.Provider key={r.id} value={runningCall}>
    <Thread key={r.id} row={r} lines={shownLines} jump={jump?.id === r.id ? jump : null} loading={loadedFor !== r.id} loadError={loadFail ?? undefined} paused={paused}
      onRetry={() => (loadedFor === r.id ? retryRef.current() : setLoadTry((n) => n + 1))} stream={stream} activity={runningCall ? callStep({ data: { tool: runningCall.tool }, text: runningCall.detail }) : activity} projects={projects} busy={busy || Boolean(locked[r.id])} onBack={goList}
      sending={pending[r.id] ?? []}
      setSending={(f) => setPending((m) => ({ ...m, [r.id]: f(m[r.id] ?? []) }))}
      onSend={(t) => deliverTo(r.id, () => api.prompt(r.id, t))}
      onAnswer={(t, ask) => deliverTo(r.id, () => api.answer(r.id, t, ask))}
      onInterrupt={() => act(() => api.interrupt(r.id), "stop the turn")}
      onArchive={() => archiveRow(r)}
      rows={rows} onOpenSession={openSession}
      onRename={async (t) => { await api.rename(r.id, t); await refresh(); }}
      onModel={(m, plugin) => act(() => api.model(r.id, m, plugin), "change model")}
      onEffort={(e) => act(() => api.effort(r.id, e), "change effort")}
      onAssign={(p) => act(() => api.assign(r.id, p), "move the session")}
      onContext={() => setSub("context")}
      onPortal={() => setSub("portal")}
      onAck={() => act(() => api.ack(r.id), "mark it seen")}
      onStopOrb={async () => { if (await confirmStopOrb(r.jobs)) act(() => api.stopOrb(r.id), "stop the orb"); }}
      onNewProject={() => { setPalQuery("New project"); setPalette(true); }}
      onStartProject={home ? async (project, draft) => { if (await confirmFailedBuild(projects.find((p) => p.slug === project))) act(async () => {
        // The draft moves, unsent: the new session's composer holds it.
        const created = await api.create(home, "", "project", project);
        markCreated(created.id);
        try {
          if (draft.trim()) localStorage.setItem("bough:draft:" + created.id, draft);
          localStorage.removeItem("bough:draft:" + r.id);
          localStorage.removeItem("bough:draft-atts:" + r.id);
        } catch { /* storage off */ }
        goProject(project, created.id);
      }, "start a project session"); } : undefined} />
    </RunningCallCtx.Provider>
  );

  return (
    <div className="app" data-pane={pane} tabIndex={-1}>
      {row && !sub && view === "sessions" && (
        <div className="skip-links">
          <button className="skip-link" onClick={() => { const t = document.querySelector<HTMLElement>(".transcript"); if (t) focusRegion(t); }}>Skip to transcript</button>
          <button className="skip-link" onClick={() => document.getElementById("composer")?.focus()}>Skip to composer</button>
        </div>
      )}
      <Palette open={palette} onClose={closePalette} rows={rows} mode={palMode} visited={visited}
               commands={commands} onOpenSession={(id, seq, q) => { openSession(id); if (seq) setJump({ id, turn: 0, seq, q, at: Date.now() }); }} initialQuery={palQuery} current={selected}
               onOpenWikiPage={(path) => goWiki({ at: "page", path })}
               onStart={palCwd || home ? (text) => start(palCwd || home, text) : undefined}
               onStartIn={(path) => { setPalCwd(path); setPalette(true); }}
               startIn={palCwd ? (palCwd === home ? "home" : palCwd.split("/").filter(Boolean).pop() || palCwd) : undefined} />
      <Sidebar rows={visible} projects={projects} selected={sidebarSelected(view, lost, selected, lastId)} active={pane === "list"}
               onSelect={(id) => {
                 openSession(id);
                 // A row there only for what was said lands on the line that said it.
                 const line = said.get(id), r = rows.find((x) => x.id === id);
                 if (line && r && !getSearchMatch(r, query.trim().toLowerCase())) setJump({ id, turn: 0, seq: line.seq, q: query.trim(), at: Date.now() });
               }}
               query={query} onQuery={setQuery} said={said}
               saidElsewhere={{ count: [...said.keys()].filter((id) => !rows.some((r) => r.id === id)).length, open: () => { setPalQuery(query.trim()); setPalette(true); } }}
               onTurn={(id, turn) => { if (id !== selected || view !== "sessions" || sub) openSession(id); else setPane("thread"); setJump({ id, turn, at: Date.now() }); }}
               view={view === "project" ? "projects" : view} wikiFlags={wikiFlags} onNew={newSession} onFind={() => setPalette(true)}
               onView={onView}
               showArchived={archived} onToggleArchived={() => setArchived((v) => !v)}
               archivedState={!archived || rowsAll ? "ready" : loadErr ? "failed" : "loading"} onRetryArchived={() => void refresh()}
               onAck={(id) => act(() => api.ack(id), "mark it seen")}
               onShowList={() => setPane("list")} reveal={reveal} onOpenProject={goProject}
               onMove={(id, p) => act(() => api.assign(id, p), "move the session")}
               loadedAt={loadedAt} loadErr={loadErr} onRetry={() => void refresh()} />
      <main className="app-main">
      {lost !== null && view === "sessions" && !selected ? (
        <div className="thread empty">
          <EmptyState glyph="search" title="Nothing at this address" primary={false} action={{ label: "Show all sessions", onClick: goList }}>
            The link may be old, or the session was deleted.
          </EmptyState>
        </div>
      ) : view === "wiki" ? (
        <WikiPage route={wikiRoute} onRoute={goWiki} onBack={goList} onOpenSession={openSession}
                  onSearch={(text) => { setPalQuery(text.replace(/\s+/g, " ").slice(0, 60)); setPalette(true); }} />
      ) : view === "me" ? (
        <MeView rows={rows} projectNames={Object.fromEntries(projects.map((p) => [p.slug, p.name]))} onBack={goList}
                onOpenSession={openSession} onOpenProject={(slug) => goProject(slug)} onOpenPage={(path) => goWiki({ at: "page", path })} />
      ) : view === "hooks" ? (
        <HooksPage onBack={goList} rows={rows} />
      ) : view === "projects" ? (
        <ProjectsView
          projects={projects} rows={rows}
          onOpen={openSession}
          onBack={goList}
          onAssign={(id, p) => act(() => api.assign(id, p), "move the session")}
          // Refused ones re-read too, as act does: a 409 for a project an
          // agent wrote, or a rename refused because its yaml broke, is the
          // server knowing something this list does not show yet.
          onCreate={async (name) => { try { return await api.newProject(name); } finally { await refresh(); } }}
          onRename={async (slug, name) => { try { await api.renameProject(slug, name); } finally { await refresh(); } }}
          onAssignMany={async (ids, p) => {
            // Per session: what moved is done, what did not stays selected there.
            const out = await Promise.allSettled(ids.map((id) => api.assign(id, p)));
            await refresh();
            return ids.filter((_, i) => out[i].status === "rejected");
          }}
          onDelete={(slug) => act(() => api.deleteProject(slug), "delete the project")}
          orbOpen={orbOpen} onOrbOpen={setOrbOpen} onOrbChanged={() => refresh()}
          onNewSession={home ? async (p) => { if (await confirmFailedBuild(p)) void start(home, "", { mode: "project", project: p.slug }); } : undefined} />
      ) : view === "project" ? (
        <ProjectView slug={projectSlug} rows={rows} conversation={selected && starting ? <StartingThread /> : row ? threadFor(row) : undefined} focus={projectFocus}
          onShow={showProjectSession} onBack={goList} onOpenSession={openSession}
          onChanged={() => { void refresh(); }}
          onSeen={(id) => act(() => api.ack(id), "mark it seen")}
          onNewThread={home ? async () => {
            const created = await api.create(home, "", "project", projectSlug);
            markCreated(created.id);
            await refresh();
            return created.id;
          } : undefined}
          onStartThread={home ? async (prompt) => {
            const created = await api.createThread(prompt, projectSlug);
            markCreated(created.id);
            await refresh();
            return created.id;
          } : undefined} />
      ) : row && sub === "changes" ? (
        <ChangesPage row={row} tick={lines.length} onBack={() => setSub(null)} />
      ) : row && context ? (
        <ContextPage session={row.id} model={row.model} used={loadedFor === row.id ? sessionUsage(lines)?.lastIn : undefined} onBack={() => setSub(null)} />
      ) : selected && starting ? (
        // Before the row: serve lists a booting child, so it has one.
        <StartingThread />
      ) : row ? (
        threadFor(row)
      ) : selected && pending[selected]?.length && !missing && !loadFail ? (
        <PendingThread sending={pending[selected]} />
      ) : (
        <div className={"thread" + (selected ? " empty" : "")}>
          {!selected ? (showWelcome ? <Welcome onStart={async (cwd, p) => { if (!(await start(cwd, p))) throw new Error(START_REFUSED); }} onSkip={() => { setWelcome("off"); if (narrow) goList(); }} onBack={narrow ? () => { setWelcome("off"); goList(); } : undefined} /> : <>
            <ControlOverview actions={home ? <>
                <ModePicker projects={projects} value={newMode} onChange={setNewMode} />
                <button className="btn ov-new" onClick={async () => { if (newMode.mode === "project" && !(await confirmFailedBuild(projects.find((p) => p.slug === newMode.project)))) return; void start(newMode.mode === "local" && startDir?.checkout ? startDir.path : home, "", newMode); }}>New session</button>
              </> : undefined} rows={rows} onOpenFailure={(id, seq) => { openSession(id); if (seq) setJump({ id, turn: 0, seq, at: Date.now() }); }} onReveal={(id) => { setPane("list"); setQuery(""); setReveal({ id, at: Date.now() }); }} loadedAt={loadedAt} loadErr={loadErr} onRetry={() => void refresh()} />
          </>) : (
            // A link to a session the list does not hold: looked up on its
            // own, so an empty or slow list never leaves a blank pane.
            missing ? (
              <EmptyState glyph="search" title="Session not found" primary={false}
                action={{ label: "Show all sessions", onClick: goList }}
                secondary={{ label: "Search archived", onClick: () => { setArchived(true); setPane("list"); setPalQuery(""); setPalette(true); } }}>
                This session isn’t on this server. It may be archived or deleted.
              </EmptyState>
            ) : loadFail ? (
              <ErrorNote title="Couldn’t load this session" err={loadFail}
                action={{ label: "Retry", onClick: () => setLoadTry((n) => n + 1) }} secondary={{ label: "Show all sessions", onClick: goList }} />
            ) : <div className="lookup" role="status"><p className="lookup-body">Loading session…</p></div>
          )}
        </div>
      )}
      </main>
      {/* The portal sits beside the thread, not instead of it: the point
          is to watch the page while the agent changes it. */}
      {row && sub === "portal" && view === "sessions" && (
        <PortalPane row={row} onClose={() => setSub(null)} onAsk={(t) => api.prompt(row.id, t)} />
      )}
      <DialogHost />
      {narrow && (pane === "list" || view !== "sessions") && (
        <ViewNav phone view={pane === "list" ? "sessions" : view === "project" ? "projects" : view} onView={onView} wikiFlags={wikiFlags} />
      )}
      {creating && <StartingStatus />}
      {updated && (
        <div className="updated" role="status">
          <span className="updated-text">bough updated. Reload to get the new control room.</span>
          <button className="btn" onClick={() => window.location.reload()}>Reload</button>
          <button className="updated-x" aria-label="Dismiss" onClick={() => setUpdated(false)}>
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" aria-hidden="true"><path d="M6 6l12 12M18 6L6 18" /></svg>
          </button>
        </div>
      )}
      {/* Stays until dismissed or a later action succeeds; Esc is the turn's, not the toast's. */}
      {toast ? <ErrorToast toast={toast} busy={busy} onDismiss={() => setErr(null)} /> : null}
    </div>
  );
}
