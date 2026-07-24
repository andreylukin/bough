/**
 * Background shells — bash's detached sibling, for commands that outlive a single
 * program round (dev servers, long builds, watchers). A shell lands here two ways:
 *   - explicitly, via `bashBg` (fire-and-forget; spawned WITHOUT the turn signal so
 *     it survives the user's stop button, like a dev server should);
 *   - automatically, when a foreground `bash` is still running at the background
 *     threshold (bash.ts) — the running child is `promote`d here instead of being
 *     killed, so long commands are never lost and never block the turn.
 *
 * Reads work while a shell is still running: `bashOutput` returns output accrued
 * since the caller's last read plus a `[running]`/`[exited …]` status line — the
 * supervisor can watch progress mid-flight. `bashWait` blocks until exit (the bash
 * analog of subagent join). On exit a shell posts a one-line completion note via
 * ctx.notify (→ turn.ts postSystemNote), so the model is TOLD it finished and never
 * writes a poll loop — unless the model already claimed the result with bashWait.
 *
 * Shells are registered per session, in memory: they persist across run_steps
 * rounds and turns of the same session, and die with the server process. A
 * session's shells are invisible to every other session.
 */
import type { ToolRunCtx } from "./types.ts";
import { shellInvocation } from "./bash.ts";

/** Retained output per shell; when exceeded, the oldest chars are dropped.
 * Shared with sh() (bash.ts) so every shell surface has the same output ceiling. */
export const MAX_BUF = 400_000;
/** Running shells per session — a brake on loops that spawn and forget. */
const MAX_RUNNING = 8;
/** Grace between SIGTERM and the SIGKILL backstop. */
const KILL_GRACE_MS = 2_000;

interface BgShell {
  id: string;
  command: string;
  /** The session key this shell was promoted under (job events carry it). */
  sessionKey: string;
  startedAt: number;
  endedAt: number | null;
  /** Set by bashKill so the registry reports "killed", not a plain exit. */
  killed: boolean;
  child: Deno.ChildProcess;
  /** Combined stdout+stderr in arrival order; capped at MAX_BUF. */
  buf: string;
  /** Chars of `buf` already returned by bashOutput. */
  readAt: number;
  /** The cap dropped output the caller never saw — reported once, then cleared. */
  droppedUnread: boolean;
  status: Deno.CommandStatus | null;
  /** Resolves when both stdout+stderr streams have fully drained. */
  pumps: Promise<void>;
  /** Fired after `status` is set — the completion notifier (set by promote). */
  onExit?: () => void;
  /** bashWait sets this so the exit note is suppressed (result claimed in-band). */
  claimed: boolean;
  /** Guards against a double completion note. */
  notified: boolean;
}

/** sessionKey → shellId → shell. Module-level: shells outlive turns, not the server. */
const sessions = new Map<string, Map<string, BgShell>>();
let seq = 0;

// ---- job registry surface (jobs API + TUI visibility) ------------------------

/** How long an exited shell stays in listJobs — long enough for the UI to show
 * the outcome, short enough that old jobs don't pile up in the response. */
const RECENT_MS = 5 * 60_000;
/** Lines of output tail a job row carries (the TUI's live-card preview). */
const TAIL_LINES = 3;

export interface JobInfo {
  id: string;
  sessionId: string;
  command: string;
  startedAt: number;
  status: "running" | "exited" | "killed";
  exitCode?: number;
  /** Last few non-empty output lines — the live-card preview. */
  tailLines: string[];
}

export type JobEvent = { type: "job.spawned" | "job.exited"; sessionId: string; job: JobInfo };

/** Job lifecycle listeners (the server republishes these on its event bus). */
const jobListeners = new Set<(ev: JobEvent) => void>();
export function onJobEvent(cb: (ev: JobEvent) => void): () => void {
  jobListeners.add(cb);
  return () => jobListeners.delete(cb);
}

function jobInfo(sh: BgShell): JobInfo {
  const status = sh.status === null ? "running" : sh.killed ? "killed" : "exited";
  const tail = sh.buf.trimEnd().split("\n").filter((l) => l.trim()).slice(-TAIL_LINES);
  return {
    id: sh.id,
    sessionId: sh.sessionKey,
    command: sh.command,
    startedAt: sh.startedAt,
    status,
    ...(sh.status ? { exitCode: sh.status.code } : {}),
    tailLines: tail,
  };
}

function emitJob(type: JobEvent["type"], sh: BgShell) {
  const ev: JobEvent = { type, sessionId: sh.sessionKey, job: jobInfo(sh) };
  for (const cb of jobListeners) {
    try {
      cb(ev);
    } catch {
      // a broken listener must not break the shell lifecycle
    }
  }
}

