/** Run a shell command in the session workspace, capturing combined output. */
import { z } from "zod/v4";
import { wrap } from "../sandbox/seatbelt.ts";
import { clawpatrolCaEnv, clawpatrolRunPrefix } from "../net/gateway.ts";
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

export const bash: ToolDef = {
  name: "bash",
  description:
    "Run a shell command with `sh -c` in the session workspace and return combined stdout/stderr. " +
    "A non-zero exit is reported in the output; it is not an error you need to retry blindly.",
  schema,
  async run(input: unknown, ctx: ToolRunCtx): Promise<string> {
    const { command, timeout_ms } = input as z.infer<typeof schema>;
    // Wrap the shell in the Seatbelt profile when sandboxed (darwin only). The
    // profile confines writes to the workspace + the session snapshot dir; on other
    // platforms we run the command unwrapped (the FS sandbox is macOS-only).
    let argv = ["/bin/sh", "-c", command];
    if (ctx.sandbox && Deno.build.os === "darwin" && Deno.env.get("BOUGH_NO_SANDBOX") !== "1") {
      argv = wrap(argv, { workspace: ctx.workspace, allowWrite: [ctx.sandbox.sessionDir] });
    }
    // Route egress through Claw Patrol when the gateway is running (opt-in). Seatbelt
    // still confines the filesystem; `clawpatrol run` captures the network at L3, and
    // the CA env makes TLS clients trust the gateway's interception cert.
    const runPrefix = clawpatrolRunPrefix();
    argv = [...runPrefix, ...argv];
    const cmd = new Deno.Command(argv[0], {
      args: argv.slice(1),
      cwd: ctx.workspace,
      env: runPrefix.length ? clawpatrolCaEnv() : undefined,
      stdout: "piped",
      stderr: "piped",
      signal: AbortSignal.timeout(timeout_ms ?? 120_000),
    });
    let out: Deno.CommandOutput;
    try {
      out = await cmd.output();
    } catch (e) {
      // AbortSignal timeout or spawn failure surfaces here.
      throw new Error(`command did not complete: ${(e as Error).message}`);
    }
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
