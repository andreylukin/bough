/**
 * Reading a secret out of the credential store another client on this machine
 * already put it in, for MCP servers that client has already authorized.
 *
 * WHY THIS EXISTS. bough's own way into a remote MCP server is OAuth: dynamic
 * client registration, a browser round trip, tokens in `~/.bough/mcp-auth.json`
 * (`oauth.ts`). That is the right default and it is not the only situation. Some
 * servers are already authorized on this machine by another client — the claude.ai
 * connectors are authorized by Claude Code, which keeps its credentials in a store
 * of its own, and running a second, parallel authorization for the same account
 * gets you a second grant to manage and revoke rather than access you did not have.
 * So a registry entry may say "the bearer token for this server is THAT item", and
 * this module is the read.
 *
 * TWO STORES, ONE REFERENCE SYNTAX. Claude Code keeps the item in the macOS login
 * keychain on a Mac and in a plain file (`$CLAUDE_CONFIG_DIR/.credentials.json`,
 * default `~/.claude/.credentials.json`) everywhere else, and on a Mac where the
 * keychain has been opted out of. `${keychain:…}` is the reference either way and
 * `defaultCredentialReader` picks the store, because the alternative is a registry
 * entry whose syntax has to be rewritten when it moves between machines. The name
 * of the syntax is a historical accident (the keychain came first) and not a claim
 * about which store answered; `KeychainResult.store` is what says that, and it is
 * what makes a failure message name a real path instead of advising a Linux user to
 * go and check a keychain they do not have.
 *
 * THE INVARIANT THIS HOLDS, and it is the registry's own rule extended one step:
 * **the registry stores a REFERENCE, never a secret.** `${VAR}` already worked that
 * way for a spawned server's `env` (`config.ts`) precisely because the registry is
 * served over HTTP by `GET /mcp/servers` and rendered in the `/mcp` panel — a
 * literal there would sit in a response body and, worse, in the model's context.
 * `${keychain:…}` is the same promise about a different vault: what is written down
 * is the item's name, the read happens at CONNECT time, and the value goes into one
 * request header and nowhere else. It is never logged, never persisted, never part
 * of a status response, and never reachable from a program.
 *
 * SECOND — **`security` is executed as ARGV, never through a shell.** A service
 * name is user-supplied text with spaces in it (`Claude Code-credentials`), and the
 * one-line version of this that everyone writes first is a template string handed
 * to `sh -c`. Then a service name is a command.
 *
 * THIRD — **an expired token is reported, not refreshed.** An OAuth blob usually
 * carries the expiry beside the token, so a stale one is knowable before the
 * request goes out, and saying so beats a 401 the user has to trace back. Bough
 * does NOT mint a new one from the refresh token: those tokens belong to the client
 * that obtained them, refreshing on its behalf is impersonation rather than
 * plumbing, and the fix — open that client once — is both trivial and the user's.
 *
 * A reference that no store on this machine can answer fails with a sentence
 * naming the stores that were tried, rather than a spawn error.
 */
import { readFile } from "node:fs/promises";
import { homedir } from "node:os";
import { join } from "node:path";
import { McpError } from "../errors.ts";

/** How a credential read is performed. Injected so tests never touch a real store. */
export type KeychainReader = (service: string) => Promise<KeychainResult>;

export interface KeychainResult {
  /** The item's secret, verbatim. Empty when `code` is non-zero. */
  value: string;
  /**
   * `security`'s exit code, or the file reader's imitation of one. 44 is "the item
   * does not exist"; 128 is a denied prompt.
   */
  code: number;
  /** Whatever the store said, trimmed. Never contains the secret. */
  error: string;
  /**
   * Which store produced this. Absent means the keychain, so an injected reader in a
   * test keeps the wording it was written against.
   */
  store?: "keychain" | "file";
}