/** Registered jobs of a session: everything running, plus shells that ended in
 * the last RECENT_MS (so the UI can show the outcome). Running first, then newest. */
export function listJobs(sessionId: string): JobInfo[] {
  const shells = sessions.get(sessionId);
  if (!shells) return [];
  const now = Date.now();
  return [...shells.values()]
    .filter((sh) => sh.status === null || (sh.endedAt !== null && now - sh.endedAt < RECENT_MS))
    .sort((a, b) =>
      (a.status === null) === (b.status === null)
        ? b.startedAt - a.startedAt
        : a.status === null
        ? -1
        : 1
    )
    .map(jobInfo);
}

/** Ids of the session's still-running shells (the interrupt-survivor note). */
export function runningIds(sessionId: string): string[] {
  return [...(sessions.get(sessionId)?.values() ?? [])]
    .filter((sh) => sh.status === null)
    .map((sh) => sh.id);
}

function shellsOf(ctx: ToolRunCtx): Map<string, BgShell> {
  const key = ctx.sessionId ?? "(no-session)";
  let m = sessions.get(key);
  if (!m) sessions.set(key, m = new Map());
  return m;
}

function append(sh: BgShell, text: string) {
  sh.buf += text;
  const over = sh.buf.length - MAX_BUF;
  if (over > 0) {
    if (sh.readAt < over) sh.droppedUnread = true;
    sh.buf = sh.buf.slice(over);
    sh.readAt = Math.max(0, sh.readAt - over);
  }
}

async function pump(stream: ReadableStream<Uint8Array>, sh: BgShell) {
  const dec = new TextDecoder();
  try {
    for await (const chunk of stream) append(sh, dec.decode(chunk, { stream: true }));
  } catch {
    // The stream broke with the process; the exit status still reports.
  }
}

/**
 * Wrap a spawned child in a shell struct and start pumping its output. Does NOT
 * register it — a foreground `bash` uses this to stream while it decides whether to
 * background (promote) or return inline. `status`/output are tracked either way.
 */
export function newShell(command: string, child: Deno.ChildProcess): BgShell {
  const sh: BgShell = {
    id: "",
    command,
    sessionKey: "",
    startedAt: Date.now(),
    endedAt: null,
    killed: false,
    child,
    buf: "",
    readAt: 0,
    droppedUnread: false,
    status: null,
    pumps: Promise.resolve(),
    claimed: false,
    notified: false,
  };
  sh.pumps = Promise.all([pump(child.stdout, sh), pump(child.stderr, sh)]).then(() => {});
  child.status.then((s) => {
    sh.status = s;
    sh.endedAt = Date.now();
    sh.onExit?.();
  });
  return sh;
}

/**
 * Register a running shell so later rounds/turns can read it, and wire its
 * completion note. Returns the assigned id, or null when the session is already at
 * MAX_RUNNING (the caller keeps waiting rather than detach). Used by both bashBg
 * (explicit) and bash's auto-background.
 */
export function promote(ctx: ToolRunCtx, sh: BgShell): string | null {
  const shells = shellsOf(ctx);
  const running = [...shells.values()].filter((s) => s.status === null).length;
  if (running >= MAX_RUNNING) return null;
  sh.id = `bg_${++seq}`;
  sh.sessionKey = ctx.sessionId ?? "(no-session)";
  shells.set(sh.id, sh);
  const notify = ctx.notify;
  sh.onExit = () => {
    emitJob("job.exited", sh);
    // Claimed (bashWait/bashKill) or already-noted → the model has/will get the
    // result in band; don't also wake it with a note.
    if (!notify || sh.notified || sh.claimed) return;
    const st = sh.status;
    const lines = sh.buf ? sh.buf.trimEnd().split("\n").filter(Boolean).length : 0;
    // A clean, silent fire-and-forget exit (code 0, no signal, no output) has
    // nothing to report — posting a note here wakes an IDLE session into a whole
    // LLM turn just to say "bg_N finished". Suppress the wake in that case; the
    // job.exited event above still surfaces the outcome in the jobs panel, and
    // non-zero exits, signal deaths, and any output all still notify.
    if ((st?.code ?? 0) === 0 && !st?.signal && lines === 0) {
      sh.notified = true;
      return;
    }
    sh.notified = true;
    notify(
      `[background] ${sh.id} finished (exit ${st?.code ?? "?"}${
        st?.signal ? ` on ${st.signal}` : ""
      }) — command "${sh.command.slice(0, 60)}", ${lines} line${lines === 1 ? "" : "s"} of ` +
        `output. Read it with bashOutput("${sh.id}").`,
    );
  };
  // Raced a near-instant exit between the caller's threshold and here.
  if (sh.status !== null) sh.onExit();
  else emitJob("job.spawned", sh);
  return sh.id;
}

