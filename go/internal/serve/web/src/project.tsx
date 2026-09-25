import { createContext, useCallback, useEffect, useMemo, useRef, useState, type ButtonHTMLAttributes, type HTMLAttributes, type ReactNode } from "react";
import type { OrbFile, OrbState, ProjectDetail, Row, Status } from "./types";
import { api } from "./api";
import { FileEditor, ORB_AS_STATUS, type EditorInitial, OrbSessions, confirmStopOrb, orbUp } from "./orb";
import { MARKED, STATUS, StatusMark, UnseenDot, hasQuestion, isUnseen, orbWord, rowNote, shownStatus, statusWord } from "./status";
import { EmptyState, ErrorNote, Pending, ago, humanError } from "./loading";
import { hasOwnTitle, sessionTitle, titleKey } from "./render";
import { idTail } from "./palette";
import { ModeChip } from "./mode";
import { LONG, TOO_LONG } from "./context";
import { Back, displayTitle, useMedia } from "./app";

/*
 * One project: the main thread's conversation, the threads beside it,
 * and the files every session in the project is given.
 *
 * Three columns inside the shell, with the control room's own sidebar
 * still to their left: the threads, the conversation, and the project
 * itself. The conversation is passed in rather than built here — it is
 * the same <Thread> the sessions view renders, on the same stream, so a
 * thread reads and behaves identically whether it is opened here or
 * from the sidebar.
 */

/**
 * Offered to the main thread's conversation: starting a thread of the
 * project under it, with a task. Only main hands work out (a thread
 * cannot start threads), so only main's composer carries the button.
 */
export const StartThreadCtx = createContext<((prompt: string) => Promise<void>) | null>(null);

/** A remembered page preference; "" when unset or storage is off. */
const pref = (key: string) => { try { return localStorage.getItem(key) ?? ""; } catch { return ""; } };
const remember = (key: string, v: string) => { try { localStorage.setItem(key, v); } catch { /* storage off */ } };
const PANEL_PREF = "bough:prj-panel", THREADS_PREF = "bough:prj-threads-folded";

/** MEMORY.md first: it is the file this page is usually opened to write. */
export const PROJECT_FILES: readonly OrbFile[] = ["MEMORY.md", "project.yml", "Dockerfile", "setup.sh", "resume.sh"];

const MEMORY_NOTE = "Prepended to every session in this project.";