/**
 * `security find-generic-password -s <service> -w`.
 *
 * `-w` prints the password and nothing else, so there is no output format to parse
 * and no risk of a label leaking into the value. The read may raise the system's
 * "allow access?" dialog the first time — that is macOS asking the human, and it
 * is exactly the confirmation this should require.
 *
 * NO PLATFORM GATE. This used to refuse outright unless `process.platform` was
 * `darwin`, which conflated "there is no keychain here" with "this is not a Mac" and
 * made the two stores mutually exclusive by operating system rather than by what the
 * machine has. A missing `security` binary reports as "no such item" (44), the same
 * as a keychain that simply does not hold it, so `defaultCredentialReader` can try
 * both stores anywhere and take whichever one answers.
 */
export const securityReader: KeychainReader = async (service) => {
  try {
    const proc = Bun.spawn(["security", "find-generic-password", "-s", service, "-w"], {
      stdin: "ignore",
      stdout: "pipe",
      stderr: "pipe",
    });
    const [value, error, code] = await Promise.all([
      new Response(proc.stdout).text(),
      new Response(proc.stderr).text(),
      proc.exited,
    ]);
    return { value, code, error: error.trim(), store: "keychain" };
  } catch (e) {
    // No `security` on PATH: there is no keychain on this machine to hold the item.
    // 44 rather than an error, because "this store does not have it" is the truth and
    // it is what lets the next store be asked.
    return { value: "", code: 44, error: (e as Error).message, store: "keychain" };
  }
};

/**
 * Where Claude Code keeps its configuration and its credentials file.
 *
 * `CLAUDE_CONFIG_DIR` is Claude Code's own override and is honoured for the same
 * reason `BOUGH_HOME` is (`paths.ts`): a machine that has moved its config has moved
 * the credentials with it, and reading the default path would silently find nothing.
 */
export function claudeConfigDir(
  env: Record<string, string | undefined> = process.env,
  home: string = homedir(),
): string {
  const override = env["CLAUDE_CONFIG_DIR"];
  return override && override.trim() ? override : join(home, ".claude");
}

/** The credentials file inside it. */
export function credentialsPath(
  env?: Record<string, string | undefined>,
  home?: string,
): string {
  return join(claudeConfigDir(env, home), ".credentials.json");
}

/**
 * `$CLAUDE_CONFIG_DIR/.credentials.json`, the store Claude Code uses where it is not
 * using a keychain: every non-Mac platform, and a Mac that opted out of one.
 *
 * THIS READER ANSWERS FOR EXACTLY ONE ITEM. A `${keychain:…}` reference names an
 * arbitrary service, and a file holding Claude Code's login is not a general vault:
 * answering some OTHER service's read with this file's contents would hand one
 * client's credential to a reference that asked for a different one. Anything but
 * `CLAUDE_CODE_ITEM` is therefore "no such item" (44) and falls through.
 *
 * The file's permissions are the confinement here, and they are the same ones Claude
 * Code itself relies on. There is no keychain dialog to stand in for consent, which
 * is worth knowing rather than worth working around: a process that can read this
 * file can already read every other secret in that directory.
 */
export const credentialsFileReader: KeychainReader = async (service) => {
  if (service !== CLAUDE_CODE_ITEM) {
    return { value: "", code: 44, error: "", store: "file" };
  }
  const path = credentialsPath();
  try {
    return { value: await readFile(path, "utf8"), code: 0, error: "", store: "file" };
  } catch (e) {
    const err = e as NodeJS.ErrnoException;
    // Absent is the ordinary state of a Mac that uses its keychain, so it reports as
    // "not there" and lets the next store answer. A permission problem is NOT that:
    // the file exists and is being withheld, which is worth saying rather than
    // reporting as absence and blaming the setup.
    const code = err.code === "ENOENT" ? 44 : err.code === "EACCES" ? 128 : 1;
    return { value: "", code, error: `${path}: ${err.message}`, store: "file" };
  }
};

