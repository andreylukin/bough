/** Run a shell command in the session workspace, capturing combined output. */
import { z } from "zod/v4";
import { sandboxActive, wrapChild } from "../sandbox/seatbelt.ts";
import { ensureShims } from "../sandbox/shims.ts";
import { ensureVm, execCommand, GUEST_WORKSPACE, sandboxVm } from "../sandbox/vmsession.ts";
import { clawpatrolEnv } from "../net/gateway.ts";
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
 * Argv + env for running `command` under this ctx's confinement — shared by the
 * blocking bash tool and the background shells (bash_bg.ts).
 *
 * Egress routes through Claw Patrol when the proxy is running (opt-in): the proxy
 * env points the command's HTTP(S) client at THIS SESSION's intercepting proxy
 * (per-branch policy + attribution) and trusts its MITM CA. Empty when off.
 *
 * The shell is wrapped in the Seatbelt profile when sandboxed (darwin only). The
 * profile confines writes to the workspace + the session snapshot dir. When the
 * proxy is running we ALSO confine network to loopback, so the proxy is the only
 * egress route — a subprocess can't `--noproxy`/`env -u http_proxy` its way to the
 * open internet. On other platforms we run unwrapped (the sandbox is macOS-only).
 *
 * `readOnly` (the oracle's shell): the Seatbelt write-allow shrinks to the scratch
 * dir alone — the profile's "workspace" (its write root) is pointed AT the scratch
 * dir, so the real workspace is readable but not writable. Reads stay allow-default
 * either way.
 */
export async function shellInvocation(
  command: string,
  ctx: ToolRunCtx,
  opts?: { readOnly?: boolean },
): Promise<{ argv: string[]; env?: Record<string, string> }> {
  const netEnv = await clawpatrolEnv(ctx.sessionId);
  const env: Record<string, string> = { ...netEnv };
  let argv = ["/bin/sh", "-c", command];

  // VM backend: run the shell INSIDE the session's guest. The workspace is virtiofs-
  // mounted at GUEST_WORKSPACE (bash's cwd); netEnv (proxy/CA) is injected into the
  // guest via `-e`, so the host `machine exec` child carries no secrets. bash.run
  // spawns the returned argv with its own streaming/background/kill machinery — a
  // `machine exec` is just a host subprocess, so that all works unchanged.
  if (ctx.sandbox && ctx.sessionId && sandboxVm()) {
    await ensureVm(ctx.sessionId, { workspace: ctx.workspace });
    argv = execCommand(ctx.sessionId, ["/bin/sh", "-c", command], {
      cwd: GUEST_WORKSPACE,
      env: netEnv,
    });
    return { argv };
  }

  if (ctx.sandbox) {
    argv = wrapChild(argv, {
      workspace: opts?.readOnly ? ctx.sandbox.scratchDir : ctx.workspace,
      allowWrite: opts?.readOnly
        ? []
        : [ctx.sandbox.sessionDir, ctx.sandbox.scratchDir, ...(ctx.sandbox.gitWriteDirs ?? [])],
      confineNetwork: Object.keys(netEnv).length > 0,
    });
    // Shim commands the profile can't fix (setuid /bin/ps) — see sandbox/shims.ts.
    // Best-effort: an unwritable shim dir must not take bash down with it.
    if (sandboxActive()) {
      const shims = await ensureShims().catch(() => null);
      if (shims) env.PATH = `${shims}:${Deno.env.get("PATH") ?? "/usr/bin:/bin"}`;
    }
  }
  return { argv, env: Object.keys(env).length ? env : undefined };
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
    const { argv, env } = await shellInvocation(command, ctx);
    // Spawn bound to the turn's interrupt only (the user's stop button must kill the
    // actual process). We stream the output so a long command can be handed to the
    // background registry mid-run rather than blocked-then-killed.
    const child = new Deno.Command(argv[0], {
      args: argv.slice(1),
      cwd: ctx.workspace,
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
