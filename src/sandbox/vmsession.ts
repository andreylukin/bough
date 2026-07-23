/**
 * Per-session smolvm VM manager — the sandbox analog of the Claw Patrol gateway's
 * per-session proxy map. One VM per bough session, created lazily on first use,
 * egress locked to the gate host, and reused across the session's turns AND
 * across server restarts (machines persist; reattach = status/start + re-stamp
 * the store-gateway remote). Concurrent turn tool-calls share one create (the
 * `starting` latch). Torn down at session archive, flushing unpushed work first.
 *
 * Two modes, chosen by the origin:
 *   - git origin (guest-owned): no host mount. The working copy is a clone at
 *     GUEST_REPO on the guest's persistent ext4, bootstrapped from the session's
 *     shadow store through the git gateway (vcs/gitgateway.ts). Snapshots are
 *     guest pushes (vcs/guestgit.ts).
 *   - non-git origin dir: the origin is virtiofs-mounted rw at GUEST_WORKSPACE,
 *     unchanged from the pre-guest-owned behavior (clonefile snapshots).
 */
import {
  createSession,
  exec,
  execArgs,
  type ExecOpts,
  type ExecResult,
  remove,
  smolvmBin,
  start,
  status,
} from "./vm.ts";
import { gateHostIp } from "./gatehost.ts";
import { join } from "node:path";
import { bootstrapClone, GUEST_REPO, guestTrack, stampRemote } from "../vcs/guestgit.ts";
import { revokeSessionToken, startGitGateway } from "../vcs/gitgateway.ts";

export { GUEST_REPO };

/** Guest mount point for non-git origin dirs. Bash's cwd in that mode. */
export const GUEST_WORKSPACE = "/workspace";

/** Whether the VM sandbox backend is active. Without it (or without a golden),
 *  sessions run in the host-worktree world (bash.ts fallback). */
export function sandboxVm(): boolean {
  return Deno.env.get("BOUGH_SANDBOX_VM") === "1";
}

/** The golden rootfs directory (`--image`): $BOUGH_GOLDEN_DIR, else ~/.bough/golden-rootfs. */
export function goldenDir(): string {
  const override = Deno.env.get("BOUGH_GOLDEN_DIR");
  if (override) return override;
  const home = Deno.env.get("HOME") ?? "";
  return join(home, ".bough", "golden-rootfs");
}

/** smolvm machine name for a session — namespaced so it never collides with a
 *  user's own machines or another tool's. */
export function machineName(sessionId: string): string {
  return `bough-${sessionId}`;
}

interface VmHandle {
  /** The session's ORIGIN dir — identity/bookkeeping only, never re-mounted. */
  origin: string;
  /** Guest-owned (clone at GUEST_REPO) vs mounted non-git dir. */
  git: boolean;
}
const live = new Map<string, VmHandle>();
const starting = new Map<string, Promise<void>>();
// Per-session teardown latch: while a teardown's flush runs, an ensureVm racing
// in from a still-executing tool must wait — otherwise it would re-register a
// machine that `remove` is about to delete underneath it.
const tearing = new Map<string, Promise<void>>();

export interface EnsureOpts {
  /** The session's ORIGIN dir (the workspace column). Never a worktree path. */
  origin: string;
  /** Git origin → guest-owned: no mount, clone at GUEST_REPO via the store
   *  gateway. Default false → `origin` is virtiofs-mounted rw at GUEST_WORKSPACE
   *  (non-git origin dirs). */
  gitOrigin?: boolean;
  /** Baked-in workload env (rarely used; per-turn proxy/CA vars go on exec). */
  env?: Record<string, string>;
  /** Golden rootfs override (tests); defaults to {@link goldenDir}. */
  golden?: string;
  /** Guest storage disk size in GiB (`--storage`) for large repos; smolvm's
   *  default (20G) when unset. */
  storageGiB?: number;
}

/** Loopback→gate bridge for the AWS creds broker. AWS SDKs reject a non-loopback
 *  plain-http container-credentials URI, so the guest keeps the loopback URI and a
 *  socat daemon (baked into the golden) forwards 127.0.0.1:<port> to the broker at
 *  the gate host — the one address `--allow-cidr` permits. Best-effort: without a
 *  broker (or socat) sessions just run without AWS creds, as before. */
async function startBrokerBridge(name: string): Promise<void> {
  const url = Deno.env.get("BOUGH_AWS_BROKER_URL");
  if (!url) return;
  let port: string;
  try {
    port = new URL(url).port || "80";
  } catch {
    return;
  }
  const fwd = `socat TCP-LISTEN:${port},bind=127.0.0.1,fork,reuseaddr TCP:${gateHostIp()}:${port}`;
  await exec(name, ["sh", "-c", `nohup ${fwd} >/dev/null 2>&1 & sleep 0.2`]).catch(() => {});
}

/**
 * Ensure the session's VM is running, creating it only when absent. Idempotent
 * and concurrency-safe: parallel calls in a turn await one create. An existing
 * machine (server restart) is reused — started if stopped, and for git origins
 * the store-gateway remote URL + token are re-stamped, since the gateway's
 * port and token map are new each server run.
 */
