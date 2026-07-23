/**
 * Per-session smolvm VM manager — the sandbox analog of the Claw Patrol gateway's
 * per-session proxy map. One VM per bough session, created lazily on first use with
 * the session's host workspace virtiofs-mounted at {@link GUEST_WORKSPACE}, egress
 * locked to the gate host, and reused across the session's turns. Concurrent turn
 * tool-calls share one create (the `#starting` latch). Torn down at session archive.
 *
 * The host worktree is the same filesystem the in-process file tools (read/write/
 * edit) operate on, so bash-in-guest and the host-side file tools see one workspace.
 */
import { createSession, exec, type ExecOpts, type ExecResult, remove } from "./vm.ts";
import { gateHostIp } from "./gatehost.ts";
import { join } from "node:path";

/** Guest mount point for the session's host workspace. Bash's cwd. */
export const GUEST_WORKSPACE = "/workspace";

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
  /** Host workspace path the VM was created mounting — a change forces a recreate. */
  workspace: string;
}
const live = new Map<string, VmHandle>();
const starting = new Map<string, Promise<void>>();

export interface EnsureOpts {
  /** Host workspace (shadow worktree) to virtiofs-mount at GUEST_WORKSPACE. */
  workspace: string;
  /** Baked-in workload env (rarely used; per-turn proxy/CA vars go on exec). */
  env?: Record<string, string>;
  /** Golden rootfs override (tests); defaults to {@link goldenDir}. */
  golden?: string;
}

/**
 * Ensure the session's VM is running with `opts.workspace` mounted, creating it if
 * absent. Idempotent and concurrency-safe: parallel calls in a turn await one
 * create. Re-creates if the workspace path changed (rare — the shadow worktree is
 * stable after the first turn).
 */
export async function ensureVm(sessionId: string, opts: EnsureOpts): Promise<void> {
  const existing = live.get(sessionId);
  if (existing) {
    if (existing.workspace === opts.workspace) return;
    await teardownVm(sessionId); // workspace moved — rebuild against the new path
  }
  let s = starting.get(sessionId);
  if (!s) {
    const name = machineName(sessionId);
    s = (async () => {
      await remove(name).catch(() => {}); // clear any stale machine from a prior run
      await createSession({
        sid: name,
        goldenDir: opts.golden ?? goldenDir(),
        gateCidr: gateHostIp(),
        mounts: [{ host: opts.workspace, guest: GUEST_WORKSPACE }],
        env: opts.env,
      });
      live.set(sessionId, { workspace: opts.workspace });
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

/** Whether the session currently has a live VM (no smolvm call). */
export function hasVm(sessionId: string): boolean {
  return live.has(sessionId);
}

/** Stop and delete the session's VM (session archived). Best-effort + idempotent. */
export async function teardownVm(sessionId: string): Promise<void> {
  await starting.get(sessionId)?.catch(() => {});
  live.delete(sessionId);
  starting.delete(sessionId);
  await remove(machineName(sessionId)).catch(() => {});
}
