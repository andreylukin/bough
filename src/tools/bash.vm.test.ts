/**
 * Live integration: the bash TOOL, in guest-owned VM mode, runs its command inside
 * the session's guest against the guest-owned clone at /workspace/repo (no host
 * worktree, no virtiofs workspace mount). Proves the shellInvocation→vmsession wire
 * plus the guestgit/gitgateway bootstrap end-to-end (no server/LLM). Pins two
 * regressions: the ident-DNS commit stall (a guest `git commit` must be fast) and
 * the snapshot path (guest write → guestTrack → diff visible in the host store).
 * Gated on BOUGH_GOLDEN_DIR (+ smolvm); CI-skips.
 *   Run: BOUGH_SMOLVM_BIN=/abs/smolvm BOUGH_GOLDEN_DIR=/abs/golden-rootfs \
 *        deno test -A src/tools/bash.vm.test.ts
 */
import { assert, assertStringIncludes } from "jsr:@std/assert@1";
import { bash } from "./bash.ts";
import type { ToolRunCtx } from "./types.ts";
import { ensureVm, machineName, teardownVm } from "../sandbox/vmsession.ts";
import { GUEST_REPO, guestTrack } from "../vcs/guestgit.ts";
import {
  createSessionWorkspace,
  refFor,
  setOriginResolver,
  storeForSession,
} from "../vcs/shadow.ts";
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

async function git(cwd: string, ...args: string[]): Promise<string> {
  const r = await new Deno.Command("git", {
    args,
    cwd,
    env: { GIT_CONFIG_GLOBAL: "/dev/null", GIT_CONFIG_SYSTEM: "/dev/null" },
    stdout: "piped",
    stderr: "piped",
  }).output();
  const dec = new TextDecoder();
  if (r.code !== 0) {
    throw new Error(`git ${args.join(" ")} failed (${r.code}): ${dec.decode(r.stderr)}`);
  }
  return dec.decode(r.stdout);
}

/** Set env for the test, remembering the prior value for restore. */
function setEnv(saved: Map<string, string | undefined>, key: string, value: string) {
  saved.set(key, Deno.env.get(key));
  Deno.env.set(key, value);
}

Deno.test({
  name: "bash tool (guest-owned VM mode): cwd /workspace/repo, fast commit, guestTrack diff",
  ignore: !(await runnable()),
  async fn() {
    const sid = `bt-${crypto.randomUUID().slice(0, 8)}`;
    const tmp = await Deno.makeTempDir({ prefix: "bashvm-" });
    const origin = `${tmp}/origin`;
    const saved = new Map<string, string | undefined>();
    setEnv(saved, "BOUGH_SANDBOX_VM", "1");
    setEnv(saved, "BOUGH_SHADOW_BASE", `${tmp}/shadow`);
    setEnv(saved, "BOUGH_SUBAGENT_BASE", `${tmp}/workspaces`);
    setEnv(saved, "BOUGH_DB", `${tmp}/bough.db`);

    // A real git origin: the store is derived from it, and the guest clones the
    // store through the gateway (never the origin itself).
    await Deno.mkdir(origin, { recursive: true });
    await git(origin, "init", "-q");
    await git(origin, "config", "user.name", "t");
    await git(origin, "config", "user.email", "t@localhost");
    await Deno.writeTextFile(`${origin}/marker.txt`, "origin-file-visible-in-guest\n");
    await git(origin, "add", "-A");
    await git(origin, "commit", "-qm", "origin-base");

    // Session row: guestgit/gitgateway resolve the store from the session's
    // originDir via the DB (BOUGH_DB points them at this test's copy). The
    // resolver registration covers processes where shadow's lazy fallback DB
    // was already opened against another path.
    const db = openDb();
    db.createSession({
      id: sid,
      parentId: null,
      title: "bash vm test",
      kind: "root",
      createdAt: Date.now(),
      workspace: origin,
      originDir: origin,
    });
    db.close();
    setOriginResolver((id) => (id === sid ? origin : null));

    const sessionDir = `${tmp}/session`;
    const scratchDir = `${tmp}/scratch`;
    await Deno.mkdir(sessionDir, { recursive: true });
    await Deno.mkdir(scratchDir, { recursive: true });
    const ctx: ToolRunCtx = {
      workspace: origin,
      sessionId: sid,
      signal: new AbortController().signal,
      sandbox: { sessionDir, scratchDir },
      guestFs: { sessionId: sid, root: GUEST_REPO },
    };

    try {
      // Guest-owned setup: store refs without a host worktree (prepareShadow's VM
      // branch), then ensureVm — which starts the gateway, mints the token, boots
      // the machine, and bootstraps the clone at GUEST_REPO.
      await createSessionWorkspace(origin, sid, { worktree: false });
      await ensureVm(sid, { origin, gitOrigin: true });

      // Ran on the Linux guest; cwd is the guest-owned clone, with the origin's
      // tree in it.
      const out = await bash.run(
        { command: "uname -s; pwd; cat marker.txt; git status --porcelain" },
        ctx,
      );
      assertStringIncludes(out, "Linux");
      assertStringIncludes(out, GUEST_REPO);
      assertStringIncludes(out, "origin-file-visible-in-guest");
      assert(!/^\s*[MADRCU?]{1,2} /m.test(out), `clone not clean after bootstrap:\n${out}`);

      // Identity is baked into the golden: `git commit` succeeds and returns fast.
      // The regression this pins: ident auto-detection resolving the guest hostname
      // over unreachable DNS stalled EVERY ref-writing git op a flat 5s (and commit
      // failed outright with no identity).
      const t0 = performance.now();
      const commit = await bash.run(
        {
          command: "echo made-in-guest > guest.txt && git add -A && " +
            "git commit -qm guest-commit && git log -1 --format=%s",
        },
        ctx,
      );
      const ms = performance.now() - t0;
      assertStringIncludes(commit, "guest-commit");
      assert(!commit.includes("[exit code"), `guest commit failed:\n${commit}`);
      assert(ms < 2000, `guest commit round-trip took ${Math.round(ms)}ms (ident stall?)`);

      // Snapshot path: guestTrack pushes the guest tip; the host store's
      // base..session diff shows the guest-made file, no worktree anywhere.
      await guestTrack(sid);
      const store = await storeForSession(sid);
      const names = await git(
        tmp,
        "--git-dir",
        store,
        "diff",
        "--name-only",
        `refs/bough/base/${sid}`,
        refFor(sid),
      );
      assertStringIncludes(names, "guest.txt");
    } finally {
      await teardownVm(sid); // flushes + revokes the session token itself
      for (const [k, v] of saved) v === undefined ? Deno.env.delete(k) : Deno.env.set(k, v);
      await Deno.remove(tmp, { recursive: true }).catch(() => {});
    }
    // machineName is exercised by teardown; sanity that it's namespaced.
    assert(machineName(sid).startsWith("bough-"));
  },
});
