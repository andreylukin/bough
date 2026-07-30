/**
 * Background shells and the retained-output registry every shell surface reads
 * from.
 *
 * WHY THIS EXISTS. A shell lands here two ways, and the difference matters:
 *
 *   - **explicitly**, via `bashBg` — fire-and-forget work that is *supposed* to
 *     outlive the turn (a dev server, a watcher), so it is spawned WITHOUT the
 *     turn's abort signal and survives the user's stop button;
 *   - **automatically**, when a foreground `bash` is still running at the
 *     background threshold (`shell.ts`) — the running child is `promote`d here
 *     instead of being killed.
 *
 * THE INVARIANT THIS HOLDS: **a long command is never lost and never blocks the
 * turn** (plan §6.7). The auto-background handoff only works if the output the
 * command already produced, and everything it produces afterwards, stays readable
 * from a later round — otherwise "moved to background as bg_N" would be a promise
 * the harness cannot keep, and the model would go back to writing sleep/poll
 * loops. So:
 *
 *   1. Buffers are retained per shell and readable **while running**.
 *      `bashOutput` returns what accrued since the caller's last read plus a
 *      `[running]`/`[exited …]` status line; `bashWait` blocks (the bash analog of
 *      subagent join) so nobody polls.
 *   2. Retention is bounded but **deterministic**: head and tail kept verbatim
 *      with an explicit omission marker in between (spec §6). There is no LLM
 *      digestion of output and no `extract()` (spec §17) — a bounded buffer that
 *      dropped the *head* would silently rewrite what the model already saw.
 *   3. Exit is **announced**, not discovered. Every shell publishes `job.spawned`
 *      and `job.exited` on the bus, and an unclaimed exit posts a `[background]`
 *      system note so the model is told it finished.
 *
 * Shells are registered per session, in memory: they persist across `run_steps`
 * rounds and across turns of the same session, and die with the server process.
 * That is why `BackgroundJob` is not a table — a persisted row would always be a
 * lie after a restart (schema/parts.ts).
 *
 * Ported from `src/tools/bash_bg.ts`. Deltas from that port are marked `NOTE:`.
 */

import type { Subprocess } from "bun";
import type { BackgroundJob } from "../schema/parts.ts";
import type { Bus } from "../types.ts";
import { ConflictError, NotFoundError, ProgramError } from "../errors.ts";

// ---------------------------------------------------------------------------
// Deterministic truncation
// ---------------------------------------------------------------------------

/**
 * Verbatim head retained per shell. Smaller than the tail on purpose: the head of
 * a build log is the invocation and the first failure, the tail is where it ended
 * up, and the middle is the part nobody reads.
 */
export const MAX_HEAD_CHARS = 100_000;
/** Verbatim tail retained per shell. */
export const MAX_TAIL_CHARS = 300_000;

/** How much output a single shell retains before the middle starts being omitted. */
export const MAX_BUF = MAX_HEAD_CHARS + MAX_TAIL_CHARS;

/** Head/tail budget for one retained buffer. Injected so tests can use small ones. */
export interface TruncateLimits {
  head?: number;
  tail?: number;
}

/**
 * The omission marker.
 *
 * NOTE: the port dropped the OLDEST chars and said so in one line. Spec §6 asks
 * for head + tail verbatim instead, which means the marker has to name how much
 * went missing from the *middle* — and, because error text is a product surface,
 * name the move that avoids it next time (filter at the source).
 */
function omissionMarker(omitted: number, total: number): string {
  return `\n[… ${omitted} chars omitted from the middle of ${total} — head and tail are ` +
    `verbatim. Filter at the source (rg, head, tail, targeted reads) instead of dumping ` +
    `output …]\n`;
}

/**
 * Keep the first `head` and last `tail` characters verbatim, with an explicit
 * marker where the middle used to be. Pure and deterministic: the same input
 * always yields the same output, and nothing summarizes anything.
 */
export function truncateMiddle(text: string, limits: TruncateLimits = {}): string {
  const head = limits.head ?? MAX_HEAD_CHARS;
  const tail = limits.tail ?? MAX_TAIL_CHARS;
  if (text.length <= head + tail) return text;
  const omitted = text.length - head - tail;
  return text.slice(0, head) + omissionMarker(omitted, text.length) +
    text.slice(text.length - tail);
}

// ---------------------------------------------------------------------------
// One shell
// ---------------------------------------------------------------------------