const clock = (iso: string) =>
  new Date(iso).toLocaleString([], { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" });

/**
 * The five groups the threads list is cut into, most urgent first. They
 * are the session statuses this page can act on, not a second
 * vocabulary: the group comes from the same hasQuestion/shownStatus the
 * sidebar and the overview read.
 */
export type ThreadGroup = "needs-you" | "error" | "running" | "interrupted" | "unseen" | "done" | "idle" | "empty";

export const GROUP_ORDER: readonly ThreadGroup[] = ["needs-you", "error", "running", "interrupted", "unseen", "done", "idle", "empty"];

export const GROUP_LABEL: Record<ThreadGroup, string> = {
  "needs-you": "Needs you", error: "Error", running: "Running", interrupted: "Interrupted", unseen: "Finished — unseen", done: "Done", idle: "Idle", empty: "Empty",
};

/** Done is most of a busy project; empty is nothing at all. Both start closed. */
const CLOSED: ReadonlySet<ThreadGroup> = new Set<ThreadGroup>(["done", "idle", "empty"]);

export function threadGroup(r: Row): ThreadGroup {
  if (hasQuestion(r)) return "needs-you";
  const s = shownStatus(r);
  // Queued is moving too: it is waiting for a slot, not for a person.
  if (s === "error") return "error";
  if (s === "running" || s === "queued") return "running";
  if (s === "interrupted") return "interrupted";
  // A session nobody typed into is not idle work, it is nothing: last, and folded.
  if (r.empty) return "empty";
  // A finish nobody has looked at yet is news, not history: above Done.
  if (isUnseen(r)) return "unseen";
  if (s === "done") return "done";
  return "idle";
}

/**
 * The project a session is a thread of: the one membership rule, shared
 * by the sidebar's project group and this page (serve's projectDetail
 * files threads by the same `project` field). A thread main started, a
 * background run and a thread nobody typed into are all members. The
 * sidebar used to fold main's children into main's row, move background
 * runs to their own section and drop empty ones, so the page listed
 * running threads the sidebar's project group never showed.
 */
export const projectOf = (r: Row): string | undefined => (r.archived ? undefined : r.project || undefined);

/** Group order, then newest first: how both lists of a project's threads are ordered. */
export function byThread(a: Row, b: Row): number {
  const when = (r: Row) => Date.parse(r.lastAt || r.modified) || 0;
  return GROUP_ORDER.indexOf(threadGroup(a)) - GROUP_ORDER.indexOf(threadGroup(b)) || when(b) - when(a) || (a.id < b.id ? 1 : -1);
}

/** The threads in group order, newest first inside each. An empty group is not a group. */
export function groupThreads(rows: Row[]): { group: ThreadGroup; rows: Row[] }[] {
  const m = new Map<ThreadGroup, Row[]>();
  for (const r of rows) {
    const g = threadGroup(r);
    if (!m.has(g)) m.set(g, []);
    m.get(g)!.push(r);
  }
  return GROUP_ORDER.filter((g) => m.has(g))
    .map((g) => ({ group: g, rows: m.get(g)!.sort(byThread) }));
}

/**
 * Whether a thread dragged onto a group lands there. Only Done takes one:
 * the other groups are states the agent puts a thread in (a question, an
 * error, a run), not ones a person can. Done is "I have seen it", the one
 * change a person makes, so only a finish nobody has looked at moves. An
 * errored or interrupted thread keeps its status once seen, so a drop on
 * Done would have looked like it did nothing.
 */
export function threadDrop(r: Row, to: ThreadGroup): boolean {
  return to === "done" && threadGroup(r) === "unseen";
}

/** The session a drag carries; a type of its own, so a text field never takes the drop. */
export const DRAG_SESSION = "application/x-bough-session";

/**
 * Dragging a thread between groups. `row` makes a thread draggable when
 * some group would take it; `group` makes a group a drop target while a
 * thread it takes is in the air, and `withTargets` adds that group when it is
 * empty, since an empty group is otherwise not drawn to drop on.
 */
function useThreadDrag(onSeen?: (id: string) => void) {
  const [drag, setDrag] = useState<Row | null>(null);
  const [over, setOver] = useState<ThreadGroup | null>(null);
  const end = () => { setDrag(null); setOver(null); };
  const row = (r: Row): ButtonHTMLAttributes<HTMLButtonElement> => !onSeen || !GROUP_ORDER.some((g) => threadDrop(r, g)) ? {} : {
    draggable: true,
    onDragStart: (e) => { e.dataTransfer.setData(DRAG_SESSION, r.id); e.dataTransfer.effectAllowed = "move"; setDrag(r); },
    onDragEnd: end,
  };
  const group = (g: ThreadGroup): HTMLAttributes<HTMLDivElement> & { "data-drop"?: string } => !drag || !onSeen || !threadDrop(drag, g) ? {} : {
    "data-drop": over === g ? "over" : "ok",
    onDragOver: (e) => { e.preventDefault(); e.dataTransfer.dropEffect = "move"; setOver(g); },
    onDragLeave: (e) => { if (!e.currentTarget.contains(e.relatedTarget as Node | null)) setOver((o) => (o === g ? null : o)); },
    onDrop: (e) => { e.preventDefault(); const id = drag.id; end(); onSeen(id); },
  };
  const withTargets = (list: { group: ThreadGroup; rows: Row[] }[]) => !drag || !onSeen ? list
    : GROUP_ORDER.flatMap((g) => { const hit = list.find((x) => x.group === g); return hit ? [hit] : threadDrop(drag, g) ? [{ group: g, rows: [] as Row[] }] : []; });
  return { row, group, withTargets, hint: (g: ThreadGroup) => Boolean(drag && onSeen && threadDrop(drag, g)) };
}

/** Said inside a group a dragged thread can land in. */
const DROP_HINT = <p className="prj-drop-hint">Drop to mark it seen</p>;

/** Lines as an editor counts them: a trailing newline ends the last line, it does not start one. */
export function lineCount(text: string): number {
  if (!text) return 0;
  return text.endsWith("\n") ? text.split("\n").length - 1 : text.split("\n").length;
}

/** A context file is paid for on every turn, so its length is worth saying. Same numbers as internal/serve/context.go. */
export function lineTone(n: number): string {
  return n > TOO_LONG ? "ctx-lines-max" : n > LONG ? "ctx-lines-long" : "";
}

/**
 * A thread's state as the sidebar marks a row: the glyph at the same size
 * in the same 16px column, and only for the states a list marks (MARKED);
 * the resting ones keep the column empty. A thread looks the same here
 * and in the sidebar's project group. The word rides along for screen
 * readers on the row.
 */
function Mark({ status, unseen }: { status?: Status; unseen?: boolean }) {
  return <span className="row-mark">{status && MARKED.has(status) ? <StatusMark status={status} size={16} bare /> : unseen ? <UnseenDot /> : null}</span>;
}

/** The rows of one group; rows whose titles read the same each carry their id tail, as in the sidebar. */
function rowsOf(rows: Row[], on: string, onOpen: (id: string) => void, drag?: (r: Row) => ButtonHTMLAttributes<HTMLButtonElement>) {
  const key = (r: Row) => titleKey(displayTitle(r) || sessionTitle(r));
  const seen = new Map<string, number>();
  for (const r of rows) seen.set(key(r), (seen.get(key(r)) ?? 0) + 1);
  return rows.map((r) => (
    <ThreadRow key={r.id} row={r} on={on === r.id} onOpen={onOpen} twin={(seen.get(key(r)) ?? 0) > 1} drag={drag?.(r)} />
  ));
}

/**
 * One thread, said as the sidebar's project group says it: the same
 * title, id tail, mark, second line (rowNote) and orb chip beside the
 * age, so the row reads the same in both lists. `tag` names the main
 * thread, which the page pins apart.
 */
function ThreadRow({ row, on, onOpen, twin, tag, className = "", drag }: {
  row: Row; on: boolean; onOpen: (id: string) => void; twin?: boolean; tag?: string; className?: string;
  /** Makes the row draggable onto a group that takes it. */
  drag?: ButtonHTMLAttributes<HTMLButtonElement>;
}) {
  const st = shownStatus(row);
  const at = row.lastAt || row.modified;
  const own = displayTitle(row);
  const title = own || sessionTitle(row);
  const chip = twin || (!hasOwnTitle(row) && !own);
  const { failed, asking, label, plain } = rowNote(row);
  // The open thread is being looked at; its ack is already on the way.
  const unseen = isUnseen(row) && !on;
  // The project's environment failed to set up: said on the second line, as the sidebar does.
  const setup = row.mode === "project" && row.orb?.status === "failed" ? row.orb.project : "";
  const note = (label && !plain) || setup ? label : "";
  return (
    <button type="button" {...drag} className={"prj-thread" + (on ? " is-on" : "") + (className ? " " + className : "")} aria-current={on || undefined}
            onClick={() => onOpen(row.id)} aria-label={[title, tag, note || statusWord(st), unseen && "not seen yet", setup && `${setup}: setup failed`, ago(at)].filter(Boolean).join(", ")}>
      <Mark status={failed ? "error" : st} unseen={unseen} />
      <span className="prj-thread-main">
        <span className="prj-thread-name">
          <span className="prj-thread-title" title={title}>{title}</span>
          {chip && <span className="mono row-id" title={`Session id ending ${idTail(row.id)}`}>{idTail(row.id)}</span>}
          {tag && <span className="prj-dim">· {tag}</span>}
        </span>
        {note || setup ? (
          <span className={"num prj-thread-note" + (failed ? " row-meta-bad" : asking ? " row-meta-ask" : "")} title={note}>
            <ModeChip row={row} bare name={setup} />{note}
          </span>
        ) : null}
      </span>
      <span className="num prj-thread-when row-when" title={clock(at)}>
        {!note && !setup && <ModeChip row={row} bare />}
        {!note && !setup && row.mode === "project" && row.orb && row.orb.status !== "failed" && row.orb.status !== "stopped" && row.orb.status !== "" && <span className="row-when-sep" aria-hidden="true">·</span>}
        {ago(failed === "tests failed" && row.testsAt ? row.testsAt : at)}
      </span>
    </button>
  );
}

/** One status group: its label, its count flush right, its rows under it. */
function Group({ group, rows, open, folded, onFold, onOpen, dnd }: {
  group: ThreadGroup; rows: Row[]; open: string; folded: boolean;
  onFold: (g: ThreadGroup) => void; onOpen: (id: string) => void;
  dnd: ReturnType<typeof useThreadDrag>;
}) {
  return (
    <div className="prj-group" {...dnd.group(group)}>
      <button type="button" className="prj-group-head" aria-expanded={!folded} onClick={() => onFold(group)}>
        <span className="prj-group-label">{GROUP_LABEL[group]}</span>
        <span className="num prj-group-count">{rows.length}</span>
      </button>
      {dnd.hint(group) && DROP_HINT}
      {!folded && rowsOf(rows, open, onOpen, dnd.row)}
    </div>
  );
}

/**
 * The orb line: one sentence about the main thread's container, and the
 * one thing that can be done to it. Orbs are per session — there is no
 * container for the project as a whole — so it never claims that
 * stopping this one stops the threads.
 */
function OrbLine({ orb, messaged, onStop }: { orb?: OrbState; messaged: boolean; onStop: () => void }) {
  // A project with threads has been messaged: the orb line then talks about main alone, not the project.
  if (!orb || !orb.status) return <p className="prj-orb-line prj-orb-down">{messaged ? "Main thread has no orb yet." : "No orb yet. It starts when you first message the project."}</p>;
  if (!orbUp(orb)) return <p className="prj-orb-line prj-orb-down">Main thread’s orb is stopped. It starts when you message the project.</p>;
  const st = ORB_AS_STATUS[orb.status];
  return (
    <p className="prj-orb-line">
      <span>Main thread’s orb —</span>
      {/* The session vocabulary's glyph with the orb's own word: "Building"
          is not "Running", and the word is said once, not twice. */}
      <span className="orb-mark" style={{ color: STATUS[st].tone }}>
        <svg className="state-mark" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.5"
             strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">{STATUS[st].glyph}</svg>
        {orbWord(orb.status)}
      </span>
      {orb.updatedAt && <span className="num prj-dim">· {ago(orb.updatedAt)} ago</span>}
      <button type="button" className="link prj-orb-stop" onClick={onStop}>Stop</button>
    </p>
  );
}

/** How many threads are in each state; the header line and the queue read the same numbers. */
export function threadCounts(rows: Row[]): Record<ThreadGroup, number> {
  const n: Record<ThreadGroup, number> = { "needs-you": 0, error: 0, running: 0, interrupted: 0, unseen: 0, done: 0, idle: 0, empty: 0 };
  for (const r of rows) n[threadGroup(r)]++;
  return n;
}

/** The header's one line about the project: what wants a person, what is moving, how much there is. */
export function countsLine(rows: Row[]): string {
  const n = threadCounts(rows);
  const parts: string[] = [];
  if (n["needs-you"]) parts.push(`${n["needs-you"]} need${n["needs-you"] === 1 ? "s" : ""} you`);
  if (n.error) parts.push(`${n.error} ${n.error === 1 ? "error" : "errors"}`);
  if (n.running) parts.push(`${n.running} running`);
  parts.push(`${rows.length} ${rows.length === 1 ? "thread" : "threads"}`);
  return parts.join(" · ");
}

/**
 * Threads beyond this many fold behind one line on the home; empty ones
 * fold entirely. Needs-you and running never fold: they are the work.
 */
const IDLE_SHOWN = 8;
const FOLDED_ON_HOME: ReadonlySet<ThreadGroup> = new Set<ThreadGroup>(["error", "interrupted", "done", "idle"]);

/**
 * The composer on the home. Messaging the project is messaging its main
 * thread — the one that hands work out — and creates it the first time.
 */
function ProjectComposer({ onMessage, line, autoFocus, initial }: {
  onMessage: (text: string) => Promise<void>; line?: string; autoFocus?: boolean; initial?: ComposerInitial;
}) {
  const [text, setText] = useState(initial?.text ?? "");
  const [busy, setBusy] = useState(initial?.busy ?? false);
  const [err, setErr] = useState(initial?.err ?? "");
  const box = useRef<HTMLTextAreaElement>(null);
  useEffect(() => { if (autoFocus) box.current?.focus(); }, [autoFocus]);
  const send = async () => {
    const t = text.trim();
    if (!t || busy) return;
    setBusy(true); setErr("");
    try { await onMessage(t); setText(""); }
    catch (e) { setErr(e instanceof Error ? e.message : String(e)); }
    finally { setBusy(false); }
  };
  return (
    <div className="prj-first">
      {line && <p className="prj-first-line">{line}</p>}
      {/* The same shell as the thread composer under a conversation, so the two boxes on this page read as one thing. */}
      <div className="composer composer-multi prj-first-shell">
        <textarea ref={box} className="prj-first-box" rows={2} value={text} aria-label="Message the project"
                  placeholder="Message the project. Main answers here, or hands the work to a thread." disabled={busy}
                  onChange={(e) => setText(e.target.value)}
                  onKeyDown={(e) => { if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); void send(); } }} />
        <div className="composer-bar">
          <div className="composer-actions">
            <button type="button" className="btn btn-primary" disabled={!text.trim() || busy} onClick={() => { void send(); }}>
              {busy ? "Sending…" : "Send"}
            </button>
          </div>
        </div>
      </div>
      {err && <p className="err prj-first-err" role="alert">{humanError(err)}</p>}
    </div>
  );
}

