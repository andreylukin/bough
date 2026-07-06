/**
 * One shared shape for "what MCP state does this session see": the registry,
 * remote-server auth state, the scope's activations, and live connections.
 * Consumed by GET /mcp/servers (the API), the always-on mcpStatus() host
 * function (turn.ts), and the UI's MCP rail tab — one builder so they can't
 * drift. Registry env values are ${VAR} references, never expanded secrets.
 */
import { activationsFor, loadRegistry, type Registry } from "./config.ts";
import { hasTokens } from "./oauth.ts";
import { type ConnStatus, mcpManager } from "./manager.ts";

export interface McpStatus {
  registry: Registry;
  /** Remote (url) servers only: whether stored OAuth tokens exist. */
  auth: Record<string, { authorized: boolean }>;
  /** Server names enabled for this scope (session + global), TTL-filtered. */
  active: string[];
  connections: ConnStatus[];
}

export function mcpStatusFor(sessionId?: string): McpStatus {
  const registry = loadRegistry();
  const auth = Object.fromEntries(
    Object.entries(registry.servers)
      .filter(([, cfg]) => cfg.url)
      .map(([name]) => [name, { authorized: hasTokens(name) }]),
  );
  return {
    registry,
    auth,
    active: activationsFor(sessionId),
    connections: mcpManager().statuses(sessionId),
  };
}
