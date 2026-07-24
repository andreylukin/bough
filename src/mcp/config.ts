/**
 * MCP configuration: the server REGISTRY and per-session ACTIVATIONS, both plain
 * JSON files under ~/.bough/mcp (override with BOUGH_MCP_DIR in tests).
 *
 * Registry (servers.json) — defines named servers once, globally. A registry entry
 * grants nothing by itself: a session only gets a server's tools when a skill's
 * `mcp:` frontmatter references it, or it was enabled for the session via
 * /mcp/servers/:name/enable (the /mcp builtin skill's path).
 *
 * Activations (activations.json) — the manual per-session grants, with optional
 * expiry (same TTL forms as plugin activations; a lapsed activation fails closed).
 * The "" session key is the global scope: enabled for every session.
 *
 * `env` values support ${VAR} expansion from bough's own environment so secrets
 * live in the env, not in the registry file. Expanded values reach the SERVER CHILD
 * PROCESS only — never the sandbox VM, never the model's context.
 */
import { z } from "zod/v4";
import { join } from "node:path";
import { homedir } from "node:os";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { boughPath } from "../paths.ts";

export const ServerConfig = z.object({
  /** stdio transport: the executable + args to spawn (host-side, minimal env). */
  command: z.string().min(1).optional(),
  args: z.array(z.string()).default([]),
  /** Extra child env; values may reference ${VAR} from bough's environment. */
  env: z.record(z.string(), z.string()).default({}),
  /** Extra write roots for the server child (a leading ~ expands to the user's
   * home) — for servers that keep state outside the workspace. Seatbelt-era
   * enforcement is gone; kept for config compatibility. */
  allowWrite: z.array(z.string()).default([]),
  /** Remote transport — the manager connects these over McpRemoteClient. */
  url: z.string().optional(),
}).superRefine((s, ctx) => {
  if (!s.command === !s.url) ctx.addIssue("a server needs exactly one of command or url");
});
export type ServerConfig = z.infer<typeof ServerConfig>;

const NAME_RE = /^[a-z0-9][a-z0-9_-]*$/;

export const Registry = z.object({
  servers: z.record(z.string().regex(NAME_RE, "server names are lowercase slugs"), ServerConfig)
    .default({}),
});
export type Registry = z.infer<typeof Registry>;

const Activations = z.object({
  /** sessionId → activations; "" is the global scope (every session). */
  sessions: z.record(
    z.string(),
    z.array(z.object({ name: z.string(), expires: z.string().optional() })),
  ).default({}),
});
type Activations = z.infer<typeof Activations>;

export function mcpDir(): string {
  return Deno.env.get("BOUGH_MCP_DIR") ?? boughPath("mcp");
}

function readJson(path: string): unknown {
  try {
    return JSON.parse(readFileSync(path, "utf-8"));
  } catch {
    return undefined; // absent or corrupt — caller falls back to empty
  }
}

function writeJson(path: string, value: unknown): void {
  mkdirSync(mcpDir(), { recursive: true });
  writeFileSync(path, JSON.stringify(value, null, 2) + "\n");
}

/** The registry file. A missing or corrupt file contributes nothing (fail closed). */
export function loadRegistry(): Registry {
  const parsed = Registry.safeParse(readJson(join(mcpDir(), "servers.json")) ?? {});
  return parsed.success ? parsed.data : { servers: {} };
}

/** Parse with a human-readable failure — these messages surface as 400 bodies in
 * the /mcp skill's shell output and the UI's inline error, not in logs. */
function parseReadable<T>(schema: z.ZodType<T>, raw: unknown): T {
  const r = schema.safeParse(raw);
  if (!r.success) throw new Error(z.prettifyError(r.error));
  return r.data;
}

/** Validate and persist the registry. Throws on an invalid shape (PUT → 400). */
export function saveRegistry(raw: unknown): Registry {
  const reg = parseReadable(Registry, raw);
  writeJson(join(mcpDir(), "servers.json"), reg);
  return reg;
}

/**
 * Add or replace ONE server entry. Throws on a bad name or entry shape — the
 * per-server PUT exists so callers never have to round-trip the whole registry
 * (a read-modify-write in shell is where secrets and sibling entries get mangled).
 */
export function upsertServer(name: string, raw: unknown): Registry {
  if (!NAME_RE.test(name)) throw new Error("server names are lowercase slugs");
  const reg = loadRegistry();
  reg.servers[name] = parseReadable(ServerConfig, raw);
  writeJson(join(mcpDir(), "servers.json"), reg);
  return reg;
}

/** Remove one server entry. Returns false when the name wasn't registered. */
export function removeServer(name: string): boolean {
  const reg = loadRegistry();
  if (!(name in reg.servers)) return false;
  delete reg.servers[name];
  writeJson(join(mcpDir(), "servers.json"), reg);
  return true;
}

/**
 * Expand ${VAR} references in a server's env values from bough's own environment.
 * A missing variable throws — a server silently started with an empty secret is
 * harder to debug than one that refuses to start.
 */
export function expandEnv(env: Record<string, string>): Record<string, string> {
  const out: Record<string, string> = {};
  for (const [key, value] of Object.entries(env)) {
    out[key] = value.replace(/\$\{(\w+)\}/g, (_, name: string) => {
      const v = Deno.env.get(name);
      if (v === undefined) throw new Error(`env ${key} references \${${name}}, which is not set`);
      return v;
    });
  }
  return out;
}

/** Expand a leading ~ to the user's home (for allowWrite roots). */
export function expandHome(p: string): string {
  return p === "~" || p.startsWith("~/") ? join(homedir(), p.slice(1)) : p;
}

function loadActivations(): Activations {
  const parsed = Activations.safeParse(readJson(join(mcpDir(), "activations.json")) ?? {});
  return parsed.success ? parsed.data : { sessions: {} };
}

/**
 * Server names manually enabled for this session (its own scope + the global ""
 * scope), expired activations filtered out — a lapsed TTL fails closed.
 */
export function activationsFor(sessionId: string | undefined, now = Date.now()): string[] {
  const acts = loadActivations();
  const scopes = sessionId ? ["", sessionId] : [""];
  const names = new Set<string>();
  for (const scope of scopes) {
    for (const a of acts.sessions[scope] ?? []) {
      if (a.expires && Date.parse(a.expires) <= now) continue;
      names.add(a.name);
    }
  }
  return [...names];
}

/**
 * Enable/disable a server for a scope (sessionId, or undefined = global). Enable
 * replaces any existing activation for the same name so a new TTL takes effect.
 */
export function setActivation(
  sessionId: string | undefined,
  name: string,
  on: boolean,
  expires?: string,
): void {
  const acts = loadActivations();
  const scope = sessionId ?? "";
  const rest = (acts.sessions[scope] ?? []).filter((a) => a.name !== name);
  if (on) rest.push({ name, ...(expires ? { expires } : {}) });
  if (rest.length) acts.sessions[scope] = rest;
  else delete acts.sessions[scope];
  writeJson(join(mcpDir(), "activations.json"), acts);
}

/** Parse a "90m" | "2h" | "7d" TTL into an absolute ISO expiry (activation TTLs). */
export function ttlToExpires(ttl: string, now = Date.now()): string {
  const m = ttl.trim().match(/^(\d+)\s*(m|h|d)$/);
  if (!m) throw new Error(`invalid ttl "${ttl}" — use e.g. "90m", "2h", "7d"`);
  const unit = { m: 60_000, h: 3_600_000, d: 86_400_000 }[m[2] as "m" | "h" | "d"];
  return new Date(now + Number(m[1]) * unit).toISOString();
}
