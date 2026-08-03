/**
 * MCP as a SERVICE: the process connects granted remote servers and keeps them
 * connected, independently of any conversation.
 *
 * WHY THIS EXISTS. Every connection used to be made on demand, by a turn, in a
 * conversation's name — so "is Slack connected?" had no process-level answer, only a
 * per-conversation one, and the honest answer in a fresh conversation was always
 * "no". Three symptoms, one cause: a new conversation showed every server
 * disconnected; the panel could do nothing before the first message was sent; and
 * proving a server worked meant spending a turn on a tool call. Registering a server
 * is a statement about this machine, and so is granting it (`config.ts`'s global
 * scope) — the connection had no business being narrower than either.
 *
 * WHAT IT OWNS, and the boundary is deliberate: **remote servers only.** A stdio
 * server is a subprocess whose cwd is the conversation's checkout, so it cannot be
 * shared and must not be started before someone asks — starting every registered
 * command at boot would spawn processes for conversations that may never happen, in
 * a directory that is not theirs. `manager.ts`'s `scopeFor` draws the same line for
 * the same reason.
 *
 * WHAT IT IS NOT: a cache. It holds live connections, never answers. `mcpStatusFor`
 * still re-reads the registry, the grant and the connection table on every call
 * (plan §6.13), and this module only changes WHEN a connection is opened, never what
 * is reported about one.
 *
 * FAILURE IS NORMAL AND SILENT HERE. A server that is down, unauthorized or
 * misconfigured must not delay start-up or print a stack: the failure is already
 * recorded by the manager and surfaces as a `failed` row in the panel and in
 * `bough mcp` / `bough mcp doctor`, with the reason. Reconciling is best-effort by construction.
 */
import { activationsFor, isStdio, loadRegistry, type McpConfigOptions } from "./config.ts";
import { McpManager, mcpManager, SHARED_SCOPE } from "./manager.ts";

export interface ReconcileResult {
  /** Servers connected (or already connected) after this pass. */
  connected: string[];
  /** Servers that were tried and failed, with the reason the panel will show. */
  failed: { name: string; error: string }[];
  /** Live connections closed because the grant went away. */
  closed: string[];
}

export interface ServiceDeps {
  manager?: McpManager;
  config?: McpConfigOptions;
  /** Where a connection is attempted from. Unused by remote servers; here for tests. */
  workspace?: string;
}

/**
 * Bring the process's connections in line with the registry and the global grant.
 *
 * Idempotent, and safe to call as often as something changes: an already-live
 * connection is reused rather than reopened (`ensure` → `#acquire`), and a server
 * whose grant was withdrawn is dropped so it stops answering — a revoked server that
 * kept serving from an open connection would be a permission that outlived its
 * revocation, which is the one thing this layer must never do.
 */
export async function reconcileMcp(deps: ServiceDeps = {}): Promise<ReconcileResult> {
  const manager = deps.manager ?? mcpManager();
  const config = deps.config ?? {};
  const registry = loadRegistry(config).servers;
  // The GLOBAL grant, which is what a human's ⏎ in the panel now writes. A
  // session-scoped grant is a skill's or a TTL's, and belongs to the turn that has
  // it — not to a process-wide connection.
  const granted = new Set(activationsFor(undefined, config));

  const wanted = Object.entries(registry)
    .filter(([name, cfg]) => granted.has(name) && !isStdio(cfg))
    .map(([name]) => name);

  // Drop first: a connection whose grant is gone must stop being usable before
  // anything else happens, and closing it cannot fail in a way worth reporting.
  const closed: string[] = [];
  for (const conn of manager.statuses(SHARED_SCOPE)) {
    if (!wanted.includes(conn.server)) {
      closed.push(conn.server);
      await manager.drop(SHARED_SCOPE, conn.server);
    }
  }

  if (wanted.length === 0) return { connected: [], failed: [], closed };

  const catalogs = await manager.ensure(SHARED_SCOPE, wanted, {
    workspace: deps.workspace ?? process.cwd(),
  });
  const connected: string[] = [];
  const failed: { name: string; error: string }[] = [];
  for (const c of catalogs) {
    if (c.error) failed.push({ name: c.name, error: c.error });
    else connected.push(c.name);
  }
  return { connected, failed, closed };
}

/** One line for the boot log. Says nothing at all when there is nothing to say. */
export function reconcileSummary(r: ReconcileResult): string | null {
  const bits: string[] = [];
  if (r.connected.length > 0) bits.push(`connected ${r.connected.join(", ")}`);
  if (r.failed.length > 0) {
    // The reason, not just the count: a server that fails at boot is exactly the one
    // whose reason nobody will go looking for.
    bits.push(...r.failed.map((f) => `${f.name} failed (${f.error})`));
  }
  if (r.closed.length > 0) bits.push(`closed ${r.closed.join(", ")}`);
  return bits.length > 0 ? `MCP: ${bits.join(" · ")}` : null;
}
