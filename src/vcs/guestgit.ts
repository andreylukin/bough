/**
 * Guest-side git driver for the guest-owned workspace. In VM mode the session's
 * working copy is a real clone at {@link GUEST_REPO} on the guest's persistent
 * ext4 — not a host mount — wired to the host store gateway (gitgateway.ts)
 * over smart HTTP. Every operation here shells into the guest via `execIn`;
 * the host never touches the clone's filesystem. Snapshots are pushes: the
 * guest commits and pushes `HEAD:refs/bough/sessions/<id>`, and all store-side
 * consumers (diff, materialize, ship) read the pushed tip.
 */
import { execIn } from "../sandbox/vmsession.ts";
import { gitGatewayUrl, mintSessionToken } from "./gitgateway.ts";
import { refFor } from "./shadow.ts";

/** The session clone inside the guest — bash's cwd in guest-owned mode. */
export const GUEST_REPO = "/workspace/repo";

/** Fail fast instead of hanging on a credential prompt (e.g. an in-guest push
 *  to github without a credential binding 401s into a prompt otherwise). */
const GIT_ENV = { GIT_TERMINAL_PROMPT: "0" };

/** Run git in the guest clone; throws on non-zero exit (lifecycle ops). */
async function guestGit(sessionId: string, args: string[]): Promise<string> {
  const r = await execIn(sessionId, ["git", ...args], { cwd: GUEST_REPO, env: GIT_ENV });
  if (r.code !== 0) {
    throw new Error(`guest git ${args.join(" ")} failed (${r.code}): ${r.stderr.trim()}`);
  }
  return r.stdout;
}

/**
 * Stamp the store-gateway remote URL and a fresh bearer token into the guest
 * clone. Run at bootstrap and again on every reattach — the gateway's port
 * (and so the token map) is new each server run.
 */
export async function stampRemote(sessionId: string): Promise<void> {
  const url = gitGatewayUrl(sessionId);
  const set = await execIn(sessionId, ["git", "remote", "set-url", "origin", url], {
    cwd: GUEST_REPO,
    env: GIT_ENV,
  });
  if (set.code !== 0) await guestGit(sessionId, ["remote", "add", "origin", url]);
  // URL-scoped header: an unscoped http.extraHeader rides EVERY http(s) remote
  // the agent adds — leaking the gateway token to e.g. github, whose 401 on a
  // bogus Authorization then breaks the fetch outright. Scoped to the gateway
  // URL it only travels to the store. Stale stamps (prior server runs = prior
  // ports/tokens) are dropped first so exactly one applies.
  const token = mintSessionToken(sessionId);
  const stamp = await execIn(sessionId, [
    "/bin/sh",
    "-c",
    `git config --local --name-only --get-regexp '^http\\..*extraheader$' | ` +
      `while IFS= read -r k; do git config --local --unset-all "$k"; done; ` +
      `git config --local ${shq(`http.${url}.extraHeader`)} ` +
      shq(`Authorization: Bearer ${token}`),
  ], { cwd: GUEST_REPO, env: GIT_ENV });
  if (stamp.code !== 0) {
    throw new Error(`guest token stamp failed (${stamp.code}): ${stamp.stderr.trim()}`);
  }
}

/**
 * Build the session clone inside a freshly created VM. A plain `git clone` is
 * wrong-shaped against a shadow store (refs/heads is empty, HEAD unborn), so
 * the bootstrap is explicit: init, stamp remote + token, fetch the session's
 * refs (tip, base, originbase — the last so `git diff refs/bough/originbase`
 * works in-guest), then check out a `work` branch at the session tip. Identity
 * is set per-repo as well as baked into the golden so commits match the
 * store's pinned snapshot identity even on a pre-identity golden.
 */
export async function bootstrapClone(sessionId: string): Promise<void> {
  const init = await execIn(sessionId, ["git", "init", "-q", GUEST_REPO], { env: GIT_ENV });
  if (init.code !== 0) {
    throw new Error(`guest git init failed (${init.code}): ${init.stderr.trim()}`);
  }
  await guestGit(sessionId, ["config", "user.name", "bough"]);
  await guestGit(sessionId, ["config", "user.email", "bough@localhost"]);
  // Token must be stamped BEFORE the first fetch — the gateway 401s without it.
  await stampRemote(sessionId);
  await guestGit(sessionId, [
    "fetch",
    "-q",
    "origin",
    `+${refFor(sessionId)}:refs/remotes/origin/session`,
    `+refs/bough/base/${sessionId}:refs/bough/base`,
    `+refs/bough/originbase/${sessionId}:refs/bough/originbase`,
  ]);
  await guestGit(sessionId, ["checkout", "-q", "-B", "work", "refs/remotes/origin/session"]);
}

