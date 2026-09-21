import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import type { OrbFile, OrbState, ProjectDetail, Row, Status } from "./types";
import { api } from "./api";
import { FileEditor, ORB_AS_STATUS, OrbSessions, confirmStopOrb, orbUp } from "./orb";
import { STATUS, StatusMark, TESTS_FAILED_GLYPH, hasQuestion, orbWord, shownStatus, statusWord } from "./status";
import { ErrorNote, Pending, ago, humanError } from "./loading";
import { sessionTitle } from "./render";
import { LONG, TOO_LONG } from "./context";
import { Back, useMedia } from "./app";

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

/** MEMORY.md first: it is the file this page is usually opened to write. */
export const PROJECT_FILES: readonly OrbFile[] = ["MEMORY.md", "project.yml", "Dockerfile", "setup.sh", "resume.sh"];

const MEMORY_NOTE = "Prepended to every session in this project. Nothing writes this but you and the agent.";

const clock = (iso: string) =>
  new Date(iso).toLocaleString([], { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" });

/**
 * The five groups the threads list is cut into, most urgent first. They
 * are the session statuses this page can act on, not a second
 * vocabulary: the group comes from the same hasQuestion/shownStatus the
 * sidebar and the overview read.
 */
export type ThreadGroup = "needs-you" | "error" | "running" | "interrupted" | "done" | "idle" | "empty";

export const GROUP_ORDER: readonly ThreadGroup[] = ["needs-you", "error", "running", "interrupted", "done", "idle", "empty"];

export const GROUP_LABEL: Record<ThreadGroup, string> = {
  "needs-you": "Needs you", error: "Error", running: "Running", interrupted: "Interrupted", done: "Done", idle: "Idle", empty: "Empty",
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
  if (s === "done") return "done";
  return "idle";
}

/** The threads in group order, newest first inside each. An empty group is not a group. */
export function groupThreads(rows: Row[]): { group: ThreadGroup; rows: Row[] }[] {
  const m = new Map<ThreadGroup, Row[]>();
  for (const r of rows) {
    const g = threadGroup(r);
    if (!m.has(g)) m.set(g, []);
    m.get(g)!.push(r);
  }
  const when = (r: Row) => Date.parse(r.lastAt || r.modified) || 0;
  return GROUP_ORDER.filter((g) => m.has(g))
    .map((g) => ({ group: g, rows: m.get(g)!.sort((a, b) => when(b) - when(a) || (a.id < b.id ? 1 : -1)) }));
}

/**
 * The dim line under a thread's title: what it is waiting on, what broke,
 * or what it is about. Never its id — the title already names the work.
 */
export function threadNote(r: Row): string {
  const t = r.ask?.text || r.trouble || r.error || r.summary || "";
  return t.split("\n")[0].trim().slice(0, 120);
}

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
 * A thread's state in a column too narrow for a word: a 6px dot in the
 * status colour, except a failure, which wears the triangle the rest of
 * the UI gives a failed test so it survives greyscale. The word itself
 * rides along for screen readers on the row.
 */
function Dot({ status }: { status: Status }) {
  if (status === "error") {
    return (
      <svg className="prj-tri" width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.2"
           strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">{TESTS_FAILED_GLYPH}</svg>
    );
  }
  return <span className="prj-dot" data-status={status} style={{ color: STATUS[status]?.tone }} aria-hidden="true" />;
}

function ThreadRow({ row, on, onOpen }: { row: Row; on: boolean; onOpen: (id: string) => void }) {
  const st = shownStatus(row);
  const at = row.lastAt || row.modified;
  const note = threadNote(row);
  const title = sessionTitle(row);
  return (
    <button type="button" className={"prj-thread" + (on ? " is-on" : "")} aria-current={on || undefined}
            onClick={() => onOpen(row.id)} aria-label={[title, note, statusWord(st), ago(at)].filter(Boolean).join(", ")}>
      <Dot status={st} />
      <span className="prj-thread-main">
        <span className="prj-thread-title" title={title}>{title}</span>
        {note && <span className="prj-thread-note" title={note}>{note}</span>}
      </span>
      <span className="num prj-thread-when" title={clock(at)}>{ago(at)}</span>
    </button>
  );
}

/** One status group: its label, its count flush right, its rows under it. */
function Group({ group, rows, open, folded, onFold, onOpen }: {
  group: ThreadGroup; rows: Row[]; open: string; folded: boolean;
  onFold: (g: ThreadGroup) => void; onOpen: (id: string) => void;
}) {
  return (
    <div className="prj-group">
      <button type="button" className="prj-group-head" aria-expanded={!folded} onClick={() => onFold(group)}>
        <span className="prj-group-label">{GROUP_LABEL[group]}</span>
        <span className="num prj-group-count">{rows.length}</span>
      </button>
      {!folded && rows.map((r) => <ThreadRow key={r.id} row={r} on={open === r.id} onOpen={onOpen} />)}
    </div>
  );
}

/**
 * The orb line: one sentence about the main thread's container, and the
 * one thing that can be done to it. Orbs are per session — there is no
 * container for the project as a whole — so it never claims that
 * stopping this one stops the threads.
 */
function OrbLine({ orb, onStop }: { orb?: OrbState; onStop: () => void }) {
  if (!orb || !orb.status) return <p className="prj-orb-line prj-orb-down">No orb yet. It starts when you first message the project.</p>;
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
  const n: Record<ThreadGroup, number> = { "needs-you": 0, error: 0, running: 0, interrupted: 0, done: 0, idle: 0, empty: 0 };
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

/** Done and idle threads beyond this many fold behind one line on the home; empty ones fold entirely. */
const IDLE_SHOWN = 8;
const FOLDED_ON_HOME: ReadonlySet<ThreadGroup> = new Set<ThreadGroup>(["done", "idle"]);

/**
 * The composer on the home. Messaging the project is messaging its main
 * thread — the one that hands work out — and creates it the first time.
 */
function ProjectComposer({ onMessage, line, autoFocus }: { onMessage: (text: string) => Promise<void>; line?: string; autoFocus?: boolean }) {
  const [text, setText] = useState("");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState("");
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
      <textarea ref={box} className="field prj-first-box" rows={3} value={text} aria-label="Message the project"
                placeholder="Message the project. Main answers here, or hands the work to a thread." disabled={busy}
                onChange={(e) => setText(e.target.value)}
                onKeyDown={(e) => { if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); void send(); } }} />
      {err && <p className="err prj-first-err" role="alert">{humanError(err)}</p>}
      <button type="button" className="btn btn-primary" disabled={!text.trim() || busy} onClick={() => { void send(); }}>
        {busy ? "Sending…" : "Send"}
      </button>
    </div>
  );
}

