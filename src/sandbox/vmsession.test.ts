/**
 * Live per-session VM manager test. Boots a real machine from the golden rootfs.
 * Gated on BOUGH_GOLDEN_DIR (+ smolvm on PATH); skips cleanly in CI.
 *   Run: BOUGH_SMOLVM_BIN=/abs/smolvm BOUGH_GOLDEN_DIR=/abs/golden-rootfs \
 *        deno test -A src/sandbox/vmsession.test.ts
 */
import { assert, assertEquals, assertStringIncludes } from "jsr:@std/assert@1";
import { ensureVm, execIn, GUEST_WORKSPACE, hasVm, teardownVm } from "./vmsession.ts";

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
  name: "vmsession: ensure (lazy+idempotent), workspace mount, exec, teardown",
  ignore: !(await runnable()),
  async fn() {
    const sid = `t-${crypto.randomUUID().slice(0, 8)}`;
    const ws = await Deno.makeTempDir({ prefix: "vmsess-ws-" });
    await Deno.writeTextFile(`${ws}/hello.txt`, "from-host-workspace\n");

    try {
      assert(!hasVm(sid), "no VM before ensure");
      // Concurrent ensures share one create.
      await Promise.all([ensureVm(sid, { workspace: ws }), ensureVm(sid, { workspace: ws })]);
      assert(hasVm(sid), "VM live after ensure");

      // The host workspace is mounted at GUEST_WORKSPACE and is the exec cwd.
      const cat = await execIn(sid, ["cat", "hello.txt"], { cwd: GUEST_WORKSPACE });
      assertEquals(cat.code, 0, cat.stderr);
      assertStringIncludes(cat.stdout, "from-host-workspace");

      // A guest write to the workspace is visible on the HOST (rw virtiofs).
      const wrote = await execIn(sid, ["/bin/sh", "-c", "echo guest-made > /workspace/out.txt"]);
      assertEquals(wrote.code, 0, wrote.stderr);
      assertStringIncludes(await Deno.readTextFile(`${ws}/out.txt`), "guest-made");

      // Idempotent: same workspace → no rebuild, still reachable.
      await ensureVm(sid, { workspace: ws });
      assertEquals((await execIn(sid, ["true"], {})).code, 0);
    } finally {
      await teardownVm(sid);
      await Deno.remove(ws, { recursive: true }).catch(() => {});
    }
    assert(!hasVm(sid), "VM gone after teardown");
  },
});
