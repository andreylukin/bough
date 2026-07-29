/**
 * `bough sync-mcp` — adopt Claude Code's MCP servers into bough's registry.
 *
 * WHY THIS EXISTS. Both tools speak MCP and neither reads the other's config, so
 * every server a person has already set up — the command, the args, the env, the
 * URL — has to be typed a second time to be usable here. That is not a hard
 * problem, it is just friction that sits between someone and switching, and it is
 * the kind of friction that is paid on every new machine.
 *
 * THE INVARIANT THIS HOLDS, inherited from `mcp/config.ts` and non-negotiable:
 * **what gets written down is a REFERENCE, never a secret.** bough's registry is
 * served by `GET /mcp/servers` and rendered in the `/mcp` panel, so a token copied
 * into it would sit in a response body and, from there, in the model's context.
 * "Pull the tokens from the Mac secrets" is therefore implemented as
 * `${keychain:Claude Code-credentials#claudeAiOauth.accessToken}` — the item's
 * NAME is stored, the read happens at connect time in `mcp/keychain.ts`, and the
 * value goes into one request header and nowhere else. The effect is what was
 * asked for (bough connects using the authorization Claude Code already holds)
 * without a second copy of the credential to leak or to revoke.
 *
 * AND THE TOKEN IS NOT ATTACHED TO STRANGERS. A keychain header is added only for
 * hosts the credential actually belongs to (`claude.ai`, `*.anthropic.com`). The
 * obvious generalization — "it is a remote server, give it the bearer token" —
 * would send the user's Anthropic OAuth token to whatever third party is on the
 * other end of a `url` in a config file. That is not a convenience, it is a
 * credential leak with a helpful tone of voice, so a third-party remote server is
 * registered WITHOUT auth and reported as needing its own `/mcp` → `a` flow.
 *
 * WHAT IT READS, in Claude Code's own order of scope:
 *
 *   ~/.claude.json            → `mcpServers`                    (user scope)
 *   ~/.claude.json            → `projects[<dir>].mcpServers`    (that project)
 *   <dir>/.mcp.json           → `mcpServers`                    (checked in)
 *
 * WHAT IT NEVER DOES: overwrite. A name already in bough's registry is left
 * exactly as it is and reported, because the local definition may be the one that
 * was fixed up by hand — `--force` is how you say otherwise. Nothing here grants
 * anything either: registering is a definition, and a turn only sees a server's
 * tools once something activates it (`mcp/config.ts`). The report says so, since
 * "synced 4 servers" followed by an agent that cannot see them is the obvious way
 * for this to be misunderstood.
 *
 * Every effect is injected — the file reads, the registry write, both writers, the
 * environment — and `runSyncMcp` returns an exit code rather than exiting, so the
 * whole command is tested against a temporary registry with no `~/.claude.json`,
 * no keychain and no network in sight. The `import.meta.main` block at the bottom
 * is the only code that touches a real process.
 *
 * Exit codes: 0 synced (or nothing to do), 1 a source was unreadable, 2 usage.
 */
import { homedir } from "node:os";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { z } from "zod";
import { loadRegistry, type McpConfigOptions, upsertServer } from "../mcp/config.ts";

/**
 * The keychain item Claude Code keeps its claude.ai OAuth blob in, and the path to
 * the access token inside it. Same reference `mcp/config.ts` documents.
 */
const CLAUDE_TOKEN_REF = "${keychain:Claude Code-credentials#claudeAiOauth.accessToken}";

/**
 * Hosts the claude.ai credential belongs to.
 *
 * Matched on the host's SUFFIX after a dot, never with `includes` — `claude.ai` as
 * a substring test also matches `claude.ai.evil.example`, which is precisely the
 * case that must not receive the token.
 */
const ANTHROPIC_HOSTS = ["claude.ai", "anthropic.com"];

function isAnthropicHost(url: string): boolean {
  let host: string;
  try {
    host = new URL(url).hostname.toLowerCase();
  } catch {
    return false;
  }
  return ANTHROPIC_HOSTS.some((h) => host === h || host.endsWith(`.${h}`));
}

/**
 * Claude Code's server entry, read permissively.
 *
 * `passthrough`, and every field optional, because this is ANOTHER tool's file: a
 * field bough does not know about is not an error, and a strict schema here would
 * turn "Claude Code added a key" into "sync-mcp is broken".
 */
const ClaudeServer = z.object({
  type: z.string().optional(),
  command: z.string().optional(),
  args: z.array(z.string()).optional(),
  env: z.record(z.string(), z.string()).optional(),
  cwd: z.string().optional(),
  url: z.string().optional(),
  headers: z.record(z.string(), z.string()).optional(),
}).passthrough();
type ClaudeServer = z.infer<typeof ClaudeServer>;

/** One server found in one of Claude Code's files, with where it came from. */
export interface Found {
  name: string;
  server: ClaudeServer;
  /** Human-readable origin, for the report: a scope is why two entries disagree. */
  source: string;
}