export async function ensureVm(sessionId: string, opts: EnsureOpts): Promise<void> {
  await tearing.get(sessionId); // never race a teardown's flush-then-delete
  if (live.has(sessionId)) return;
  let s = starting.get(sessionId);
  if (!s) {
    const name = machineName(sessionId);
    const git = opts.gitOrigin ?? false;
    s = (async () => {
      if (git) startGitGateway(); // idempotent; the clone's remote needs it live
      const st = await status(name);
      if (st) {
        // Reattach: the machine (and the session's uncommitted guest work)
        // survived a server restart. Start if not clearly running — a failed
        // start on an already-running machine is harmless noise.
        const state = String(st.state ?? st.status ?? "").toLowerCase();
        if (!state.includes("run")) await start(name).catch(() => {});
        await startBrokerBridge(name); // the socat daemon dies with a stop
        if (git) {
          // The machine may exist WITHOUT a usable clone: a bootstrap that
          // failed after create, or a machine from before guest-owned mode.
          // Re-stamping such a machine would wedge the session forever, so
          // probe and (re)bootstrap instead — the bootstrap sequence is
          // idempotent over a partial clone.
          const probe = await exec(name, [
            "git",
            "-C",
            GUEST_REPO,
            "rev-parse",
            "--verify",
            "-q",
            "HEAD",
          ]);
          if (probe.code === 0) await stampRemote(sessionId);
          else await bootstrapClone(sessionId);
        }
      } else {
        await createSession({
          sid: name,
          goldenDir: opts.golden ?? goldenDir(),
          gateCidr: gateHostIp(),
          mounts: git ? [] : [{ host: opts.origin, guest: GUEST_WORKSPACE }],
          env: opts.env,
          storageGiB: opts.storageGiB,
        });
        await startBrokerBridge(name);
        if (git) await bootstrapClone(sessionId);
      }
      live.set(sessionId, { origin: opts.origin, git });
    })();
    starting.set(sessionId, s.finally(() => starting.delete(sessionId)));
  }
  await starting.get(sessionId);
}

/** Run `argv` in the session's VM. Caller must have `ensureVm`'d it first. */
export function execIn(
  sessionId: string,
  argv: string[],
  opts?: ExecOpts,
): Promise<ExecResult> {
  return exec(machineName(sessionId), argv, opts);
}

/**
 * The full command vector — `[smolvmBin, machine, exec, …, --, …argv]` — to run
 * `argv` in the session's VM. For callers (bash.ts) that spawn the child with their
 * own streaming / background / kill machinery. `ensureVm` must have run first.
 */
export function execCommand(
  sessionId: string,
  argv: string[],
  opts?: ExecOpts,
): string[] {
  return [smolvmBin(), ...execArgs(machineName(sessionId), argv, opts)];
}

/** Whether the session has a VM attached in THIS process (no smolvm call).
 *  NOT a liveness check: after a server restart the machine persists while
 *  this returns false — use {@link attachVm} when the machine itself matters. */
export function hasVm(sessionId: string): boolean {
  return live.has(sessionId);
}

/**
 * Attach to a machine that survived a server restart WITHOUT creating one:
 * true when the session now has a live handle (already attached, or the
 * machine existed and was reattached — started, bridged, remote re-stamped).
 * False when no machine exists (archived / never booted): callers fall back
 * to the last-pushed store tip, the freshest host-visible state.
 */
export async function attachVm(sessionId: string, opts: EnsureOpts): Promise<boolean> {
  if (live.has(sessionId)) return true;
  await tearing.get(sessionId);
  await starting.get(sessionId)?.catch(() => {});
  if (live.has(sessionId)) return true;
  const st = await status(machineName(sessionId)).catch(() => null);
  if (!st) return false;
  await ensureVm(sessionId, opts);
  return true;
}

/**
 * Flush and delete the session's VM (session archived). Guest-owned sessions
 * push a final snapshot first so post-archive diff/ship run store-only off the
 * last state. The flush must not depend on the in-memory handle: after a server
 * restart the machine (and its unpushed work) persists while `live` is empty,
 * so an unattached machine is probed — started if needed, remote re-stamped —
 * and flushed before `remove` destroys it. Best-effort (a dead VM can't flush);
 * idempotent; concurrent ensureVm calls wait out the teardown (tearing latch).
 */
export function teardownVm(sessionId: string): Promise<void> {
  const prior = tearing.get(sessionId);
  if (prior) return prior;
  const p = (async () => {
    await starting.get(sessionId)?.catch(() => {});
    const handle = live.get(sessionId);
    live.delete(sessionId);
    starting.delete(sessionId);
    const name = machineName(sessionId);
    try {
      if (handle?.git) {
        await guestTrack(sessionId);
      } else if (!handle) {
        // No handle — this process never attached. The machine may still hold
        // unpushed guest work; probe it (status throws when smolvm itself is
        // absent — then there is nothing to flush or remove anyway). Guest-owned
        // is recognized by the clone's gateway remote (`…/git/<sid>`) — a mere
        // repo at GUEST_REPO isn't enough, since a non-git-origin machine mounts
        // a HOST dir at /workspace and stamping a user repo there would mutate
        // real data.
        const st = await status(name).catch(() => null);
        if (st) {
          const state = String(st.state ?? st.status ?? "").toLowerCase();
          if (!state.includes("run")) await start(name).catch(() => {});
          const probe = await exec(name, [
            "git",
            "-C",
            GUEST_REPO,
            "config",
            "--get",
            "remote.origin.url",
          ]);
          if (probe.code === 0 && probe.stdout.includes(`/git/${sessionId}`)) {
            startGitGateway();
            await stampRemote(sessionId);
            await guestTrack(sessionId);
          }
        }
      }
    } catch (e) {
      console.error(`teardownVm: final flush failed for ${sessionId}: ${(e as Error).message}`);
    }
    revokeSessionToken(sessionId);
    await remove(name).catch(() => {});
  })();
  tearing.set(sessionId, p.finally(() => tearing.delete(sessionId)));
  return tearing.get(sessionId)!;
}
