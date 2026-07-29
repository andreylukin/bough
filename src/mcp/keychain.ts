/**
 * Reading a secret out of the macOS login keychain, for MCP servers that are
 * already authorized somewhere else on this machine.
 *
 * WHY THIS EXISTS. bough's own way into a remote MCP server is OAuth: dynamic
 * client registration, a browser round trip, tokens in `~/.bough/mcp-auth.json`
 * (`oauth.ts`). That is the right default and it is not the only situation. Some
 * servers are already authorized on this machine by another client — the claude.ai
 * connectors are authorized by Claude Code, which keeps its credentials in a
 * keychain item — and running a second, parallel authorization for the same account
 * gets you a second grant to manage and revoke rather than access you did not have.
 * So a registry entry may say "the bearer token for this server is THAT keychain
 * item", and this module is the read.
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
 * macOS only, by construction: this is the `security` binary. On any other platform
 * the reference fails with a sentence saying so rather than a spawn error.
 */
import { McpError } from "../errors.ts";

/** How a keychain read is performed. Injected so tests never touch a real keychain. */
export type KeychainReader = (service: string) => Promise<KeychainResult>;

export interface KeychainResult {
  /** The item's secret, verbatim. Empty when `code` is non-zero. */
  value: string;
  /** `security`'s exit code. 44 is "the item does not exist"; 128 is a denied prompt. */
  code: number;
  /** Whatever `security` said on stderr, trimmed. Never contains the secret. */
  error: string;
}

/**
 * `security find-generic-password -s <service> -w`.
 *
 * `-w` prints the password and nothing else, so there is no output format to parse
 * and no risk of a label leaking into the value. The read may raise the system's
 * "allow access?" dialog the first time — that is macOS asking the human, and it
 * is exactly the confirmation this should require.
 */
export const securityReader: KeychainReader = async (service) => {
  if (process.platform !== "darwin") {
    return {
      value: "",
      code: -1,
      error: `the login keychain is a macOS facility and this is ${process.platform}`,
    };
  }
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
  return { value, code, error: error.trim() };
};

export interface KeychainOptions {
  /** Absent = the real `security` binary. */
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
  const read = opts.keychain ?? securityReader;
  const { value: raw, code, error } = await read(ref.service);
  // `security -w` terminates its output with a newline that is not part of the
  // secret. Stripped HERE rather than in the reader so it holds for every reader:
  // a token with a newline welded on produces a header the remote end rejects for
  // reasons it will not explain, and that is a bad afternoon.
  const value = raw.replace(/\r?\n$/, "");
  if (code !== 0 || !value) {
    throw new McpError(400, keychainFailure(ref.service, code, error));
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
 * or this is not a Mac, the connection proceeds exactly as it did before — a 401
 * from the endpoint puts the server back on the ordinary `a`-to-authorize path.
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
function walk(root: unknown, path: string[]): { found: unknown; container: unknown } {
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
 * What went wrong, in the user's terms.
 *
 * `security`'s own exit codes are the only reliable signal here — its stderr is one
 * line of C-program diagnostics — and the two that matter are worth telling apart:
 * "there is no such item" is a setup problem, "you said no to the dialog" is a
 * decision the user just made and must not read as a bug.
 */
function keychainFailure(service: string, code: number, error: string): string {
  const head = `could not read keychain item "${service}"`;
  if (code === 44 || /could not be found/i.test(error)) {
    return `${head}: no generic-password item with that service name is in the login ` +
      `keychain. Check the name with \`security find-generic-password -s "${service}"\`, ` +
      `and make sure the client that owns it has been logged in on this machine.`;
  }
  if (code === 128 || /User interaction|denied|cancel/i.test(error)) {
    return `${head}: the keychain access prompt was denied or cancelled. macOS asks once ` +
      `per program — answer "Always Allow" to stop it asking again.`;
  }
  return `${head}: security exited ${code}${error ? ` — ${error}` : ""}.`;
}
