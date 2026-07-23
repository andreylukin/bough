/**
 * Live integration: the bash TOOL, in VM mode, runs its command inside the session's
 * guest against the mounted workspace. Proves the shellInvocation→vmsession wire
 * end-to-end (no server/LLM). Gated on BOUGH_GOLDEN_DIR (+ smolvm); CI-skips.
 *   Run: BOUGH_SMOLVM_BIN=/abs/smolvm BOUGH_GOLDEN_DIR=/abs/golden-rootfs \
 *        deno test -A src/tools/bash.vm.test.ts
 */
import { assert, assertStringIncludes } from "jsr:@std/assert@1";
import { bash } from "./bash.ts";
import type { ToolRunCtx } from "./types.ts";
import { machineName, teardownVm } from "../sandbox/vmsession.ts";

const GOLDEN = Deno.env.get("BOUGH_GOLDEN_DIR") ?? "";

async function runnable(): Promise<boolean> {
  if (!GOLDEN) return false;
  try {
    if (!(await Deno.stat(GOLDEN)).isDirectory) return false;
  } catch {
    return false;
  }
  const bin = Deno.env.get("BOUGH_SMOLVM_BIN") ?? "smolvm";
  try {
    return (await new Deno.Command(bin, {
      args: ["machine", "ls", "--json"],
      stdout: "piped",
      stderr: "null",
    })
      .output()).code === 0;
  } catch {
    return false;
  }
}

Deno.test({
  name: "bash tool (VM mode): command runs in the guest against the mounted workspace",
  ignore: !(await runnable()),
  async fn() {
    Deno.env.set("BOUGH_SANDBOX_VM", "1");
    const sid = `bt-${crypto.randomUUID().slice(0, 8)}`;
    const ws = await Deno.makeTempDir({ prefix: "bashvm-ws-" });
    await Deno.writeTextFile(`${ws}/marker.txt`, "workspace-file-visible-in-guest");
    const sessionDir = await Deno.makeTempDir({ prefix: "bashvm-sess-" });
    const scratchDir = await Deno.makeTempDir({ prefix: "bashvm-scratch-" });
    const ctx: ToolRunCtx = {
      workspace: ws,
      sessionId: sid,
      signal: new AbortController().signal,
      sandbox: { sessionDir, scratchDir },
    } as ToolRunCtx;

    try {
      const out = await bash.run(
        { command: "uname -s; pwd; cat marker.txt; echo made-in-guest > new.txt" },
        ctx,
      );
      // Ran on the Linux guest, not the macOS host.
      assertStringIncludes(out, "Linux");
      // cwd is the guest workspace mount, and the host file is visible there.
      assertStringIncludes(out, "/workspace");
      assertStringIncludes(out, "workspace-file-visible-in-guest");
      // A guest write to the workspace lands on the HOST worktree (virtiofs rw).
      const back = await Deno.readTextFile(`${ws}/new.txt`);
      assertStringIncludes(back, "made-in-guest");
    } finally {
      await teardownVm(sid);
      Deno.env.delete("BOUGH_SANDBOX_VM");
      for (const d of [ws, sessionDir, scratchDir]) {
        await Deno.remove(d, { recursive: true }).catch(() => {});
      }
    }
    // machineName is exercised by teardown; sanity that it's namespaced.
    assert(machineName(sid).startsWith("bough-"));
  },
});
