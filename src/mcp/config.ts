/**
 * The MCP server REGISTRY and the per-session GRANTS over it — one JSON document
 * under `~/.bough`, and every rule about what may be in it.
 *
 * THE INVARIANT THIS HOLDS: **being registered grants nothing.** A registry entry
 * is a definition — this is what `linear` means, this is how you start it. A turn
 * only gets a server's tools when something *granted* it: an activation written
 * here (the `/mcp` flow's `POST /mcp/servers/:name/enable`), or a skill's `mcp:`
 * frontmatter naming it. Two separate questions, two separate reads, and the
 * connection layer above must ask both — which is why `loadRegistry()` and
 * `activationsFor()` are different functions returning different things rather
 * than one convenient "servers for this session" call that would let a definition
 * silently become a grant.
 *
 * Three properties follow, and each is load-bearing:
 *
 * **Grants expire, and a lapsed one fails CLOSED.** An activation may carry a TTL
 * ("2h" → an absolute ISO expiry). `activationsFor` filters expired entries at read
 * time against an injected `now`, so a grant that lapsed while the server was down
 * is gone on the next read — it never has to be swept.
 *
 * **Secrets live in the environment, not in this file.** An `env` value may be
 * `${VAR}`, expanded from bough's own environment when the child is spawned. The
 * expansion is deliberately NOT done at load: the registry is served over HTTP and
 * rendered in the `/mcp` UI, and an expanded token would then be sitting in a
 * response body and, worse, in the model's context. A missing variable THROWS
 * rather than expanding to empty — a server started with a blank token fails later,
 * in a place that looks like the server's fault (spec §10, §6's error bar).
 *
 * **MCP state is never cached** (plan §6.13). Nothing here memoizes: every call
 * re-reads the file, because grants and connections change between turns and a
 * cached catalog is how the model ends up confidently calling a tool that was
 * revoked two turns ago. The file is small and the reads are rare (turn start,
 * `/mcp` requests).
 *
 * WHERE IT LIVES. `paths.ts` owns the layout and names this file
 * `~/.bough/mcp.json`, with OAuth tokens beside it in `~/.bough/mcp-auth.json`
 * (T7.2). Both registry and activations live in that one document rather than in
 * two files, because there is exactly one accessor for the MCP registry and no
 * module may build a `~/.bough` path by concatenation (`paths.ts`). Callers that
 * need a hermetic store — every test in this tree — pass `{file}` instead of
 * mutating the environment.
 *
 * Ported from `src/mcp/config.ts`. Deltas from that port are marked `NOTE:`.
 */
import { dirname } from "node:path";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { z } from "zod";
import { McpError } from "../errors.ts";
import { mcpRegistryPath } from "../paths.ts";
import {
  type KeychainOptions,
  parseKeychainRef,
  readKeychainRef,
} from "./keychain.ts";

// ---------------------------------------------------------------------------
// Shapes
// ---------------------------------------------------------------------------

/**
 * Server names are lowercase slugs. Not cosmetic: the name is what a program
 * passes to `mcp(server, tool, args)` and what the prompt catalog prints, so it
 * has to be typeable by a model without quoting rules, and it must never be
 * mistakable for a path segment.
 */
const NAME_RE = /^[a-z0-9][a-z0-9_-]*$/;
const NAME_MESSAGE =
  "server names are lowercase slugs (a-z, 0-9, - and _, starting with a letter or digit)";

/**
 * One registry entry — a local stdio subprocess or a remote Streamable HTTP
 * endpoint (spec §10).
 *
 * Kept as ONE object with a cross-field rule rather than a discriminated union so
 * the failure a user actually hits — an entry with neither `command` nor `url`, or
 * with both — reports as that sentence instead of as two parallel union failures
 * neither of which names the real problem.
 *
 * NOTE (port): `allowWrite` is dropped. It described seatbelt write roots for the
 * spawned child, and there is no sandbox in this design (spec §17) — a key that
 * enforces nothing is worse than absent, because it reads like a boundary. An old
 * entry carrying one still loads; the key is ignored.
 */