/** Grace between SIGTERM and the SIGKILL backstop in `bashKill`. */
const KILL_GRACE_MS = 2_000;
/** Running `bashBg` shells per session — a brake on loops that spawn and forget. */
const MAX_RUNNING = 8;
/**
 * How long an exited shell stays in `listJobs`. Long enough that the outcome of a
 * job you started is still there when you look up from something else — five
 * minutes meant a failed build aged out before anyone saw it.
 */
const RECENT_MS = 30 * 60_000;

/**
 * A tracked shell. The buffer is three fields rather than one string because
 * retention is head + tail: `head` fills once and is then immutable, `tail` rolls,
 * and `written` counts everything the process ever produced so a reader can tell
 * how much it missed.
 */
export interface ExitStatus {
  code: number;
  signal: NodeJS.Signals | null;
}

export interface Shell {
  /** Assigned by `promote`; `""` while a foreground `bash` still owns it. */
  id: string;
  /**
   * What this job IS, in the words of whoever started it — `dev server`, `full
   * test run`. Assigned by `promote` alongside the id.
   *
   * Required of `bashBg` and never inferred there: `bg_7` beside 200 characters of
   * `NODE_ENV=test npx vitest run --reporter=…` told the user which shell it was
   * only if they read the command and already knew why it was running. An
   * auto-backgrounded `bash` gets `deriveName(command)` instead, because the model
   * never chose to background that one and cannot be asked after the fact.
   */
  name: string;
  command: string;
  sessionId: string;
  pid: number;
  startedAt: number;
  endedAt: number | null;
  /** Set by `bashKill`/`killJobsOf` so the exit reads as a kill, not a plain exit. */
  killed: boolean;
  child: Subprocess<"ignore", "pipe", "pipe">;
  /** First `limits.head` chars, verbatim and immutable once full. */
  head: string;
  /** Rolling last `limits.tail` chars. */
  tail: string;
  /** Total chars the process has produced, including what retention dropped. */
  written: number;
  /** Chars of the stream already handed to `bashOutput`. */
  readTo: number;
  status: ExitStatus | null;
  /** Resolves with the exit status. Bun's `exited` yields a bare code; this carries
   * the signal too, which every exit line and kill report needs. */
  exit: Promise<ExitStatus>;
  /** Resolves when both stdout and stderr have fully drained. */
  pumps: Promise<void>;
  /** Fired after `status` is set — wired by `promote`. */
  onExit?: () => void;
  /** `bashWait`/`bashKill` set this: the result was taken in band, suppress the note. */
  claimed: boolean;
  /** Guards against a double completion note. */
  notified: boolean;
  limits: Required<TruncateLimits>;
}

/**
 * Argv + cwd for running `command`.
 *
 * There is NO confinement of any kind. The shell is a plain host `/bin/sh` running
 * as the user, starting in the session workspace; it may cd and write anywhere the
 * user can, and egress goes direct with the host's own credentials. The workspace
 * is a starting point, not a boundary (spec §2.2).
 *
 * This is deliberate. Running in the real checkout is what makes `git commit &&
 * git push` simply work, which is the whole delivery mechanism (spec §2.3).
 */
/** Longest job name kept. Past this it is a description, not a label. */
const MAX_NAME_CHARS = 60;

/**
 * A job name, normalized — or `null` when there is nothing usable in it.
 *
 * Whitespace collapses (a name arriving off a template literal is often indented)
 * and control characters go, because this string is painted into a single rail row
 * and an embedded escape sequence would repaint the screen.
 */
export function normalizeJobName(raw: unknown): string | null {
  if (typeof raw !== "string") return null;
  // deno-lint-ignore no-control-regex
  const clean = raw.replace(/[\x00-\x1f\x7f]/g, " ").replace(/\s+/g, " ").trim();
  if (!clean) return null;
  return clean.length > MAX_NAME_CHARS ? `${clean.slice(0, MAX_NAME_CHARS - 1)}…` : clean;
}

/**
 * A name for a shell nobody named: the auto-background path, where the model ran a
 * plain `bash` and the 60s threshold — not a decision — made it a job.
 *
 * The command's first meaningful words, with a `cd … &&` prelude and leading
 * `VAR=value` assignments dropped, because `NODE_ENV=test npm test` is a test run
 * and not an environment variable.
 */
