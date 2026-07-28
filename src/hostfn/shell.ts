/**
 * The shell verbs a program calls: `bash`, `sh`, and the four job verbs wired
 * through to the registry in `jobs.ts`.
 *
 * WHY THIS EXISTS. A program runs as the user with the user's full authority, and
 * could call `Bun.spawn` directly for any of this. These exist for the three
 * things a raw spawn cannot do: carry the **turn's interrupt**, hand a long command
 * to the **background registry** instead of blocking the round on it, and bound the
 * output that crosses back into the model's context **deterministically**. Nothing
 * here is confinement (spec §2.2).
 *
 * THE INVARIANT THIS HOLDS: **a foreground command never blocks the turn and is
 * never killed for taking too long** (plan §6.7). Past the threshold, `bash`
 * returns "…moved to background as bg_N" and the command KEEPS RUNNING — the model
 * reads it with `bashOutput`, blocks on it with `bashWait`, and is told when it
 * exits. That is the whole reason a program never has to write a sleep/poll loop,
 * so "it timed out, try again" is not an outcome this module is allowed to produce.
 *
 * The second rule, and the reason `sh` is not implemented in terms of `bash`:
 * **`sh` never throws on a non-zero exit.** Its purpose is fanning out commands
 * that are ALLOWED to fail — linters, greps, per-package builds — and inspecting
 * the codes, so the exit code is returned as data, per command, in input order. It
 * also must not auto-background: a backgrounded shell has no exit code yet, and
 * `[{code, out}]` with a missing code is a contract the caller cannot branch on.
 *
 * Ported from `src/tools/bash.ts`. Deltas from that port are marked `NOTE:`.
 */

import { z } from "zod";
import { ProgramError } from "../errors.ts";
import type { HostFns, TurnCtx } from "../types.ts";
import {
  backgroundNote,
  formatFinal,
  type JobRegistry,
  jobs,
  type Shell,
  shellText,
  signalTree,
  truncateMiddle,
} from "./jobs.ts";

/**
 * What the shell verbs need from a turn. `TurnCtx` satisfies it structurally, so
 * `hostfn/` still imports nothing from `server/` (plan §3, module boundary rule).
 */
export type ShellCtx = Pick<TurnCtx, "sessionId" | "workspace"> & {
  /** The turn's interrupt. Absent in unit tests that never interrupt. */
  signal?: AbortSignal;
};

/** Injected seams. Every default is a constant, never a hidden global. */
export interface ShellOptions {
  /** Where background shells live. Defaults to the process-wide registry. */
  registry?: JobRegistry;
  /** Auto-background threshold for `bash`. Default `defaultBgAfterMs()`. */
  bgAfterMs?: number;
  /** Per-command wall clock for `sh`. Default `SH_TIMEOUT_MS`. */
  shTimeoutMs?: number;
}

/**
 * A foreground command still running this long auto-backgrounds instead of
 * blocking the turn. ~60s only backgrounds genuinely long commands (builds,
 * servers), not the medium ones a program legitimately waits on.
 */
export const DEFAULT_BG_AFTER_MS = 60_000;

/**
 * Per-command wall clock for `sh`. Unlike `bash`, `sh` has no background escape
 * hatch — it owes the caller an exit code — so a hung command must not burn the
 * whole program's budget.
 */
export const SH_TIMEOUT_MS = 120_000;

/**
 * The threshold, with an env override for operators.
 *
 * NOTE: the port read the env var on every call. It is read once per resolution
 * here and is overridden by `ShellOptions.bgAfterMs`, which is what tests use — a
 * test that had to set an environment variable to exercise the handoff would be
 * neither hermetic nor parallel-safe.
 */
export function defaultBgAfterMs(): number {
  const n = Number(process.env.BOUGH_BASH_BG_AFTER_MS);
  return Number.isFinite(n) && n > 0 ? n : DEFAULT_BG_AFTER_MS;
}

/** Resolve `"exit"` when the child finishes, or `"timeout"` after `ms`. */
function raceExit(shell: Shell, ms: number): Promise<"exit" | "timeout"> {
  return new Promise((resolve) => {
    const timer = setTimeout(() => resolve("timeout"), ms);
    // Unref'd: a pending threshold timer must not keep the process awake once the
    // command has already been dealt with.
    timer.unref();
    shell.exit.then(() => {
      clearTimeout(timer);
      resolve("exit");
    });
  });
}

/**
 * Wait for the shell's output streams to finish draining, but no longer than `ms`.
 *
 * Used on the interrupt path, where the partial output is the whole point: the turn
 * runner attaches what the command printed before the stop to the tool record, and
 * returning before the pipes have drained would drop the last chunk. Bounded
 * because a child that ignores SIGTERM must not turn a stop into a hang.
 */