/**
 * The project's home: the composer, then the queue. The durable things
 * here are threads, and the page indexes them — it is not one of them.
 * Main is pinned first as the thread the composer talks to; the rest sit
 * under their state, most urgent first, with idle capped behind a line.
 */
export function ProjectHome({ detail, mainRow, onOpen, onMessage, onNewThread }: {
  detail: ProjectDetail; mainRow?: Row; onOpen: (id: string) => void;
  onMessage: (text: string) => Promise<void>; onNewThread?: () => void;
}) {
  const [shownAll, setShownAll] = useState<Set<ThreadGroup>>(() => new Set());
  const groups = useMemo(() => groupThreads(detail.threads), [detail.threads]);
  const mainNote = mainRow ? threadNote(mainRow) : "";
  return (
    <div className="scroll prj-home">
      <ProjectComposer onMessage={onMessage} autoFocus
                       line={!detail.main ? "No main thread yet. The first message starts one." : detail.mainArchived ? "Archived. A message reopens it." : undefined} />
      <div className="prj-queue">
        <div className="prj-queue-head">
          <span className="eyebrow">Threads</span>
          <span className="num prj-threads-count">{detail.threads.length}</span>
          {onNewThread && <button type="button" className="btn btn-ghost btn-sm" onClick={onNewThread}>New thread</button>}
        </div>
        {detail.main && (
          <button type="button" className="prj-thread prj-main-row" onClick={() => onOpen(detail.main!)}
                  aria-label={["Main thread", mainNote, mainRow ? statusWord(shownStatus(mainRow)) : "", mainRow ? ago(mainRow.lastAt || mainRow.modified) : ""].filter(Boolean).join(", ")}>
            {mainRow ? <Dot status={shownStatus(mainRow)} /> : <span className="prj-dot" data-status="idle" aria-hidden="true" />}
            <span className="prj-thread-main">
              <span className="prj-thread-title">Main thread <span className="prj-dim">· the one the composer talks to</span></span>
              {mainNote && <span className="prj-thread-note" title={mainNote}>{mainNote}</span>}
            </span>
            {mainRow && <span className="num prj-thread-when" title={clock(mainRow.lastAt || mainRow.modified)}>{ago(mainRow.lastAt || mainRow.modified)}</span>}
          </button>
        )}
        {detail.threads.length === 0 && <p className="prj-none">No threads yet. Ask the main thread to start work, or create one.</p>}
        {groups.map((g) => {
          const all = shownAll.has(g.group);
          const limit = g.group === "empty" ? 0 : FOLDED_ON_HOME.has(g.group) ? IDLE_SHOWN : Infinity;
          const capped = !all && g.rows.length > limit;
          const rows = capped ? g.rows.slice(0, limit) : g.rows;
          return (
            <div key={g.group} className="prj-group" data-group={g.group}>
              <div className="prj-group-head">
                <span className="prj-group-label">{GROUP_LABEL[g.group]}</span>
                <span className="num prj-group-count">{g.rows.length}</span>
              </div>
              {rows.map((r) => <ThreadRow key={r.id} row={r} on={false} onOpen={onOpen} />)}
              {capped && <button type="button" className="link prj-more" onClick={() => setShownAll((prev) => new Set(prev).add(g.group))}>Show all {g.rows.length}</button>}
            </div>
          );
        })}
      </div>
    </div>
  );
}