export function deriveName(command: string): string {
  let rest = command.trim().split("\n")[0] ?? "";
  rest = rest.replace(/^(?:cd\s+\S+\s*&&\s*)+/, "");
  rest = rest.replace(/^(?:[A-Za-z_][A-Za-z0-9_]*=(?:"[^"]*"|'[^']*'|\S*)\s+)+/, "");
  return normalizeJobName(rest.split(/\s*(?:\||&&|;)\s*/)[0] ?? "") ?? "shell";
}

export function shellInvocation(
  command: string,
  cwd?: string,
): { argv: [string, ...string[]]; cwd?: string } {
  return { argv: ["/bin/sh", "-c", command], cwd };
}

/** Append to the retained buffer, maintaining head-then-rolling-tail. */
function append(shell: Shell, text: string): void {
  shell.written += text.length;
  let rest = text;
  if (shell.head.length < shell.limits.head) {
    const take = Math.min(shell.limits.head - shell.head.length, rest.length);
    shell.head += rest.slice(0, take);
    rest = rest.slice(take);
  }
  if (!rest) return;
  shell.tail += rest;
  if (shell.tail.length > shell.limits.tail) {
    shell.tail = shell.tail.slice(shell.tail.length - shell.limits.tail);
  }
}

/**
 * The retained stream from absolute offset `from`, with the marker standing in for
 * whatever retention dropped. `omitted` is reported separately so `bashOutput` can
 * say that output the *caller* never read went missing, which is a different fact
 * from the buffer having a hole in it.
 */
function retainedFrom(shell: Shell, from: number): { text: string; omitted: number } {
  const headEnd = shell.head.length;
  const tailStart = shell.written - shell.tail.length;
  const parts: string[] = [];
  if (from < headEnd) parts.push(shell.head.slice(from));
  const gapFrom = Math.max(from, headEnd);
  const omitted = gapFrom < tailStart ? tailStart - gapFrom : 0;
  if (omitted > 0) parts.push(omissionMarker(omitted, shell.written));
  const tailFrom = Math.max(from, tailStart);
  if (tailFrom < shell.written) parts.push(shell.tail.slice(tailFrom - tailStart));
  return { text: parts.join(""), omitted };
}

/** Everything retained, from the beginning. */
export function shellText(shell: Shell): string {
  return retainedFrom(shell, 0).text;
}

async function pump(stream: ReadableStream<Uint8Array>, shell: Shell): Promise<void> {
  const dec = new TextDecoder();
  try {
    for await (const chunk of stream) append(shell, dec.decode(chunk, { stream: true }));
  } catch {
    // The stream broke with the process; the exit status still reports.
  }
}

/** The inline format for a finished foreground command (see `shell.ts`). */
export function formatFinal(shell: Shell): string {
  const body = shellText(shell).trimEnd();
  const parts: string[] = [];
  if (body) parts.push(body);
  const code = shell.status?.code ?? 0;
  const signal = shell.status?.signal;
  if (code !== 0 || signal) {
    parts.push(`[exit code ${code}${signal ? ` on ${signal}` : ""}]`);
  }
  return parts.join("\n") || "(no output)";
}

/**
 * What a foreground `bash` returns once it auto-backgrounds.
 *
 * Every clause is load-bearing: the command is still ALIVE (so the model must not
 * re-run it), the id is how to reach it, and the three verbs are named outright so
 * the next round reads progress instead of inventing a sleep loop (plan §6.7).
 */
export function backgroundNote(shell: Shell, id: string, afterMs: number): string {
  const { text } = retainedFrom(shell, shell.readTo);
  shell.readTo = shell.written;
  const head = `[still running after ${Math.round(afterMs / 1000)}s — moved to background as ` +
    `${id}${shell.name ? ` "${shell.name}"` : ""}. It keeps running; you'll be notified ` +
    `when it finishes. Read progress: ` +
    `bashOutput("${id}"); block until done: bashWait("${id}"); stop it: bashKill("${id}").]`;
  const soFar = text.trimEnd();
  return soFar ? `${head}\n${soFar}` : head;
}

// ---------------------------------------------------------------------------
// The registry
// ---------------------------------------------------------------------------

/** What a job verb needs to know about its caller. A `TurnCtx` satisfies it. */
export interface JobCtx {
  sessionId: string;
  workspace: string;
}

export interface JobRegistryOptions {
  /** Where `job.spawned`/`job.exited` go. Absent = no events (unit tests). */
  bus?: Bus;
  /**
   * Posts the `[background] bg_N finished …` system note (spec §6).
   *
   * NOTE: the port read this off `ctx.notify`. `TurnCtx` (types.ts, frozen) has no
   * such field, and `hostfn/` may not import from `server/`, so the seam is here:
   * the turn runner hands the registry a notifier at wiring time.
   */
  notify?: (sessionId: string, text: string) => void;
  /** Injected clock. Absent = `Date.now`. */
  now?: () => number;
  /** Retention budget per shell. */
  limits?: TruncateLimits;
  /** Concurrent `bashBg` shells allowed per session. */
  maxRunning?: number;
}

/**
 * The per-session shell registry.
 *
 * It is a class rather than module state so a test can construct one, drive it,
 * and throw it away — but the server still needs exactly ONE for the process,
 * because a job outlives the turn that started it and `AppCtx` (frozen) carries no
 * slot to thread it through. Hence `jobs` below: injectable everywhere, defaulted
 * once.
 */
export class JobRegistry {
  /** sessionId → shellId → shell. */
  readonly #sessions = new Map<string, Map<string, Shell>>();
  /** Foreground shells currently inside `bash`, by session (see `inflightForegroundOutput`). */
  readonly #foreground = new Map<string, Set<Shell>>();
  #seq = 0;
  #bus: Bus | undefined;
  #notify: ((sessionId: string, text: string) => void) | undefined;
  readonly #now: () => number;
  readonly #limits: Required<TruncateLimits>;
  readonly #maxRunning: number;

  constructor(options: JobRegistryOptions = {}) {
    this.#bus = options.bus;
    this.#notify = options.notify;
    this.#now = options.now ?? Date.now;
    this.#limits = {
      head: options.limits?.head ?? MAX_HEAD_CHARS,
      tail: options.limits?.tail ?? MAX_TAIL_CHARS,
    };
    this.#maxRunning = options.maxRunning ?? MAX_RUNNING;
  }

  /** Wire the bus after construction — the server builds one before the other. */
  attachBus(bus: Bus): void {
    this.#bus = bus;
  }

  /**
   * Wire the `[background]` system-note poster after construction. Same reason as
   * `attachBus`: the process-wide registry exists before the turn runner that
   * knows how to post a note does.
   */
  attachNotifier(notify: (sessionId: string, text: string) => void): void {
    this.#notify = notify;
  }

  // -- spawning -------------------------------------------------------------

  /**
   * Spawn a shell and start pumping its output. Does NOT register it: a foreground
   * `bash` uses this to stream while it decides whether to background (`promote`)
   * or return inline. Output and status are tracked either way.
   *
   * `signal` is honoured by `killTreeOnAbort` in `shell.ts` and is DELIBERATELY NOT
   * handed to `Bun.spawn`. Bun's `signal` option registers its own abort listener at
   * spawn time — before the caller can add one — and it SIGTERMs the direct child
   * only. `sh -c 'sleep 300; echo done'` forks rather than execs, so Bun killed the
   * `sh`, the `sleep` reparented onto init, and the tree walk that runs next found an
   * empty tree: the interrupt printed "the program's children are killed" while the
   * command ran to completion. One killer, and it is the one that walks the tree.
   */
  spawn(
    command: string,
    opts: { cwd?: string; signal?: AbortSignal; scratch?: string } = {},
  ): Shell {
    const { argv, cwd } = shellInvocation(command, opts.cwd);
    const child = Bun.spawn(argv, {
      cwd,
      // `$BOUGH_SCRATCH` in the shell, because the prompt's sentence about a
      // scratchpad reaches the model and not the command it writes. A `curl -o` or a
      // `pytest --junitxml` is composed as text, and without a variable to name the
      // one place it should point at, it points at /tmp — the exact gap every report
      // of this problem describes.
      ...(opts.scratch ? { env: { ...process.env, BOUGH_SCRATCH: opts.scratch } } : {}),
      stdin: "ignore",
      stdout: "pipe",
      stderr: "pipe",
    });
    const exit: Promise<ExitStatus> = child.exited.then(() => ({
      code: child.exitCode ?? 0,
      signal: child.signalCode ?? null,
    }));
    const shell: Shell = {
      id: "",
      name: "",
      command,
      sessionId: "",
      pid: child.pid,
      startedAt: this.#now(),
      endedAt: null,
      killed: false,
      child,
      exit,
      head: "",
      tail: "",
      written: 0,
      readTo: 0,
      status: null,
      pumps: Promise.resolve(),
      claimed: false,
      notified: false,
      limits: this.#limits,
    };
    shell.pumps = Promise.all([pump(child.stdout, shell), pump(child.stderr, shell)])
      .then(() => {});
    exit.then((status) => {
      shell.status = status;
      shell.endedAt = this.#now();
      shell.onExit?.();
    });
    return shell;
  }

  /**
   * Register a running shell so later rounds and turns can read it, and wire its
   * completion note. Returns the assigned id, or `null` when the session is already
   * at the concurrency cap.
   *
   * NOTE: `force` is a delta from the port. There, a full registry made the
   * foreground `bash` fall back to blocking and then SIGKILL the command at a hard
   * cap. Plan §6.7 says auto-background never kills, and the cap exists to brake
   * `bashBg` loops the model chose to write — not to punish a command that merely
   * took a while. So the auto-background path forces registration and the cap stays
   * where it belongs, on explicit `bashBg`.
   */
  promote(
    shell: Shell,
    ctx: JobCtx,
    opts: { force?: boolean; name?: string } = {},
  ): string | null {
    const shells = this.#shellsOf(ctx.sessionId);
    if (!opts.force && this.#runningCount(ctx.sessionId) >= this.#maxRunning) return null;
    shell.id = `bg_${++this.#seq}`;
    // Never empty. `bashBg` has already refused a blank one; the auto-background
    // path passes none and gets the command's own first words.
    shell.name = normalizeJobName(opts.name) ?? deriveName(shell.command);
    shell.sessionId = ctx.sessionId;
    shells.set(shell.id, shell);
    shell.onExit = () => this.#onExit(shell);
    // Raced a near-instant exit between the caller's threshold and here.
    if (shell.status !== null) shell.onExit();
    else this.#emit("job.spawned", shell);
    return shell.id;
  }

  // -- the four job verbs ---------------------------------------------------

  /**
   * Spawn `command` detached under `name`; returns `{id, name, pid}` as JSON.
   *
   * The name is REQUIRED and refused when blank rather than derived from the
   * command. A background job is the one thing here the user watches without
   * having read the round that started it — the rail row, the completion note and
   * the job view all lead with this string — and only the caller knows whether
   * `npm run build` is "the release build" or "reproducing the cache bug". The
   * auto-background path is the exception (`promote`), because nothing chose it.
   */
  /**
   * `opts.wake: false` suppresses the `[background]` completion note.
   *
   * For a shell the USER started (`!command` in the composer — `server/jobs.ts`) the
   * note is not just noise, it is a bill: the note wakes an idle session into a whole
   * LLM turn, and the first thing that turn does is `bashOutput(bg_N)`, which ALSO
   * advances the read cursor and empties the card the user was about to read. Measured
   * on `!ls -1`: a 20k-token round trip in which the agent read the user's `ls` output
   * and narrated the directory listing back to them. The job still emits its events and
   * still sits in the rail and the job list, so the model can read it when ASKED.
   */
  bashBg(name: string, command: string, ctx: JobCtx, opts: { wake?: boolean } = {}): string {
    const label = normalizeJobName(name);
    if (!label) {
      throw new ProgramError(
        `bashBg needs a NAME for the job before the command: ` +
          `bashBg("dev server", "npm run dev"). The name is what the user sees in ` +
          `the live-work rail and in the finished-job note, so make it say what the ` +
          `job is for, not what the command is.`,
      );
    }
    if (!command || !command.trim()) {
      throw new ProgramError(
        `bashBg("${label}", …) has no command to run — the name comes first now: ` +
          `bashBg(name, cmd).`,
      );
    }
    const running = this.#runningCount(ctx.sessionId);
    if (running >= this.#maxRunning) {
      throw new ConflictError(
        `this session already has ${running} running background shells (the cap is ` +
          `${this.#maxRunning}) — bashKill one of ${
            this.runningIds(ctx.sessionId).join(", ")
          } first, or wait for one to finish with bashWait.`,
      );
    }
    // No signal: an explicit background shell survives the turn's stop button.
    const shell = this.spawn(command, { cwd: ctx.workspace });
    const id = this.promote(shell, ctx, { force: true, name: label })!;
    // Set BEFORE the process can exit: `notified` is the flag `#onExit` checks, and a
    // fast command (`echo`, `ls`) can finish inside this same tick.
    if (opts.wake === false) shell.notified = true;
    return JSON.stringify({ id, name: shell.name, pid: shell.pid });
  }

  /**
   * Output accrued since the last `bashOutput(id)` call, plus a status line. Safe
   * while the shell is still running — this is how a program watches progress
   * without polling the process itself.
   */
  bashOutput(id: string, sessionId: string): string {
    const shell = this.#require(id, sessionId);
    const { text } = retainedFrom(shell, shell.readTo);
    shell.readTo = shell.written;
    const fresh = text.trimEnd();
    const status = shell.status === null
      ? "[running]"
      : `[exited with code ${shell.status.code}${
        shell.status.signal ? ` on ${shell.status.signal}` : ""
      }]`;
    return `${fresh || "(no new output)"}\n${status}`;
  }

  /**
   * Block until the shell exits (returns immediately if it already has), then
   * return its remaining output and exit line. The bash analog of subagent join:
   * use it when the result is needed before continuing, instead of a poll loop.
   */
  async bashWait(id: string, sessionId: string): Promise<string> {
    const shell = this.#require(id, sessionId);
    shell.claimed = true; // result taken in band — suppress the exit note
    if (shell.status === null) await shell.exit;
    await shell.pumps.catch(() => {});
    return this.bashOutput(id, sessionId);
  }

  /**
   * SIGTERM the shell (graceful for servers that forward it) with a SIGKILL
   * backstop. Waits for the process to actually die, so the result reports the real
   * outcome rather than the intent.
   */
  async bashKill(id: string, sessionId: string): Promise<string> {
    const shell = this.#require(id, sessionId);
    if (shell.status !== null) {
      return `${id} already exited with code ${shell.status.code}`;
    }
    shell.claimed = true; // a deliberate kill — don't also post a completion note
    shell.killed = true;
    signalTree(shell, "SIGTERM");
    // Backstop for processes that ignore SIGTERM. Unref'd: an idle timer must not
    // hold the server's event loop hostage.
    const backstop = setTimeout(() => {
      if (shell.status === null) signalTree(shell, "SIGKILL");
    }, KILL_GRACE_MS);
    backstop.unref();
    const status = await shell.exit; // bounded: SIGKILL lands after the grace
    clearTimeout(backstop);
    await shell.pumps.catch(() => {});
    return `killed ${id} (${status.signal ?? `exit ${status.code}`})`;
  }

  // -- the jobs API surface (T6.8 reads these) ------------------------------

  /**
   * The session's jobs: everything running, plus shells that ended within
   * `RECENT_MS`. Running first, then newest.
   */
  listJobs(sessionId: string): BackgroundJob[] {
    const shells = this.#sessions.get(sessionId);
    if (!shells) return [];
    const now = this.#now();
    return [...shells.values()]
      .filter((s) => s.status === null || (s.endedAt !== null && now - s.endedAt < RECENT_MS))
      .sort((a, b) =>
        (a.status === null) === (b.status === null)
          ? b.startedAt - a.startedAt
          : a.status === null
          ? -1
          : 1
      )
      .map(jobInfo);
  }

  /**
   * The shell's whole retained buffer, for the jobs tab's output view.
   *
   * Deliberately does NOT advance `readTo`: that cursor belongs to the model's
   * `bashOutput`, and a UI read that stole from it would make output vanish from
   * the agent's context just because a human looked at it.
   */
  /**
   * The last `lines` lines of a shell's buffer, plus how many there are in total.
   *
   * Non-destructive, like `jobOutput` and for the same reason: a human glancing at a
   * log must not make that output vanish from the agent's next `bashOutput`. This
   * exists because the job LISTING carried no output at all, so every job card in the
   * transcript rendered its header and nothing else — the `tail` field the renderer has
   * always had was never populated by anything but tests, and the only way to see what
   * a command printed was to open it.
   */
  jobTail(id: string, lines = 5): { tail: string[]; outputLines: number } | null {
    const shell = this.#find(id);
    if (!shell) return null;
    const body = shellText(shell).trimEnd();
    if (!body) return { tail: [], outputLines: 0 };
    const all = body.split("\n");
    return { tail: all.slice(-lines), outputLines: all.length };
  }

  jobOutput(id: string): { output: string; job: BackgroundJob } | null {
    const shell = this.#find(id);
    if (!shell) return null;
    return { output: shellText(shell).trimEnd(), job: jobInfo(shell) };
  }

  /**
   * Kill by id alone — the UI's kill path. The jobs endpoint aggregates a session's
   * shells with its subagents', so anything the UI can *list* it must also be able
   * to read and kill; keying that off the open session 404'd on every subagent row.
   */
  killJob(id: string): Promise<string> {
    const shell = this.#find(id);
    if (!shell) {
      throw new NotFoundError(
        `no background shell ${id} — it may have aged out of the job list, or belong ` +
          `to no session this server knows about.`,
      );
    }
    return this.bashKill(id, shell.sessionId);
  }

  /**
   * Wait for every tracked shell's pipes to finish draining.
   *
   * A killed process closes its pipes asynchronously, so "the status resolved" and
   * "the output is complete" are different moments. Shutdown wants the second one,
   * and so does a test that must not leave a half-read pipe behind.
   */
  async drain(): Promise<void> {
    const pending: Promise<unknown>[] = [];
    for (const shells of this.#sessions.values()) {
      for (const shell of shells.values()) {
        pending.push(shell.exit.catch(() => {}), shell.pumps.catch(() => {}));
      }
    }
    await Promise.all(pending);
  }

  /** Ids of the session's still-running shells (the interrupt-survivor note). */
  runningIds(sessionId: string): string[] {
    return [...(this.#sessions.get(sessionId)?.values() ?? [])]
      .filter((s) => s.status === null)
      .map((s) => s.id);
  }

  /**
   * SIGTERM the session's running shells — "stop everything in this conversation"
   * reaching background work the same way it reaches subagents. Best-effort and
   * synchronous.
   */
  killJobsOf(sessionId: string): number {
    let n = 0;
    for (const shell of this.#sessions.get(sessionId)?.values() ?? []) {
      if (shell.status !== null) continue;
      shell.killed = true;
      shell.claimed = true; // a deliberate stop — don't also wake the model with a note
      signalTree(shell, "SIGTERM");
      n++;
    }
    return n;
  }

  /**
   * SIGTERM every running shell, for server shutdown.
   *
   * Background shells are in-memory by design, so killing the server must take
   * their processes with it. It very nearly did so by accident: a shell that writes
   * output dies of SIGPIPE when the server's end of its stdout pipe closes. A
   * SILENT one (a bare sleep, an idle dev server, a build between writes) never
   * touches the broken pipe and survives, reparented and invisible, with nothing
   * left that knows it exists.
   */
  killAll(): number {
    let n = 0;
    for (const sessionId of this.#sessions.keys()) n += this.killJobsOf(sessionId);
    return n;
  }

  // -- foreground tracking (interrupt-time partial output) -------------------

  /**
   * Track a foreground shell for the duration of a `bash` call. Returns the
   * untrack thunk.
   *
   * An interrupt terminates the program's worker before the host call can return,
   * so output the command already produced would vanish with it. The turn runner
   * reads these buffers at interrupt time and attaches them to the tool record
   * instead.
   */
  trackForeground(shell: Shell, sessionId: string): () => void {
    let set = this.#foreground.get(sessionId);
    if (!set) this.#foreground.set(sessionId, set = new Set());
    set.add(shell);
    return () => {
      set!.delete(shell);
      if (set!.size === 0) this.#foreground.delete(sessionId);
    };
  }

  /**
   * Partial output of this session's in-flight foreground `bash` calls, one block
   * per command, or `null` when there is none. Read-only; the buffers keep filling.
   */
  inflightForegroundOutput(sessionId: string): string | null {
    const set = this.#foreground.get(sessionId);
    if (!set?.size) return null;
    const blocks = [...set]
      .map((shell) => ({ shell, body: shellText(shell).trimEnd() }))
      .filter(({ body }) => body.length > 0)
      .map(({ shell, body }) =>
        `[interrupted] bash "${shell.command.slice(0, 60)}" — output before the interrupt:\n` +
        body
      );
    return blocks.length ? blocks.join("\n") : null;
  }

  // -- internals ------------------------------------------------------------

  #shellsOf(sessionId: string): Map<string, Shell> {
    let m = this.#sessions.get(sessionId);
    if (!m) this.#sessions.set(sessionId, m = new Map());
    return m;
  }

  #runningCount(sessionId: string): number {
    let n = 0;
    for (const shell of this.#sessions.get(sessionId)?.values() ?? []) {
      if (shell.status === null) n++;
    }
    return n;
  }

  /** Lookup across every session — the jobs API aggregates subagent rows. */
  #find(id: string): Shell | null {
    for (const shells of this.#sessions.values()) {
      const shell = shells.get(id);
      if (shell) return shell;
    }
    return null;
  }

  /**
   * The session-scoped lookup the model's verbs use. Names the ids that DO exist,
   * because the usual cause is a copied id from another session's transcript and
   * "not found" alone gives the next round nothing to act on (spec §6).
   */
  #require(id: string, sessionId: string): Shell {
    const shell = this.#sessions.get(sessionId)?.get(id);
    if (shell) return shell;
    const known = [...(this.#sessions.get(sessionId)?.values() ?? [])]
      .map((s) => (s.name ? `${s.id} "${s.name}"` : s.id));
    throw new NotFoundError(
      `no background shell ${id} in this session` +
        (known.length
          ? ` — this session has ${known.join(", ")}.`
          : ` — this session has started none; bashBg(name, cmd) starts one, and a bash() ` +
            `command that runs past the background threshold reports the id it was ` +
            `moved to.`),
    );
  }

  #emit(type: "job.spawned" | "job.exited", shell: Shell): void {
    this.#bus?.publish({ type, sessionId: shell.sessionId, data: jobInfo(shell) });
  }

  #onExit(shell: Shell): void {
    this.#emit("job.exited", shell);
    // Claimed (bashWait/bashKill) or already noted → the model has, or will get,
    // the result in band; don't also wake it with a note.
    if (!this.#notify || shell.notified || shell.claimed) return;
    shell.notified = true;
    const status = shell.status;
    const body = shellText(shell).trimEnd();
    const lines = body ? body.split("\n").filter(Boolean).length : 0;
    // A clean, silent fire-and-forget exit (code 0, no signal, no output) has
    // nothing to report — a note there wakes an IDLE session into a whole LLM turn
    // just to say "bg_N finished". The `job.exited` event above still surfaces the
    // outcome; non-zero exits, signal deaths and any output all still notify.
    if ((status?.code ?? 0) === 0 && !status?.signal && lines === 0) return;
    this.#notify(
      shell.sessionId,
      `[background] ${shell.id} "${shell.name}" finished (exit ${status?.code ?? "?"}${
        status?.signal ? ` on ${status.signal}` : ""
      }) — command "${shell.command.slice(0, 60)}", ${lines} line${
        lines === 1 ? "" : "s"
      } of output. Read it with bashOutput("${shell.id}").`,
    );
  }
}

/**
 * The wire shape of a shell.
 *
 * NOTE: `BackgroundJob` (schema/parts.ts, frozen) has only `running` and `exited`,
 * so a killed shell reports as `exited` — its exit code and the `killed` flag on
 * the `Shell` carry the distinction internally, and `bashKill`'s return string
 * carries it to the model.
 */
function jobInfo(shell: Shell): BackgroundJob {
  return {
    id: shell.id,
    name: shell.name,
    sessionId: shell.sessionId,
    pid: shell.pid,
    command: shell.command,
    status: shell.status === null ? "running" : "exited",
    exitCode: shell.status?.code ?? null,
    signal: shell.status?.signal ?? null,
    startedAt: shell.startedAt,
    exitedAt: shell.endedAt,
  };
}

// ---------------------------------------------------------------------------
// Process trees
// ---------------------------------------------------------------------------

/**
 * Every descendant pid of `root`, deepest first.
 *
 * Signalling the shell is not enough: `sh -c 'sleep 900'` does not forward SIGTERM
 * to its foreground child, so killing the shell orphaned the grandchild — a stopped
 * `npm run dev` left node holding the port. macOS has no `setsid`, so there is no
 * process group to signal; the portable answer is to read the tree out of `ps`.
 * Synchronous because a shutdown handler has no chance to await.
 */
export function descendantPids(root: number): number[] {
  let text = "";
  try {
    const r = Bun.spawnSync(["ps", "-Ao", "pid=,ppid="], {
      stdout: "pipe",
      stderr: "ignore",
    });
    text = new TextDecoder().decode(r.stdout);
  } catch {
    return []; // no ps: signal the shell alone
  }
  const kids = new Map<number, number[]>();
  for (const line of text.split("\n")) {
    const m = /^\s*(\d+)\s+(\d+)\s*$/.exec(line);
    if (!m) continue;
    const [pid, ppid] = [Number(m[1]), Number(m[2])];
    kids.set(ppid, [...(kids.get(ppid) ?? []), pid]);
  }
  const out: number[] = [];
  const seen = new Set<number>([root]);
  const walk = (p: number) => {
    for (const c of kids.get(p) ?? []) {
      if (seen.has(c)) continue; // ps raced a reparent; don't loop
      seen.add(c);
      walk(c);
      out.push(c);
    }
  };
  walk(root);
  return out;
}

/**
 * Signal the shell AND everything it spawned, descendants first so a parent cannot
 * restart one after we have passed it.
 */
export function signalTree(shell: Shell, sig: NodeJS.Signals): void {
  for (const pid of descendantPids(shell.pid)) {
    if (pid <= 1 || pid === process.pid) continue; // never signal init or ourselves
    try {
      process.kill(pid, sig);
    } catch {
      // already gone
    }
  }
  try {
    shell.child.kill(sig);
  } catch {
    // raced a natural exit
  }
}

/**
 * The process-wide registry.
 *
 * Jobs outlive the turn that started them and `AppCtx` carries no slot to thread a
 * registry through, so the server wires this one at boot (`attachBus`, `notify` via
 * a `JobRegistry` it constructs) and every shell host function defaults to it.
 * Tests pass their own instance and never touch this.
 */
export const jobs: JobRegistry = new JobRegistry();
