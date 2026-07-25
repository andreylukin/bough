/** Run a shell command in the session workspace, capturing combined output. */
import { z } from "zod/v4";
import type { ToolDef, ToolRunCtx } from "./types.ts";
import { backgroundNote, formatFinal, MAX_BUF, newShell, promote } from "./bash_bg.ts";

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
 * Argv + cwd for running `command` — shared by the blocking bash tool and the
 * background shells (bash_bg.ts).
 *
 * There is NO confinement of any kind. The shell is a plain host `/bin/sh` running
 * as the user, starting in the session workspace; it may cd and write anywhere the
 * user can, and egress goes direct with the host's own credentials (gh/git resolve
 * the host login). The workspace is a starting point, not a boundary.
 *
 * This is deliberate. The overlay that used to sit here (agentfs copy-on-write)
 * bought isolation git already provides for a repo, and paid for it by breaking
 * git: the agent's edits lived in a delta the real tree couldn't see, so
 * `git status`/`git diff`/`git commit` all reported on the wrong filesystem and
 * work could only reach the repo through a bespoke ship() host function. Running
 * in the checkout means `git commit && git push` simply work, which is the whole
 * point.
 */
export function shellInvocation(
  command: string,
  ctx: ToolRunCtx,
): { argv: string[]; cwd?: string } {
  return { argv: ["/bin/sh", "-c", command], cwd: ctx.workspace };
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
    const { argv, cwd } = await shellInvocation(command, ctx);
    // Spawn bound to the turn's interrupt only (the user's stop button must kill the
    // actual process). We stream the output so a long command can be handed to the
    // background registry mid-run rather than blocked-then-killed.
    const child = new Deno.Command(argv[0], {
      args: argv.slice(1),
      cwd,
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

/**
 * Run `commands` CONCURRENTLY under this ctx's confinement, one {code, out} per
 * command in input order. Backs the sh() host function.
 *
 * This deliberately does NOT go through bash.run: sh is the parallel primitive, so
 * it must not auto-background (a backgrounded shell has no exit code yet) and it
 * must report the code as data rather than as an "[exit code N]" line the program
 * would have to parse. A non-zero exit is a normal result, never a throw — the
 * point of sh is fanning out commands that are ALLOWED to fail (linters, greps,
 * per-package builds) and inspecting the codes.
 *
 * Overlap is real, not simulated: the postMessage bridge is id-keyed and each host
 * call is awaited independently (harness/vm.ts), and nothing in this file serializes
 * shells — so the Promise.all below runs N subprocesses at once.
 */
export async function shConcurrent(
  commands: string[],
  ctx: ToolRunCtx,
): Promise<{ code: number; out: string }[]> {
  return await Promise.all(commands.map(async (command) => {
    const { argv, cwd } = shellInvocation(command, ctx);
    const child = new Deno.Command(argv[0], {
      args: argv.slice(1),
      cwd,
      stdin: "null",
      stdout: "piped",
      stderr: "piped",
      signal: ctx.signal,
    }).spawn();
    // Hard cap per command: sh has no background escape hatch, so a hung command
    // must not burn the whole program budget.
    const timer = setTimeout(() => {
      try {
        child.kill("SIGKILL");
      } catch { /* raced a natural exit */ }
    }, SH_TIMEOUT_MS);
    Deno.unrefTimer(timer);
    try {
      // Stream into a capped rolling buffer rather than child.output(): sh is the
      // path the model is told to prefer, and `sh("cat huge.log")` must not pull the
      // whole file into host memory and then across the bridge. Same MAX_BUF and
      // same drop-the-oldest rule as a foreground/background bash shell.
      let out = "";
      let dropped = false;
      const pump = async (stream: ReadableStream<Uint8Array>) => {
        const dec = new TextDecoder();
        for await (const chunk of stream) {
          out += dec.decode(chunk, { stream: true });
          const over = out.length - MAX_BUF;
          if (over > 0) {
            dropped = true;
            out = out.slice(over);
          }
        }
      };
      await Promise.all([pump(child.stdout), pump(child.stderr)]);
      const { code } = await child.status;
      out = out.trimEnd();
      if (dropped) out = `[oldest output dropped — over ${MAX_BUF} chars]\n${out}`;
      return { code, out };
    } catch (err) {
      // Spawn/IO failure (including the turn's interrupt aborting the child).
      return { code: -1, out: (err as Error).message ?? String(err) };
    } finally {
      clearTimeout(timer);
    }
  }));
}

/** Per-command wall clock for sh(); matches bash's hard cap. */
const SH_TIMEOUT_MS = 120_000;

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