/**
 * Snapshot the guest clone: stage everything, commit when the staged tree
 * differs from HEAD, and push the tip to the session ref in the store. One
 * guest round-trip. "Nothing to commit" is detected explicitly (diff --cached
 * --quiet) rather than by swallowing commit's exit code — a REAL commit
 * failure (full disk, mangled repo state) must fail the snapshot, or the push
 * would ship the stale HEAD as a "successful" flush. The push runs even when
 * the commit no-ops, so earlier unpushed commits still land. Throws when
 * anything fails — a snapshot that didn't reach the store didn't happen.
 */
export async function guestTrack(sessionId: string, message?: string): Promise<void> {
  const msg = message ?? "bough: snapshot";
  const r = await execIn(sessionId, [
    "/bin/sh",
    "-c",
    `git add -A && { git diff --cached --quiet || git commit -q -m ${shq(msg)}; } && ` +
    `git push -q origin HEAD:${refFor(sessionId)}`,
  ], { cwd: GUEST_REPO, env: GIT_ENV });
  if (r.code !== 0) {
    throw new Error(`guestTrack ${sessionId} failed (${r.code}): ${r.stderr.trim()}`);
  }
}

/**
 * Per-path revert inside the guest: restore ONLY `paths` back to `base`
 * (paths absent at base are deleted, mirroring host restorePath), then
 * snapshot so the pushed tip reflects the revert. `base` is a commit sha the
 * clone is expected to hold (the fetched base, or a former guest tip after
 * accept advanced the rail cursor); a missing object is re-fetched from the
 * store before anything is touched — misreading "object absent" as "path
 * absent at base" would delete files.
 */
export async function guestRevert(
  sessionId: string,
  base: string,
  paths: string[],
): Promise<void> {
  if (paths.length === 0) return;
  await guestTrack(sessionId);
  const have = await execIn(sessionId, ["git", "rev-parse", "--verify", "-q", `${base}^{commit}`], {
    cwd: GUEST_REPO,
    env: GIT_ENV,
  });
  if (have.code !== 0) {
    await guestGit(sessionId, [
      "fetch",
      "-q",
      "origin",
      `+refs/bough/base/${sessionId}:refs/bough/base`,
    ]);
    await guestGit(sessionId, ["rev-parse", "--verify", "-q", `${base}^{commit}`]);
  }
  for (const p of paths) {
    const at = await execIn(sessionId, ["git", "cat-file", "-e", `${base}:${p}`], {
      cwd: GUEST_REPO,
      env: GIT_ENV,
    });
    if (at.code === 0) {
      await guestGit(sessionId, ["checkout", "-q", base, "--", p]);
    } else {
      // Path absent at base → delete it. Checked: a silently failed rm would
      // let revertChanges report the path reverted while the file survives.
      const rm = await execIn(sessionId, ["rm", "-f", "--", p], { cwd: GUEST_REPO });
      if (rm.code !== 0) {
        throw new Error(`guestRevert: rm ${p} failed (${rm.code}): ${rm.stderr.trim()}`);
      }
      // --ignore-unmatch: "not in the index" is fine (untracked file); any
      // other failure throws via guestGit.
      await guestGit(sessionId, [
        "rm",
        "--cached",
        "-q",
        "--force",
        "--ignore-unmatch",
        "--",
        p,
      ]);
    }
  }
  await guestTrack(sessionId, "bough: revert paths");
}

/**
 * Adopt a subagent's pushed work into the PARENT's guest clone: fetch the
 * sub's session tip through the parent's remote (both sessions share one
 * store, so the gateway serves the ref), 3-way-apply the sub's base..tip
 * patch, and snapshot the parent. The caller guestTracks the SUB first and
 * advances the sub's base ref in the store afterwards (subagent.ts), same
 * split as host adoptChanges. Throws on apply conflict, like the host path.
 */
export async function guestAdopt(
  parentSessionId: string,
  subSessionId: string,
  subBase: string,
): Promise<void> {
  await guestTrack(parentSessionId);
  const tmp = "refs/bough/adopt-tmp";
  await guestGit(parentSessionId, ["fetch", "-q", "origin", `+${refFor(subSessionId)}:${tmp}`]);
  try {
    const tip = (await guestGit(parentSessionId, ["rev-parse", tmp])).trim();
    if (tip === subBase) return; // nothing to adopt
    const r = await execIn(parentSessionId, [
      "/bin/sh",
      "-c",
      `git diff --binary ${subBase} ${tmp} | git apply --3way --whitespace=nowarn`,
    ], { cwd: GUEST_REPO, env: GIT_ENV });
    if (r.code !== 0) {
      throw new Error(`guestAdopt apply failed (${r.code}): ${r.stderr.trim()}`);
    }
    await guestTrack(parentSessionId, `bough: adopt ${subSessionId}`);
  } finally {
    await execIn(parentSessionId, ["git", "update-ref", "-d", tmp], {
      cwd: GUEST_REPO,
      env: GIT_ENV,
    }).catch(() => {});
  }
}

/** Single-quote a string for `sh -c` (wrap, and escape embedded quotes). */
function shq(s: string): string {
  return `'${s.replaceAll("'", `'\\''`)}'`;
}
