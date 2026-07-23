/**
 * Live per-session VM manager test — guest-owned workspace edition. Boots a real
 * machine from the golden rootfs and serves a real shadow store through the git
 * gateway. Gated on BOUGH_GOLDEN_DIR (+ smolvm on PATH); skips cleanly in CI.
 *   Run: BOUGH_SMOLVM_BIN=/abs/smolvm BOUGH_GOLDEN_DIR=/abs/golden-rootfs \
 *        deno test -A src/sandbox/vmsession.test.ts
 *
 * Pins the guest-owned invariants from docs/guest-owned-workspace.md §6:
 *   - git origin: bootstrap clone at GUEST_REPO on the guest ext4, NO virtiofs
 *     mount of the origin;
 *   - reattach after a "server restart" reuses the machine (uncommitted guest
 *     work survives, remote+token re-stamped);
 *   - teardown flushes: the last guest state is visible in the store afterwards;
 *   - non-git origin dirs keep the rw virtiofs mount at GUEST_WORKSPACE.
 */
import { assert, assertEquals, assertStringIncludes } from "jsr:@std/assert@1";
import { join } from "node:path";
import { ensureVm, execIn, GUEST_REPO, GUEST_WORKSPACE, hasVm, teardownVm } from "./vmsession.ts";
import { createSessionWorkspace, refFor, setOriginResolver, storeDirFor } from "../vcs/shadow.ts";
import { openDb } from "../db/db.ts";

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

async function hostGit(cwd: string, ...args: string[]): Promise<string> {
  const r = await new Deno.Command("git", {
    args,
    cwd,
    env: { GIT_CONFIG_GLOBAL: "/dev/null", GIT_CONFIG_SYSTEM: "/dev/null" },
    stdout: "piped",
    stderr: "piped",
  }).output();
  if (r.code !== 0) {
    throw new Error(`git ${args.join(" ")}: ${new TextDecoder().decode(r.stderr)}`);
  }
  return new TextDecoder().decode(r.stdout);
}

/** A throwaway origin repo with one committed file. */
async function makeOrigin(): Promise<string> {
  const dir = await Deno.makeTempDir({ prefix: "vmsess-origin-" });
  await hostGit(dir, "init", "-q", "-b", "main");
  await hostGit(dir, "config", "user.name", "t");
  await hostGit(dir, "config", "user.email", "t@t");
  await Deno.writeTextFile(join(dir, "seed.txt"), "from-origin\n");
  await hostGit(dir, "add", "-A");
  await hostGit(dir, "commit", "-q", "-m", "seed");
  return dir;
}