/** The bash-tool inline format for a finished foreground command (see bash.ts). */
export function formatFinal(sh: BgShell): string {
  const body = sh.buf.trimEnd();
  const parts: string[] = [];
  if (body) parts.push(body);
  const code = sh.status?.code ?? 0;
  if (code !== 0) parts.push(`[exit code ${code}]`);
  return parts.join("\n") || "(no output)";
}

/** The string a foreground bash returns once it auto-backgrounds at the threshold. */
export function backgroundNote(sh: BgShell, id: string, afterMs: number): string {
  const soFar = sh.buf.slice(sh.readAt).trimEnd();
  sh.readAt = sh.buf.length;
  const head = `[still running after ${Math.round(afterMs / 1000)}s — moved to background as ` +
    `${id}. It keeps running; you'll be notified when it finishes. Read progress: ` +
    `bashOutput("${id}"); block until done: bashWait("${id}"); stop it: bashKill("${id}").]`;
  return soFar ? `${head}\n${soFar}` : head;
}

/** Spawn `command` detached; returns {id, pid} as JSON immediately. */
export async function bashBg(command: string, ctx: ToolRunCtx): Promise<string> {
  const running = [...shellsOf(ctx).values()].filter((s) => s.status === null).length;
  if (running >= MAX_RUNNING) {
    throw new Error(
      `this session already has ${running} running background shells; bashKill one first`,
    );
  }
  const { argv, env, cwd } = await shellInvocation(command, ctx);
  // No ctx.signal: an explicit background shell survives the turn's stop button.
  const child = new Deno.Command(argv[0], {
    args: argv.slice(1),
    cwd,
    env,
    stdin: "null",
    stdout: "piped",
    stderr: "piped",
  }).spawn();
  const sh = newShell(command, child);
  const id = promote(ctx, sh)!; // cap re-checked above; promote can't fail here
  return JSON.stringify({ id, pid: child.pid });
}

/** Output accrued since the last bashOutput(id) call, plus a status line. Safe to
 * call while the shell is still running — this is how the supervisor watches progress. */
export function bashOutput(id: string, ctx: ToolRunCtx): string {
  const sh = shellsOf(ctx).get(id);
  if (!sh) throw new Error(`no background shell ${id} in this session`);
  const parts: string[] = [];
  if (sh.droppedUnread) {
    parts.push(`[oldest output dropped — over ${MAX_BUF} chars accrued unread]`);
    sh.droppedUnread = false;
  }
  const fresh = sh.buf.slice(sh.readAt).trimEnd();
  sh.readAt = sh.buf.length;
  parts.push(fresh || "(no new output)");
  parts.push(
    sh.status === null
      ? "[running]"
      : `[exited with code ${sh.status.code}${sh.status.signal ? ` on ${sh.status.signal}` : ""}]`,
  );
  return parts.join("\n");
}

/** Block until the shell exits (returns immediately if already done), then return
 * its remaining output + exit line. The bash analog of subagent join: use it when
 * the result is needed before continuing, instead of a poll loop. */
export async function bashWait(id: string, ctx: ToolRunCtx): Promise<string> {
  const sh = shellsOf(ctx).get(id);
  if (!sh) throw new Error(`no background shell ${id} in this session`);
  sh.claimed = true; // result taken in-band — suppress the exit note
  if (sh.status === null) await sh.child.status;
  await sh.pumps.catch(() => {});
  return bashOutput(id, ctx);
}

/** SIGTERM the shell (graceful for servers that forward it), SIGKILL backstop.
 * Waits for the process to actually die (bounded by the backstop) so the result
 * reports the real outcome — `killed bg_3 (SIGTERM)` — not just intent. */
export async function bashKill(id: string, ctx: ToolRunCtx): Promise<string> {
  const sh = shellsOf(ctx).get(id);
  if (!sh) throw new Error(`no background shell ${id} in this session`);
  if (sh.status !== null) return `${id} already exited with code ${sh.status.code}`;
  sh.claimed = true; // deliberate kill — don't also post a completion note
  sh.killed = true; // the registry reports "killed", not a plain exit
  try {
    sh.child.kill("SIGTERM");
  } catch {
    // raced a natural exit
  }
  // Backstop for processes that ignore SIGTERM. unref: an idle timer must not
  // hold the server's event loop (or a test's op sanitizer) hostage.
  const backstop = setTimeout(() => {
    if (sh.status === null) {
      try {
        sh.child.kill("SIGKILL");
      } catch {
        // exited during the grace period
      }
    }
  }, KILL_GRACE_MS);
  Deno.unrefTimer(backstop);
  const st = await sh.child.status; // bounded: SIGKILL lands after the grace
  return `killed ${id} (${st.signal ?? `exit ${st.code}`})`;
}