/**
 * The project's home: the composer, then the queue. The durable things
 * here are threads, and the page indexes them — it is not one of them.
 * Main is pinned first as the thread the composer talks to; the rest sit
 * under their state, most urgent first, with idle capped behind a line.
 */
export function ProjectHome({ detail, mainRow, onOpen, onMessage, onNewThread, onSeen, composer }: {
  detail: ProjectDetail; mainRow?: Row; onOpen: (id: string) => void;
  /** Where the composer starts; see PageInitial. */
  composer?: ComposerInitial;
  onMessage: (text: string) => Promise<void>; onNewThread?: () => void;
  /** Mark a thread seen: what dropping it on Done does. */
  onSeen?: (id: string) => void;
}) {
  const [shownAll, setShownAll] = useState<Set<ThreadGroup>>(() => new Set());
  const dnd = useThreadDrag(onSeen);
  const groups = dnd.withTargets(useMemo(() => groupThreads(detail.threads), [detail.threads]));
  return (
    <div className="scroll prj-home">
      <ProjectComposer onMessage={onMessage} autoFocus initial={composer}
                       line={!detail.main ? "No main thread yet. The first message starts one." : detail.mainArchived ? "Archived. A message reopens it." : undefined} />
      <div className="prj-queue">
        <div className="prj-queue-head">
          <span className="eyebrow">Threads</span>
          <span className="num prj-threads-count">{detail.threads.length}</span>
          {onNewThread && <button type="button" className="btn btn-ghost btn-sm" onClick={onNewThread}>New thread</button>}
        </div>
        {/* Main by its own name, as the sidebar lists it; the tag says which thread it is. */}
        {detail.main && (mainRow
          ? <ThreadRow row={mainRow} on={false} onOpen={onOpen} tag="Main thread" className="prj-main-row" />
          : (
            <button type="button" className="prj-thread prj-main-row" onClick={() => onOpen(detail.main!)} aria-label="Main thread">
              <Mark />
              <span className="prj-thread-main"><span className="prj-thread-title">Main thread</span></span>
            </button>
          ))}
        {detail.threads.length === 0 && <p className="prj-none">No threads yet. Ask the main thread to start work, or create one.</p>}
        {groups.map((g) => {
          const all = shownAll.has(g.group);
          const limit = g.group === "empty" ? 0 : FOLDED_ON_HOME.has(g.group) ? IDLE_SHOWN : Infinity;
          const capped = !all && g.rows.length > limit;
          const rows = capped ? g.rows.slice(0, limit) : g.rows;
          return (
            <div key={g.group} className="prj-group" data-group={g.group} {...dnd.group(g.group)}>
              <div className="prj-group-head">
                <span className="prj-group-label">{GROUP_LABEL[g.group]}</span>
                <span className="num prj-group-count">{g.rows.length}</span>
              </div>
              {dnd.hint(g.group) && DROP_HINT}
              {rowsOf(rows, "", onOpen, dnd.row)}
              {capped && <button type="button" className="link prj-more" onClick={() => setShownAll((prev) => new Set(prev).add(g.group))}>Show all {g.rows.length}</button>}
            </div>
          );
        })}
      </div>
    </div>
  );
}