Deno.test({
  name: "vmsession: guest-owned clone, reattach reuse, teardown flush",
  ignore: !(await runnable()),
  async fn() {
    const sid = `t-${crypto.randomUUID().slice(0, 8)}`;
    const tmp = await Deno.makeTempDir({ prefix: "vmsess-go-" });
    // Isolate the store + db so the gateway (which resolves sid → originDir →
    // store via the default db) serves this test's fixtures only.
    Deno.env.set("BOUGH_SHADOW_BASE", join(tmp, "shadow"));
    Deno.env.set("BOUGH_SUBAGENT_BASE", join(tmp, "workspaces"));
    Deno.env.set("BOUGH_DB", join(tmp, "bough.db"));
    const origin = await makeOrigin();
    const db = openDb();
    db.createSession({
      id: sid,
      parentId: null,
      title: "t",
      kind: "root",
      createdAt: Date.now(),
      workspace: origin,
      originDir: origin,
    });
    // The gateway resolves sid → store through this (server registers its Db).
    setOriginResolver((id) => db.getSession(id)?.originDir ?? null);
    // Store refs without a host worktree — the guest-owned prepareShadow shape.
    await createSessionWorkspace(origin, sid, { worktree: false });
    const store = await storeDirFor(origin);

    try {
      assert(!hasVm(sid), "no VM before ensure");
      // Concurrent ensures share one create + bootstrap.
      await Promise.all([
        ensureVm(sid, { origin, gitOrigin: true }),
        ensureVm(sid, { origin, gitOrigin: true }),
      ]);
      assert(hasVm(sid), "VM live after ensure");

      // The clone exists at GUEST_REPO with the origin's grafted content...
      const seed = await execIn(sid, ["cat", "seed.txt"], { cwd: GUEST_REPO });
      assertEquals(seed.code, 0, seed.stderr);
      assertStringIncludes(seed.stdout, "from-origin");
      const top = await execIn(sid, ["git", "rev-parse", "--show-toplevel"], { cwd: GUEST_REPO });
      assertEquals(top.stdout.trim(), GUEST_REPO);

      // ...on the guest's own disk: /workspace is ext4 (/dev/vda), NOT virtiofs,
      // and guest writes never appear in the origin.
      const mounts = await execIn(sid, ["cat", "/proc/mounts"]);
      const wsLine = mounts.stdout.split("\n").find((l) => l.includes(" /workspace ")) ?? "";
      assert(!wsLine.includes("virtiofs"), `no virtiofs at /workspace: ${wsLine}`);
      const wrote = await execIn(sid, [
        "/bin/sh",
        "-c",
        `echo guest-made > ${GUEST_REPO}/guest.txt`,
      ]);
      assertEquals(wrote.code, 0, wrote.stderr);
      assertEquals(
        await Deno.stat(join(origin, "guest.txt")).then(() => true, () => false),
        false,
        "guest write must not land in the origin",
      );

      // Reattach: a fresh module instance (= restarted server, empty live map)
      // must reuse the machine — the uncommitted guest.txt survives — and
      // re-stamp the remote so pushes still work.
      const fresh = await import("./vmsession.ts?reattach");
      assert(!fresh.hasVm(sid), "fresh module starts unattached");
      await fresh.ensureVm(sid, { origin, gitOrigin: true });
      const survived = await fresh.execIn(sid, ["cat", `${GUEST_REPO}/guest.txt`]);
      assertEquals(survived.code, 0, "uncommitted guest work survives reattach");
      assertStringIncludes(survived.stdout, "guest-made");
      // attachVm reports the surviving machine even before anything attaches.
      const fresh2 = await import("./vmsession.ts?attach");
      assert(!fresh2.hasVm(sid), "second fresh module starts unattached");
      assert(
        await fresh2.attachVm(sid, { origin, gitOrigin: true }),
        "attachVm reattaches a surviving machine",
      );

      // Teardown flushes WITHOUT an in-process attach: archive-after-restart is
      // exactly when the flush matters most (the machine holds unpushed work
      // while the live map is empty). A third module instance goes straight to
      // teardownVm — it must probe, re-stamp, and push before deleting.
      await fresh.execIn(sid, ["/bin/sh", "-c", `echo second > ${GUEST_REPO}/guest2.txt`]);
      const cold = await import("./vmsession.ts?teardown");
      assert(!cold.hasVm(sid), "teardown module never attached");
      await cold.teardownVm(sid);
      const shown = await hostGit(store, "show", `${refFor(sid)}:guest.txt`);
      assertStringIncludes(shown, "guest-made");
      const shown2 = await hostGit(store, "show", `${refFor(sid)}:guest2.txt`);
      assertStringIncludes(shown2, "second");
    } finally {
      await teardownVm(sid);
      await Deno.remove(origin, { recursive: true }).catch(() => {});
      await Deno.remove(tmp, { recursive: true }).catch(() => {});
    }
    assert(!hasVm(sid), "VM gone after teardown");
  },
});

Deno.test({
  name: "vmsession: non-git origin keeps the rw virtiofs mount",
  ignore: !(await runnable()),
  async fn() {
    const sid = `t-${crypto.randomUUID().slice(0, 8)}`;
    const dir = await Deno.makeTempDir({ prefix: "vmsess-dir-" });
    await Deno.writeTextFile(`${dir}/hello.txt`, "from-host-dir\n");
    try {
      await ensureVm(sid, { origin: dir });
      const cat = await execIn(sid, ["cat", "hello.txt"], { cwd: GUEST_WORKSPACE });
      assertEquals(cat.code, 0, cat.stderr);
      assertStringIncludes(cat.stdout, "from-host-dir");
      // A guest write to the mount is visible on the HOST (rw virtiofs).
      const wrote = await execIn(sid, ["/bin/sh", "-c", "echo guest-made > /workspace/out.txt"]);
      assertEquals(wrote.code, 0, wrote.stderr);
      assertStringIncludes(await Deno.readTextFile(`${dir}/out.txt`), "guest-made");
    } finally {
      await teardownVm(sid);
      await Deno.remove(dir, { recursive: true }).catch(() => {});
    }
  },
});
