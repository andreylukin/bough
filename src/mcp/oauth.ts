/**
 * OAuth for remote MCP servers, and the callback bough hosts for it.
 *
 * THE INVARIANT THIS HOLDS: **an unauthorized server is a QUESTION, never a
 * failure and never a hang.** Everything here exists so that a remote server
 * answering 401 turns into one sentence a human can act on — "not authorized —
 * open the mcp panel (^p) and press a" — and one URL they can open. The SDK's `auth()` drives
 * discovery, dynamic client registration and PKCE; this module supplies the
 * `OAuthClientProvider` it needs and owns three things the SDK deliberately does
 * not: where credentials live, who the callback belongs to, and the fact that
 * bough never navigates anything.
 *
 * **bough is a PUBLIC client and hosts its own redirect.** `token_endpoint_auth_method`
 * is `"none"` — there is no client secret to keep, PKCE carries the proof — and the
 * authorization server sends the browser back to `GET /mcp/oauth/callback` on
 * bough's own port. No shim binary, no second listener, no cloud redirect: the port
 * is already bound and already loopback-only (spec §17), so the one HTTP surface
 * bough has is the one the flow uses.
 *
 * **The provider captures, it does not redirect.** `redirectToAuthorization` stores
 * the URL instead of opening it. A headless server that shells out to a browser is
 * a server that hangs when there is no browser, and the model must never be handed
 * a URL to "click": `beginAuth()` returns the URL to the human through the API and
 * the browser half of the flow is theirs.
 *
 * **Credentials are per server, private, and outside the model's reach.**
 * `~/.bough/mcp/tokens/<server>.json`, dir 0700, file 0600, holding the dynamic
 * client registration, the tokens, the in-flight PKCE verifier and state nonce, and
 * cached discovery. Tokens reach the transport (`remote.ts`) and nothing else —
 * they are never in a prompt, a part, or an event.
 *
 * **The `state` round-trip binds a callback to the server that started it.** The
 * nonce is minted per flow and stored as `<server>.<nonce>`; the callback splits it,
 * matches the stored nonce, and refuses otherwise. Without that check, any request
 * to the callback could graft tokens onto whichever server it named.
 *
 * NOTE (port, from `src/mcp/oauth.ts`): four deltas. (1) The store is injected
 * (`TokenStoreOptions.dir`) instead of read from a `BOUGH_MCP_DIR` env var, per the
 * dependency-injection ground rule — a test gets a hermetic store with no env
 * mutation. (2) `invalidateCredentials` is implemented; without it the SDK's
 * recovery path for a rejected refresh token (`InvalidGrantError` → drop tokens →
 * retry) is a no-op that loops back into the same rejection and escapes as a raw
 * OAuth error instead of an authorization prompt — which is exactly the "an expired
 * refresh token degrades the same way" requirement (plan T7.2). (3) Discovery state
 * is cached, so a reconnect does not re-probe three well-known endpoints.
 * (4) The HTTP handlers live here rather than in `server/`, and they build their own
 * `Response` rather than importing `json` from `server/app.ts`: `app.ts` imports this
 * module for the route entry, and closing that cycle would make this module's
 * evaluation depend on `app.ts` having evaluated first.
 */
import { chmodSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import {
  auth,
  type OAuthClientProvider,
  type OAuthDiscoveryState,
} from "@modelcontextprotocol/sdk/client/auth.js";
import type {
  OAuthClientInformationMixed,
  OAuthClientMetadata,
  OAuthTokens,
} from "@modelcontextprotocol/sdk/shared/auth.js";
import type { FetchLike } from "@modelcontextprotocol/sdk/shared/transport.js";
import { McpError } from "../errors.ts";
import { boughPath, confine } from "../paths.ts";
import { expandEnv, getServer, type McpConfigOptions, upsertServer } from "./config.ts";
import type { AppCtx } from "../types.ts";

// ---------------------------------------------------------------------------
// Where the callback lives
// ---------------------------------------------------------------------------

/**
 * The port the callback URL advertises. Set once at boot from the port the
 * listener actually bound (`server/main.ts`), because the redirect URI is
 * registered with the authorization server and baked into the authorization
 * request: if it names a port nothing is listening on, the user approves access in
 * their browser and lands on a connection error with no way back.
 *
 * Module-level rather than injected, and this is the one place it is justified:
 * the value is a property of the PROCESS, and every reader — the provider's
 * `redirectUrl`, its `clientMetadata`, the `/mcp` panel showing the human where the
 * flow returns — must agree with the socket. The fallback keeps it honest anyway:
 * absent wiring, it reads `BOUGH_PORT` exactly as `main.ts` does.
 */
let configuredPort: number | undefined;

/** Boot wiring: pin the callback to the port the server actually bound. */
export function configureOAuthCallback(opts: { port: number }): void {
  configuredPort = opts.port;
}

/** The port the callback URL names. */
export function callbackPort(): number {
  return configuredPort ?? Number(process.env.BOUGH_PORT ?? 4321);
}

/** The redirect target — bough's own HTTP surface, loopback only. */
export function callbackUrl(): string {
  return `http://127.0.0.1:${callbackPort()}/mcp/oauth/callback`;
}

/** The path half of {@link callbackUrl}, so the route entry and the URL agree. */
export const CALLBACK_PATH = "/mcp/oauth/callback";

// ---------------------------------------------------------------------------
// The store
// ---------------------------------------------------------------------------

/**
 * Registry names are slugs (`mcp/config.ts` owns that rule). Restated here rather
 * than imported because this is a different job: config validates what may be
 * WRITTEN to the registry, and this validates what may become a FILENAME. The
 * callback's `state` parameter arrives from a browser, so the server name in it is
 * untrusted input steering the server's own path construction (`paths.confine` is
 * the second half of the same guard).
 */
const NAME_RE = /^[a-z0-9][a-z0-9_-]*$/;

function assertServerName(server: string): string {
  if (!NAME_RE.test(server)) {
    throw new McpError(
      400,
      `${JSON.stringify(server)} is not a valid MCP server name — names are lowercase ` +
        `slugs (a-z, 0-9, - and _, starting with a letter or digit). Nothing was read ` +
        `or written.`,
    );
  }
  return server;
}

/** Everything one server's flow needs to survive a restart. */
interface Stored {
  /** Dynamic client registration — survives a token clear; re-registering is wasteful. */
  client?: OAuthClientInformationMixed;
  tokens?: OAuthTokens;
  /** Absolute ms when the access token expires, derived from `expires_in` at save. */
  expiresAt?: number;
  /** In-flight PKCE verifier; consumed by the code exchange. */
  codeVerifier?: string;
  /** In-flight authorization nonce; cleared when tokens land. */
  state?: string;
  /** Cached RFC 9728 / RFC 8414 discovery, so a reconnect re-probes nothing. */
  discovery?: OAuthDiscoveryState;
}

export interface TokenStoreOptions {
  /** Where token files live. Absent = `~/.bough/mcp/tokens`. Injected in tests. */
  dir?: string;
}

/** `~/.bough/mcp/tokens` — one file per server, never one file for all of them. */
export function defaultTokensDir(): string {
  return boughPath("mcp", "tokens");
}

/**
 * Per-server credential files. Synchronous on purpose: every caller is inside the
 * SDK's `OAuthClientProvider`, whose methods may return a value or a promise, and a
 * store that cannot lose a write to an interleaving is worth more here than the
 * microseconds an async read would save.
 */
export class TokenStore {
  readonly dir: string;

  constructor(opts: TokenStoreOptions = {}) {
    this.dir = opts.dir ?? defaultTokensDir();
  }

  /** The file one server's credentials live in. Confined to `dir`. */
  fileFor(server: string): string {
    return confine(this.dir, `${assertServerName(server)}.json`);
  }

  /** Everything stored for `server`. Absent or unreadable = nothing stored. */
  load(server: string): Stored {
    try {
      const raw = JSON.parse(readFileSync(this.fileFor(server), "utf-8"));
      return raw && typeof raw === "object" ? raw as Stored : {};
    } catch (error) {
      // A corrupt credential file must fail CLOSED — as "not authorized", which
      // the human can fix with one command — rather than as a parse error in the
      // middle of a turn. A missing file is the ordinary case and reads the same.
      if (error instanceof McpError) throw error; // a bad NAME is not a missing file
      return {};
    }
  }

  /** Replace the whole document. Creates the directory 0700, the file 0600. */
  write(server: string, stored: Stored): void {
    const file = this.fileFor(server);
    mkdirSync(this.dir, { recursive: true, mode: 0o700 });
    try {
      chmodSync(this.dir, 0o700); // mkdir's mode is umask-masked; this is not
    } catch { /* not ours to chmod — the file mode below still applies */ }
    writeFileSync(file, JSON.stringify(stored, null, 2) + "\n");
    chmodSync(file, 0o600);
  }

  /** Merge a delta into what is stored. */
  patch(server: string, delta: Partial<Stored>): void {
    this.write(server, { ...this.load(server), ...delta });
  }

  /** Forget everything for one server ("logout"). Returns whether there was any. */
  clear(server: string): boolean {
    try {
      rmSync(this.fileFor(server));
      return true;
    } catch {
      return false; // nothing stored — already clear
    }
  }
}

/** The process-default store: `~/.bough/mcp/tokens`, relocated by `BOUGH_HOME`. */
function storeFor(opts: TokenStoreOptions = {}): TokenStore {
  return new TokenStore(opts);
}

// ---------------------------------------------------------------------------
// The provider
// ---------------------------------------------------------------------------

export interface ProviderOptions extends TokenStoreOptions {
  /** Override the redirect URI. Absent = {@link callbackUrl}. */
  redirectUrl?: string;
  /** Clock, injected so a token-expiry assertion needs no sleeping. */
  now?: () => number;
  /**
   * Where the REGISTRY is read from, for the pre-registered-client fallback.
   *
   * Injected for the same reason `dir` is: a provider test must not read the
   * developer's own `~/.bough/mcp.json`, and it carries the `env` lookup that
   * resolves a `${VAR}` secret without touching the real environment. Production
   * call sites pass nothing and get the real registry.
   */
  config?: McpConfigOptions;
}

/**
 * bough's `OAuthClientProvider`: persistence plus one refusal.
 *
 * The refusal is `redirectToAuthorization`, which captures rather than navigates —
 * see the module comment. Everything else is storage, and every method is written
 * so that a half-finished flow leaves the previous state alone: saving tokens keeps
 * the registration and drops the nonce, invalidating tokens keeps the registration,
 * and only an explicit `clearAuth`/`invalidateCredentials("all")` throws it away.
 */
export class BoughOAuthProvider implements OAuthClientProvider {
  /** Set when `auth()` wanted the user agent sent somewhere. Captured, not followed. */
  authorizationUrl?: URL;

  readonly #store: TokenStore;
  readonly #redirectUrl: string | undefined;
  readonly #now: () => number;
  readonly #config: McpConfigOptions;

  constructor(readonly server: string, opts: ProviderOptions = {}) {
    assertServerName(server);
    this.#store = storeFor(opts);
    this.#redirectUrl = opts.redirectUrl;
    this.#now = opts.now ?? Date.now;
    this.#config = opts.config ?? {};
  }

  get redirectUrl(): string {
    return this.#redirectUrl ?? callbackUrl();
  }

  get clientMetadata(): OAuthClientMetadata {
    return {
      client_name: "bough",
      redirect_uris: [this.redirectUrl],
      grant_types: ["authorization_code", "refresh_token"],
      response_types: ["code"],
      // Public client. There is no secret to store, and PKCE carries the proof.
      token_endpoint_auth_method: "none",
    };
  }

  /** `<server>.<nonce>` — the callback needs both halves. */
  state(): string {
    const nonce = crypto.randomUUID();
    this.#store.patch(this.server, { state: nonce });
    return `${this.server}.${nonce}`;
  }

  /**
   * The OAuth client to authorize as, dynamically registered or pre-registered.
   *
   * THE GAP THIS CLOSES: this returned only what a previous DCR had stored, so
   * against an authorization server with no `registration_endpoint` the SDK went on
   * to register, failed, and the flow died with "does not support dynamic client
   * registration". Slack publishes exactly that. There was no way to say "here is
   * the app I already made".
   *
   * A STORED client WINS. One that came back from a real registration is the one
   * the authorization server issued and knows; a static id is what to fall back on
   * when there was never a registration to do. The SDK skips registration entirely
   * whenever this returns a value, which is the whole of the fix.
   *
   * Read FRESH rather than cached at construction: the panel can write a
   * `clientId` onto the entry between one attempt and the next, and the second
   * press of `a` has to see it.
   */
  clientInformation(): OAuthClientInformationMixed | undefined {
    const stored = this.#store.load(this.server).client;
    if (stored) return stored;
    const entry = getServer(this.server, this.#config);
    if (!entry?.clientId) return undefined;
    // The secret is a `${VAR}` reference by schema, expanded here and nowhere
    // earlier — `expandEnv` throws an McpError naming the variable when it is not
    // set, which is the message the user needs and the one they would otherwise
    // get as an opaque 401 from the token endpoint.
    const secret = entry.clientSecret === undefined
      ? undefined
      : expandEnv({ clientSecret: entry.clientSecret }, this.#config).clientSecret;
    return secret === undefined
      ? { client_id: entry.clientId }
      : { client_id: entry.clientId, client_secret: secret };
  }

  saveClientInformation(client: OAuthClientInformationMixed): void {
    this.#store.patch(this.server, { client });
  }

  tokens(): OAuthTokens | undefined {
    return this.#store.load(this.server).tokens;
  }

  saveTokens(tokens: OAuthTokens): void {
    const expiresIn = typeof tokens.expires_in === "number" ? tokens.expires_in : undefined;
    this.#store.write(this.server, {
      ...this.#store.load(this.server),
      tokens,
      expiresAt: expiresIn === undefined ? undefined : this.#now() + expiresIn * 1000,
      // Tokens landing means the in-flight authorization finished. Dropping the
      // nonce and the verifier is what stops a replayed callback from exchanging
      // the same code twice.
      state: undefined,
      codeVerifier: undefined,
    });
  }

  redirectToAuthorization(url: URL): void {
    this.authorizationUrl = url;
  }

  saveCodeVerifier(codeVerifier: string): void {
    this.#store.patch(this.server, { codeVerifier });
  }

  codeVerifier(): string {
    const verifier = this.#store.load(this.server).codeVerifier;
    if (!verifier) {
      throw new McpError(
        400,
        `no PKCE verifier is stored for "${this.server}", so this authorization cannot ` +
          `be completed — it was started by a different process or already finished. ` +
          `Open the mcp panel (^p) and press a on ${this.server} to start a fresh one.`,
      );
    }
    return verifier;
  }

  /**
   * The SDK's recovery hook, and the reason an expired refresh token degrades into
   * an authorization prompt instead of an OAuth stack trace: `auth()` catches
   * `InvalidGrantError`, calls this with `"tokens"`, and retries — which now finds
   * no refresh token and starts a fresh authorization, returning REDIRECT.
   */
  invalidateCredentials(scope: "all" | "client" | "tokens" | "verifier" | "discovery"): void {
    if (scope === "all") {
      this.#store.clear(this.server);
      return;
    }
    const stored = this.#store.load(this.server);
    if (scope === "client") stored.client = undefined;
    if (scope === "tokens") {
      stored.tokens = undefined;
      stored.expiresAt = undefined;
    }
    if (scope === "verifier") stored.codeVerifier = undefined;
    if (scope === "discovery") stored.discovery = undefined;
    this.#store.write(this.server, stored);
  }

  saveDiscoveryState(discovery: OAuthDiscoveryState): void {
    this.#store.patch(this.server, { discovery });
  }

  discoveryState(): OAuthDiscoveryState | undefined {
    return this.#store.load(this.server).discovery;
  }
}

// ---------------------------------------------------------------------------
// Status
// ---------------------------------------------------------------------------

/** What `/mcp` shows next to a remote server, and what the catalog reads. */
export interface AuthStatus {
  server: string;
  /** Something is stored that the transport can present or refresh. */
  authorized: boolean;
  /** The access token is past its expiry — refreshable, not broken. */
  expired: boolean;
  /** A refresh token is stored, so an expiry heals itself inside the transport. */
  refreshable: boolean;
  /** Where the browser comes back to, so the panel can say it. */
  callback: string;
}

export function authStatus(
  server: string,
  opts: TokenStoreOptions & { now?: () => number } = {},
): AuthStatus {
  const stored = storeFor(opts).load(server);
  const now = (opts.now ?? Date.now)();
  return {
    server,
    authorized: stored.tokens !== undefined,
    expired: stored.expiresAt !== undefined && stored.expiresAt <= now,
    refreshable: typeof stored.tokens?.refresh_token === "string",
    callback: callbackUrl(),
  };
}

/** Whether anything is stored for `server`. */
export function hasTokens(server: string, opts: TokenStoreOptions = {}): boolean {
  return storeFor(opts).load(server).tokens !== undefined;
}

/** Forget a server's registration and tokens ("logout"). */
export function clearAuth(server: string, opts: TokenStoreOptions = {}): boolean {
  return storeFor(opts).clear(server);
}

// ---------------------------------------------------------------------------
// The flow
// ---------------------------------------------------------------------------

export interface AuthStart {
  status: "authorized" | "redirect";
  server: string;
  /** Present for "redirect": the URL the human must open to approve access. */
  authorizationUrl?: string;
}

export interface AuthFlowOptions extends ProviderOptions {
  /** HTTP for discovery, registration and token exchange. Injected in tests. */
  fetchFn?: FetchLike;
  /** Reuse a provider (so a caller can read back `authorizationUrl`). */
  provider?: BoughOAuthProvider;
  /** Per-request deadline for the flow's HTTP. Absent = {@link AUTH_HTTP_MS}. */
  timeoutMs?: number;
}

/**
 * How long any one request in the flow may take. The flow is three or four round
 * trips to a server bough does not control, reached from an HTTP handler a human is
 * waiting on: unbounded, an authorization server that accepts a connection and
 * stalls parks that request forever, which is the same hang `remote.ts` refuses on
 * the JSON-RPC channel.
 */
const AUTH_HTTP_MS = 15_000;

/** `fetch` with a deadline, wrapping whatever was injected. */
function boundedFetch(base: FetchLike | undefined, timeoutMs: number): FetchLike {
  const inner: FetchLike = base ?? ((url: string | URL, init?: RequestInit) => fetch(url, init));
  return (url: string | URL, init?: RequestInit) => {
    const signals = [AbortSignal.timeout(timeoutMs)];
    if (init?.signal) signals.push(init.signal);
    return inner(url, { ...init, signal: AbortSignal.any(signals) });
  };
}

/**
 * Turn whatever escaped the SDK's `auth()` into a sentence naming the server and
 * the move. What escapes is a raw `TypeError: fetch failed`, an `OAuthError`, or a
 * schema complaint about a metadata document — none of which say which server, and
 * all of which reach a human through an HTTP response.
 */
function authFailure(server: string, serverUrl: string, error: unknown): McpError {
  if (error instanceof McpError) return error;
  const detail = error instanceof Error ? error.message : String(error);
  const cause = error instanceof Error && error.cause instanceof Error
    ? `: ${error.cause.message}`
    : "";
  // NOT A BROKEN URL, so it must not say "check the url". An authorization server
  // with no `registration_endpoint` is working exactly as designed and wants an app
  // the user creates; the generic advice sends them to re-check a setting that was
  // right, which is how a solvable stop becomes a dead end.
  if (/does not support dynamic client registration/i.test(detail)) {
    return new McpError(
      502,
      `"${server}" (${serverUrl}) requires an OAuth client you register yourself — its ` +
        `authorization server does not offer dynamic registration. Create an app with ` +
        `that provider, set its redirect URL to ${callbackUrl()}, then put the id and ` +
        `secret on the registry entry — \`clientId\`, and \`clientSecret\` as a ` +
        `\${VAR} reference to a variable in ~/.bough/env — and press a again. ` +
        `Nothing was stored.`,
    );
  }
  return new McpError(
    502,
    `could not run the OAuth flow for "${server}" against ${serverUrl}: ${detail}${cause}. ` +
      `Check \`url\` in the registry (GET /mcp/servers) — it must point at the MCP endpoint ` +
      `itself — and that the server is reachable. Nothing was stored.`,
  );
}

/**
 * Start — or silently finish — the OAuth flow for one remote server.
 *
 * "authorized" means the stored tokens were usable or refreshable and nothing is
 * asked of the human. "redirect" hands back the URL they must open. Discovery,
 * dynamic registration and PKCE all happen inside the SDK's `auth()`; what this
 * adds is that neither outcome is an error and neither one blocks.
 */
export async function beginAuth(
  server: string,
  serverUrl: string,
  opts: AuthFlowOptions = {},
): Promise<AuthStart> {
  const provider = opts.provider ?? new BoughOAuthProvider(server, opts);
  let result: string;
  try {
    result = await auth(provider, {
      serverUrl,
      fetchFn: boundedFetch(opts.fetchFn, opts.timeoutMs ?? AUTH_HTTP_MS),
    });
  } catch (error) {
    throw authFailure(server, serverUrl, error);
  }
  if (result === "AUTHORIZED") return { status: "authorized", server };
  if (!provider.authorizationUrl) {
    throw new McpError(
      502,
      `the authorization server for "${server}" produced no authorization URL, so there ` +
        `is nothing to approve. Check \`url\` in the registry (GET /mcp/servers) — it must ` +
        `point at the MCP endpoint itself.`,
    );
  }
  return { status: "redirect", server, authorizationUrl: String(provider.authorizationUrl) };
}

export interface CompleteAuthOptions extends AuthFlowOptions {
  /** The registry lookup. Injected so the callback can be tested without a registry. */
  serverUrlFor?: (server: string) => string | undefined;
}

/**
 * Finish the flow from the browser callback: validate the `state` round-trip,
 * exchange the code, persist the tokens. Returns the server the tokens belong to.
 *
 * The state check happens BEFORE anything touches the network, so a forged or
 * replayed callback costs one string comparison and cannot start an exchange
 * against a server it named.
 */
export async function completeAuth(
  state: string,
  code: string,
  opts: CompleteAuthOptions = {},
): Promise<string> {
  const dot = state.lastIndexOf(".");
  if (dot <= 0) {
    throw new McpError(
      400,
      `malformed state ${JSON.stringify(state)} — a bough callback carries ` +
        `"<server>.<nonce>". Start the flow again from the mcp panel (^p, then a).`,
    );
  }
  const server = assertServerName(state.slice(0, dot));
  const nonce = state.slice(dot + 1);
  if (!nonce || storeFor(opts).load(server).state !== nonce) {
    throw new McpError(
      400,
      `state mismatch for "${server}" — this callback does not match the authorization ` +
        `bough started (it may have been completed already, or started by a different ` +
        `process). Open the mcp panel (^p) and press a on ${server} to start a fresh one.`,
    );
  }
  const lookup = opts.serverUrlFor ?? ((name: string) => remoteServerUrl(name));
  const serverUrl = lookup(server);
  if (!serverUrl) {
    throw new McpError(
      404,
      `"${server}" is not a registered remote MCP server, so there is nothing to ` +
        `authorize. Register it with PUT /mcp/servers/${server} first.`,
    );
  }
  const provider = opts.provider ?? new BoughOAuthProvider(server, opts);
  let result: string;
  try {
    result = await auth(provider, {
      serverUrl,
      authorizationCode: code,
      fetchFn: boundedFetch(opts.fetchFn, opts.timeoutMs ?? AUTH_HTTP_MS),
    });
  } catch (error) {
    throw authFailure(server, serverUrl, error);
  }
  if (result !== "AUTHORIZED") {
    throw new McpError(
      502,
      `the token exchange for "${server}" did not complete — the authorization server ` +
        `accepted the code but returned no tokens. Press a on ${server} again (^p).`,
    );
  }
  return server;
}

/** The registry's `url` for a server, read FRESH (plan §6.13: MCP state is never cached). */
export function remoteServerUrl(server: string, opts: McpConfigOptions = {}): string | undefined {
  return getServer(server, opts)?.url;
}

// ---------------------------------------------------------------------------
// HTTP
// ---------------------------------------------------------------------------
//
// These build their own `Response` instead of importing `json` from `server/app.ts`:
// `app.ts` imports this module for its route entries, and importing back would
// close an evaluation cycle whose resolution depends on which module Deno happens
// to evaluate first. Domain failures are thrown as `McpError` and rendered by the
// router's one catch, exactly like every other handler.

function jsonResponse(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json; charset=utf-8" },
  });
}