/** The home composer's own state: its text, a send in flight, the last send's error. */
export interface ComposerInitial { text: string; busy: boolean; err: string }

/**
 * Where the page's own UI state starts, in place of what it derives from
 * the viewport and the remembered choices. A static render is exactly
 * this start, which is how the model test draws every state of
 * ui_project-page.fizz without a browser; the app never passes it.
 */
export interface PageInitial {
  tight?: boolean; panel?: boolean; drawer?: boolean; threadsFolded?: boolean;
  /** The column's folded groups. */
  folded?: ThreadGroup[];
  composer?: ComposerInitial;
  /** The files editor's, on the MEMORY.md tab. */
  editor?: EditorInitial;
}

export function ProjectPage({
  detail, files, error, missing, filesError, conversation, mainRow, open, onOpen, onNewThread, onStartThread, onBack, onSave, onStopOrb, onOpenSession, onMessage, onRetry, onSeen, titles = {}, initial,
}: {
  /** Absent until the first read lands. */
  detail?: ProjectDetail;
  /** The definition files, read from the project's orb; the editor waits for them. */
  files?: Partial<Record<OrbFile, string>>;
  /** Why the project could not be read, when it could not. */
  error?: string;
  /** The server said there is no such project (a 404): no Retry can bring it back. */
  missing?: boolean;
  /** Why the definition files could not be read; the Files section says so and offers the retry. */
  filesError?: string;
  /** The open thread's conversation: the same <Thread> the sessions view builds. */
  conversation?: ReactNode;
  /** The main thread's row, when the session list holds it: the one status pill. */
  mainRow?: Row;
  /** The session shown in the middle column, main included; "" is the home. */
  open: string;
  onOpen: (id: string) => void;
  /** Absent in a story; the page still lists what is there. */
  onNewThread?: () => void;
  /** Main hands a task to a new thread of the project (the conversation's Start thread button). */
  onStartThread?: (prompt: string) => Promise<void>;
  onBack?: () => void;
  onSave: (name: OrbFile, text: string) => Promise<void>;
  onStopOrb: (session: string) => void;
  /** Open a thread as a session of its own (an orb row names one). */
  onOpenSession?: (id: string) => void;
  /** The first message, which is what creates the main thread. */
  onMessage: (text: string) => Promise<void>;
  onRetry: () => void;
  /** Mark a thread seen: what dropping it on Done does. Absent, nothing drags. */
  onSeen?: (id: string) => void;
  /** Session id to title, for the orb rows. */
  titles?: Record<string, string>;
  initial?: PageInitial;
}) {
  // Under 1080px the panel cannot sit beside the conversation, and
  // under 860px neither can the thread list: each becomes a drawer over
  // the conversation instead of disappearing, since the MEMORY.md
  // editor and the threads are the whole point of this page.
  const media = useMedia("(max-width:1080px)");
  const tight = initial?.tight ?? media;
  const [folded, setFolded] = useState<Set<ThreadGroup>>(() => new Set(initial?.folded ?? CLOSED));
  // The panel opens closed and stays how it was left: beside the control
  // room's sidebar and the thread list it was a fourth column, and it
  // reopened on every visit. The thread list folds to a rail the same
  // way, remembered too; both only where they are columns, not drawers.
  // On the home the panel is half the page (MEMORY.md is what the page
  // is opened to write), so it shows unless it was closed; beside a
  // conversation it made a fourth column, so it stays closed unless it
  // was opened. One remembered choice, made by the toggle, not by drawers.
  const panelWanted = (o: string) => { const p = pref(PANEL_PREF); return p ? p === "1" : !o; };
  const [panel, setPanel] = useState(() => initial?.panel ?? (!tight && panelWanted(open)));
  const [threadsFolded, setThreadsFolded] = useState(() => initial?.threadsFolded ?? pref(THREADS_PREF) === "1");
  const [drawer, setDrawer] = useState(initial?.drawer ?? false);
  const [tab, setTab] = useState<OrbFile>("MEMORY.md");
  const [filesOpen, setFilesOpen] = useState(true);
  // A drawer covering the conversation must not be what the page opens
  // with, and a window that grew wide again shows the panel as it was left.
  const onThread = Boolean(open);
  useEffect(() => { setPanel(!tight && panelWanted(onThread ? "t" : "")); setDrawer(false); }, [tight, onThread]);
  const togglePanel = () => setPanel((v) => { if (!tight) remember(PANEL_PREF, v ? "0" : "1"); return !v; });
  const foldThreads = (v: boolean) => { remember(THREADS_PREF, v ? "1" : "0"); setThreadsFolded(v); };
  const editor = useRef<HTMLTextAreaElement>(null);
  const fold = (g: ThreadGroup) => setFolded((prev) => { const n = new Set(prev); n.has(g) ? n.delete(g) : n.add(g); return n; });
  const dnd = useThreadDrag(onSeen);
  const groups = dnd.withTargets(useMemo(() => groupThreads(detail?.threads ?? []), [detail?.threads]));

  if (!detail) {
    return (
      <div className="thread prj-loading">
        {missing
          ? <EmptyState glyph="search" title="Project not found" primary={false} action={onBack && { label: "Show all sessions", onClick: onBack }}>
              No project on this server has this name. It may have been deleted, or the link is mistyped.
            </EmptyState>
          : error
          ? <ErrorNote title="Couldn’t read this project" err={error} action={{ label: "Retry", onClick: onRetry }} />
          : <Pending what="Project" />}
      </div>
    );
  }

  const threads = detail.threads;
  const openRow = open ? (open === detail.main ? mainRow : threads.find((t) => t.id === open)) : undefined;
  const inMain = Boolean(open) && open === detail.main;
  const memory = files?.["MEMORY.md"] ?? "";
  const memoryLines = lineCount(memory);
  const threadOrbs = detail.orbs.filter((o) => o.session !== detail.main);
  const orbsFailed = threadOrbs.filter((o) => ORB_AS_STATUS[o.status] === "error").length;
  // Point the editor at the file that has to be fixed, wherever the page is.
  const editFile = (f: OrbFile) => {
    setPanel(true); setFilesOpen(true); setTab(f);
    requestAnimationFrame(() => { editor.current?.scrollIntoView({ block: "nearest" }); editor.current?.focus(); });
  };

  return (
    <div className="prj">
      {/* Beside a conversation the column is the way between threads; on
          the home the page itself is the index, and a second copy would
          be noise. */}
      {/* Folded, the column is a rail: the chevron and the count, so the
          way between threads is one click away without the width. */}
      {open && threadsFolded && !tight && (
      <aside className="prj-threads prj-threads-folded" aria-label="Threads">
        <button type="button" className="btn btn-ghost btn-sm prj-threads-fold" aria-expanded={false} aria-label="Show threads" title="Show threads"
                onClick={() => foldThreads(false)}><Chevron /></button>
        <span className="num prj-threads-count" title={`${threads.length} threads`}>{threads.length}</span>
      </aside>
      )}
      {open && !(threadsFolded && !tight) && (
      <aside className="prj-threads" aria-label="Threads" data-open={drawer || undefined}>
        <div className="prj-threads-head">
          <button type="button" className="btn btn-ghost btn-sm prj-threads-fold" aria-expanded={true} aria-label="Hide threads" title="Hide threads"
                  onClick={() => foldThreads(true)}><Chevron open /></button>
          <span className="eyebrow">Threads</span>
          <span className="num prj-threads-count">{threads.length}</span>
          {onNewThread && <button type="button" className="btn btn-ghost btn-sm" onClick={onNewThread}>New thread</button>}
          {/* Only when this column is a drawer over the conversation: it
              covers the title bar's own toggle, so it closes itself. */}
          <button type="button" className="btn btn-ghost btn-sm prj-drawer-close" onClick={() => setDrawer(false)}>Close</button>
        </div>
        <div className="scroll prj-threads-list">
          {/* The main thread is the room, not a peer: it is pinned above the
              hairline with the project it speaks for under its name. */}
          {detail.main && (
            <button type="button" className={"prj-main-thread" + (inMain ? " is-on" : "")} aria-current={inMain || undefined}
                    onClick={() => onOpen(detail.main!)}>
              <span className="prj-main-label">Main thread</span>
              <span className="prj-main-slug">{mainRow ? statusWord(shownStatus(mainRow)) : detail.mainArchived ? "Archived" : detail.slug}</span>
              {mainRow && <span className="prj-main-st"><StatusMark status={shownStatus(mainRow)} size={12} bare /></span>}
            </button>
          )}
          {threads.length === 0
            ? <p className="prj-none">No threads yet. Ask the main thread to start work, or create one.</p>
            : groups.map((g) => (
              <Group key={g.group} group={g.group} rows={g.rows} open={open} folded={folded.has(g.group)} dnd={dnd}
                     onFold={fold} onOpen={onOpen} />
            ))}
        </div>
      </aside>
      )}

      <section className="prj-main">
        <header className="prj-bar">
          <Back onBack={onBack} />
          <div className="prj-bar-main">
            <h1 className="prj-name" title={detail.name}>{detail.name}</h1>
            {/* The way back is the one control on this line, so it is
                first and never shrinks: the crumb ellipsizes the title
                instead, and a narrow conversation column used to clip
                the chip clean off the end. */}
            <p className="prj-crumb">
              {open
                ? <>
                    <button type="button" className="link prj-back" onClick={() => onOpen("")}>‹ All threads</button>
                    <span className="prj-crumb-rest" title={inMain ? "Main thread" : openRow ? sessionTitle(openRow) : ""}>· {inMain ? "Main thread" : openRow ? sessionTitle(openRow) : "…"}</span>
                  </>
                : <>
                    <span className="mono">{detail.slug}</span>
                    <span className="prj-crumb-rest">· {countsLine(threads)}</span>
                  </>}
            </p>
          </div>
          {/* No status pill here: the conversation's own header, right under this bar, already wears it. */}
          {/* Shown only where the thread list is a drawer; wider, the
              column is simply there and a toggle would be a lie. */}
          {open && <button type="button" className="btn btn-sm prj-threads-btn" aria-expanded={drawer}
                  onClick={() => { setDrawer((v) => !v); setPanel(false); }}>Threads</button>}
          <button type="button" className="btn btn-sm prj-panel-btn" aria-expanded={panel}
                  onClick={() => { togglePanel(); setDrawer(false); }}>Project</button>
        </header>

        {/* A project.yml that does not parse still has a page: the editor
            that fixes it is one click away, on this page. */}
        {detail.error && (
          <p className="callout err prj-bad" role="alert">
            project.yml didn’t parse · {detail.error}
            <button type="button" className="link" onClick={() => editFile("project.yml")}>Edit project.yml</button>
          </p>
        )}

        <div className="prj-conv">
          {!open
            ? <ProjectHome detail={detail} mainRow={mainRow} onOpen={onOpen} onMessage={onMessage} onNewThread={onNewThread} onSeen={onSeen} composer={initial?.composer} />
            : <StartThreadCtx.Provider value={inMain && onStartThread ? onStartThread : null}>
                {conversation ?? <div className="lookup" role="status"><p className="lookup-body">Loading thread…</p></div>}
              </StartThreadCtx.Provider>}
        </div>
      </section>

      {/* Both drawers cover the conversation, so a tap outside closes
          them; at full width it is not drawn at all. */}
      {(panel || drawer) && (
        <button type="button" className="prj-scrim" aria-label="Close" tabIndex={-1}
                onClick={() => { setPanel(false); setDrawer(false); }} />
      )}
      {panel && (
        <aside className="prj-panel" aria-label="Project">
          {/* As a drawer it says what it is: a lone Close over the Orbs caption named nothing. */}
          <div className="prj-drawer-head">
            <h2>Project</h2>
            <button type="button" className="btn btn-ghost btn-sm prj-drawer-close" onClick={() => setPanel(false)}>Close</button>
          </div>
          <section className="prj-sec">
            <h2 className="prj-sec-h eyebrow">Orbs</h2>
            <OrbLine orb={detail.mainOrb} messaged={Boolean(detail.main) || threads.length > 0} onStop={() => detail.main && onStopOrb(detail.main)} />
            <details className="prj-orbs">
              {/* The fold line carries the state, so a failed orb is not hidden under a bare count. Stopping one container stops one thread; the others keep theirs. */}
              <summary title="Each thread runs in a container of its own.">Thread orbs <span className="num">{threadOrbs.length}</span>{orbsFailed > 0 && <span className="prj-dim" style={{ margin: 0 }}>· {orbsFailed} failed</span>}</summary>
              <OrbSessions orbs={threadOrbs} titles={titles} onOpen={onOpenSession} onStopOrb={onStopOrb}
                           none="No thread has started a container yet." />
            </details>
          </section>

          <section className="prj-sec">
            <details className="prj-files" open={filesOpen} onToggle={(e) => setFilesOpen(e.currentTarget.open)}>
              <summary>
                <span className="eyebrow">Files</span>
                {/* Closed, the section still says the one thing worth knowing about it. */}
                {!filesOpen && <span className="prj-files-sum"><span className="mono">MEMORY.md</span> · <span className={("num " + lineTone(memoryLines)).trim()}>{memoryLines} lines</span></span>}
              </summary>
              {files
                ? <FileEditor order={PROJECT_FILES} files={files} tab={tab} onTab={setTab} onSave={onSave} editorRef={editor} announce initial={initial?.editor}
                              note={tab === "MEMORY.md" ? MEMORY_NOTE : undefined}
                              meta={(f, text) => {
                                const n = lineCount(text);
                                return (
                                  <p className="file-meta">
                                    <span className={("num " + lineTone(n)).trim()}>{n} {n === 1 ? "line" : "lines"}</span>
                                    {n > LONG && <span className={lineTone(n)}> · long — injected every turn</span>}
                                  </p>
                                );
                              }} />
                : <Pending what="Files" inline err={filesError || null} onRetry={onRetry} />}
            </details>
          </section>
        </aside>
      )}
    </div>
  );
}

