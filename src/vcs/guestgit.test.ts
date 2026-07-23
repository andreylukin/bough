/**
 * Live guest-git driver test. Boots real machines from the golden rootfs and
 * exercises the full guest-owned loop against a REAL shadow store — one with
 * `objects/info/alternates` into the origin (the case a mounted-store clone
 * can never serve; the history graft must arrive via the host gateway's
 * upload-pack). Gated on BOUGH_GOLDEN_DIR (+ smolvm on PATH); skips in CI.
 *   Run: BOUGH_SMOLVM_BIN=/abs/smolvm BOUGH_GOLDEN_DIR=/abs/golden-rootfs \
 *        deno test -A src/vcs/guestgit.test.ts
 *
 * Pins docs/guest-owned-workspace.md §6:
 *   - bootstrapClone against an alternates-bearing store: grafted origin
 *     history is visible in-guest;
 *   - guestTrack: guest work lands on the store's session ref;
 *   - guestRevert: explicit-path restore, pushed;
 *   - guestAdopt: fetch+3way between two VMs sharing one store through the
 *     gateway — the exact topology the mounted-store spike broke.
 */
import { assert, assertEquals, assertStringIncludes } from "jsr:@std/assert@1";
import { join } from "node:path";
import { GUEST_REPO, guestAdopt, guestRevert, guestTrack } from "./guestgit.ts";
import { createSessionWorkspace, refFor, setOriginResolver, storeDirFor } from "./shadow.ts";
import { ensureVm, execIn, teardownVm } from "../sandbox/vmsession.ts";
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

Deno.test({
  name: "guestgit: bootstrap (alternates graft), track, revert, adopt across two VMs",
  ignore: !(await runnable()),
  async fn() {
    const parent = `t-${crypto.randomUUID().slice(0, 8)}`;
    const sub = `${parent}-sub`;
    const tmp = await Deno.makeTempDir({ prefix: "guestgit-" });
    Deno.env.set("BOUGH_SHADOW_BASE", join(tmp, "shadow"));
    Deno.env.set("BOUGH_SUBAGENT_BASE", join(tmp, "workspaces"));
    Deno.env.set("BOUGH_DB", join(tmp, "bough.db"));

    // Origin with real history (two commits) so the graft is a chain, not a
    // single commit — its parents only resolve through the store's alternates.
    const origin = await Deno.makeTempDir({ prefix: "guestgit-origin-" });
    await hostGit(origin, "init", "-q", "-b", "main");
    await hostGit(origin, "config", "user.name", "t");
    await hostGit(origin, "config", "user.email", "t@t");
    await Deno.writeTextFile(join(origin, "seed.txt"), "v1\n");
    await hostGit(origin, "add", "-A");
    await hostGit(origin, "commit", "-q", "-m", "origin-first");
    await Deno.writeTextFile(join(origin, "seed.txt"), "v2\n");
    await hostGit(origin, "commit", "-q", "-am", "origin-second");

    const db = openDb();
    for (const id of [parent, sub]) {
      db.createSession({
        id,
        parentId: id === sub ? parent : null,
        title: "t",
        kind: id === sub ? "subagent" : "root",
        createdAt: Date.now(),
        workspace: origin,
        originDir: origin,
      });
    }
    // The gateway resolves sid → store through this (server registers its Db).
    setOriginResolver((id) => db.getSession(id)?.originDir ?? null);
    await createSessionWorkspace(origin, parent, { worktree: false });
    const store = await storeDirFor(origin);
    assert(
      await Deno.stat(join(store, "objects/info/alternates")).then(() => true, () => false),
      "fixture must be an alternates-bearing store",
    );

    try {
      await ensureVm(parent, { origin, gitOrigin: true });

      // Grafted origin history transferred through the gateway's upload-pack.
      const log = await execIn(parent, ["git", "log", "--format=%s"], { cwd: GUEST_REPO });
      assertEquals(log.code, 0, log.stderr);
      assertStringIncludes(log.stdout, "origin-second");
      assertStringIncludes(log.stdout, "origin-first");
      // originbase was fetched, so the ship-note's diff form works in-guest.
      const ob = await execIn(parent, ["git", "diff", "--stat", "refs/bough/originbase"], {
        cwd: GUEST_REPO,
      });
      assertEquals(ob.code, 0, ob.stderr);

      // guestTrack: guest work lands on the store's session ref.
      await execIn(parent, ["/bin/sh", "-c", `echo parent-work > ${GUEST_REPO}/parent.txt`]);
      await guestTrack(parent);
      assertStringIncludes(
        await hostGit(store, "show", `${refFor(parent)}:parent.txt`),
        "parent-work",
      );

      // guestRevert: explicit-path restore back to base, pushed.
      const base = (await hostGit(store, "rev-parse", `refs/bough/base/${parent}`)).trim();
      await execIn(parent, ["/bin/sh", "-c", `echo mangled > ${GUEST_REPO}/seed.txt`]);
      await guestRevert(parent, base, ["seed.txt"]);
      const seed = await execIn(parent, ["cat", "seed.txt"], { cwd: GUEST_REPO });
      assertEquals(seed.stdout.trim(), "v2", "seed.txt restored to base");
      assertStringIncludes(
        await hostGit(store, "show", `${refFor(parent)}:seed.txt`),
        "v2",
      );
      // The revert never touched the unrelated path.
      assertStringIncludes(
        await hostGit(store, "show", `${refFor(parent)}:parent.txt`),
        "parent-work",
      );

      // Subagent branch: refs off the parent's pushed tip (the ref shape
      // addWorkspace({worktree:false}) produces), then a second VM clones the
      // SAME store through the gateway.
      const parentTip = (await hostGit(store, "rev-parse", refFor(parent))).trim();
      for (const ref of [`refs/bough/base/${sub}`, `refs/bough/originbase/${sub}`, refFor(sub)]) {
        await hostGit(store, "update-ref", ref, parentTip);
      }
      await ensureVm(sub, { origin, gitOrigin: true });
      const inherited = await execIn(sub, ["cat", "parent.txt"], { cwd: GUEST_REPO });
      assertEquals(inherited.code, 0, "sub clone starts from the parent tip");

      // Sub does work, pushes; parent adopts via fetch+3way — two VMs, one
      // store, both sides through the host gateway (the spike's 13/15 failure
      // topology, now green).
      await execIn(sub, ["/bin/sh", "-c", `echo sub-work > ${GUEST_REPO}/sub.txt`]);
      await guestTrack(sub);
      await guestAdopt(parent, sub, parentTip);
      const adopted = await execIn(parent, ["cat", "sub.txt"], { cwd: GUEST_REPO });
      assertEquals(adopted.code, 0, "adopted file present in parent guest");
      assertStringIncludes(adopted.stdout, "sub-work");
      assertStringIncludes(
        await hostGit(store, "show", `${refFor(parent)}:sub.txt`),
        "sub-work",
      );
    } finally {
      await teardownVm(parent);
      await teardownVm(sub);
      await Deno.remove(origin, { recursive: true }).catch(() => {});
      await Deno.remove(tmp, { recursive: true }).catch(() => {});
    }
  },
});