export const ServerConfig = z.object({
  /** stdio transport: the executable to spawn. Mutually exclusive with `url`. */
  command: z.string().min(1).optional(),
  args: z.array(z.string()).default([]),
  /** Extra child env; a value may reference `${VAR}` from bough's environment. */
  env: z.record(z.string(), z.string()).default({}),
  /** Working directory for the child. Absent = the server process's own. */
  cwd: z.string().optional(),
  /** Remote transport: the Streamable HTTP endpoint. Mutually exclusive with `command`. */
  url: z.string().url().optional(),
  /**
   * Static headers for a remote server. bough's OWN OAuth tokens are not stored
   * here (T7.2) — they live in `~/.bough/mcp-auth.json`.
   *
   * A value may be a reference, resolved at connect time by `expandHeaders` and
   * never at load: `${VAR}` from bough's environment, or
   * `${keychain:<item>#<a.b.c>}` from the macOS login keychain (`keychain.ts`) —
   * which is how a server that ANOTHER client on this machine already authorized
   * gets its bearer token, e.g.
   *
   *     "Authorization": "Bearer ${keychain:Claude Code-credentials#claudeAiOauth.accessToken}"
   *
   * Write the reference, never the secret: this document is served by
   * `GET /mcp/servers` and rendered in the `/mcp` panel.
   */
  headers: z.record(z.string(), z.string()).default({}),
  /**
   * A PRE-REGISTERED OAuth client for this server.
   *
   * Dynamic client registration (RFC7591) is the path bough takes by default and
   * the only one it had: the SDK's `auth()` registers on the fly and the result is
   * stored beside the tokens. Not every authorization server offers it — Slack's
   * publishes `registration_endpoint: null` — and for those the only way in is an
   * app the user created themselves, which means a `client_id` bough is told
   * rather than one it earns.
   */
  clientId: z.string().min(1).optional(),
  /**
   * The pre-registered client's secret, as a `${VAR}` REFERENCE and never a literal.
   *
   * The registry is served over HTTP by `GET /mcp/servers` and rendered in the
   * `/mcp` panel, so a literal here would sit in a response body and in the model's
   * context. Same rule the `env` map has held since this module was written, and
   * the same expansion (`expandEnv`) resolves it at the moment it is used.
   */
  clientSecret: z.string().optional(),
}).superRefine((s, ctx) => {
  if (!s.command === !s.url) {
    ctx.addIssue({
      code: z.ZodIssueCode.custom,
      message: "a server needs exactly one of `command` (stdio) or `url` (remote)",
    });
    return;
  }
  if (s.url && (s.args.length > 0 || Object.keys(s.env).length > 0 || s.cwd !== undefined)) {
    ctx.addIssue({
      code: z.ZodIssueCode.custom,
      message: "a remote server takes `url` and `headers` — `args`, `env` and `cwd` " +
        "describe a subprocess and there is none",
    });
  }
  if (s.command && Object.keys(s.headers).length > 0) {
    ctx.addIssue({
      code: z.ZodIssueCode.custom,
      message: "a stdio server takes `env` — `headers` are sent on an HTTP request " +
        "and there is none",
    });
  }
  if (s.command && (s.clientId !== undefined || s.clientSecret !== undefined)) {
    ctx.addIssue({
      code: z.ZodIssueCode.custom,
      message: "a stdio server takes `env` — `clientId`/`clientSecret` are an OAuth " +
        "client for a remote authorization server and there is none",
    });
  }
  if (s.clientSecret !== undefined && s.clientId === undefined) {
    ctx.addIssue({
      code: z.ZodIssueCode.custom,
      message: "`clientSecret` needs the `clientId` it belongs to — a secret alone " +
        "identifies nothing",
    });
  }
  if (s.clientSecret !== undefined && !/^\$\{\w+\}$/.test(s.clientSecret)) {
    ctx.addIssue({
      code: z.ZodIssueCode.custom,
      message: "`clientSecret` must be a `${VAR}` reference, not the secret itself — " +
        "this file is served by GET /mcp/servers and rendered in the /mcp panel, so a " +
        "literal would sit in a response body and in the model's context",
    });
  }
});
export type ServerConfig = z.infer<typeof ServerConfig>;