/**
 * The store this machine actually keeps Claude Code's credentials in.
 *
 * BOTH STORES ARE TRIED, ON EVERY PLATFORM, because which one holds the credential is
 * a property of the machine and not of its operating system. A Mac can be running with
 * the keychain opted out and the token in a file; a Linux box has the file and no
 * keychain; a container can have the file mounted in with a `security` binary present
 * and useless. Selecting by `process.platform` got all three of those wrong in one
 * direction or another, and the failure it produced was the confusing kind: a token
 * that plainly exists on disk, and a message about a facility the user does not have.
 *
 * ORDER IS BY AUTHORITY, not availability. The keychain goes first WHERE IT IS THE
 * ONE CLAUDE CODE WRITES TO, so a stale `.credentials.json` left behind by an older
 * install cannot shadow a live token; everywhere else the file is what gets written and
 * is asked first, so the ordinary case costs no spawn. Either way the other store is
 * still consulted, so neither setup is out of reach.
 *
 * The failure that gets REPORTED is the most specific one seen: "you denied the
 * prompt" and "that file is not readable" are both actionable, and "no such item"
 * from the store that was never going to have it is not.
 */
export const defaultCredentialReader: KeychainReader = (service) =>
  readFromStores(service, credentialStores());

/**
 * The same store selection, but for a caller that knows what it needs to find.
 *
 * `defaultCredentialReader` cannot express "the store that has the grants" because a
 * `KeychainReader` is handed a service name and nothing else. Anything reading a
 * FIELD out of the item — `readKeychainRef`, and `sync-mcp` reading the `mcpOAuth`
 * map — needs the store chosen by content, since the two stores on one machine can
 * hold different halves of the same item (see `readFromStores`).
 */
export function credentialReaderFor(
  satisfies: (value: string) => boolean,
): KeychainReader {
  return (service) => readFromStores(service, credentialStores(), satisfies);
}

/**
 * The two stores, in the order this platform should ask them. Exported so the ordering
 * can be asserted without spawning anything.
 */
export function credentialStores(
  platform: string = process.platform,
): KeychainReader[] {
  return platform === "darwin"
    ? [securityReader, credentialsFileReader]
    : [credentialsFileReader, securityReader];
}

/**
 * First store that SATISFIES the read wins; if none does, the most specific failure
 * is what gets reported. Separated from `defaultCredentialReader` so this rule is
 * testable against fake stores rather than against the developer's own login.
 *
 * WHY `satisfies` EXISTS, and why "first store with bytes wins" was wrong. Claude
 * Code keeps two different things under one item name: `claudeAiOauth` (its own
 * login) and `mcpOAuth` (one grant per remote MCP server it has authorized). Those
 * two do not have to live in the same store, and on this developer's own Mac they
 * do not: the keychain item holds `claudeAiOauth` alone. Asking "did this store
 * return bytes" makes the keychain win every read there, so a
 * `#mcpOAuth.<key>.accessToken` reference resolves against a blob that cannot
 * contain it while the grant sits in the file the next store would have read.
 *
 * HONEST PROVENANCE. This is a latent bug found while investigating a DIFFERENT
 * failure, and it was not that failure's cause. On the machine that reported "has no
 * string at #mcpOAuth…" the keychain does hold `mcpOAuth` — deduced from the server
 * connecting a server whose grant was long expired in the FILE, which it could only
 * do by reading a fresher copy from the keychain — so both stores had the path and
 * the ordering already picked the right one. That failure was an empty grant (see
 * `cli/sync_mcp.ts`). The split above is real on this developer's own Mac, which is
 * why this stays; it has not yet been observed to break anyone.
 *
 * A MEASUREMENT TRAP worth leaving written down: over SSH the login keychain is not
 * unlocked, so `security` returns nothing and every probe concludes "this machine has
 * only a file". A launchd user agent sees the opposite. Diagnose store questions from
 * the server's own context, never from a remote shell.
 *
 * So the question a store has to answer is not "do you have this item" but "do you
 * have what was asked for". A store that returns an item missing the requested path
 * is a MISS, and the next store gets asked.
 *
 * The unsatisfying bytes are still remembered and returned when nothing satisfies,
 * because the caller's error message names what the item DOES hold — losing that in
 * favour of a bare "no such item" would trade a diagnosis for a shrug.
 */