function drained(shell: Shell, ms: number): Promise<void> {
  return new Promise((resolve) => {
    let done = false;
    const finish = () => {
      if (done) return;
      done = true;
      clearTimeout(timer);
      resolve();
    };
    const timer = setTimeout(finish, ms);
    timer.unref();
    shell.pumps.then(finish, finish);
  });
}

/** How long a stopped command gets to flush its pipes before we give up. */
const DRAIN_GRACE_MS = 1_000;

/**
 * Make the turn's interrupt reach the whole process TREE, not just the shell.
 *
 * `Bun.spawn`'s `signal` option SIGTERMs the direct child only, and `sh -c
 * 'printf x; sleep 60'` does not forward SIGTERM to its foreground child. The
 * shell dies, `sleep` is reparented and keeps the inherited stdout pipe open, and
 * the reader waits on a process nobody can see — the stop button looks like it
 * worked while the work kept running (plan §6.3). Returns the detach thunk.
 */
function killTreeOnAbort(shell: Shell, signal: AbortSignal | undefined): () => void {
  if (!signal) return () => {};
  const onAbort = () => signalTree(shell, "SIGTERM");
  // A listener added to an ALREADY-aborted signal never fires. `JobRegistry.spawn`
  // no longer passes the signal to `Bun.spawn` (see its docstring), so nothing else
  // would kill this shell — it would run on with the turn already stopped.
  if (signal.aborted) {
    onAbort();
    return () => {};
  }
  signal.addEventListener("abort", onAbort, { once: true });
  return () => signal.removeEventListener("abort", onAbort);
}

/**
 * Spec §6 requires an interrupt to say WHICH stop happened and what survived. A
 * bare "killed" would leave the next round unable to tell an interrupt from a
 * crash.
 */
function interruptedError(command: string): ProgramError {
  return new ProgramError(
    `command killed: the turn was interrupted by the user — \`${command.slice(0, 80)}\` ` +
      `did not finish. Anything it had already done (files written, commands run) still ` +
      `stands; nothing was rolled back.`,
  );
}

// ---------------------------------------------------------------------------
// bash
// ---------------------------------------------------------------------------

/**
 * Run one command with `sh -c` in the session workspace and return its combined
 * output. A non-zero exit is reported in the output as `[exit code N]`, not thrown
 * — it is a result to read, not an error to retry blind.
 *
 * Past `bgAfterMs` the running child is handed to the background registry and this
 * returns the handoff note. **The command is not killed and not restarted**; it
 * keeps running under the id in the note.
 */
export async function bash(
  command: string,
  ctx: ShellCtx,
  opts: ShellOptions = {},
): Promise<string> {
  const registry = opts.registry ?? jobs;
  const bgAfterMs = opts.bgAfterMs ?? defaultBgAfterMs();
  // Already stopped: spawning here would produce a process nobody waits on.
  if (ctx.signal?.aborted) throw interruptedError(command);

  // Bound to the turn's interrupt only — the user's stop button must kill the
  // actual process. The output is streamed rather than collected with
  // `child.output()` so a long command can be handed to the registry mid-run
  // instead of being blocked on and then killed.
  const shell = registry.spawn(command, { cwd: ctx.workspace, signal: ctx.signal });
  const untrack = registry.trackForeground(shell, ctx.sessionId);
  // Stays attached past a promotion on purpose: an interrupt kills the running
  // program's children (spec §5), and an auto-backgrounded shell is one of them.
  // Only a command that has already finished detaches — signalling a dead child's
  // recycled pid is the one way this could reach an unrelated process.
  const detach = killTreeOnAbort(shell, ctx.signal);
  try {
    if (await raceExit(shell, bgAfterMs) === "exit") {
      detach();
      // Bounded: the process is gone, so its pipes flush immediately — unless a
      // grandchild it backgrounded inherited them, and a finished command must not
      // become an unbounded wait on somebody else's dev server.
      await drained(shell, DRAIN_GRACE_MS);
      if (ctx.signal?.aborted) throw interruptedError(command);
      return formatFinal(shell);
    }
    // Still running at the threshold. Stopped mid-wait dies like any interrupt.
    if (ctx.signal?.aborted) {
      await drained(shell, DRAIN_GRACE_MS);
      throw interruptedError(command);
    }
    // Hand the running child to the registry. The program continues; the model
    // reads it with bashOutput/bashWait and is told when it exits, so it never
    // waits (or writes a poll loop) on a long command.
    //
    // NOTE: `force` — the port fell back to blocking and then SIGKILLed at a hard
    // cap when the registry was full. Plan §6.7 says auto-background never kills,
    // and the concurrency cap exists to brake `bashBg` loops, not to punish a
    // command for being slow. So this promotion always succeeds.
    const id = registry.promote(shell, ctx, { force: true })!;
    return backgroundNote(shell, id, bgAfterMs);
  } finally {
    untrack();
  }
}