/**
 * One project's page with its fetching: the detail (polled, because
 * threads start and finish without anything here clicking), and the
 * definition files, which change only when someone saves one.
 *
 * `onShow` names the session the middle column is on. The App owns the
 * transcript, the stream and the composer for it — this only says which
 * session that is, so opening a thread here costs the same as opening it
 * from the sidebar.
 */
export function ProjectView({ slug, rows, conversation, focus, onShow, onBack, onOpenSession, onNewThread, onStartThread, onChanged, onSeen }: {
  slug: string;
  /** A thread the page was opened on (a session just started in this project, or a link to one). */
  focus?: { id: string; at: number };
  rows: Row[];
  conversation?: ReactNode;
  onShow: (id: string) => void;
  onBack?: () => void;
  onOpenSession?: (id: string) => void;
  /** Starts a thread in this project and resolves to its id. */
  onNewThread?: () => Promise<string>;
  /** Starts a thread under main, on a task: what main's Start thread button does. */
  onStartThread?: (prompt: string, main: string) => Promise<string>;
  /** Something here changed a session: the fleet the App holds is stale. */
  onChanged?: () => void;
  /** Marks a session seen, saying so when it could not; the page rereads after. */
  onSeen?: (id: string) => Promise<unknown>;
}) {
  const [detail, setDetail] = useState<ProjectDetail>();
  const [files, setFiles] = useState<Partial<Record<OrbFile, string>>>();
  const [err, setErr] = useState("");
  const [missing, setMissing] = useState(false);
  const [filesErr, setFilesErr] = useState("");
  // The session on screen, main included; "" is the home.
  const [open, setOpen] = useState("");

  // Leaving the page (or turning to another project) cancels its reads.
  // The orb read probes the container runtime and can take seconds; left
  // running, those reads queued behind the page's event streams and
  // reached serve after the page was gone — a 404 in the console once
  // the project was deleted meanwhile.
  const reads = useRef(new AbortController());
  useEffect(() => { const c = new AbortController(); reads.current = c; return () => c.abort(); }, [slug]);

  // When the detail on screen was read; a focus newer than it waits for the next read.
  const loadedAt = useRef(0);
  const load = useCallback(async (): Promise<ProjectDetail | undefined> => {
    const { signal } = reads.current;
    try { const d = await api.project(slug, signal); loadedAt.current = Date.now(); setDetail(d); setErr(""); setMissing(false); return d; }
    catch (e) {
      if (signal.aborted) return undefined;
      setErr(e instanceof Error ? e.message : String(e));
      // Only an authoritative 404 says the project is gone; anything else can be retried.
      setMissing((e as { status?: number }).status === 404);
      return undefined;
    }
  }, [slug]);
  const loadFiles = useCallback(async () => {
    // The files come from the orb detail, which is the endpoint that
    // writes them too. A project whose runtime is down still has files.
    // A failure here is said out loud: the project itself reads fine, so
    // swallowing it left the editor spinning with nothing to click.
    const { signal } = reads.current;
    try { setFiles((await api.orb(slug, signal)).files); setFilesErr(""); }
    catch (e) { if (!signal.aborted) setFilesErr(e instanceof Error ? e.message : String(e)); }
  }, [slug]);

  useEffect(() => { setDetail(undefined); setFiles(undefined); setFilesErr(""); setMissing(false); setOpen(""); }, [slug]);
  useEffect(() => { if (focus) { setOpen(focus.id); void load(); } }, [focus, load]);
  useEffect(() => { void load(); void loadFiles(); }, [load, loadFiles]);
  useEffect(() => {
    const t = setInterval(() => { if (!document.hidden) void load(); }, 4000);
    return () => clearInterval(t);
  }, [load]);

  // A thread that is no longer in the project (archived, moved) stops being the one on screen.
  // A detail read before the thread was asked for cannot know about it yet.
  useEffect(() => {
    if (open && detail && open !== detail.main && !detail.threads.some((t) => t.id === open) && !(focus?.id === open && focus.at > loadedAt.current)) setOpen("");
  }, [open, detail, focus]);

  const show = open;
  useEffect(() => { onShow(show); }, [show, onShow]);

  // The reload comes FIRST: selecting a thread the loaded detail does
  // not hold yet trips the stale-thread effect above, which cleared the
  // selection before the new thread landed — the button read as doing
  // nothing and the conversation snapped back to the main thread.
  const newThread = onNewThread
    ? () => { void (async () => { const id = await onNewThread(); await load(); setOpen(id); })(); }
    : undefined;

  const titles = useMemo(() => Object.fromEntries(rows.map((r) => [r.id, r.title])), [rows]);
  const mainRow = detail?.main ? rows.find((r) => r.id === detail.main) : undefined;

  return (
    <ProjectPage
      detail={detail} files={files} error={err} missing={missing} filesError={filesErr} conversation={conversation} mainRow={mainRow}
      open={open} onOpen={setOpen} onBack={onBack} onOpenSession={onOpenSession} titles={titles}
      // A retry is said as one: the error gives way to the pending line
      // until the reads answer, or the button looked like it did nothing.
      onRetry={() => { setErr(""); setFilesErr(""); void load(); void loadFiles(); }}
      onSeen={onSeen ? (id) => { void onSeen(id).then(() => load()); } : undefined}
      onNewThread={onNewThread ? newThread : undefined}
      // The list reloads so the new thread shows beside main at once; main stays on screen, it is where the reply lands.
      onStartThread={onStartThread && detail?.main ? async (prompt) => { await onStartThread(prompt, detail.main!); await load(); onChanged?.(); } : undefined}
      onMessage={async (text) => { await api.messageProject(slug, text); const d = await load(); if (d?.main) setOpen(d.main); onChanged?.(); }}
      onSave={async (name, text) => { await api.putOrbFile(slug, name, text); await loadFiles(); await load(); }}
      onStopOrb={(session) => { void (async () => {
        if (!(await confirmStopOrb(rows.find((r) => r.id === session)?.jobs))) return;
        await api.stopOrb(session);
        await load();
        onChanged?.();
      })(); }} />
  );
}

/** The fold mark: points right folded, down open — the sidebar's own chevron. */
function Chevron({ open }: { open?: boolean }) {
  return (
    <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true"
         style={open ? { transform: "rotate(90deg)" } : undefined}><path d="M9 6l6 6-6 6" /></svg>
  );
}