/** What one name's sync did. `reason` is filled for `skipped` and `failed`. */
export interface SyncResult {
  name: string;
  source: string;
  action: "added" | "updated" | "skipped" | "failed";
  /** True when the entry carries the keychain reference. */
  authed?: boolean;
  reason?: string;
}

export interface SyncArgs {
  /** Directories whose project-scope and `.mcp.json` entries are included. */
  dirs: string[];
  force: boolean;
  dryRun: boolean;
  help: boolean;
}

/** Pure, total, and it never throws — the same contract `parseExecArgs` holds. */
export function parseSyncArgs(argv: string[]): { args: SyncArgs } | { usage: string } {
  const args: SyncArgs = { dirs: [], force: false, dryRun: false, help: false };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === "-h" || a === "--help") args.help = true;
    else if (a === "--force") args.force = true;
    else if (a === "--dry-run" || a === "-n") args.dryRun = true;
    else if (a === "--from" || a === "-C") {
      const dir = argv[++i];
      if (!dir) return { usage: `${a} needs a directory` };
      args.dirs.push(dir);
    } else if (a.startsWith("-")) return { usage: `unknown flag ${a}` };
    else return { usage: `unexpected argument "${a}" — sync-mcp takes flags only` };
  }
  return { args };
}

export const USAGE = [
  "usage: bough sync-mcp [--from DIR]... [--dry-run] [--force]",
  "",
  "  Adopt Claude Code's MCP servers (~/.claude.json, <dir>/.mcp.json) into",
  "  bough's registry. Existing entries are kept unless --force is given.",
  "",
  "  -C, --from DIR   also read that project's scope and its .mcp.json",
  "                   (default: the current directory)",
  "  -n, --dry-run    report what would change and write nothing",
  "      --force      replace entries bough already has under the same name",
  "",
  "  Tokens are never copied: a claude.ai server is registered with a keychain",
  "  REFERENCE, resolved at connect time. Registering grants nothing — activate",
  "  a server in the /mcp panel before a turn can use it.",
].join("\n");

/** Reads one JSON file. `null` for absent; throws only on unreadable/malformed. */
type ReadJson = (path: string) => unknown | null;

const realReadJson: ReadJson = (path) => {
  let text: string;
  try {
    text = readFileSync(path, "utf8");
  } catch (e) {
    // Absent is the normal case — most directories have no `.mcp.json` — and it is
    // not news. Anything else (a permission, a directory) is worth saying out loud.
    if ((e as NodeJS.ErrnoException).code === "ENOENT") return null;
    throw new Error(`${path}: ${(e as Error).message}`);
  }
  try {
    return JSON.parse(text) as unknown;
  } catch (e) {
    throw new Error(`${path} is not valid JSON: ${(e as Error).message}`);
  }
};

/** `mcpServers` out of an arbitrary blob, ignoring everything malformed in it. */
function serversIn(blob: unknown): Record<string, unknown> {
  if (!blob || typeof blob !== "object") return {};
  const raw = (blob as { mcpServers?: unknown }).mcpServers;
  return raw && typeof raw === "object" ? raw as Record<string, unknown> : {};
}

/**
 * Every server Claude Code would offer, later sources winning on name.
 *
 * The order mirrors Claude Code's own scope precedence — user, then project, then
 * the checked-in file — so a team `.mcp.json` overriding a personal entry here
 * lands the same way round it does there.
 */
export function collectClaudeServers(
  dirs: string[],
  readJson: ReadJson,
  home: string,
): { found: Found[]; errors: string[] } {
  const errors: string[] = [];
  const byName = new Map<string, Found>();
  const read = (path: string): unknown | null => {
    try {
      return readJson(path);
    } catch (e) {
      errors.push((e as Error).message);
      return null;
    }
  };

  const claudeJsonPath = join(home, ".claude.json");
  const claudeJson = read(claudeJsonPath);
  const take = (raw: Record<string, unknown>, source: string) => {
    for (const [name, value] of Object.entries(raw)) {
      const parsed = ClaudeServer.safeParse(value);
      if (!parsed.success) {
        errors.push(`${source}: ${name} is not a server definition — skipped`);
        continue;
      }
      byName.set(name, { name, server: parsed.data, source });
    }
  };

  take(serversIn(claudeJson), "~/.claude.json");
  for (const dir of dirs) {
    const projects = (claudeJson as { projects?: Record<string, unknown> } | null)?.projects;
    const project = projects?.[dir];
    if (project) take(serversIn(project), `~/.claude.json projects[${dir}]`);
    const local = read(join(dir, ".mcp.json"));
    if (local) take(serversIn(local), join(dir, ".mcp.json"));
  }
  return { found: [...byName.values()], errors };
}

/**
 * One Claude Code entry as a bough registry entry, or a reason it cannot be.
 *
 * The two transports are kept strictly apart because bough's schema refuses a
 * mixture — a remote server with `args` is rejected as "describes a subprocess and
 * there is none" — and a config in the wild may carry both keys.
 */