/** A registry entry that names a subprocess. Narrowed by `isStdio`. */
export type StdioServerConfig = ServerConfig & { command: string };

/** True when this entry is a local stdio server (`client.ts` can connect it). */
export function isStdio(server: ServerConfig): server is StdioServerConfig {
  return typeof server.command === "string" && server.command.length > 0;
}

/** One grant: a server name, optionally until an absolute ISO instant. */
const Activation = z.object({
  name: z.string(),
  /** ISO 8601. Absent = until revoked. */
  expires: z.string().optional(),
});
type Activation = z.infer<typeof Activation>;

/**
 * The whole document. `servers` are definitions; `activations` are grants keyed by
 * session id, with `""` as the GLOBAL scope (granted to every session).
 */
const ConfigFile = z.object({
  servers: z.record(z.string().regex(NAME_RE, NAME_MESSAGE), ServerConfig).default({}),
  activations: z.record(z.string(), z.array(Activation)).default({}),
});
type ConfigFile = z.infer<typeof ConfigFile>;

/** What the registry surface returns: definitions only, never grants. */
export interface Registry {
  servers: Record<string, ServerConfig>;
}

/** Reads one variable from bough's environment. Injected so tests need no real env. */
export type EnvLookup = (name: string) => string | undefined;

/**
 * Where the store is and where `${VAR}` comes from.
 *
 * Injected rather than read from the environment at each call site, per the
 * dependency-injection ground rule: a test points `file` at a temp path and gets a
 * hermetic registry, with no `BOUGH_HOME` mutation and nothing written under the
 * real `~/.bough`.
 */
export interface McpConfigOptions {
  /** The registry document. Absent = `~/.bough/mcp.json` (`paths.ts`). */
  file?: string;
  /** `${VAR}` source. Absent = the process environment. */
  env?: EnvLookup;
}

/** The registry document this call reads and writes. */
export function registryFile(opts: McpConfigOptions = {}): string {
  return opts.file ?? mcpRegistryPath();
}

// ---------------------------------------------------------------------------
// Persistence
// ---------------------------------------------------------------------------

function readDocument(opts: McpConfigOptions): ConfigFile {
  let raw: unknown;
  try {
    raw = JSON.parse(readFileSync(registryFile(opts), "utf-8"));
  } catch {
    // Absent or corrupt. Fail CLOSED: no servers, no grants. A half-parsed
    // registry that granted some servers and dropped others would be the worst
    // outcome — the model would see a catalog that is wrong rather than empty.
    return { servers: {}, activations: {} };
  }
  const parsed = ConfigFile.safeParse(raw);
  return parsed.success ? parsed.data : { servers: {}, activations: {} };
}

function writeDocument(doc: ConfigFile, opts: McpConfigOptions): void {
  const path = registryFile(opts);
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, JSON.stringify(doc, null, 2) + "\n");
}

/**
 * Parse with a readable failure. These messages are a product surface, not log
 * text: they come back as the 400 body of `PUT /mcp/servers/:name` and as the
 * inline error under the field in the `/mcp` UI.
 */
function parseReadable<S extends z.ZodTypeAny>(schema: S, raw: unknown, what: string): z.infer<S> {
  const parsed = schema.safeParse(raw);
  if (parsed.success) return parsed.data;
  const issues = parsed.error.issues
    .map((i) => `${i.path.join(".") || "(root)"}: ${i.message}`)
    .join("; ");
  throw new McpError(400, `${what}: ${issues}`);
}