export function ProjectPage({
  detail, files, error, filesError, conversation, mainRow, open, onOpen, onNewThread, onBack, onSave, onStopOrb, onOpenSession, onMessage, onRetry, titles = {},
}: {
  /** Absent until the first read lands. */
  detail?: ProjectDetail;
  /** The definition files, read from the project's orb; the editor waits for them. */
  files?: Partial<Record<OrbFile, string>>;
  /** Why the project could not be read, when it could not. */
  error?: string;
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
  onBack?: () => void;
  onSave: (name: OrbFile, text: string) => Promise<void>;
  onStopOrb: (session: string) => void;
  /** Open a thread as a session of its own (an orb row names one). */
  onOpenSession?: (id: string) => void;
  /** The first message, which is what creates the main thread. */
  onMessage: (text: string) => Promise<void>;
  onRetry: () => void;
  /** Session id to title, for the orb rows. */
  titles?: Record<string, string>;
}) {
  // Under 1080px the panel cannot sit beside the conversation, and
  // under 860px neither can the thread list: each becomes a drawer over
  // the conversation instead of disappearing, since the MEMORY.md
  // editor and the threads are the whole point of this page.
  const tight = useMedia("(max-width:1080px)");
  const [folded, setFolded] = useState<Set<ThreadGroup>>(() => new Set(CLOSED));
  const [panel, setPanel] = useState(!tight);
  const [drawer, setDrawer] = useState(false);
  const [tab, setTab] = useState<OrbFile>("MEMORY.md");
  const [filesOpen, setFilesOpen] = useState(true);
  // A drawer covering the conversation must not be what the page opens
  // with, and a window that grew wide again has room for the panel.
  useEffect(() => { setPanel(!tight); setDrawer(false); }, [tight]);
  const editor = useRef<HTMLTextAreaElement>(null);
  const fold = (g: ThreadGroup) => setFolded((prev) => { const n = new Set(prev); n.has(g) ? n.delete(g) : n.add(g); return n; });
  const groups = useMemo(() => groupThreads(detail?.threads ?? []), [detail?.threads]);

  if (!detail) {
    return (
      <div className="thread prj-loading">
        {error
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
      {open && (
      <aside className="prj-threads" aria-label="Threads" data-open={drawer || undefined}>
        <div className="prj-threads-head">
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
              <span className="mono prj-main-slug">{detail.slug}</span>
              {mainRow && <span className="prj-main-st"><StatusMark status={shownStatus(mainRow)} size={12} bare /></span>}
            </button>
          )}
          {threads.length === 0
            ? <p className="prj-none">No threads yet. Ask the main thread to start work, or create one.</p>
            : groups.map((g) => (
              <Group key={g.group} group={g.group} rows={g.rows} open={open} folded={folded.has(g.group)}
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
                    <button type="button" className="link prj-back" onClick={() => onOpen("")}>‹ {detail.name}</button>
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
                  onClick={() => { setPanel((v) => !v); setDrawer(false); }}>Project</button>
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
            ? <ProjectHome detail={detail} mainRow={mainRow} onOpen={onOpen} onMessage={onMessage} onNewThread={onNewThread} />
            : conversation ?? <div className="lookup" role="status"><p className="lookup-body">Loading thread…</p></div>}
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
          <button type="button" className="btn btn-ghost btn-sm prj-drawer-close" onClick={() => setPanel(false)}>Close</button>
          <section className="prj-sec">
            <h3 className="prj-sec-h eyebrow">Orbs</h3>
            <OrbLine orb={detail.mainOrb} onStop={() => detail.main && onStopOrb(detail.main)} />
            <details className="prj-orbs">
              <summary>Thread orbs <span className="num">{threadOrbs.length}</span></summary>
              {/* Stopping one container stops one thread; the others keep theirs. */}
              <p className="prj-dim">Each thread runs in a container of its own.</p>
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
                ? <FileEditor order={PROJECT_FILES} files={files} tab={tab} onTab={setTab} onSave={onSave} editorRef={editor} announce
                              note={tab === "MEMORY.md" ? MEMORY_NOTE : undefined}
                              meta={(f, text) => {
                                const n = lineCount(text);
                                return (
                                  <p className="file-meta">
                                    <span className="mono">{f}</span> · <span className={("num " + lineTone(n)).trim()}>{n} {n === 1 ? "line" : "lines"}</span>
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
export function ProjectView({ slug, rows, conversation, focus, onShow, onBack, onOpenSession, onNewThread, onChanged }: {
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
  /** Something here changed a session: the fleet the App holds is stale. */
  onChanged?: () => void;
}) {
  const [detail, setDetail] = useState<ProjectDetail>();
  const [files, setFiles] = useState<Partial<Record<OrbFile, string>>>();
  const [err, setErr] = useState("");
  const [filesErr, setFilesErr] = useState("");
  // The session on screen, main included; "" is the home.
  const [open, setOpen] = useState("");

  // When the detail on screen was read; a focus newer than it waits for the next read.
  const loadedAt = useRef(0);
  const load = useCallback(async (): Promise<ProjectDetail | undefined> => {
    try { const d = await api.project(slug); loadedAt.current = Date.now(); setDetail(d); setErr(""); return d; }
    catch (e) { setErr(e instanceof Error ? e.message : String(e)); return undefined; }
  }, [slug]);
  const loadFiles = useCallback(async () => {
    // The files come from the orb detail, which is the endpoint that
    // writes them too. A project whose runtime is down still has files.
    // A failure here is said out loud: the project itself reads fine, so
    // swallowing it left the editor spinning with nothing to click.
    try { setFiles((await api.orb(slug)).files); setFilesErr(""); }
    catch (e) { setFilesErr(e instanceof Error ? e.message : String(e)); }
  }, [slug]);

  useEffect(() => { setDetail(undefined); setFiles(undefined); setFilesErr(""); setOpen(""); }, [slug]);
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
      detail={detail} files={files} error={err} filesError={filesErr} conversation={conversation} mainRow={mainRow}
      open={open} onOpen={setOpen} onBack={onBack} onOpenSession={onOpenSession} titles={titles}
      onRetry={() => { void load(); void loadFiles(); }}
      onNewThread={onNewThread ? newThread : undefined}
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