export function toBoughServer(
  s: ClaudeServer,
): { server: Record<string, unknown>; authed: boolean } | { reason: string } {
  const remote = s.url && (!s.command || s.type === "http" || s.type === "sse");
  if (remote && s.url) {
    const headers = { ...(s.headers ?? {}) };
    const hasAuth = Object.keys(headers).some((h) => h.toLowerCase() === "authorization");
    // The credential is Anthropic's, so it goes only to Anthropic. See the header.
    const authed = !hasAuth && isAnthropicHost(s.url);
    if (authed) headers["Authorization"] = `Bearer ${CLAUDE_TOKEN_REF}`;
    return { server: { url: s.url, headers }, authed };
  }
  if (!s.command) {
    return { reason: "has neither a `command` nor a `url` bough can use" };
  }
  return {
    server: {
      command: s.command,
      args: s.args ?? [],
      env: s.env ?? {},
      ...(s.cwd ? { cwd: s.cwd } : {}),
    },
    authed: false,
  };
}

/**
 * An env value that looks like a pasted credential rather than a setting.
 *
 * A heuristic, and it is allowed to be: it drives a WARNING, never a refusal. The
 * point is that `bough sync-mcp` can move a literal secret out of one file and
 * into one that is served over HTTP, and the person doing it should hear about it
 * once rather than discover it in a response body.
 */
export function looksSecret(key: string, value: string): boolean {
  if (/^\$\{[^}]+\}$/.test(value)) return false; // already a reference
  return /(token|secret|key|password|passwd|credential)/i.test(key) && value.length >= 12;
}

export interface SyncDeps {
  readJson?: ReadJson;
  /** Injected so tests write to a temp file rather than `~/.bough`. */
  config?: McpConfigOptions;
  home?: string;
  cwd?: string;
  out?: (line: string) => void;
  err?: (line: string) => void;
}

export async function runSyncMcp(argv: string[], deps: SyncDeps = {}): Promise<number> {
  const out = deps.out ?? ((l: string) => console.log(l));
  const err = deps.err ?? ((l: string) => console.error(l));
  const readJson = deps.readJson ?? realReadJson;
  const home = deps.home ?? homedir();
  const config = deps.config ?? {};

  const parsed = parseSyncArgs(argv);
  if ("usage" in parsed) {
    err(`error: ${parsed.usage}`);
    err(USAGE);
    return 2;
  }
  if (parsed.args.help) {
    out(USAGE);
    return 0;
  }
  const dirs = parsed.args.dirs.length > 0 ? parsed.args.dirs : [deps.cwd ?? process.cwd()];

  const { found, errors } = collectClaudeServers(dirs, readJson, home);
  for (const e of errors) err(`warning: ${e}`);
  if (found.length === 0) {
    out("no MCP servers found in Claude Code's config — nothing to sync.");
    // Not a failure: a person with no servers configured ran a command and got a
    // true answer. The exit code is for a source that could not be READ.
    return errors.length > 0 ? 1 : 0;
  }

  const existing = loadRegistry(config).servers;
  const results: SyncResult[] = [];
  const warnings: string[] = [];

  for (const { name, server, source } of found) {
    if (existing[name] && !parsed.args.force) {
      results.push({
        name,
        source,
        action: "skipped",
        reason: "already registered here — --force replaces it",
      });
      continue;
    }
    const mapped = toBoughServer(server);
    if ("reason" in mapped) {
      results.push({ name, source, action: "failed", reason: mapped.reason });
      continue;
    }
    for (const [k, v] of Object.entries(server.env ?? {})) {
      if (looksSecret(k, v)) {
        warnings.push(
          `${name}: env ${k} looks like a literal secret. bough's registry is served ` +
            `by GET /mcp/servers — prefer \${${k}} and put the value in ~/.bough/env.`,
        );
      }
    }
    const action = existing[name] ? "updated" : "added";
    if (!parsed.args.dryRun) {
      try {
        upsertServer(name, mapped.server, config);
      } catch (e) {
        results.push({ name, source, action: "failed", reason: (e as Error).message });
        continue;
      }
    }
    results.push({ name, source, action, authed: mapped.authed });
  }

  for (const r of results) {
    const mark = r.action === "added" || r.action === "updated" ? "✓" : "·";
    const note = r.reason ? ` — ${r.reason}` : r.authed ? " — using Claude Code's keychain token" : "";
    out(`${mark} ${r.name}  ${r.action}${note}   (${r.source})`);
  }
  for (const w of warnings) err(`warning: ${w}`);

  const wrote = results.filter((r) => r.action === "added" || r.action === "updated").length;
  if (parsed.args.dryRun) {
    out(`\n--dry-run: ${wrote} entr${wrote === 1 ? "y" : "ies"} would change, nothing written.`);
    return 0;
  }
  if (wrote > 0) {
    // Said every time, because this is the step whose absence looks like a bug:
    // the servers are registered, the agent still cannot see them, and nothing
    // else on the path says why.
    out(
      `\n${wrote} server${wrote === 1 ? "" : "s"} registered. Registering grants nothing — ` +
        `open the /mcp panel and enable the ones a turn should be able to use.`,
    );
  }
  return results.some((r) => r.action === "failed") || errors.length > 0 ? 1 : 0;
}

if (import.meta.main) {
  process.exit(await runSyncMcp(process.argv.slice(2)));
}