// ---------------------------------------------------------------------------
// The registry
// ---------------------------------------------------------------------------

/** Every registered server. A missing or corrupt file contributes nothing. */
export function loadRegistry(opts: McpConfigOptions = {}): Registry {
  return { servers: readDocument(opts).servers };
}

/** One entry, or `undefined` when the name is not registered. */
export function getServer(name: string, opts: McpConfigOptions = {}): ServerConfig | undefined {
  return readDocument(opts).servers[name];
}

/**
 * One entry, or a 404 that NAMES the alternatives. The message is what a program
 * catches when it calls `mcp("linaer", …)`, so it has to be enough to fix the call
 * without another round trip (spec §6).
 */
export function requireServer(name: string, opts: McpConfigOptions = {}): ServerConfig {
  const servers = readDocument(opts).servers;
  const found = servers[name];
  if (found) return found;
  const known = Object.keys(servers).sort();
  throw new McpError(
    404,
    `no MCP server named "${name}" is registered. ` +
      (known.length > 0
        ? `Registered servers: ${known.join(", ")}.`
        : "No servers are registered yet.") +
      ` Register one with PUT /mcp/servers/${name}.`,
  );
}

/**
 * Validate and persist the WHOLE registry, preserving activations.
 *
 * The activations live in the same document, so a naive whole-file write here
 * would revoke every grant as a side effect of renaming a server. They are merged
 * back deliberately.
 */
export function saveRegistry(raw: unknown, opts: McpConfigOptions = {}): Registry {
  const parsed = parseReadable(
    z.object({
      servers: z.record(z.string().regex(NAME_RE, NAME_MESSAGE), ServerConfig).default({}),
    }),
    raw,
    "invalid MCP registry",
  );
  const doc = readDocument(opts);
  const next: ConfigFile = { servers: parsed.servers, activations: doc.activations };
  writeDocument(pruneActivations(next), opts);
  return { servers: next.servers };
}

/**
 * Add or replace ONE entry.
 *
 * This exists so a caller never has to round-trip the whole registry to change one
 * server: a read-modify-write in shell is exactly where a sibling entry gets
 * dropped and a `${VAR}` reference gets expanded into a literal secret.
 */
export function upsertServer(
  name: string,
  raw: unknown,
  opts: McpConfigOptions = {},
): Registry {
  if (!NAME_RE.test(name)) {
    throw new McpError(400, `invalid server name "${name}" — ${NAME_MESSAGE}`);
  }
  const doc = readDocument(opts);
  doc.servers[name] = parseReadable(ServerConfig, raw, `invalid MCP server "${name}"`);
  writeDocument(doc, opts);
  return { servers: doc.servers };
}

/**
 * Remove one entry. Returns false when the name was not registered.
 *
 * NOTE (port): removal also drops the server's ACTIVATIONS. The old
 * implementation left them behind, so re-registering a name silently restored
 * every grant it used to hold — a revoked-then-recreated server should start
 * ungranted.
 */
export function removeServer(name: string, opts: McpConfigOptions = {}): boolean {
  const doc = readDocument(opts);
  if (!(name in doc.servers)) return false;
  delete doc.servers[name];
  writeDocument(pruneActivations(doc), opts);
  return true;
}

/** Drop grants naming a server that no longer exists. */
function pruneActivations(doc: ConfigFile): ConfigFile {
  const activations: Record<string, Activation[]> = {};
  for (const [scope, list] of Object.entries(doc.activations)) {
    const kept = list.filter((a) => a.name in doc.servers);
    if (kept.length > 0) activations[scope] = kept;
  }
  return { servers: doc.servers, activations };
}

// ---------------------------------------------------------------------------
// The child environment
// ---------------------------------------------------------------------------

