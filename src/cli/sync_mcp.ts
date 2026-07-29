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
 * EVERY SERVER GETS THE CREDENTIAL THAT IS ACTUALLY ITS OWN. The login item holds
 * TWO things: the account token above, and `mcpOAuth` — one OAuth grant per remote
 * server Claude Code has authorized, keyed `<serverName>|<hash>`, each with its own
 * token, URL and expiry. A server with a grant is referenced to THAT grant. Only a
 * host the account token belongs to (`claude.ai`, `*.anthropic.com`) is referenced
 * to the account token, and everything else is registered unauthenticated. The
 * generalization this refuses — "it is remote, so give it the bearer token" — would
 * post the user's Anthropic credential to whatever third party a config file names,
 * which is a credential leak with a helpful tone of voice.
 *
 * WHAT IT READS, in Claude Code's own order of scope:
 *
 *   ~/.claude.json            → `mcpServers`                    (user scope)
 *   ~/.claude.json            → `projects[<dir>].mcpServers`    (that project)
 *   <dir>/.mcp.json           → `mcpServers`                    (checked in)
 *   login keychain            → `mcpOAuth`                      (authorized remotes)
 *
 * That last source is not only a credential lookup — it DEFINES servers. A
 * connector authorized through Claude Code (Slack is the case that exposed this)
 * leaves nothing behind in any config file, so the keychain entry is the only
 * record that the server exists, and it carries both halves: a URL and a token.
 * Without it, `sync-mcp` had neither a definition to copy nor a credential to point
 * at, and simply reported nothing to do.
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
import { CLAUDE_CODE_ITEM, type KeychainReader, securityReader } from "../mcp/keychain.ts";

/**
 * One `mcpOAuth` entry, read for the two fields that describe a server. The tokens
 * in it are deliberately NOT read into this shape: nothing in this process needs
 * their value, and a secret that is never loaded cannot be logged by accident.
 */
const Grant = z.object({
  serverName: z.string().min(1),
  serverUrl: z.string().url(),
  expiresAt: z.number().optional(),
}).passthrough();

/** A remote server Claude Code has authorized, with the key its grant is under. */
export interface KeychainGrant {
  key: string;
  name: string;
  url: string;
  /** Epoch ms, when the entry carried one. */
  expiresAt?: number;
}

/**
 * Every server in the login item's `mcpOAuth` map.
 *
 * A read failure is NOT an error here: no keychain (another platform), a denied
 * dialog, or simply no such item all mean "there are no grants to adopt", and the
 * config-file half of this command must still work. The one thing worth saying out
 * loud is a denied prompt, since that is a decision the user just made and might
 * want to reverse.
 */
export async function readGrants(
  read: KeychainReader,
): Promise<{ grants: KeychainGrant[]; note: string | null }> {
  const { value, code, error } = await read(CLAUDE_CODE_ITEM);
  if (code !== 0 || !value) {
    // 128 is the "allow access?" dialog being dismissed — worth a sentence.
    const note = code === 128
      ? `keychain access to "${CLAUDE_CODE_ITEM}" was denied, so no authorized ` +
        `remote servers were adopted. Re-run and choose Allow to include them.`
      : null;
    return { grants: [], note: note ?? (error && code !== 44 ? error : null) };
  }
  let parsed: unknown;
  try {
    parsed = JSON.parse(value);
  } catch {
    return { grants: [], note: `keychain item "${CLAUDE_CODE_ITEM}" is not JSON` };
  }
  const map = (parsed as { mcpOAuth?: Record<string, unknown> } | null)?.mcpOAuth;
  if (!map || typeof map !== "object") return { grants: [], note: null };
  const grants: KeychainGrant[] = [];
  for (const [key, raw] of Object.entries(map)) {
    const entry = Grant.safeParse(raw);
    if (!entry.success) continue; // a shape we do not recognize is not a server
    grants.push({
      key,
      name: entry.data.serverName,
      url: entry.data.serverUrl,
      ...(entry.data.expiresAt === undefined ? {} : { expiresAt: entry.data.expiresAt }),
    });
  }
  return { grants, note: null };
}

