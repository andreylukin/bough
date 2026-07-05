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

export const ServerConfig = z.object({
  /** stdio transport: the executable + args to spawn (seatbelt-wrapped when sandboxed). */
  command: z.string().min(1).optional(),
  args: z.array(z.string()).default([]),
  /** Extra child env; values may reference ${VAR} from bough's environment. */
  env: z.record(z.string(), z.string()).default({}),
  /** Remote transport (phase 2 — accepted by the schema, rejected by the manager). */
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
  return Deno.env.get("BOUGH_MCP_DIR") ?? join(homedir(), ".bough", "mcp");
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

/** The server registry; a missing or corrupt file is an empty registry (fail closed). */
export function loadRegistry(): Registry {
  const parsed = Registry.safeParse(readJson(join(mcpDir(), "servers.json")) ?? {});
  return parsed.success ? parsed.data : { servers: {} };
}

/** Validate and persist the registry. Throws on an invalid shape (PUT → 400). */
export function saveRegistry(raw: unknown): Registry {
  const reg = Registry.parse(raw);
  writeJson(join(mcpDir(), "servers.json"), reg);
  return reg;
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