export async function readFromStores(
  service: string,
  stores: readonly KeychainReader[],
  satisfies?: (value: string) => boolean,
): Promise<KeychainResult> {
  let worst: KeychainResult | null = null;
  let unsatisfying: KeychainResult | null = null;
  for (const read of stores) {
    const result = await read(service);
    if (result.code === 0 && result.value) {
      if (!satisfies || satisfies(result.value)) return result;
      unsatisfying ??= result;
      continue;
    }
    if (!worst || (worst.code === 44 && result.code !== 44)) worst = result;
  }
  return unsatisfying ?? worst ?? { value: "", code: 44, error: "", store: "file" };
}

/**
 * Does this item hold a usable string at `path`? The `satisfies` predicate for an
 * ordinary `${keychain:…#a.b}` reference.
 *
 * An empty path means the whole item is the secret, and any bytes satisfy that —
 * there is nothing to look inside for.
 */
export function holdsPath(value: string, path: readonly string[]): boolean {
  if (path.length === 0) return true;
  let parsed: unknown;
  try {
    parsed = JSON.parse(value);
  } catch {
    return false;
  }
  const { found } = walk(parsed, path);
  return typeof found === "string" && found.length > 0;
}

export interface KeychainOptions {
  /** Absent = whichever store this machine keeps the credential in. */
  keychain?: KeychainReader;
}

/**
 * A parsed `${keychain:<service>}` or `${keychain:<service>#<a.b.c>}` reference.
 *
 * The path is optional because not every item is JSON. When it is present the item
 * is parsed as JSON and walked — which is what the interesting case needs, since a
 * client that stores OAuth state keeps the token in a field beside its expiry
 * rather than as the whole secret.
 */
export interface KeychainRef {
  service: string;
  /** Dotted path into the item's JSON. Empty = the whole secret, verbatim. */
  path: string[];
}