/**
 * Expand `${VAR}` references in a server's `env` values.
 *
 * A missing variable throws. Silently expanding to empty produces a server that
 * starts, connects, advertises its tools, and fails every call with the remote
 * service's "unauthorized" — a failure that looks like the server's fault and
 * costs a turn to diagnose. Refusing to start says the true thing in the catalog.
 */
export function expandEnv(
  env: Record<string, string>,
  opts: McpConfigOptions = {},
): Record<string, string> {
  const lookup: EnvLookup = opts.env ?? ((name) => process.env[name]);
  const out: Record<string, string> = {};
  for (const [key, value] of Object.entries(env)) {
    out[key] = value.replace(/\$\{(\w+)\}/g, (_match, name: string) => {
      const found = lookup(name);
      if (found === undefined) {
        throw new McpError(
          400,
          `MCP server env ${key} references \${${name}}, which is not set. ` +
            `Export ${name} in ~/.bough/env (or the server's launch environment) ` +
            `and try again — the value is never stored in the registry.`,
        );
      }
      return found;
    });
  }
  return out;
}

/**
 * Expand a remote server's `headers` at the moment they are sent.
 *
 * Two reference kinds, one rule. `${VAR}` reads bough's environment, exactly as a
 * spawned server's `env` does. `${keychain:<item>}` — optionally `#a.b.c` into a
 * JSON item — reads the macOS login keychain (`keychain.ts`), which is how a server
 * that some OTHER client on this machine already authorized gets its bearer token
 * without bough running a second, parallel OAuth flow for the same account.
 *
 * The rule both share: **the registry stores the reference and never the secret.**
 * `GET /mcp/servers` serves this document and the `/mcp` panel renders it, so a
 * literal token in `headers` would sit in a response body and in the model's
 * context. Expansion happens HERE, at connect, and the result goes into one request
 * and is not stored, logged or returned.
 *
 * Async because a keychain read is a subprocess (and may raise the system's "allow
 * access?" dialog, which is macOS asking the human — the right gate for this).
 */
export async function expandHeaders(
  headers: Record<string, string>,
  opts: McpConfigOptions & KeychainOptions = {},
): Promise<Record<string, string>> {
  const out: Record<string, string> = {};
  for (const [key, value] of Object.entries(headers)) {
    const ref = parseKeychainRef(value);
    // A keychain reference is the WHOLE value, never interpolated into a larger
    // string: `Bearer ${keychain:…}` is written as two parts by the caller, and a
    // partial match here would be a second, subtler way to end up with a secret in
    // a string this module also has to keep out of logs.
    if (ref) {
      out[key] = await readKeychainRef(ref, opts);
      continue;
    }
    const bearer = /^Bearer\s+(\$\{keychain:[^{}]+\})$/i.exec(value.trim());
    if (bearer) {
      const inner = parseKeychainRef(bearer[1]);
      if (inner) {
        out[key] = `Bearer ${await readKeychainRef(inner, opts)}`;
        continue;
      }
    }
    out[key] = expandEnv({ [key]: value }, opts)[key];
  }
  return out;
}

/**
 * Variables inherited by a spawned server, by name.
 *
 * The child gets a COMPOSED environment (`clearEnv` at the spawn), not bough's
 * own: a server is a third-party binary reading whatever it likes, and handing it
 * every provider key in the process environment is a leak with no upside. What it
 * does need: a PATH to find its own interpreter, a HOME for its caches and its
 * own credential files, a temp directory, and the proxy settings — a server that
 * cannot reach the network through the user's proxy is a server that hangs.
 */
export const INHERITED_ENV = [
  "PATH",
  "HOME",
  "TMPDIR",
  "LANG",
  "TZ",
  "SHELL",
  "HTTP_PROXY",
  "HTTPS_PROXY",
  "NO_PROXY",
  "ALL_PROXY",
  "http_proxy",
  "https_proxy",
  "no_proxy",
  "all_proxy",
  "NODE_EXTRA_CA_CERTS",
  "SSL_CERT_FILE",
  "DENO_DIR",
] as const;