/** The registry entry a remote-auth request is about, or a readable refusal. */
function requireRemote(name: string): string {
  assertServerName(name);
  const server = getServer(name);
  if (!server) {
    throw new McpError(
      404,
      `"${name}" is not a registered MCP server. Register it with PUT /mcp/servers/${name}.`,
    );
  }
  if (!server.url) {
    throw new McpError(
      400,
      `"${name}" is a local stdio server — it runs as a subprocess and has no OAuth. ` +
        `Authorization applies to remote (\`url\`) servers only.`,
    );
  }
  return server.url;
}

/** `GET /mcp/servers/:name/auth` — is this server authorized, and where does the flow return? */
export function authStatusH(_req: Request, _ctx: AppCtx, params: Record<string, string>): Response {
  const name = params.name ?? "";
  requireRemote(name);
  return jsonResponse(authStatus(name));
}

/**
 * `POST /mcp/servers/:name/auth` — start the flow. This is what the mcp panel's `a`
 * calls. It returns the URL; it never opens a browser and never blocks waiting for
 * one, so a headless install behaves the same as a desktop one.
 */
export async function beginAuthH(
  _req: Request,
  _ctx: AppCtx,
  params: Record<string, string>,
): Promise<Response> {
  const name = params.name ?? "";
  const url = requireRemote(name);
  try {
    return jsonResponse(await beginAuth(name, url));
  } catch (error) {
    // THE URL IN THE DOCS IS OFTEN NOT THE URL THE FLOW WANTS. Linear publishes
    // `https://mcp.linear.app/sse`; that endpoint's RFC9728 metadata declares its
    // resource as `https://mcp.linear.app/mcp`, and the SDK refuses the mismatch —
    // correctly, since a resource indicator that does not match is how a token gets
    // minted for the wrong audience. But the server has just TOLD us the right URL,
    // and making the user read a 502, edit the registry by hand and try again is
    // friction over a fact bough is already holding.
    //
    // Same-origin only. Following a cross-origin redeclaration would let a server
    // point bough's registry at someone else's endpoint.
    const advertised = declaredResource(error, url);
    if (!advertised) throw error;
    // The whole entry is rewritten with the corrected url; every other field is
    // carried through, because `upsertServer` replaces rather than merges.
    upsertServer(name, { ...getServer(name), url: advertised });
    return jsonResponse({ ...(await beginAuth(name, advertised)), correctedUrl: advertised });
  }
}

