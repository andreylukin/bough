/** Run a shell command in the session workspace, capturing combined output. */
import { z } from "zod/v4";
import {
  ensure as ensureAgentfs,
  execCommand as agentfsExecCommand,
  sandboxAgentfs,
} from "../sandbox/agentfs.ts";
import type { ToolDef, ToolRunCtx } from "./types.ts";
import { backgroundNote, formatFinal, newShell, promote } from "./bash_bg.ts";

const schema = z.object({
  command: z.string().describe("The shell command to run via `sh -c`."),
  timeout_ms: z
    .number()
    .int()
    .positive()
    .optional()
    .describe(
      "Hard cap in ms; only reached if the background registry is full (default 120000). " +
        "A command still running at the background threshold (default 60s) is moved to the " +
        "background instead of blocking — read it with bashOutput/bashWait; you're notified when it exits.",
    ),
});

/** A foreground command still running this long auto-backgrounds instead of blocking
 * the turn. Env-tunable; per the harness research, ~60s only backgrounds genuinely
 * long commands (builds, servers), not the medium ones a program waits on. */
function bgAfterMs(): number {
  const n = Number(Deno.env.get("BOUGH_BASH_BG_AFTER_MS"));
  return Number.isFinite(n) && n > 0 ? n : 60_000;
}

/**
 * Argv + env + cwd for running `command` under this ctx's confinement — shared by
 * the blocking bash tool and the background shells (bash_bg.ts).
 *
 * Egress goes DIRECT: there is no proxy or egress firewall. Commands use the host's
 * own credentials (gh/git resolve the host login) and reach the network unrouted.
 *
 * agentfs mode: the shell runs inside the session's copy-on-write overlay of the
 * host workspace, which is the whole confinement story — writes land in the
 * session's delta, the real tree is untouched.
 *
 * `readOnly` (the oracle's shell) shares the session overlay; read-only is not
 * enforced yet (agentfs has no per-run ro overlay).
 */
export function shellInvocation(
  command: string,
  ctx: ToolRunCtx,
  _opts?: { readOnly?: boolean },
): { argv: string[]; env?: Record<string, string>; cwd?: string } {
  const argv = ["/bin/sh", "-c", command];

  // agentfs backend (the only sandbox; on by default): run the shell inside the
  // session's copy-on-write overlay of the host workspace. The wrapper argv drops
  // agentfs's banner and merges the command's stderr into stdout; the child's cwd
  // is the workspace, which is the overlay's copy-on-write base. bash.run spawns
  // the returned argv with its own streaming/background/kill machinery — it is
  // just a host `/bin/sh` subprocess, so all of that works unchanged.
  if (ctx.sandbox && ctx.sessionId && sandboxAgentfs()) {
    ensureAgentfs(ctx.sessionId, { origin: ctx.workspace });
    return {
      argv: agentfsExecCommand(ctx.sessionId, argv),
      cwd: ctx.workspace,
    };
  }

  // No sandbox (tests / CI / BOUGH_SANDBOX_AGENTFS=0): run unwrapped on the host,
  // in the workspace.
  return { argv, cwd: ctx.workspace };
}

/**
 * Foreground shells currently inside bash.run, by session. An interrupt terminates
 * the program's worker before the host call can return, so output the command
 * already produced would vanish with it — run_steps reads these buffers at
 * interrupt time and attaches them to the tool record instead.
 */
const inflight = new Map<string, Set<{ command: string; buf: string }>>();

/** Partial output of this session's in-flight foreground bash calls (one block per
 * command), or null when there is none. Read-only; the buffers keep filling. */
export function inflightForegroundOutput(sessionId: string | undefined): string | null {
  const set = inflight.get(sessionId ?? "(no-session)");
  if (!set?.size) return null;
  const blocks = [...set]
    .filter((sh) => sh.buf.trim().length > 0)
    .map((sh) =>
      `[interrupted] bash "${sh.command.slice(0, 60)}" — output before the interrupt:\n` +
      sh.buf.trimEnd()
    );
  return blocks.length ? blocks.join("\n") : null;
}

export const bash: ToolDef = {
  name: "bash",
  description:
    "Run a shell command with `sh -c` in the session workspace and return combined stdout/stderr. " +
    "A non-zero exit is reported in the output; it is not an error you need to retry blindly.",
  schema,
  async run(input: unknown, ctx: ToolRunCtx): Promise<string> {
    const { command, timeout_ms } = input as z.infer<typeof schema>;
    const { argv, env, cwd } = await shellInvocation(command, ctx);
    // Spawn bound to the turn's interrupt only (the user's stop button must kill the
    // actual process). We stream the output so a long command can be handed to the
    // background registry mid-run rather than blocked-then-killed.
    const child = new Deno.Command(argv[0], {
      args: argv.slice(1),
      cwd,
      env,
      stdin: "null",
      stdout: "piped",
      stderr: "piped",
      signal: ctx.signal,
    }).spawn();
    const sh = newShell(command, child);
    const key = ctx.sessionId ?? "(no-session)";
    let fg = inflight.get(key);
    if (!fg) inflight.set(key, fg = new Set());
    fg.add(sh);
    try {
      const soft = bgAfterMs();
      const first = await raceExit(sh, soft);
      if (first === "exit") {
        await sh.pumps;
        if (ctx.signal?.aborted) throw new Error("command killed: turn interrupted");
        return formatFinal(sh);
      }
      // Still running at the threshold — stopped mid-wait dies like any interrupt.
      if (ctx.signal?.aborted) throw new Error("command killed: turn interrupted");
      // Hand the running child to the background registry (auto-background). The
      // program continues; the model reads it via bashOutput/bashWait and is notified
      // when it exits — it no longer waits (or writes a poll loop) for a long command.
      const id = promote(ctx, sh);
      if (id) return backgroundNote(sh, id, soft);
      // Registry full — fall back to blocking up to the hard cap, then kill.
      const hard = (timeout_ms ?? 120_000) - soft;
      if ((await raceExit(sh, Math.max(0, hard))) === "exit") {
        await sh.pumps;
        if (ctx.signal?.aborted) throw new Error("command killed: turn interrupted");
        return formatFinal(sh);
      }
      try {
        child.kill("SIGKILL");
      } catch { /* raced a natural exit */ }
      return `command killed after ${(timeout_ms ?? 120_000) / 1000}s ` +
        `(background registry full, could not detach)`;
    } finally {
      fg.delete(sh);
      if (fg.size === 0) inflight.delete(key);
    }
  },
};

/** Resolve "exit" when the child finishes, or "timeout" after `ms`. */
function raceExit(sh: { child: Deno.ChildProcess }, ms: number): Promise<"exit" | "timeout"> {
  return new Promise((resolve) => {
    const timer = setTimeout(() => resolve("timeout"), ms);
    Deno.unrefTimer(timer);
    sh.child.status.then(() => {
      clearTimeout(timer);
      resolve("exit");
    });
  });
}
