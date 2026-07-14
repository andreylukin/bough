/** Run a shell command in the session workspace, capturing combined output. */
import { z } from "zod/v4";
import { wrapChild } from "../sandbox/seatbelt.ts";
import { clawpatrolEnv } from "../net/gateway.ts";
import type { ToolDef, ToolRunCtx } from "./types.ts";

const schema = z.object({
  command: z.string().describe("The shell command to run via `sh -c`."),
  timeout_ms: z
    .number()
    .int()
    .positive()
    .optional()
    .describe("Max runtime in milliseconds before the command is killed (default 120000)."),
});

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
    // The child dies on whichever fires first: the per-command timeout, or the
    // turn's interrupt (ctx.signal) — the user's stop button must kill the actual
    // process, not leave it running to completion in the background.
    const timeout = AbortSignal.timeout(timeout_ms ?? 120_000);
    const cmd = new Deno.Command(argv[0], {
      args: argv.slice(1),
      cwd: ctx.workspace,
      env,
      stdout: "piped",
      stderr: "piped",
      signal: ctx.signal ? AbortSignal.any([timeout, ctx.signal]) : timeout,
    });
    let out: Deno.CommandOutput;
    try {
      out = await cmd.output();
    } catch (e) {
      // Timeout, interrupt, or spawn failure surfaces here.
      if (ctx.signal?.aborted) throw new Error("command killed: turn interrupted");
      throw new Error(`command did not complete: ${(e as Error).message}`);
    }
    // An abort can also surface as a normal completion carrying the kill status —
    // report the interrupt explicitly either way.
    if (ctx.signal?.aborted) throw new Error("command killed: turn interrupted");
    const dec = new TextDecoder();
    const chunks: string[] = [];
    const stdout = dec.decode(out.stdout).trimEnd();
    const stderr = dec.decode(out.stderr).trimEnd();
    if (stdout) chunks.push(stdout);
    if (stderr) chunks.push(stderr);
    if (out.code !== 0) chunks.push(`[exit code ${out.code}]`);
    return chunks.join("\n") || "(no output)";
  },
};