/**
 * The resource URL a failed flow says the server actually declares, when it is safe
 * to adopt: same origin, and genuinely different from what we tried.
 *
 * Read out of the SDK's message rather than re-fetching the metadata, because the
 * SDK has already done that fetch and its comparison is the thing that failed.
 */
export function declaredResource(error: unknown, tried: string): string | null {
  const text = error instanceof Error ? error.message : String(error);
  const m = /Protected resource (\S+) does not match expected (\S+)/.exec(text);
  if (!m) return null;
  try {
    const found = new URL(m[1]!);
    if (found.origin !== new URL(tried).origin) return null;
    return found.href === new URL(tried).href ? null : found.href;
  } catch {
    return null;
  }
}

/** `DELETE /mcp/servers/:name/auth` — forget the tokens ("logout"). */
export function clearAuthH(_req: Request, _ctx: AppCtx, params: Record<string, string>): Response {
  const name = params.name ?? "";
  assertServerName(name);
  return jsonResponse({ server: name, cleared: clearAuth(name) });
}

/**
 * `GET /mcp/oauth/callback` — where the user's browser lands.
 *
 * The audience is a HUMAN in a browser tab, so every outcome is a readable page
 * rather than a JSON error: they cannot act on `{"error": …}` and they will not see
 * a status code. The page is self-contained — no CDN, no font, no image (spec §11's
 * bar applies to anything bough serves).
 */