/** `${keychain:NAME}` / `${keychain:NAME#a.b}`. The service may contain spaces. */
const KEYCHAIN_RE = /^\$\{keychain:([^#{}]+?)(?:#([^{}]*))?\}$/;

/** Parse a whole-value reference, or `null` when this is not one. */
export function parseKeychainRef(value: string): KeychainRef | null {
  const m = KEYCHAIN_RE.exec(value.trim());
  if (!m) return null;
  const service = m[1].trim();
  if (!service) return null;
  const path = (m[2] ?? "").split(".").map((p) => p.trim()).filter(Boolean);
  return { service, path };
}

/**
 * Resolve one reference to its secret.
 *
 * Every failure names the item and says what to do about it, because all of them
 * are recoverable by the human and none of them is diagnosable from the 401 that
 * would otherwise arrive several seconds later at a different layer (spec §6).
 */
export async function readKeychainRef(
  ref: KeychainRef,
  opts: KeychainOptions = {},
): Promise<string> {
  // An INJECTED reader is one store and is asked as one; the store-picking rule
  // below only has meaning when there is more than one store to pick between.
  const { value: raw, code, error, store } = opts.keychain
    ? await opts.keychain(ref.service)
    : await readFromStores(
      ref.service,
      credentialStores(),
      (v) => holdsPath(v, ref.path),
    );
  // `security -w` terminates its output with a newline that is not part of the
  // secret. Stripped HERE rather than in the reader so it holds for every reader:
  // a token with a newline welded on produces a header the remote end rejects for
  // reasons it will not explain, and that is a bad afternoon.
  const value = raw.replace(/\r?\n$/, "");
  if (code !== 0 || !value) {
    throw new McpError(400, keychainFailure(ref.service, code, error, store));
  }
  if (ref.path.length === 0) return value;

  let parsed: unknown;
  try {
    parsed = JSON.parse(value);
  } catch {
    throw new McpError(
      400,
      `the keychain item "${ref.service}" is not JSON, so #${ref.path.join(".")} ` +
        `cannot be read out of it — drop the #path to use the whole item as the secret.`,
    );
  }
  const { found, container } = walk(parsed, ref.path);
  if (typeof found !== "string" || !found) {
    throw new McpError(
      400,
      `the keychain item "${ref.service}" has no string at #${ref.path.join(".")}. ` +
        `It holds: ${describe(parsed)}.`,
    );
  }
  assertFresh(ref, container);
  return found;
};

// ---------------------------------------------------------------------------
// Prefill
// ---------------------------------------------------------------------------

/**
 * The item Claude Code keeps its login in, and the field the bearer token is at.
 *
 * Named here rather than configured because prefill is a convenience with one
 * subject: the account this machine is already logged into. Anything else is the
 * explicit `${keychain:…}` header reference, which says out loud which credential
 * is going to which server.
 */
export const CLAUDE_CODE_ITEM = "Claude Code-credentials";
const CLAUDE_CODE_PATH = ["claudeAiOauth", "accessToken"];

/**
 * Hosts the Claude Code credential BELONGS to.
 *
 * THE POINT OF THIS LIST. Prefill happens without anybody pressing a key, so the
 * question it has to answer is not "would a token help here" but "may this server
 * be told this secret". An MCP server receives the Authorization header verbatim
 * and is usually somebody else's process on somebody else's machine — prefilling a
 * third-party endpoint with an Anthropic credential would hand that credential to
 * whoever runs it, silently, as a side effect of registering a server. So the
 * automatic path is confined to the issuer's own hosts, and reaching any other one
 * with this credential requires writing it into the entry by hand.
 */
const COVERED_HOSTS = ["claude.ai", "anthropic.com"];

export function isCoveredHost(url: string): boolean {
  let host: string;
  try {
    host = new URL(url).hostname.toLowerCase();
  } catch {
    return false;
  }
  return COVERED_HOSTS.some((h) => host === h || host.endsWith(`.${h}`));
}

/**
 * The bearer token to START a covered server's connection with, or `undefined`.
 *
 * PREFILL, not authorization: it is what the server is tried with before anyone is
 * asked to authorize anything, and a stored token from bough's own OAuth flow
 * always wins over it (`BoughOAuthProvider.tokens`). If it is wrong, absent, stale,
 * or no store on this machine has it, the connection proceeds exactly as it did
 * before: a 401 from the endpoint puts the server back on the ordinary
 * `a`-to-authorize path.
 *
 * Hence: **every failure here is silent and returns `undefined`.** That is the
 * opposite of the rule the rest of this module follows, and it is deliberate. A
 * missing `${keychain:…}` reference is a broken configuration and must be reported
 * loudly; a missing prefill is the ordinary state of a machine that never ran
 * Claude Code, and turning it into an error would break every server that has its
 * own perfectly good OAuth flow.
 */
export async function claudeCodePrefill(
  url: string,
  opts: KeychainOptions = {},
): Promise<string | undefined> {
  if (!isCoveredHost(url)) return undefined;
  try {
    return await readKeychainRef(
      { service: CLAUDE_CODE_ITEM, path: CLAUDE_CODE_PATH },
      opts,
    );
  } catch {
    return undefined;
  }
}

/**
 * An `expiresAt` sitting beside the token is a fact about the token, so it is
 * checked. Epoch milliseconds or an ISO string — both are common and neither is
 * worth guessing wrong about.
 */
function assertFresh(ref: KeychainRef, container: unknown): void {
  if (!container || typeof container !== "object") return;
  const raw = (container as Record<string, unknown>)["expiresAt"];
  const at = typeof raw === "number" ? raw : typeof raw === "string" ? Date.parse(raw) : NaN;
  if (!Number.isFinite(at) || at > Date.now()) return;
  throw new McpError(
    400,
    `the token in keychain item "${ref.service}" expired at ${new Date(at).toISOString()}. ` +
      `bough does not refresh a credential it did not obtain — open the client that owns ` +
      `this item (for "Claude Code-credentials", run \`claude\` once) and it will refresh ` +
      `it in place.`,
  );
}

/** The value at `path`, and the object it was found in (for the expiry check). */
function walk(root: unknown, path: readonly string[]): { found: unknown; container: unknown } {
  let container: unknown = undefined;
  let node: unknown = root;
  for (let i = 0; i < path.length; i++) {
    if (!node || typeof node !== "object") return { found: undefined, container };
    const here = node as Record<string, unknown>;
    container = node;
    if (path[i] in here) {
      node = here[path[i]];
      continue;
    }
    // A KEY THAT CONTAINS DOTS. The path is dotted, so a literal key with a dot in
    // it — `mcpOAuth."slack|mcp.example.com#a1b2"`, which is the shape Claude Code
    // stores a per-server OAuth grant under — is unaddressable by splitting alone.
    // Rejoining the remaining segments longest-first finds it, and only ever runs
    // when the plain segment missed, so an exact key still wins and no existing
    // reference changes meaning.
    let matched = false;
    for (let end = path.length; end > i + 1; end--) {
      const joined = path.slice(i, end).join(".");
      if (joined in here) {
        node = here[joined];
        i = end - 1;
        matched = true;
        break;
      }
    }
    if (!matched) return { found: undefined, container };
  }
  return { found: node, container };
}

/** The item's SHAPE, for an error message. Never a value — this is a secret. */
function describe(parsed: unknown): string {
  if (Array.isArray(parsed)) return `an array of ${parsed.length}`;
  if (parsed && typeof parsed === "object") {
    const keys = Object.keys(parsed as object);
    return keys.length ? `an object with ${keys.join(", ")}` : "an empty object";
  }
  return typeof parsed;
}

/**
 * What went wrong, in the user's terms, and in terms of the store that said so.
 *
 * The store's own exit codes are the only reliable signal here (stderr is one line
 * of C-program diagnostics), and the two that matter are worth telling apart: "there
 * is no such item" is a setup problem, "you said no" is a decision the user just made
 * and must not read as a bug.
 *
 * `store` is what keeps the advice true. Telling someone on Linux to run
 * `security find-generic-password` is a dead end they have to discover for
 * themselves, and telling a Mac user to go and look at a `.credentials.json` that is
 * deliberately not there is the same dead end pointing the other way.
 *
 * "NOT FOUND" NAMES BOTH STORES, because both were tried. A message that mentions
 * only the one that happened to answer last reads as though the other was never
 * looked at, which sends someone to go and check a location that has already been
 * checked. Every other failure names one store, correctly: those are specific to it.
 */
function keychainFailure(
  service: string,
  code: number,
  error: string,
  store: KeychainResult["store"],
): string {
  const file = store === "file";
  const head = `could not read credential item "${service}"`;
  if (code === 44 || /could not be found/i.test(error)) {
    const advice = `Make sure the client that owns it has been logged in on this ` +
      `machine. For "${CLAUDE_CODE_ITEM}", run \`claude\` once, and set ` +
      `CLAUDE_CONFIG_DIR if its configuration lives somewhere else.`;
    return file
      ? `${head}: it is in neither ${credentialsPath()} nor the login keychain. ${advice}`
      : `${head}: no generic-password item with that service name is in the login ` +
        `keychain, and ${credentialsPath()} does not hold it either. Check the name ` +
        `with \`security find-generic-password -s "${service}"\`. ${advice}`;
  }
  if (code === 128 || /User interaction|denied|cancel/i.test(error)) {
    return file
      ? `${head}: ${credentialsPath()} is not readable by this process${
        error ? `: ${error}` : ""
      }.`
      : `${head}: the keychain access prompt was denied or cancelled. macOS asks once ` +
        `per program — answer "Always Allow" to stop it asking again.`;
  }
  return file
    ? `${head}: ${error || `reading ${credentialsPath()} failed`}.`
    : `${head}: security exited ${code}${error ? ` — ${error}` : ""}.`;
}
