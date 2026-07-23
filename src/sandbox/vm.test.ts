/**
 * Live smolvm VM-session test. Boots a real machine from the golden rootfs, so it
 * needs the `smolvm` binary + a working hypervisor — gated to skip cleanly in CI.
 * Set BOUGH_SMOLVM_BIN to the binary path; the whole test is `ignore`d when it
 * isn't runnable, so `deno test` stays green on a machine without smolvm.
 *
 * Run: BOUGH_SMOLVM_BIN=/abs/smolvm deno test -A vm.test.ts
 */
import { assert, assertEquals, assertStringIncludes } from "jsr:@std/assert@1";
import { createSession, exec, list, readFile, remove, status, writeFile } from "./vm.ts";

// Test config comes from the environment so the test carries no host specifics:
//   BOUGH_GOLDEN_DIR — abs path to an unpacked golden rootfs (scripts/guest-image/build-golden.sh)
//   BOUGH_GATE_IP    — host LAN IP the guest may reach (ipconfig getifaddr en0)
// When either is unset, or smolvm isn't runnable, the test skips cleanly (CI).
const GOLDEN = Deno.env.get("BOUGH_GOLDEN_DIR") ?? "";
const GATE = Deno.env.get("BOUGH_GATE_IP") ?? "";

/** Runnable only when smolvm resolves AND the golden/gate env is configured. */
async function runnable(): Promise<boolean> {
  if (!GOLDEN || !GATE) return false;
  try {
    if (!(await Deno.stat(GOLDEN)).isDirectory) return false;
  } catch {
    return false;
  }
  const bin = Deno.env.get("BOUGH_SMOLVM_BIN") ?? "smolvm";
  try {
    const r = await new Deno.Command(bin, {
      args: ["machine", "ls", "--json"],
      stdout: "piped",
      stderr: "null",
    })
      .output();
    return r.code === 0;
  } catch {
    return false;
  }
}

Deno.test({
  name: "vm session: boot, exec, file round-trip, mounts, egress lockdown, teardown",
  ignore: !(await runnable()),
  async fn() {
    const sid = `wf1-${crypto.randomUUID().slice(0, 8)}`;
    const roDir = await Deno.makeTempDir({ prefix: "wf1-ro-" });
    const rwDir = await Deno.makeTempDir({ prefix: "wf1-rw-" });
    await Deno.writeTextFile(`${roDir}/seed.txt`, "from-host-ro\n");

    try {
      // 1. Boot the session from the golden rootfs with egress locked to the gate.
      await createSession({
        sid,
        goldenDir: GOLDEN,
        gateCidr: GATE,
        mounts: [
          { host: roDir, guest: "/mnt/ro", ro: true },
          { host: rwDir, guest: "/mnt/rw" },
        ],
      });

      // 2. exec a real binary from the image.
      const git = await exec(sid, ["git", "--version"]);
      assertEquals(git.code, 0, git.stderr);
      assertStringIncludes(git.stdout, "git version");

      // 3. writeFile + readFile round-trip through the exec base64 seam.
      //    Explicit bytes incl. NUL and a high byte prove the path is binary-safe.
      const payload = new Uint8Array([0x68, 0x69, 0x0a, 0x00, 0xc3, 0xa9, 0xff, 0x41]);
      await writeFile(sid, "/root/rt.txt", payload);
      const back = await readFile(sid, "/root/rt.txt");
      assertEquals(back, payload);

      // 3b. a large payload (> the write chunk) exercises multi-chunk writeFile
      //     (append path) and proves the base64 group alignment holds.
      const big = new Uint8Array(200_000);
      for (let i = 0; i < big.length; i++) big[i] = (i * 7 + 13) & 0xff;
      await writeFile(sid, "/root/big.bin", big);
      const bigBack = await readFile(sid, "/root/big.bin");
      assertEquals(bigBack.length, big.length);
      assertEquals(bigBack, big);

      // 4a. ro mount is readable from the guest...
      const roRead = await exec(sid, ["cat", "/mnt/ro/seed.txt"]);
      assertEquals(roRead.code, 0, roRead.stderr);
      assertStringIncludes(roRead.stdout, "from-host-ro");
      // ...but a guest write fails with EROFS.
      const roWrite = await exec(sid, ["/bin/sh", "-c", "echo x > /mnt/ro/nope.txt"]);
      assert(roWrite.code !== 0, "expected ro mount write to fail");
      assertStringIncludes(roWrite.stderr.toLowerCase(), "read-only");

      // 4b. rw mount write is visible on the HOST.
      const rwWrite = await exec(sid, ["/bin/sh", "-c", "echo from-guest > /mnt/rw/out.txt"]);
      assertEquals(rwWrite.code, 0, rwWrite.stderr);
      assertEquals(await Deno.readTextFile(`${rwDir}/out.txt`), "from-guest\n");

      // 5. Egress lockdown: everything except the gate/32 is refused.
      const egress = await exec(sid, [
        "/bin/sh",
        "-c",
        "wget -T 3 -q -O- http://1.1.1.1/ >/dev/null 2>&1; echo EXIT:$?",
      ]);
      assertStringIncludes(egress.stdout, "EXIT:");
      const rc = Number(egress.stdout.match(/EXIT:(\d+)/)?.[1]);
      assert(rc !== 0, `expected egress to 1.1.1.1 to be refused, got exit ${rc}`);

      // 6. Streaming variant delivers lines live and still returns the full result.
      const lines: string[] = [];
      const streamed = await exec(sid, ["/bin/sh", "-c", "echo a; echo b; echo c"], {
        stream: (l) => lines.push(l),
      });
      assertEquals(streamed.code, 0);
      assert(lines.some((l) => l.includes("a")) && lines.some((l) => l.includes("c")));

      // status finds the live machine.
      const st = await status(sid);
      assert(st !== null, "status should find the running machine");
    } finally {
      // 7. Teardown: remove the machine and assert it's gone.
      await remove(sid).catch(() => {});
      await Deno.remove(roDir, { recursive: true }).catch(() => {});
      await Deno.remove(rwDir, { recursive: true }).catch(() => {});
    }

    assertEquals(await status(sid), null, "machine should be gone after remove");
    const names = (await list()).map((m) => m.name);
    assert(!names.includes(sid), `machine ${sid} still listed after remove`);
  },
});
