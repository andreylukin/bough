# Port spec: `src/mcp/` → Rust

Source files (all under `src/mcp/`): `client.ts` (658), `config.ts` (649),
`keychain.ts` (557), `oauth.ts` (821), `remote.ts` (592), `manager.ts` (722), `service.ts` (107),
`status.ts` (442). Consumers: `cli/mcp.ts`, `cli/sync_mcp.ts`, `server/app.ts`, `server/main.ts`,
`tui/api.ts`, `prompt/assemble.ts` (type-only).

---

## 1. Purpose & invariants

MCP support = a **registry** of server definitions (stdio subprocess or remote Streamable HTTP),
**grants** (per-session or global activations, TTL-able), **connections** managed per (session,
server) for stdio and process-wide for remote, **OAuth** for remote servers (DCR + PKCE, tokens on
disk), **keychain credential borrowing** from Claude Code, and one **status builder** that the CLI
(`bough mcp`), TUI panel (`^p`), HTTP API and prompt catalog all read.

Invariant comments, verbatim (each opens its module and each is load-bearing):

- `client.ts`: "THE INVARIANT THIS HOLDS: **a server that does not work fails, by name, in bounded
  time.** Never a hang (plan T7.1)." Every path out terminates: missing binary fails at spawn naming
  the command; a process that never answers `initialize` fails on the connect deadline with stderr
  attached; a process that dies fails everything in flight immediately from its exit handler; every
  request carries its own deadline; `tools/list` pagination is bounded; a server-initiated request
  (sampling, roots, elicitation) is REFUSED with a JSON-RPC error rather than ignored.
- `config.ts`: "THE INVARIANT THIS HOLDS: **being registered grants nothing.**" Plus three derived
  properties: "**Grants expire, and a lapsed one fails CLOSED.**" — "**Secrets live in the
  environment, not in this file.**" (a missing `${VAR}` THROWS rather than expanding to empty) —
  "**MCP state is never cached** (plan §6.13)": every call re-reads the file.
- `keychain.ts`: "**the registry stores a REFERENCE, never a secret.**" — "**`security` is executed
  as ARGV, never through a shell.**" — "**an expired token is reported, not refreshed.**" (refreshing
  another client's token "is impersonation rather than plumbing").
- `oauth.ts`: "THE INVARIANT THIS HOLDS: **an unauthorized server is a QUESTION, never a failure and
  never a hang.**" — "**bough is a PUBLIC client and hosts its own redirect.**"
  (`token_endpoint_auth_method: "none"`, PKCE) — "**The provider captures, it does not redirect.**"
  — "**Credentials are per server, private, and outside the model's reach.**" (`~/.bough/mcp/tokens/
  <server>.json`, dir 0700, file 0600) — "**The `state` round-trip binds a callback to the server
  that started it.**"
- `remote.ts`: same as stdio "with one addition …: **a server that is merely UNAUTHORIZED fails as a
  question, not as a fault.**" — "**Every HTTP request is bounded**" (except the long-lived SSE GET,
  which carries only the connection abort) — "**A 401 is remembered even when the auth flow fails
  afterwards.**" — "**Refresh is the transport's job, not ours.**" — "**The JSON-RPC channel goes
  DIRECT**" (no proxy, no sandbox).
- `manager.ts`: "THE INVARIANT THIS HOLDS: **a turn may call exactly the servers a human granted it
  — and a subagent doing part of that granted work may call the same set, and nothing else.**" —
  "NOTHING HERE IS CACHED" — "A SERVER THAT DOES NOT WORK IS A NAMED STATUS, NEVER A HANG" — "A
  STDIO CONNECTION IS PER (SESSION, SERVER); A REMOTE ONE IS SHARED."
- `service.ts`: MCP as a SERVICE — "the process connects granted remote servers and keeps them
  connected, independently of any conversation." Remote only; "FAILURE IS NORMAL AND SILENT HERE."
- `status.ts`: "THE INVARIANT THIS HOLDS: **there is exactly one builder, and it never serves a
  cached answer.**" — "THE FOUR KEYS ARE FIXED. `{registry, auth, active, connections}` is what
  `prompt/mcp-status.md` promises the model" — "`active` IS THE EFFECTIVE GRANT, NOT THE FILE." —
  "SECRETS NEVER APPEAR HERE."

Error philosophy throughout: every failure is an `McpError { status: u16, message }` whose message
names the server, what failed, and the move that resolves it — it reaches the model as a caught
exception and the human as an HTTP body, so message wording is a product surface. Preserve the exact
sentences where practical; they are asserted in tests.

## 2. Public API

### client.ts
- `interface McpConnection` — the transport-agnostic surface both clients satisfy: `name: string`
  (readonly), `listTools() -> Vec<McpToolInfo>`, `callTool(name, args) -> McpCallResult`,
  `close()` (idempotent, never throws), `alive: bool`, `stderrTail: String` (stdio stderr / remote
  last transport error).
- `interface McpToolInfo { name, description?, inputSchema?: {properties?, required?}, annotations? }`
  — annotations are server-supplied and untrusted ("they may SEED a classification, they never grant one").
- `interface McpCallResult { content?: [{type, text?}], structuredContent?, isError? }` — `isError`
  means the tool failed; that is DATA, not a transport failure.
- `interface McpServerInfo { name?, version?, protocolVersion }` — from the handshake, for `/mcp` status.
- `interface McpTimeouts { connectMs?, requestMs?, callMs? }` — defaults 30 000 / 30 000 / 300 000.
  Injected so tests assert no-hang in milliseconds.
- `interface McpStdioOptions { name?, argv: string[], cwd?, env: Record<string,string>, timeouts? }`
  — `env` is the child's ENTIRE environment (clear-env spawn).
- `class McpStdioClient implements McpConnection` — `static connect(opts) -> McpStdioClient`
  (spawns, runs `initialize` handshake, validates version, sends `notifications/initialized`;
  rejects, never hangs, never resolves half-connected); `serverInfo` getter; `terminate() -> bool`
  (sync SIGTERM, for signal handlers).
- `killAllMcpServers() -> number` — SIGTERM every live client, sync best-effort, returns count.
  Called from the server's shutdown signal handler (`server/main.ts:536`).
- `liveMcpServerCount() -> number`.
- Constants: `KILL_GRACE_MS = 2000`, `STDERR_TAIL_BYTES = 4096`, `STDERR_NOTE_BYTES = 500`
  (stderr excerpt in error text), `MAX_TOOL_PAGES = 50`.

### config.ts
- `ServerConfig` (zod schema + type) — see §3. `StdioServerConfig`, `isStdio(server) -> bool`
  (true iff `command` is a non-empty string).
- `interface Registry { servers: Record<string, ServerConfig> }` — definitions only, never grants.
- `type EnvLookup = (name) -> string | undefined`.
- `interface McpConfigOptions { file?, env? }` — injected store path + env source (DI ground rule:
  tests get hermetic stores, no env mutation).
