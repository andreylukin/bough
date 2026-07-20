/** Run a shell command in the session workspace, capturing combined output. */
import { z } from "zod/v4";
import { wrapChild } from "../sandbox/seatbelt.ts";
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
  let argv = ["/bin/sh", "-c", command];
  if (ctx.sandbox) {
    argv = wrapChild(argv, {
      workspace: opts?.readOnly ? ctx.sandbox.scratchDir : ctx.workspace,
      allowWrite: opts?.readOnly ? [] : [ctx.sandbox.sessionDir, ctx.sandbox.scratchDir],
      confineNetwork: Object.keys(netEnv).length > 0,
    });
  }
  return { argv, env: Object.keys(netEnv).length ? netEnv : undefined };
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