// ---------------------------------------------------------------------------
// sh
// ---------------------------------------------------------------------------

/** One command's outcome. The code is DATA — `sh` never throws for a non-zero one. */
export interface ShResult {
  code: number;
  out: string;
}

/**
 * Run `commands` CONCURRENTLY, one `{code, out}` per command in input order.
 *
 * Overlap is real, not simulated: nothing here serializes the shells, and the
 * bridge awaits each host call independently, so N subprocesses run at once.
 *
 * This never rejects. A spawn failure, the per-command deadline, and the turn's
 * interrupt all come back as an ordinary result with an explanatory `out`, because
 * the caller asked for a batch of outcomes and losing the other N-1 to one thrown
 * error is never the right answer.
 */
export async function shConcurrent(
  commands: string[],
  ctx: ShellCtx,
  opts: ShellOptions = {},
): Promise<ShResult[]> {
  const registry = opts.registry ?? jobs;
  const timeoutMs = opts.shTimeoutMs ?? SH_TIMEOUT_MS;
  return await Promise.all(commands.map(async (command): Promise<ShResult> => {
    let shell: Shell;
    try {
      shell = registry.spawn(command, { cwd: ctx.workspace, signal: ctx.signal });
    } catch (err) {
      // Spawn failure (no /bin/sh, an already-aborted signal). Reported, not thrown.
      return { code: -1, out: `could not start command: ${message(err)}` };
    }
    let timedOut = false;
    const timer = setTimeout(() => {
      timedOut = true;
      signalTree(shell, "SIGKILL");
    }, timeoutMs);
    timer.unref();
    // `sh -c` does not forward SIGTERM to its foreground child, so the turn's
    // interrupt has to reach the tree the same way the deadline does.
    const detach = killTreeOnAbort(shell, ctx.signal);
    try {
      const status = await shell.exit;
      detach();
      await drained(shell, DRAIN_GRACE_MS);
      // Retention already bounded the buffer; truncate again so the same rule
      // applies to a command whose output arrived in one burst.
      let out = truncateMiddle(shellText(shell)).trimEnd();
      if (timedOut) {
        out = `[killed after ${timeoutMs / 1000}s — sh has no background handoff; use ` +
          `bashBg() for a command that needs to keep running]\n${out}`.trimEnd();
      } else if (ctx.signal?.aborted) {
        out = `[the turn was interrupted; this command was killed]\n${out}`.trimEnd();
      }
      return { code: status.code, out };
    } catch (err) {
      return { code: -1, out: message(err) };
    } finally {
      clearTimeout(timer);
      detach();
    }
  }));
}

function message(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

// ---------------------------------------------------------------------------
// The bridged surface
// ---------------------------------------------------------------------------

/**
 * The bridge is string-in/string-out (harness/protocol.ts), so `sh` receives a JSON
 * array. Parsing it is a boundary, and a boundary gets a schema: a model that sends
 * `sh("ls")` instead of `sh(["ls"])` must be told exactly that rather than watching
 * `commands.map` throw somewhere in the host.
 */
const ShCommands = z.array(z.string());

/** The six shell host functions, bound to one turn. */
export type ShellHostFns = Pick<
  HostFns,
  "bash" | "sh" | "bashBg" | "bashOutput" | "bashWait" | "bashKill"
>;

/**
 * Wire the shell verbs for one turn.
 *
 * The worker side re-inflates the JSON, so a program still writes
 * `await sh("a", "b")` and gets `[{code, out}, …]` — the serialization is invisible
 * to it and lives entirely at this boundary.
 */
export function createShellHostFns(ctx: ShellCtx, opts: ShellOptions = {}): ShellHostFns {
  const registry = opts.registry ?? jobs;
  return {
    bash: (cmd: string) => bash(cmd, ctx, opts),

    sh: async (cmdsJson: string) => {
      let raw: unknown;
      try {
        raw = JSON.parse(cmdsJson);
      } catch {
        throw new ProgramError(
          `sh expects a JSON array of command strings; got something that is not JSON. ` +
            `Call it as sh("cmd one", "cmd two").`,
        );
      }
      const parsed = ShCommands.safeParse(raw);
      if (!parsed.success) {
        throw new ProgramError(
          `sh expects a JSON array of command strings; got ${
            Array.isArray(raw) ? "an array with a non-string element" : typeof raw
          }. Call it as sh("cmd one", "cmd two").`,
        );
      }
      return JSON.stringify(await shConcurrent(parsed.data, ctx, opts));
    },

    bashBg: (cmd: string) => Promise.resolve(registry.bashBg(cmd, ctx)),
    bashOutput: (id: string) => Promise.resolve(registry.bashOutput(id, ctx.sessionId)),
    bashWait: (id: string) => registry.bashWait(id, ctx.sessionId),
    bashKill: (id: string) => registry.bashKill(id, ctx.sessionId),
  };
}