- `registryFile(opts) -> string` — `opts.file ?? mcpRegistryPath()` (= `~/.bough/mcp.json`).
- `loadRegistry(opts) -> Registry` — missing/corrupt file ⇒ `{servers:{}}` (fail closed, "a
  half-parsed registry … would be the worst outcome").
- `getServer(name, opts) -> ServerConfig | undefined`.
- `requireServer(name, opts) -> ServerConfig` — 404 naming the registered alternatives sorted, and
  "Register one with PUT /mcp/servers/<name>".
- `saveRegistry(raw, opts) -> Registry` — validates `{servers}`, preserves activations (merged back
  deliberately), prunes activations naming now-absent servers.
- `upsertServer(name, raw, opts) -> Registry` — validates name against slug regex, replaces one
  entry (never merges), 400 with readable zod issues (`path: message; …`) on bad shape.
- `removeServer(name, opts) -> bool` — false when absent; also drops the server's activations
  ("a revoked-then-recreated server should start ungranted").
- `expandEnv(env, opts) -> Record<string,string>` — substitutes `${VAR}` (regex `\$\{(\w+)\}`,
  global, inside larger strings); missing var throws 400 naming key and var.
- `expandHeaders(headers, opts) -> async Record<string,string>` — per value: whole-value
  `${keychain:…}` ref → `readKeychainRef`; `Bearer ${keychain:…}` (whole trimmed value, case-
  insensitive "Bearer") → `Bearer ` + resolved; else `expandEnv`. Never partial interpolation of
  keychain refs.
- `INHERITED_ENV` — const list: PATH, HOME, TMPDIR, LANG, TZ, SHELL, HTTP(S)_PROXY/NO_PROXY/
  ALL_PROXY upper+lower, NODE_EXTRA_CA_CERTS, SSL_CERT_FILE, DENO_DIR.
- `childEnv(server, opts) -> Record<string,string>` — inherited names present in lookup + expanded
  declared `env`; declared wins on collision.
- `interface ActivationOptions extends McpConfigOptions { now?: number }` (epoch ms).
- `activationsFor(sessionId: string|undefined, opts) -> string[]` — union of global scope `""` and
  the session scope, expired (`Date.parse(expires) <= now`) filtered, deduped, sorted.
- `setActivation(sessionId|undefined, name, on: bool, opts & {expires?}) -> void` — replaces any
  existing grant for the same name in that scope; removes the scope key when its list empties.
- `revokeEverywhere(name, opts) -> void` — removes the name from EVERY scope ("A permission surface
  may not be approximately right").
- `promoteSessionGrants(opts) -> string[]` — one-shot migration: lifts non-TTL, non-expired
  session-scoped grants into the global scope, empties all session scopes, discards TTL'd rows
  (never widen a deliberate limit), returns newly-promoted names for the boot log. No-op when no
  session scopes exist. Called at boot (`server/main.ts:505`).
- `ttlToExpires(ttl, now?) -> string` — `"90m"|"2h"|"7d"` (regex `^(\d+)\s*(m|h|d)$` on trimmed
  input) → absolute ISO expiry; anything else 400. Absolute so a file rewrite cannot restart it.

### keychain.ts
- `type KeychainReader = (service) -> async KeychainResult`.
- `interface KeychainResult { value, code, error, store?: "keychain"|"file" }` — code 0 ok, 44
  "no such item", 128 denied/unreadable, 1 other.
- `securityReader: KeychainReader` — spawns `["security", "find-generic-password", "-s", service,
  "-w"]` (argv, never shell). Spawn failure (no `security` binary) ⇒ code 44 ("this store does not
  have it" — lets the next store answer). No platform gate.
- `claudeConfigDir(env?, home?) -> string` — `$CLAUDE_CONFIG_DIR` (non-blank) else `~/.claude`.
- `credentialsPath(env?, home?) -> string` — `<dir>/.credentials.json`.
- `credentialsFileReader: KeychainReader` — answers ONLY for service `CLAUDE_CODE_ITEM`
  (anything else ⇒ 44: the file is not a general vault); ENOENT ⇒ 44, EACCES ⇒ 128, other ⇒ 1
  with `path: message` in `error`.
- `defaultCredentialReader: KeychainReader` — `readFromStores(service, credentialStores())`.
- `credentialReaderFor(satisfies) -> KeychainReader` — same stores, content-chosen.
- `credentialStores(platform?) -> KeychainReader[]` — darwin: `[security, file]`; else `[file,
  security]`. "ORDER IS BY AUTHORITY, not availability."
- `readFromStores(service, stores, satisfies?) -> KeychainResult` — first store whose value
  satisfies wins; a store that returns bytes NOT satisfying is remembered (`unsatisfying`) and
  returned when nothing satisfies (so the error can name what the item DOES hold); otherwise the
  most-specific failure (`worst`: first non-44 beats 44); fallback `{value:"",code:44,error:"",
  store:"file"}`.
- `holdsPath(value, path) -> bool` — empty path: any bytes; else parse JSON and walk, require a
  non-empty string at the path.
- `interface KeychainOptions { keychain?: KeychainReader }`.
- `interface KeychainRef { service, path: string[] }`.
- `parseKeychainRef(value) -> KeychainRef | null` — regex
  `^\$\{keychain:([^#{}]+?)(?:#([^{}]*))?\}$` on trimmed value; service trimmed and non-empty; path
  split on `.`, segments trimmed, empties dropped.
- `readKeychainRef(ref, opts) -> async string` — injected reader is asked as one store; default
  path uses `readFromStores(…, holdsPath)` predicate. Strips ONE trailing `\r?\n` from the raw
  value (the `security -w` newline "is not part of the secret"). Failures: nonzero/empty ⇒
  `keychainFailure` message (400); non-JSON with path ⇒ "is not JSON … drop the #path"; missing
  string at path ⇒ "has no string at #a.b. It holds: <shape>" (shape only — NEVER a value);
  `expiresAt` beside the token (epoch-ms number or ISO string) in the containing object, past ⇒
  "expired at <iso> … bough does not refresh a credential it did not obtain — open the client that
  owns this item".
- `CLAUDE_CODE_ITEM = "Claude Code-credentials"`; private path `["claudeAiOauth","accessToken"]`.
- `isCoveredHost(url) -> bool` — hostname equals or is a subdomain of `claude.ai` / `anthropic.com`
  (lowercased); bad URL ⇒ false.
- `claudeCodePrefill(url, opts) -> async string | undefined` — covered host only; EVERY failure is
  silent `undefined` (deliberate inversion of the loud-failure rule: "a missing prefill is the
  ordinary state of a machine that never ran Claude Code").
- Key-with-dots walk rule: when a plain segment misses, rejoin remaining segments longest-first and
  look for a literal dotted key (Claude Code stores per-server grants under
  `mcpOAuth."slack|mcp.example.com#a1b2"`); exact key always wins.

### oauth.ts
- `configureOAuthCallback({port})` / `callbackPort()` (fallback `BOUGH_PORT` env else 4321) /
  `callbackUrl()` = `http://127.0.0.1:<port>/mcp/oauth/callback` / `CALLBACK_PATH` const. Set once
  at boot from the actually-bound port (`server/main.ts:559`) — module-level state, deliberately.
- `interface TokenStoreOptions { dir? }`; `defaultTokensDir()` = `~/.bough/mcp/tokens`.
- `class TokenStore { dir; fileFor(server); load(server) -> Stored; write(server, stored);
  patch(server, delta); clear(server) -> bool }` — one JSON file per server, `confine(dir,
  "<name>.json")` (path-traversal guard from `paths.ts`), server name validated against the slug
  regex (400 with "Nothing was read or written" — the name is untrusted browser input via `state`).
  `write` creates dir 0700 (mkdir + explicit chmod, since mkdir mode is umask-masked), file 0600.
  Synchronous on purpose ("a store that cannot lose a write to an interleaving").
  Corrupt/missing file loads as `{}` (fail closed = "not authorized") — but an invalid NAME still
  throws.
- `Stored` (private) — see §3.
- `interface ProviderOptions extends TokenStoreOptions { redirectUrl?, now?, config?:
  McpConfigOptions, prefill? }`.
- `class BoughOAuthProvider implements OAuthClientProvider` — SDK provider. Members:
  - `authorizationUrl?: URL` — captured by `redirectToAuthorization`, never navigated.
  - `redirectUrl` getter; `clientMetadata` getter: `{client_name:"bough", redirect_uris:[cb],
    grant_types:["authorization_code","refresh_token"], response_types:["code"],
    token_endpoint_auth_method:"none"}`.
  - `state() -> "<server>.<nonce>"` — nonce = random UUID, persisted as `state`.
  - `clientInformation()` — stored DCR client WINS; else registry entry's `clientId` (read FRESH per
    call, not cached at construction — "the second press of `a` has to see it") with `clientSecret`
    expanded via `expandEnv` at this moment (schema forces it to be a `${VAR}` reference); else
    `undefined` (SDK proceeds to DCR).
  - `saveClientInformation`, `tokens()` (stored wins over prefill; prefill answers only when nothing
    stored, as `{access_token, token_type:"Bearer"}`), `saveTokens` (derives absolute `expiresAt` =
    now + expires_in*1000; clears `state` and `codeVerifier` — that is what stops a replayed
    callback exchanging the same code twice), `redirectToAuthorization` (capture),
    `saveCodeVerifier`, `codeVerifier()` (missing ⇒ 400 "started by a different process or already
    finished … press a … to start a fresh one"),
  - `invalidateCredentials(scope: "all"|"client"|"tokens"|"verifier"|"discovery")` — "all" clears
    the file; "tokens" also clears `expiresAt`. This is the SDK's InvalidGrantError recovery hook:
    without it an expired refresh token loops instead of degrading to an auth prompt.
  - `saveDiscoveryState` / `discoveryState()` — cached RFC 9728/8414 discovery so reconnect
    re-probes nothing.
- `interface AuthStatus { server, authorized, expired, refreshable, callback }`;
  `authStatus(server, opts)` — authorized = tokens present; expired = `expiresAt <= now`;
  refreshable = refresh_token is a string.
- `hasTokens(server, opts) -> bool`; `clearAuth(server, opts) -> bool` (logout).
- `interface AuthStart { status: "authorized"|"redirect", server, authorizationUrl? }`.
- `AUTH_HTTP_MS = 15_000` — per-request deadline on the flow's HTTP (bounded fetch wrapping the
  injected one; combines `AbortSignal.timeout` with any caller signal).
- `beginAuth(server, serverUrl, opts) -> AuthStart` — drives SDK `auth()`; "AUTHORIZED" ⇒ done
  silently (tokens usable/refreshable); else must have captured a URL or 502.
- `completeAuth(state, code, opts) -> string` (the server name) — split state at LAST `.`; validate
  name; nonce must equal stored `state` BEFORE any network ("a forged or replayed callback costs one
  string comparison"); look up registry `url` (injectable `serverUrlFor`); run `auth()` with
  `authorizationCode`; non-"AUTHORIZED" ⇒ 502.
- `authFailure(server, url, error)` — maps SDK escapes to sentences; special case: message matching
  /does not support dynamic client registration/i ⇒ the create-your-own-app instructions naming
  `clientId`/`clientSecret` and the callback URL ("Nothing was stored").
- `remoteServerUrl(server, opts)` — registry `url`, read fresh.
- `declaredResource(error, tried) -> string | null` — parses
  `Protected resource (\S+) does not match expected (\S+)` out of the SDK error; adopt only if
  SAME ORIGIN and genuinely different href; else null.
- HTTP handlers (routes registered in `server/app.ts`): `authStatusH` (GET
  `/mcp/servers/:name/auth`), `beginAuthH` (POST — on resource-mismatch failure rewrites the
  registry entry's `url` via `upsertServer({...getServer(name), url: advertised})` and retries,
  returning `correctedUrl`; the Linear `/sse` → `/mcp` case), `clearAuthH` (DELETE),
  `oauthCallbackH` (GET `/mcp/oauth/callback` — every outcome is a self-contained readable HTML
  page, never JSON: declined / not-a-bough-callback / success "Connected to <server>" / failure;
  HTML-escapes all interpolated text). `requireRemote(name)` — 404 unregistered, 400 for a stdio
  entry ("has no OAuth").

### remote.ts
- `authPrompt(server) -> string` = `` `not authorized — open the mcp panel (^p) and press a on
  ${server}` `` — exported so catalog, panel and error say identical words. (History: it used to
  say "/mcp auth <name>", a gesture that never existed — the instruction must name a real gesture.)
- `class McpAuthRequiredError extends McpError` — status 401, `authRequired = true` discriminator
  (survives loss of class identity), `server` field.
- `isAuthRequired(error) -> bool` — instance OR `McpError` with `authRequired === true`.
- `interface RemoteConnectOptions extends TokenStoreOptions { name, url, headers?, timeouts?,
  authProvider?: OAuthClientProvider | null, fetchFn?, prefill?: string | null }` — `null`
  provider = no auth at all; `null` prefill = suppress prefill (tests asserting the unauthorized
  path); absent prefill = `claudeCodePrefill(url)`.
- `class McpRemoteClient implements McpConnection` — `static connect(opts)`; `serverInfo`;
  `listTools` / `callTool` identical contracts to stdio; `close()` closes client, transport, then
  aborts (tears down in-flight SSE); `stderrTail` = accumulated transport `onerror` text (tail
  4096, note excerpt 500).
- Bounded fetch: per-request `AbortSignal.timeout(requestMs)` UNLESS the request is the SSE
  subscription (method GET and Accept contains `text/event-stream`), which gets only the
  connection-wide abort; `onStatus` records a 401 seen from the MCP endpoint itself (same origin,
  URL not containing `/.well-known/`) — the token endpoint's 401s are the auth flow's business.
- `mapError` (private): order matters — already-`McpError` passthrough; recorded-401 OR SDK
  `UnauthorizedError` ⇒ `McpAuthRequiredError` (with the underlying detail as a parenthetical when
  it wasn't a plain UnauthorizedError); JSON-RPC `RequestTimeout` code ⇒ 504 "accepted the
  connection but did not answer … up and stuck, or the URL is not an MCP endpoint"; DOMException
  Timeout/Abort ⇒ 504 "cut off before … answered"; else 502 "failed <what>: <detail> … Check `url`".
- Requests go through `client.request` with LENIENT zod shapes, NOT the SDK's `listTools`/
  `callTool` (their closed unions drop tools over schema nits and fail successful calls over one
  unrecognized content block).

### manager.ts
- `IDLE_MS = 30 * 60_000` — idle reap window; reaping is opportunistic on every manager touch
  (`#sweep` at the top of `ensure`/`call`), no background timer.
- `interface SpawnCtx { workspace: string }` — the stdio child's cwd (the session's checkout)
  unless the entry has its own `cwd`.
- `interface ServerCatalog { name, tools, error? }`.
- `type McpConnState = "connected" | "exited" | "failed"` — the old boolean couldn't tell "never
  started" from "started and died", and a failed-to-start server had no row at all.
- `interface ConnStatus { server, sessionId, state, alive, toolCount, tools: string[], lastUsed,
  error?, stderrTail? }` — `tools` is names only (the live "what can I call right now"); an exited
  row carries error "the server process exited; the next call restarts it"; stderrTail last 500.
- `type Connector = (spec: {name, server, spawn, config, timeouts?}) -> Promise<McpConnection>` —
  injected; `defaultConnector` dispatches on `isStdio`: remote ⇒ `McpRemoteClient.connect` with
  `staticHeaders` expanded HERE (never at load); stdio ⇒ `McpStdioClient.connect` with
  `argv = [command, ...args]`, `cwd = server.cwd ?? spawn.workspace`, `env = childEnv(server)`.
- `staticHeaders(name, server, config)` (private but pinned behavior): expand the entry's headers;
  if expansion THROWS and bough holds its own tokens for the server (`hasTokens`, default dir),
  the dead reference is stale baggage and headers are dropped (undefined) so the OAuth provider can
  answer; if bough has no tokens, the throw is the honest failure. (Symptom this fixes: authorize a
  server, watch it succeed, row stays `◐` forever because of a dead `sync-mcp` header.)
- `interface GrantCtx { sessionId, mcpGrant?: string[] }` — "`[]` means 'granted nothing', not
  'unset'".
- `resolveGrant(ctx, opts) -> string[]` — inherited array (even empty; test is `!== undefined`)
  wins outright; else `activationsFor(sessionId)`.
- `bindTurnGrant(ctx, opts) -> ctx` — installs `mcpGrant` as a LIVE getter (re-reads activations
  per access: a revocation is visible to the very next call) plus a non-enumerable
  `Symbol.for("bough.mcp.liveGrant")` marker distinguishing a live top-level grant from an
  inherited snapshot (a spread never carries the marker into a child ctx). Idempotent; never
  overwrites an inherited grant. Subagent spawn copies `ctx.mcpGrant`'s VALUE out — the spawn-time
  snapshot. Called per turn in `server/main.ts:840`.
- `requireGranted(ctx, server, opts)` — 404 (unregistered, naming registered ones) via
  `requireServer`; 403 (registered, ungranted) naming what IS granted here and, depending on
  `isInherited(ctx)`, either "inherited its spawner's grant and cannot widen it — report what you
  could not do rather than retrying" or "A human grants one from /mcp … a program cannot grant
  itself one"; else pass.
- `class McpManager` — maps keyed `"{scope} {server}"` (space-joined): `#conns`, `#connecting`
  (in-flight connects deduped so concurrent callers share one spawn), `#failures` (remembered
  connect failures for status). Options `{config?, now?, connect?, timeouts?, idleMs?}`.
  - `ensure(sessionId, servers, spawn) -> ServerCatalog[]` — connect-or-reuse each; unregistered ⇒
    `error: "not registered — register it with PUT /mcp/servers/<name>"` recorded as failure; a
    connect error yields an `error` entry, never throws ("one broken server must not take the other
    three down").
  - `call(sessionId, server, tool, args, spawn) -> unknown` — connects lazily (no prior `ensure`
    required — the old "not connected for this session" failure described bookkeeping, not anything
    actionable); unknown tool ⇒ 404 listing the advertised names + "Run `bough mcp` for the live
    catalog rather than guessing"; `isError` result THROWS 502 `MCP <server>:<tool> failed: <text
    of text-blocks joined \n, or "the server reported an error with no text">`; success returns
    `structuredContent ?? joined text`.
  - `restart(sessionId, server, spawn?)` — drop + reacquire; no previous conn and no spawn ⇒ 400;
    a reacquire failure returns the recorded failure row rather than throwing when one exists.
  - `statuses(sessionId?) -> ConnStatus[]` — live rows (a shared-scope conn is reported for EVERY
    session filter) then failure rows not shadowed by a live conn; a live conn deletes its stale
    failure; sorted by server name. Never connects, never throws.
  - `drop(sessionId, server)` — tries BOTH the session scope and `SHARED_SCOPE` (caller doesn't
    know which kind the entry is; "a revoke that missed a shared remote connection would leave it
    serving every OTHER conversation").
  - `dropServer(server)` — every scope's connection + failures for one server (registry edit,
    removal, cleared auth).
  - `dropAll()` — shutdown.
  - `#acquire` refreshes `lastUsed` and overwrites `spawn` on reuse ("a later turn's workspace wins
    for the next respawn"). `#connect` lists tools immediately after connect and closes the client
    if listing fails (else a leaked child nobody can reach).
- `SHARED_SCOPE = ""` — the pool scope for remote connections ("cannot collide — session ids are
  UUIDs"); `scopeFor(sessionId, cfg)` = sessionId for stdio, `SHARED_SCOPE` for remote.
- `mcpManager()` / `setMcpManager(next)` — process-wide singleton (turn runner, host fn and HTTP
  must reach the SAME child); tests construct their own.

### service.ts
- `interface ReconcileResult { connected: string[], failed: {name,error}[], closed: string[] }`.
- `interface ServiceDeps { manager?, config?, workspace? }`.
- `reconcileMcp(deps) -> ReconcileResult` — wanted = registered ∩ globally-granted
  (`activationsFor(undefined)`) ∩ remote (`!isStdio`). Drop-first: every SHARED_SCOPE connection
  not in wanted is dropped (revocation must take effect before anything else). Then
  `manager.ensure(SHARED_SCOPE, wanted, {workspace: deps.workspace ?? cwd})`. Idempotent; called at
  boot (`server/main.ts:513`, fire-and-forget) and awaited from the global-enable handler.
- `reconcileSummary(r) -> string | null` — one boot-log line `MCP: connected a, b · c failed
  (reason) · closed d`; null when nothing to say. Failures carry the REASON, not a count.

### status.ts
- `interface McpStatus { registry: Registry, auth: Record<string,{authorized:boolean}>, active:
  string[], connections: ConnStatus[] }` — the four frozen keys. `auth` covers remote (`url`)
  entries only, one boolean each, never a token. `registry.servers[].env` values are the verbatim
  `${VAR}` references.
- `type AuthLookup = (server) -> bool`; `interface McpStatusOptions extends ActivationOptions
  { sessionId?, grant?, manager?, auth? }`.
- `mcpStatusFor(opts) -> McpStatus` — read-only, never connects, never throws; an AuthLookup that
  throws reads as `false` (`safely`). `active` = `resolveGrant({sessionId: opts.sessionId ?? "",
  mcpGrant: opts.grant})` — empty sessionId is the global scope, so a no-session status reports
  exactly the grants every session has.
- HTTP handlers (`Handler` type imported type-only; routes appended in `server/app.ts`):
  - `getMcpServersH` — GET `/mcp/servers[?session=]` → the whole McpStatus. `?session=` is
    validated against the DB (404 otherwise).
  - `putMcpServersH` — PUT `/mcp/servers` whole-registry replace; computes `changed` = names whose
    JSON differs before/after; drops connections for changed names only; response = state +
    `changed`.
  - `putMcpServerH` — PUT `/mcp/servers/:name` one entry, validated by `ServerConfig` (NOT the
    request-schema subset, which would strip `cwd`); drops that server's connections.
  - `deleteMcpServerH` — DELETE; 404 when absent; drops connections.
  - `connectMcpServerH` — POST `/mcp/servers/:name/connect?session=` — the "prove it" step; stdio
    with no session ⇒ 400 ("runs in a conversation's checkout"); remote uses SHARED_SCOPE and
    cwd; a failed connect answers **200** with `{connected:false, error}` ("the request succeeded,
    and 'this server is broken, here is why' is the answer it asked for"); tools returned as
    `{name, description: first line}`.
  - `callMcpToolH` — POST `/mcp/servers/:name/tools/:tool?session=` — this IS the former `mcp()`
    host function; body must be a plain object or empty; `requireGranted({sessionId: sessionId ??
    ""})` enforced here, resolved fresh; stdio needs `?session=`; result
    `{server, tool, result: result ?? null}`. Explicitly not a security boundary: "The grant check
    is what stops a MISTAKE, not an attacker with the user's own shell."
  - `restartMcpServerH` — POST, session required (400 otherwise).
  - `setMcpActivationH(on) -> Handler` — POST `/mcp/servers/:name/enable|disable`; body
    `McpActivationBody = {sessionId: string, ttl?: string}` (`""` = global); enable requires the
    server registered; disable does not. Global disable ⇒ `revokeEverywhere` + `dropServer`;
    session disable ⇒ `setActivation(off)` + `drop`. Global enable ⇒ `setActivation` then AWAITED
    `reconcileMcp().catch(()=>{})` so the panel's response already reflects the connection attempt
    ("granted" and "not connected" on the same row is the state this removes).
- `promptMcpServers(status) -> PromptMcpServer[]` — turn-start catalog, built from `active` (the
  grant), NOT the connections (a connection-built catalog lists nothing on turn one and drops a
  granted server whose child exited). Per granted name: failed conn ⇒ `{name, error}`; conn with
  tools ⇒ `{name, tools: [{name}]}`; else `{name, note: "granted, not connected yet — the first
  \`bough mcp call\` connects it, and \`bough mcp test <name>\` lists its tools without calling
  one"}`. Pure — never connects (prompt assembly is on the turn's critical path).
  `PromptMcpServer { name, tools?, error?, note? }` lives in `prompt/assemble.ts`.

## 3. Data structures

### `~/.bough/mcp.json` (the one registry document, pretty-printed 2-space + trailing newline)
```json
{
  "servers": {
    "<slug>": {
      "command": "npx", "args": ["-y", "..."], "env": {"TOKEN": "${VAR}"}, "cwd": "...",   // stdio
      "url": "https://mcp.example.com/mcp", "headers": {"Authorization": "Bearer ${keychain:...}"},  // remote
      "clientId": "...", "clientSecret": "${VAR}"                                          // pre-registered OAuth client
    }
  },
  "activations": {
    "": [{"name": "slack"}],                                       // global scope
    "<session-uuid>": [{"name": "linear", "expires": "2026-08-05T12:00:00.000Z"}]
  }
}
```
Name regex: `^[a-z0-9][a-z0-9_-]*$` (message: "server names are lowercase slugs (a-z, 0-9, - and _,
starting with a letter or digit)"). `args` defaults `[]`, `env`/`headers` default `{}`.

Cross-field rules (one object with refinements, deliberately NOT a discriminated union so the
failure reports as one sentence):
- exactly one of `command` / `url` ("a server needs exactly one of `command` (stdio) or `url` (remote)");
- remote forbids `args`/`env`/`cwd`; stdio forbids `headers` and `clientId`/`clientSecret`;
- `clientSecret` requires `clientId`; `clientSecret` must match `^\$\{\w+\}$` (a reference, never a
  literal — this file is served over HTTP and rendered in the panel).
- Note: an old `allowWrite` key still loads and is ignored (dropped field).

### `~/.bough/mcp/tokens/<server>.json` (Stored; dir 0700, file 0600)
```json
{
  "client":       { "client_id": "...", "client_secret": "...?" },  // DCR result or pre-registered
  "tokens":       { "access_token": "...", "token_type": "Bearer", "refresh_token": "...?", "expires_in": 3600 },
  "expiresAt":    1754400000000,       // absolute ms, derived from expires_in at save
  "codeVerifier": "...",               // in-flight PKCE; consumed by the exchange
  "state":        "<nonce>",           // in-flight; cleared when tokens land
  "discovery":    { /* SDK OAuthDiscoveryState — cached RFC 9728/8414 metadata */ }
}
```
(`paths.ts` also names a legacy `~/.bough/mcp-auth.json`; the live code uses the per-server dir.)

### Wire (JSON-RPC 2.0, newline-delimited over the stdio pipe)
- Request `{"jsonrpc":"2.0","id":<n>,"method":"...","params":{...}}` — ids are a per-connection
  incrementing integer.
- `initialize` params: `{protocolVersion: <SDK LATEST>, capabilities: {}, clientInfo: {name:
  "bough", version: "0"}}`; result validated strictly (InitializeResult: `protocolVersion`,
  `serverInfo{name,version}`, capabilities); version must be in the SDK's
  SUPPORTED_PROTOCOL_VERSIONS. Then notification `notifications/initialized`.
- `tools/list` params `{cursor?}` → `{tools: [...], nextCursor?}`; `tools/call` params
  `{name, arguments}` → `{content?: [{type, text?, ...}], structuredContent?, isError?}`.
- Server→client REQUEST (has both id and method) is answered
  `{"jsonrpc":"2.0","id":…,"error":{"code":-32601,"message":"bough does not support <method>"}}`.
  Notifications (method, no id) are ignored. Non-JSON stdout lines are skipped silently.
- HTTP status endpoints: `McpStatus` (4 keys, exact names `registry`/`auth`/`active`/`connections`),
  `ConnStatus` field names as in §2, `AuthStatus` `{server, authorized, expired, refreshable,
  callback}`, `AuthStart` `{status, server, authorizationUrl?}`, activation body `{sessionId, ttl?}`.

DB tables: **none.** This subsystem is entirely file-backed (`mcp.json` + token files); it touches
the sessions DB only through `AppCtx` (`ctx.db.getSession`, `ctx.db.getSessionRuntime(...).workspace`)
in the HTTP handlers.

## 4. Behaviors & edge cases (mined from tests + code)

Stdio client (`client.test.ts`):
- Handshake + paginated tools/list + call + close round-trips against a scripted fake server.
- A server that logs junk to stdout still connects (non-JSON lines skipped); but a server whose
  initialize ANSWER isn't a handshake gets "It is probably not an MCP server, or it logs to stdout —
  MCP requires stdout to carry JSON-RPC only."
- Death mid-call fails the in-flight call NOW, by name, with the stderr tail — from the exit
  handler, not the request deadline. Message includes exit code or signal.
- A call never answered fails on its own deadline (504) while the server stays alive.
- Never-handshakes fails on connectMs; exit-during-handshake fails instead of hanging; a spawn of a
  nonexistent binary fails naming the command ("Check `command` in the registry and that it is on
  PATH") — a raw ENOENT reads as a missing FILE.
- Requests after `close()` reject ("is not running, so <method> could not be sent … Reconnect it").
- The per-request timer is CLEARED when the request settles (the old client leaked one armed timer
  per successful call).
- `close()`: fail in-flight ("disconnected while the call was in flight"), end stdin, SIGTERM,
  2s grace, SIGKILL; second `close()` awaits the child's exit rather than racing it.
- A repeated tools/list cursor and >50 pages are both 502s that still report the count gathered.
- Tool parse: SDK-strict first, name-only lenient fallback (an inputSchema missing `type:"object"`
  must not drop a callable tool); an entry with no usable name is dropped entirely.
- Unrecognized `tools/call` result shape ⇒ `{structuredContent: raw}`, never a failure.

Config (`config.test.ts`):
- Empty/absent/corrupt file ⇒ empty registry AND empty grants (never half a catalog).
- Definition ≠ grant; `saveRegistry` preserves grants; `removeServer` revokes the grants it
  orphans; `pruneActivations` also runs on whole-registry save.
- `upsertServer` replaces one entry without touching siblings.
- TTL: `"90m"/"2h"/"7d"` accepted (with internal whitespace `"2 h"` ok), everything else 400.
- Lapsed TTL is filtered at read time against the INJECTED clock (`expires <= now` is expired).
- `promoteSessionGrants`: promotes only non-TTL live session grants, drops TTL'd + expired ones,
  empties every session scope, dedups against existing global, returns only newly-promoted names,
  no-ops (and doesn't rewrite) when there are no session scopes.
- `expandEnv` on missing var: error names the env KEY and the VARIABLE and says "the value is never
  stored in the registry".
- `childEnv`: only INHERITED_ENV names that are actually set, declared env wins.

Keychain (`keychain.test.ts`):
- Ref parsing: service may contain spaces; `#` path optional; dotted path; empty service ⇒ null.
- Dots-in-key walk (longest-first rejoin; exact key wins over rejoined).
- Trailing newline stripped once; NOT part of the secret.
- Failure messages: not-found names BOTH stores (both were tried); denied prompt vs unreadable
  file are distinct; error never contains the secret, only the item's SHAPE ("an object with a, b").
- `expiresAt` (ms or ISO) in the CONTAINING object of the found field, past ⇒ report, never refresh.
- Store selection: both stores tried on every platform; darwin asks keychain first, elsewhere file
  first; the store that has the FIELD wins (`satisfies`), not the first with bytes; a whole-item
  ref (`path=[]`) takes the first store with bytes; when nothing satisfies, the unsatisfying result
  is returned so the error names what the item DOES hold; specific failure (128/1) beats bare 44;
  missing `security` binary reads as an absent store (44), not an error.
- File reader answers for exactly `CLAUDE_CODE_ITEM`, everything else 44.
- Prefill: covered hosts only (exact or subdomain of claude.ai / anthropic.com); every failure
  silent undefined. MEASUREMENT TRAP (memory + comment): over SSH the login keychain is locked, so
  `security` returns nothing — diagnose store questions from the server's own context.

OAuth (`oauth.test.ts`):
- Provider persists registration/tokens/verifier; saving tokens clears state+verifier (ends flow).
- Missing verifier ⇒ restartable 400 message, not a crash.
- `invalidateCredentials` drops exactly its scope; "tokens" clears expiresAt too.
- Public client metadata; redirect = bough's own callback with the boot-configured port.
- Token files: one per server, dir 0700 file 0600; non-slug names never become a path (assert
  before any I/O).
- `completeAuth` validates state before network; last-`.` split (a server name can't contain `.`,
  but the nonce is a UUID with `-`s; state format `<server>.<uuid>`).
- `beginAuth` captures the URL, never navigates; no URL and not authorized ⇒ 502.
- Resource redeclaration: same-origin adopted (registry rewritten, retried, `correctedUrl` in
  response); cross-origin refused; unrelated errors and identical hrefs are not corrections.
- Pre-registered client: static clientId used when nothing registered; unset secret var names
  itself (rather than an opaque 401 at the token endpoint); clientId with no secret = public
  client; a DCR'd client SHADOWS the static one; no clientId ⇒ undefined ⇒ DCR runs.
- Prefill token used only until the server has tokens of its own.
- Callback page: HTML in all outcomes, self-contained, escaped.

Remote (`remote.test.ts`):
- Connect/paginate/call round-trip; static registry headers reach the server.
- 401 ⇒ `McpAuthRequiredError` prompt, not an error — and the prompt SURVIVES an auth flow that
  fails after the 401 (the recorded-401 flag beats the shape of what escaped).
- Expired access token refreshed invisibly inside the transport within one call; expired REFRESH
  token degrades to the same auth prompt (via `invalidateCredentials("tokens")`).
- Accept-and-never-answer fails on deadline (504); unreachable host fails by name and is NOT an
  auth prompt; bad URL refused before anything opens (400); closed connection refuses calls.

Manager (`manager.test.ts`):
- Registered-but-ungranted ⇒ 403 with the grant-yourself refusal; lapsed grant fails closed with
  the injected clock; revoked grant visible to the very NEXT status call; registry edited on disk
  visible to the very next status call (no caching, ever).
- Subagent inheritance: inherited array wins even when empty; ungranted spawner hands nothing (not
  the global scope); `bindTurnGrant` is a live read, never a frozen array, never overwrites an
  inherited grant; the LIVE_GRANT symbol keeps the 403 wording right for top-level vs subagent.
- Failed connect ⇒ named `failed` status row (kept until superseded); died child ⇒ `exited` row and
  the next call respawns it; connections per session for stdio, reused across calls, reaped after
  idleMs on next touch; remote = ONE connection reported to every conversation; `dropServer` closes
  every session's connection; `ensure` isolates one broken server; status carries the live tool
  name catalog; the prompt catalog is the GRANT, not the connections.
- Concurrent acquire shares one in-flight connect promise.
- API round-trip (register → grant → connect → revoke) where every reply is the full state.

Service (`service.test.ts`):
- Granted REMOTE servers connect with no conversation in existence; revoked servers are
  disconnected (drop-first ordering); a failing server is a reported failure, never a throw;
  nothing granted ⇒ quiet no-op, `reconcileSummary` ⇒ null.

Boot wiring (`server/main.ts`): `promoteSessionGrants()` once, log the promoted names;
`void reconcileMcp()` fire-and-forget with `reconcileSummary` to the log; `configureOAuthCallback
({port})` after bind; every turn wraps its ctx in `bindTurnGrant`; shutdown calls
`killAllMcpServers()` synchronously and reports the count.

Things a naive port gets wrong (concentrated list):
1. Expanding `${VAR}`/`${keychain:…}` at load or storing expansions — secrets must only exist in
   the one spawn env / request header, never in the file, HTTP body, status, log, or error text.
2. Caching anything: registry, grants, statuses, tool catalogs. Only live connections persist.
3. Treating `{isError:true}` as transport failure (it's data at the client layer; the MANAGER turns
   it into a thrown 502 with the server's own text).
4. Using strict SDK-equivalent schemas for tools/list and tools/call results.
5. Timing out the SSE subscription GET.
6. Mapping post-401 auth-flow failures as faults instead of auth prompts (the recorded-401 flag).
7. Letting an inherited empty grant fall through to the global scope.
8. Forgetting drop-first ordering in reconcile, or dropping only one scope in `drop`.
9. Expanding-headers failure killing a connection bough could authorize itself (`staticHeaders`).
10. Failing prefill loudly.
11. Keying remote connections per session (double connections + "not connected" lies).
12. `saveRegistry` clobbering activations; `removeServer` leaving grants behind.
13. Trusting the filename from OAuth `state` without slug validation + confinement.
14. Refreshing a keychain-borrowed token, or letting the credentials file answer for arbitrary services.
15. Returning non-200 from `connect` when the server fails to start (it's 200 + `connected:false`).

## 5. Dependencies

Imports (bough modules): `../errors.ts` (`McpError(status, message)`, `NotFoundError`),
`../paths.ts` (`boughPath`, `mcpRegistryPath`, `confine` — lexical containment, NUL-byte reject),
`../types.ts` (`TurnCtx`, `AppCtx` — type-only in status/oauth), `../schema/requests.ts`
(`McpActivationBody`), `../server/http.ts` (`Handler`, type-only), `../prompt/assemble.ts`
(`PromptMcpServer`, type-only).

Imported by: `server/app.ts` (route table: oauth handlers + status handlers), `server/main.ts`
(boot/shutdown wiring listed above), `cli/mcp.ts` (`McpStatus` type; the CLI talks HTTP),
`cli/sync_mcp.ts` (`loadRegistry`, `upsertServer`, `CLAUDE_CODE_ITEM`, `claudeConfigDir`,
`credentialReaderFor` — adopts Claude Code's servers, writes `${keychain:…}` references, never
secrets, never overwrites without `--force`, never grants), `tui/api.ts` (`McpStatus`, `AuthStart`,
`AuthStatus` types), `agents/subagent.ts` (copies `ctx.mcpGrant` value at spawn),
`prompt/assemble.ts` (renders `promptMcpServers` output; section gated on `bash` since tools are
called via `bough mcp call`).

Internal DAG: `client` ← `remote` ← `manager` ← `service` ← `status`; `config` ← everyone;
`keychain` ← `config`(expandHeaders), `remote`(prefill), `sync_mcp`; `oauth` ← `remote`, `manager`
(hasTokens), `status`. No cycles; oauth/status build their own `Response` precisely to avoid an
edge back into `server/`.

## 6. External deps → Rust equivalents

| TS/Bun dependency | Used for | Rust replacement |
|---|---|---|
| `@modelcontextprotocol/sdk` constants (`LATEST_PROTOCOL_VERSION`, `SUPPORTED_PROTOCOL_VERSIONS`) | version negotiation | `rmcp` (official Rust MCP SDK) constants, or pin the version strings (`"2025-06-18"`, `"2025-03-26"`, `"2024-11-05"`) as consts — they rot either way; prefer the crate |
| SDK `InitializeResultSchema` / `ToolSchema` | strict handshake, tool parsing | `serde` structs; strict deserialization for the handshake, a lenient hand-rolled `LooseTool` (`#[serde(default)]` + `serde_json::Value` passthrough) for tools |
| SDK `StreamableHTTPClientTransport` + `Client` | remote transport | `rmcp` client with `streamable-http-client` transport feature — verify it accepts a custom HTTP layer for the bounded fetch + 401 capture; if not, hand-roll the transport on `reqwest` + `eventsource-stream` (it is POST-JSON + optional SSE GET; the hand-rolled stdio client shows the pattern) |
| SDK `auth()` + `OAuthClientProvider` (DCR, PKCE, discovery, refresh) | OAuth flows | `oauth2` crate (PKCE, token exchange, refresh) + hand-rolled RFC 9728/8414 discovery and RFC 7591 DCR over `reqwest` (small: 2 GETs + 1 POST); `rmcp`'s auth support if mature. This is the biggest porting surface — keep bough's provider semantics (stored-wins, prefill, invalidate scopes) as a trait |
| `zod` | schema validation + readable messages | `serde` + `garde`/manual validation; hand-write the cross-field refinement messages verbatim (they are API/UI surface) |
| `Bun.spawn` (clear-env, pipes, `exited`, `signalCode`, kill) | stdio child | `tokio::process::Command` with `.env_clear().envs(composed)`, `.kill_on_drop(false)` (explicit kill discipline), stdin/stdout/stderr piped; `child.wait()` in a task for the exit handler |
| `Bun.FileSink` / `TextDecoderStream` line reader | NDJSON framing | `tokio::io::BufReader::lines()` on stdout; `AsyncWriteExt` on stdin |
| `fetch` + `AbortSignal.timeout/any` | bounded HTTP | `reqwest` with `.timeout(per_request)` + a `CancellationToken` (tokio-util) for the connection-wide abort; SSE request built without the per-request timeout |
| `crypto.randomUUID()` | state nonce, keys | `uuid` v4 |
| `node:fs` sync read/write/chmod/rm, `mkdirSync` | registry + token store | `std::fs` (keep it synchronous — the TS code is deliberately sync for write-interleaving safety); `std::os::unix::fs::PermissionsExt` for 0700/0600 |
| `process.env`, `process.platform`, `homedir()` | env lookup, store order | `std::env::var`, `cfg!(target_os = "macos")` / runtime `std::env::consts::OS`, `dirs::home_dir()` |
| `security` CLI subprocess | keychain read | keep the subprocess (`tokio::process::Command`, argv only) — do NOT switch to `security-framework` crate in v1; the exit-code contract (44/128) and the "missing binary = absent store" behavior are what the fallback logic is written against |
| `URL` | origin/host checks | `url` crate |
| `Response` (HTTP handlers) | route bodies | whatever the ported server uses (axum): `Json<T>` / `Html<String>` responses |
| `Object.defineProperty` live getter + `Symbol.for` | `bindTurnGrant` | does not translate — model the grant as an enum on the Rust `TurnCtx`: `Grant::Live { session_id }` (resolved by calling `activations_for` at each use) vs `Grant::Inherited(Vec<String>)`. The enum IS the LIVE_GRANT marker |

## 7. Suggested Rust layout

```
crates/bough-mcp/
  src/lib.rs
  src/error.rs        // McpError { status: u16, message: String } or reuse workspace errors crate
  src/config.rs       // ServerConfig, Registry, registry file I/O, activations, TTL, env expansion
  src/keychain.rs     // KeychainReader trait, security/file readers, readFromStores, refs, prefill
  src/client.rs       // McpConnection trait + stdio client (tokio::process, NDJSON JSON-RPC)
  src/oauth/
    mod.rs            // provider trait impl, AuthStatus, beginAuth/completeAuth, declaredResource
    store.rs          // TokenStore (sync fs, 0700/0600, confine)
    flow.rs           // discovery + DCR + PKCE + token exchange (the SDK-auth() replacement)
  src/remote.rs       // remote client, bounded fetch, 401 capture, McpAuthRequiredError, mapError
  src/manager.rs      // McpManager, Connector trait, grants (resolve/require), scopes, statuses
  src/service.rs      // reconcile_mcp, reconcile_summary
  src/status.rs       // McpStatus builder + promptMcpServers (pure); HTTP handlers live in the
                      // server crate but call only functions from here
```

Traits & boundaries:
- `trait McpConnection: Send + Sync` with `async fn list_tools`, `async fn call_tool`,
  `async fn close`, `fn alive`, `fn diag_tail`, `fn name` — object-safe (`#[async_trait]` or RPITIT
  + `Box<dyn>`), so `Connector` = `Arc<dyn Fn(...) -> BoxFuture<Result<Box<dyn McpConnection>>>>`
  stays injectable for tests exactly as the TS `Connector` is.
- `trait KeychainReader` (or `Box<dyn Fn>`), `trait AuthLookup`, `EnvLookup` as
  `Arc<dyn Fn(&str) -> Option<String>>` — every injection point in the TS maps to a field on an
  options struct with a default; tests depend on hermetic stores/clocks/env.
- Clocks: `now: Arc<dyn Fn() -> i64>` or a `Clock` trait — the TTL/idle/expiry tests all inject it.
- `McpManager` state behind `tokio::sync::Mutex` (or `DashMap` + per-key `OnceCell` for the
  in-flight connect dedup); the `#connecting` map becomes a `HashMap<Key, Shared<BoxFuture<…>>>` so
  concurrent callers await one connect. Manager is a process singleton (`OnceLock<Arc<McpManager>>`)
  with a swap hook for tests/boot.
- Async boundaries (tokio): stdio client spawns two tasks (stdout read loop dispatching to a
  `HashMap<u64, oneshot::Sender>` pending map; stderr tail accumulator) plus one exit-watch task
  that fails all pending on child exit; per-request deadline = `tokio::time::timeout` around the
  oneshot (the "timer cleared on settle" bug is free with timeout). `kill_all_mcp_servers` must be
  callable from a sync shutdown path: keep a global registry of child PIDs and send SIGTERM via
  `nix::sys::signal` synchronously.
- Registry/token-store I/O stays blocking `std::fs` (files are tiny; call sites inside async fns
  can use it directly or via `spawn_blocking` if it ever shows up in traces — the TS is sync here
  on purpose).
- Grant inheritance: replace the getter+symbol trick with
  `enum McpGrant { Live { session_id: String }, Inherited(Vec<String>) }` on `TurnCtx`; subagent
  spawn converts Live → Inherited(resolve_now()). `resolve_grant` matches the enum;
  `require_granted`'s 403 wording branches on the variant.
- Error messages: keep the exact sentences (tests assert substrings; the model is trained on them
  by the prompt). Centralize them as `format!` sites, not a message catalog.

## 8. v1 scope cut

Must ship for a working agent loop (core):
- `config.rs` complete (registry, grants, TTL, expandEnv, childEnv, INHERITED_ENV) — everything
  reads through it.
- `client.rs` stdio client with the full no-hang contract + `kill_all_mcp_servers`.
- `manager.rs`: acquire/call/statuses/drop/sweep, grant resolution + `require_granted`, grant
  inheritance enum, `defaultConnector` (stdio arm).
- `status.rs`: `mcpStatusFor` + `promptMcpServers` + GET/PUT/DELETE servers, enable/disable,
  connect, call-tool handlers (the call route is how ALL tools are invoked — it replaced the host
  function).

Stub in v1:
- **OAuth flow internals** (`oauth/flow.rs`): stub `beginAuth` to return a 502 "OAuth not yet
  ported" AND keep `TokenStore` + `hasTokens`/`authStatus`/`clearAuth` real — existing token files
  written by the TS version then keep working for remote servers whose access+refresh tokens are
  valid, IF the remote transport ships. Alternatively stub `authorized:false` everywhere and defer.
- **`declaredResource` URL-correction retry** in `beginAuthH` — pure convenience; port with OAuth.
- **`promoteSessionGrants`** — a one-shot migration already run on the user's machines; stub as
  no-op returning `[]`.

Can follow shortly after v1 (high, not core):
- `remote.rs` + minimal token presentation (read stored/prefilled bearer, no refresh): remote
  servers matter to this user (Slack/Linear via sync-mcp), but the agent loop runs without them —
  a remote entry can degrade to a `failed` status row with an honest "remote MCP not ported yet".
- `keychain.rs`: needed the moment remote+headers ship (`${keychain:…}` refs are in the user's real
  registry via sync-mcp). Until then `expandHeaders` can hard-fail on keychain refs with a clear
  message.
- `service.rs` reconcile — meaningless until remote lands; stub returning empty result.
- `oauthCallbackH` HTML page + full `auth()` replacement (discovery/DCR/PKCE/refresh) — port last;
  it is self-contained and the token files give a bridge.

Droppable initially without breaking anything: `restartMcpServerH`, `PUT /mcp/servers` bulk
replace (keep per-name PUT), `reconcileSummary`, `liveMcpServerCount`.