/**
 * The keychain item Claude Code keeps its claude.ai OAuth blob in, and the path to
 * the access token inside it. Same reference `mcp/config.ts` documents.
 */
const CLAUDE_TOKEN_REF = `\${keychain:${CLAUDE_CODE_ITEM}#claudeAiOauth.accessToken}`;

/**
 * A per-server grant Claude Code obtained, as a reference to it.
 *
 * The SAME keychain item holds a second map — `mcpOAuth`, keyed by
 * `<serverName>|<hash>` — with one OAuth grant per remote server it has authorized:
 * Slack, Linear, Notion, anything reached by `claude mcp add` and a browser round
 * trip. That map is why the first cut of this command could not sync Slack at all:
 * a Slack connector is not in `~/.claude.json` (nothing local defines it) and the
 * claude.ai token is deliberately not offered to a third party, so there was
 * neither a definition to copy nor a credential to point at. Both are here.
 *
 * Each entry carries its own `expiresAt` beside the token, which is exactly what
 * `keychain.ts` checks before handing one out — so a stale Slack grant is reported
 * as stale rather than sent and 401'd.
 */
function grantRef(key: string): string {
  return `\${keychain:${CLAUDE_CODE_ITEM}#mcpOAuth.${key}.accessToken}`;
}

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
  /** Claude Code's name for it, when bough had to rename it to a valid slug. */
  renamedFrom?: string;
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
  "  Servers Claude Code has authorized (its keychain mcpOAuth grants) are synced",
  "  too, even when no config file defines them — that is how a Slack connector",
  "  gets here. Tokens are never copied: what is written is a keychain REFERENCE,",
  "  resolved at connect time. Registering grants nothing — activate a server in",
  "  the /mcp panel before a turn can use it.",
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
  grant?: KeychainGrant,
): { server: Record<string, unknown>; authed: boolean } | { reason: string } {
  const remote = s.url && (!s.command || s.type === "http" || s.type === "sse");
  if (remote && s.url) {
    const headers = { ...(s.headers ?? {}) };
    const hasAuth = Object.keys(headers).some((h) => h.toLowerCase() === "authorization");
    // THE SERVER'S OWN GRANT FIRST. When Claude Code has authorized this server,
    // the right credential is the one it obtained FOR it — not the account token,
    // which that server would reject anyway. This is what makes Slack work.
    if (!hasAuth && grant) {
      headers["Authorization"] = `Bearer ${grantRef(grant.key)}`;
      return { server: { url: s.url, headers }, authed: true };
    }
    // Otherwise the account token, and only to hosts it belongs to. See the header.
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
 * A name bough's registry will accept, derived from whatever Claude Code called it.
 *
 * Claude Code namespaces a plugin's server — the Slack connector arrives as
 * `plugin:slack:slack` — and bough's registry takes lowercase slugs, so adopting one
 * failed on the name alone with everything else about it correct. Renaming is the
 * right answer rather than loosening the registry: the name is what a person types
 * in `/mcp` and what a skill's `mcp:` frontmatter names, and `plugin:slack:slack` is
 * a namespace detail of the other tool, not something anyone wants to type here.
 *
 * The LAST segment is preferred (`plugin:slack:slack` → `slack`) because that is the
 * server's own name and what a person would call it. When that is taken by something
 * else, or is not a usable slug on its own, the whole name is slugified instead
 * (`plugin-slack-slack`) — ugly, but unambiguous, and reported either way. `taken`
 * carries the names already claimed in this run, so two plugins that both end in
 * `slack` cannot collapse into one entry.
 */
export function boughName(raw: string, taken: ReadonlySet<string>): string | null {
  const valid = (s: string) => /^[a-z0-9][a-z0-9_-]*$/.test(s);
  if (valid(raw)) return raw;
  const slug = (s: string) =>
    s.toLowerCase().replace(/[^a-z0-9_-]+/g, "-").replace(/^-+|-+$/g, "");
  const last = slug(raw.split(":").pop() ?? "");
  if (valid(last) && !taken.has(last)) return last;
  const whole = slug(raw);
  if (valid(whole) && !taken.has(whole)) return whole;
  return null;
}

/**
 * An env value that looks like a pasted credential rather than a setting.
 *
 * A heuristic, and it is allowed to be: it drives a WARNING, never a refusal. The
 * point is that `bough sync-mcp` can move a literal secret out of one file and
 * into one that is served over HTTP, and the person doing it should hear about it
 * once rather than discover it in a response body.
 */
/** The entry already carries a credential of its own — see `Mcp.tsx`'s row label. */
function hasAuthHeader(entry: { headers?: Record<string, string> }): boolean {
  return Object.entries(entry.headers ?? {}).some(
    ([k, v]) => k.toLowerCase() === "authorization" && v.trim() !== "",
  );
}

export function looksSecret(key: string, value: string): boolean {
  if (/^\$\{[^}]+\}$/.test(value)) return false; // already a reference
  return /(token|secret|key|password|passwd|credential)/i.test(key) && value.length >= 12;
}

export interface SyncDeps {
  readJson?: ReadJson;
  /** Injected so tests never touch a real keychain — see `mcp/keychain.ts`. */
  keychain?: KeychainReader;
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
  const { grants, note } = await readGrants(deps.keychain ?? securityReader);
  if (note) err(`warning: ${note}`);
  // Matched by NAME first, then by URL: the name is what Claude Code keys the grant
  // under and what the config calls the server, and the URL catches the case where
  // the two disagree about spelling but plainly mean the same endpoint.
  const sameUrl = (a: string, b: string) => a.replace(/\/+$/, "") === b.replace(/\/+$/, "");
  const grantFor = (name: string, url?: string): KeychainGrant | undefined =>
    grants.find((g) => g.name === name) ??
      (url ? grants.find((g) => sameUrl(g.url, url)) : undefined);

  // A grant with no definition anywhere is STILL a server — and it is the whole
  // reason Slack could not be synced before. A connector authorized through Claude
  // Code leaves nothing in `~/.claude.json`; the keychain entry is the only record
  // that it exists, and it carries both halves (a URL and a credential).
  const claimed = new Set(found.map((f) => f.name));
  for (const g of grants) {
    if (claimed.has(g.name) || found.some((f) => f.server.url && sameUrl(f.server.url, g.url))) {
      continue;
    }
    found.push({
      name: g.name,
      server: { type: "http", url: g.url },
      source: "Claude Code's keychain grants",
    });
  }
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

  // Names claimed so far, so a rename cannot land on top of another server.
  const taken = new Set([...Object.keys(existing), ...found.map((f) => f.name)]);
  // AN ENDPOINT IS A SERVER, whatever either tool calls it. Without this, adopting
  // a server bough already had under a different name minted a SECOND entry beside
  // it — `plugin-slack-slack` next to `slack`, `linear-server` next to an already
  // authorized `linear`, both pointing at the same URL, one of them working. The
  // registry is keyed by name, so nothing downstream would ever have noticed.
  const norm = (u: string) => u.replace(/\/+$/, "");
  const byUrl = new Map<string, string[]>();
  for (const n of Object.keys(existing).sort()) {
    const u = existing[n].url;
    if (u) byUrl.set(norm(u), [...(byUrl.get(norm(u)) ?? []), n]);
  }
  // A duplicate that is ALREADY there is not this run's doing and not this
  // command's to delete — but silence about it is how it survives. `F` forgets one.
  for (const [url, names] of byUrl) {
    if (names.length > 1) {
      warnings.push(
        `${names.join(" and ")} are the same endpoint (${url}). Only one is needed — ` +
          `open /mcp and press F on the one you do not want.`,
      );
    }
  }
  for (const { name: claudeName, server, source } of found) {
    // THE ENDPOINT DECIDES FIRST, and it has to: `boughName` refuses a name that is
    // taken, and when the taker IS this same server the refusal renames a server
    // into a duplicate of itself. Among entries sharing the URL, the one a person
    // would have named wins (`slack`, not `plugin-slack-slack`) — `natural` is the
    // preferred name computed against no collisions at all.
    const urlNames = server.url ? byUrl.get(norm(server.url)) ?? [] : [];
    const natural = boughName(claudeName, new Set());
    const sameEndpoint = natural && urlNames.includes(natural) ? natural : urlNames[0];
    const name = sameEndpoint ?? boughName(claudeName, taken);
    if (!name) {
      results.push({
        name: claudeName,
        source,
        action: "failed",
        reason: `no free name could be derived from "${claudeName}"`,
      });
      continue;
    }
    if (name !== claudeName) taken.add(name);
    const already = existing[name];
    if (already && !parsed.args.force) {
      // ONE THING IS STILL WORTH DOING TO AN ENTRY WE ARE NOT REPLACING: giving it
      // the credential it is missing. An entry registered before this command could
      // read grants — or by hand, or by an older bough — sits there with no
      // `Authorization` at all, so the panel says "needs auth" and pressing `a`
      // fails against a provider that does not do dynamic registration. There IS a
      // credential for it on this machine. Adding a header where there was none is
      // not the clobber `--force` guards against: nothing is overwritten, and every
      // other field is left exactly as it was found.
      const grant = grantFor(claudeName, server.url);
      if (grant && already.url && !hasAuthHeader(already)) {
        const headers = { ...already.headers, Authorization: `Bearer ${grantRef(grant.key)}` };
        if (!parsed.args.dryRun) upsertServer(name, { ...already, headers }, config);
        results.push({ name, source, action: "updated", authed: true, reason: "added the missing credential" });
        continue;
      }
      results.push({
        name,
        source,
        action: "skipped",
        reason: sameEndpoint && sameEndpoint !== claudeName
          ? `already registered as "${name}" — same endpoint`
          : "already registered here — --force replaces it",
      });
      continue;
    }
    // Matched on what CLAUDE CODE calls it — the grant is keyed by that name, and
    // the rename above is bough's business, not the keychain's.
    const grant = grantFor(claudeName, server.url);
    const mapped = toBoughServer(server, grant);
    if ("reason" in mapped) {
      results.push({ name, source, action: "failed", reason: mapped.reason });
      continue;
    }
    // Said at sync time as well as at connect time. `keychain.ts` refuses an expired
    // token when the request is about to go out, which is correct but arrives much
    // later and looks like the server's fault; the fix is the same either way and
    // the moment to mention it is while the person is here.
    if (grant?.expiresAt !== undefined && grant.expiresAt <= Date.now()) {
      warnings.push(
        `${name}: Claude Code's grant for this server expired ` +
          `${new Date(grant.expiresAt).toISOString()}. bough does not refresh a credential ` +
          `it did not obtain — run \`claude\` once to refresh it in place.`,
      );
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
    results.push({
      name,
      source,
      action,
      authed: mapped.authed,
      // A rename is not a detail to swallow: this name is what you type in `/mcp`
      // and what a skill's `mcp:` frontmatter has to say.
      ...(name === claudeName ? {} : { renamedFrom: claudeName }),
    });
  }

  for (const r of results) {
    const mark = r.action === "added" || r.action === "updated" ? "✓" : "·";
    const note = r.reason
      ? ` — ${r.reason}`
      : r.authed
      ? " — using Claude Code's keychain token"
      : "";
    const renamed = r.renamedFrom ? ` (renamed from ${r.renamedFrom})` : "";
    out(`${mark} ${r.name}${renamed}  ${r.action}${note}   (${r.source})`);
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
