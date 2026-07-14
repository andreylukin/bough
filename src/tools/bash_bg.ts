/**
 * Background shells — bash's detached sibling, for commands that must outlive a
 * single program round (dev servers, long builds, watchers). `bashBg` spawns the
 * command under the SAME confinement as bash (Seatbelt profile + Claw Patrol proxy
 * env via shellInvocation) but detached from the turn: it deliberately does NOT
 * observe ctx.signal, so it survives program timeouts, turn ends, and the user's
 * stop button. `bashOutput` returns output accrued since the caller's last read
 * plus a status line; `bashKill` terminates (SIGTERM, then a SIGKILL backstop).
 *
 * Shells are registered per session, in memory: they persist across run_steps
 * rounds and turns of the same session, and die with the server process. A
 * session's shells are invisible to every other session.
 */
import type { ToolRunCtx } from "./types.ts";
import { shellInvocation } from "./bash.ts";

/** Retained output per shell; when exceeded, the oldest chars are dropped. */
const MAX_BUF = 400_000;
/** Running shells per session — a brake on loops that spawn and forget. */
const MAX_RUNNING = 8;
/** Grace between SIGTERM and the SIGKILL backstop. */
const KILL_GRACE_MS = 2_000;

interface BgShell {
  id: string;
  command: string;
  child: Deno.ChildProcess;
  /** Combined stdout+stderr in arrival order; capped at MAX_BUF. */
  buf: string;
  /** Chars of `buf` already returned by bashOutput. */
  readAt: number;
  /** The cap dropped output the caller never saw — reported once, then cleared. */
  droppedUnread: boolean;
  status: Deno.CommandStatus | null;
}

/** sessionKey → shellId → shell. Module-level: shells outlive turns, not the server. */
const sessions = new Map<string, Map<string, BgShell>>();
let seq = 0;

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

/** Spawn `command` detached; returns {id, pid} as JSON immediately. */
export async function bashBg(command: string, ctx: ToolRunCtx): Promise<string> {
  const shells = shellsOf(ctx);
  const running = [...shells.values()].filter((s) => s.status === null).length;
  if (running >= MAX_RUNNING) {
    throw new Error(
      `this session already has ${running} running background shells; bashKill one first`,
    );
  }
  const { argv, env } = await shellInvocation(command, ctx);
  const child = new Deno.Command(argv[0], {
    args: argv.slice(1),
    cwd: ctx.workspace,
    env,
    stdin: "null",
    stdout: "piped",
    stderr: "piped",
  }).spawn();
  const sh: BgShell = {
    id: `bg_${++seq}`,
    command,
    child,
    buf: "",
    readAt: 0,
    droppedUnread: false,
    status: null,
  };
  shells.set(sh.id, sh);
  pump(child.stdout, sh);
  pump(child.stderr, sh);
  child.status.then((s) => sh.status = s);
  return JSON.stringify({ id: sh.id, pid: child.pid });
}

/** Output accrued since the last bashOutput(id) call, plus a status line. */
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

/** SIGTERM the shell (graceful for servers that forward it), SIGKILL backstop. */
export function bashKill(id: string, ctx: ToolRunCtx): string {
  const sh = shellsOf(ctx).get(id);
  if (!sh) throw new Error(`no background shell ${id} in this session`);
  if (sh.status !== null) return `${id} already exited with code ${sh.status.code}`;
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
  return `sent SIGTERM to ${id} (pid ${sh.child.pid}); SIGKILL follows in ${
    KILL_GRACE_MS / 1000
  }s if ignored`;
}