export async function oauthCallbackH(req: Request): Promise<Response> {
  const query = new URL(req.url).searchParams;
  const error = query.get("error");
  if (error) {
    // The authorization server refused, or the user declined. Their words, not ours.
    return page(
      400,
      "Authorization was declined",
      `${escapeHtml(error)}${
        query.get("error_description") ? `: ${escapeHtml(query.get("error_description")!)}` : ""
      }`,
      "Nothing was stored. Start again from bough's mcp panel (^p, then a).",
    );
  }
  const code = query.get("code");
  const state = query.get("state");
  if (!code || !state) {
    return page(
      400,
      "That link is not a bough callback",
      "The authorization server did not send a <code>code</code> and <code>state</code>.",
      "Start the flow from bough's mcp panel (^p, then a) and open the URL it prints.",
    );
  }
  try {
    const server = await completeAuth(state, code);
    return page(
      200,
      `Connected to ${escapeHtml(server)}`,
      "bough stored the tokens for this server. You can close this tab.",
      "Its tools appear in the next turn's catalog.",
    );
  } catch (e) {
    // Deliberately a page, not a throw: the router's catch would answer JSON, and
    // this response is being read by a person in a browser.
    return page(
      e instanceof McpError ? e.status : 502,
      "Authorization did not complete",
      escapeHtml(e instanceof Error ? e.message : String(e)),
      "Nothing was stored. Start again from bough's mcp panel (^p, then a).",
    );
  }
}

function page(status: number, title: string, detail: string, footer: string): Response {
  return new Response(
    `<!doctype html><meta charset="utf-8"><title>bough — ${title}</title>` +
      `<style>` +
      `body{font:15px/1.6 ui-sans-serif,system-ui,sans-serif;margin:0;padding:12vh 6vw;` +
      `background:#111;color:#eee}` +
      `@media(prefers-color-scheme:light){body{background:#fff;color:#111}}` +
      `h1{font-size:1.25rem;margin:0 0 .5rem}p{margin:.25rem 0;opacity:.85}` +
      `code{font-family:ui-monospace,monospace;opacity:1}` +
      `</style>` +
      `<h1>${title}</h1><p>${detail}</p><p>${footer}</p>\n`,
    { status, headers: { "content-type": "text/html; charset=utf-8" } },
  );
}

function escapeHtml(text: string): string {
  return text.replace(
    /[&<>"']/g,
    (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]!,
  );
}