/**
 * The child's ENTIRE environment: the inherited names above, plus the server's own
 * declared `env` with `${VAR}` expanded. Declared values win on a collision — a
 * server that overrides PATH meant to.
 *
 * Throws (via `expandEnv`) when a referenced variable is unset, so the failure
 * lands at "could not start this server", named, and not inside the server.
 */
export function childEnv(
  server: ServerConfig,
  opts: McpConfigOptions = {},
): Record<string, string> {
  const lookup: EnvLookup = opts.env ?? ((name) => process.env[name]);
  const out: Record<string, string> = {};
  for (const name of INHERITED_ENV) {
    const value = lookup(name);
    if (value !== undefined) out[name] = value;
  }
  return { ...out, ...expandEnv(server.env, opts) };
}

// ---------------------------------------------------------------------------
// Grants
// ---------------------------------------------------------------------------

/** The global scope key: a grant every session sees. */
const GLOBAL_SCOPE = "";

/** Reading grants needs a clock; writing them needs one to resolve a TTL. */
export interface ActivationOptions extends McpConfigOptions {
  /** Injected clock, epoch ms. Absent = `Date.now()`. */
  now?: number;
}

/**
 * Server names manually granted to this session: its own scope plus the global
 * one, with expired grants filtered out.
 *
 * This is only half of a turn's grant — a skill's `mcp:` frontmatter is the other
 * half, resolved by the layer that assembles the turn. A subagent inherits its
 * spawner's resolved grant rather than reading this (spec §7), which is why this
 * takes a session id and not a lineage.
 */
export function activationsFor(
  sessionId: string | undefined,
  opts: ActivationOptions = {},
): string[] {
  const now = opts.now ?? Date.now();
  const doc = readDocument(opts);
  const scopes = sessionId ? [GLOBAL_SCOPE, sessionId] : [GLOBAL_SCOPE];
  const names = new Set<string>();
  for (const scope of scopes) {
    for (const activation of doc.activations[scope] ?? []) {
      if (activation.expires && Date.parse(activation.expires) <= now) continue;
      names.add(activation.name);
    }
  }
  return [...names].sort();
}

/**
 * Grant or revoke a server for one scope (a session id, or `undefined` = global).
 *
 * A grant REPLACES any existing one for the same name, so re-enabling with a fresh
 * TTL extends it rather than leaving a lapsed entry beside a live one.
 */
export function setActivation(
  sessionId: string | undefined,
  name: string,
  on: boolean,
  opts: ActivationOptions & { expires?: string } = {},
): void {
  const doc = readDocument(opts);
  const scope = sessionId ?? GLOBAL_SCOPE;
  const rest = (doc.activations[scope] ?? []).filter((a) => a.name !== name);
  if (on) rest.push({ name, ...(opts.expires ? { expires: opts.expires } : {}) });
  if (rest.length > 0) doc.activations[scope] = rest;
  else delete doc.activations[scope];
  writeDocument(doc, opts);
}

/**
 * Parse a `"90m" | "2h" | "7d"` TTL into an absolute ISO expiry.
 *
 * Absolute, not a duration stored as-is: a duration would silently restart every
 * time the file was rewritten, and a grant meant to last two hours would outlive
 * the machine.
 */
export function ttlToExpires(ttl: string, now: number = Date.now()): string {
  const match = ttl.trim().match(/^(\d+)\s*(m|h|d)$/);
  if (!match) {
    throw new McpError(
      400,
      `invalid ttl "${ttl}" — use a whole number of minutes, hours or days, ` +
        `e.g. "90m", "2h", "7d". Omit it entirely to grant until revoked.`,
    );
  }
  const unit = { m: 60_000, h: 3_600_000, d: 86_400_000 }[match[2] as "m" | "h" | "d"];
  return new Date(now + Number(match[1]) * unit).toISOString();
}
