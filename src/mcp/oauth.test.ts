/**
 * Tests for the OAuth provider, the credential store, and the callback bough hosts.
 *
 * Two properties carry the weight here:
 *
 *   - **the callback belongs to exactly one flow.** The `state` round-trip is
 *     checked before anything touches the network, so a forged or replayed callback
 *     cannot graft tokens onto a server it named;
 *   - **credentials are private and per server.** Directory 0700, file 0600, one
 *     file per server, and a name that is not a slug never becomes a path.
 *
 * The rest is the flow itself, driven end to end against a loopback authorization
 * server: dynamic registration, PKCE, the code exchange through the real HTTP
 * handler, and the route table entry that makes the redirect URI resolvable.
 *
 * Hermetic: loopback only, no real `~/.bough` (the store is injected, or
 * `BOUGH_HOME` is relocated), no API keys, no outbound network.
 */
import { test } from "bun:test";
import assert from "node:assert/strict";
import { mkdtempSync, statSync, writeFileSync} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { McpError } from "../errors.ts";
import { saveRegistry } from "./config.ts";
import { mcpRegistryPath } from "../paths.ts";
import { routes } from "../server/app.ts";
import type { AppCtx } from "../types.ts";
import {
  authStatus,
  authStatusH,
  beginAuth,
  beginAuthH,
  BoughOAuthProvider,
  CALLBACK_PATH,
  callbackUrl,
  clearAuth,
  clearAuthH,
  completeAuth,
  configureOAuthCallback,
  defaultTokensDir,
  hasTokens,
  oauthCallbackH,
  TokenStore,
  declaredResource,
} from "./oauth.ts";

/** A throwaway store. Nothing under the real `~/.bough` is read or written. */
function tempStore(): TokenStore {
  return new TokenStore({ dir: mkdtempSync(join(tmpdir(), "bough-oauth-")) });
}

/**
 * Run `body` with `BOUGH_HOME` at a fresh temp root, then put the environment
 * back. Used only by the tests that drive the HTTP handlers, which read the
 * process-default registry and store by design.
 */
async function withHome(body: (home: string) => Promise<void>): Promise<void> {
  const home = mkdtempSync(join(tmpdir(), "bough-oauth-home-"));
  const prior = process.env.BOUGH_HOME;
  process.env.BOUGH_HOME = home;
  try {
    await body(home);
  } finally {
    if (prior === undefined) delete process.env.BOUGH_HOME;
    else process.env.BOUGH_HOME = prior;
  }
}

/** A loopback authorization server: discovery, registration, token exchange. */
function startAuthServer(codes: Record<string, string> = { "the-code": "granted-1" }) {
  const seen: { verifiers: (string | null)[]; grants: string[] } = { verifiers: [], grants: [] };
  let base = "";
  const json = (body: unknown, status = 200) =>
    new Response(JSON.stringify(body), {
      status,
      headers: { "content-type": "application/json" },
    });
  const handler = async (req: Request) => {
    const { pathname } = new URL(req.url);
    if (pathname.startsWith("/.well-known/oauth-protected-resource")) {
      return json({ resource: `${base}/mcp`, authorization_servers: [base] });
    }
    if (pathname.startsWith("/.well-known/")) {
      return json({
        issuer: base,
        authorization_endpoint: `${base}/authorize`,
        token_endpoint: `${base}/token`,
        registration_endpoint: `${base}/register`,
        response_types_supported: ["code"],
        code_challenge_methods_supported: ["S256"],
        token_endpoint_auth_methods_supported: ["none"],
      });
    }
    if (pathname === "/register") {
      const metadata = await req.json() as { redirect_uris?: string[] };
      return json({
        client_id: "dyn-client",
        redirect_uris: metadata.redirect_uris ?? [],
        token_endpoint_auth_method: "none",
      }, 201);
    }
    if (pathname === "/token") {
      const form = new URLSearchParams(await req.text());
      seen.grants.push(form.get("grant_type") ?? "");
      seen.verifiers.push(form.get("code_verifier"));
      const minted = codes[form.get("code") ?? ""];
      if (!minted) return json({ error: "invalid_grant" }, 400);
      return json({ access_token: minted, token_type: "Bearer", expires_in: 3600 });
    }
    return new Response("not found", { status: 404 });
  };
  const server = Bun.serve({ port: 0, fetch: handler });
  base = `http://127.0.0.1:${server.port}`;
  return { base, mcpUrl: () => `${base}/mcp`, seen, close: () => server.stop() };
}

// ---------------------------------------------------------------------------
// The store and the provider
// ---------------------------------------------------------------------------

test("the provider persists registration, tokens and verifier; tokens end the flow", () => {
  const store = tempStore();
  const provider = new BoughOAuthProvider("notion", { dir: store.dir, now: () => 1_000 });

  assert.equal(provider.clientInformation(), undefined);
  assert.equal(hasTokens("notion", { dir: store.dir }), false);

  provider.saveClientInformation({ client_id: "abc" });
  provider.saveCodeVerifier("ver1");
  const state = provider.state();
  assert.ok(state.startsWith("notion."));
  assert.deepEqual(provider.clientInformation(), { client_id: "abc" });
  assert.equal(provider.codeVerifier(), "ver1");

  provider.saveTokens({ access_token: "tok", token_type: "Bearer", expires_in: 60 });
  assert.equal(hasTokens("notion", { dir: store.dir }), true);
  assert.equal(provider.tokens()?.access_token, "tok");
  // The registration survives; the in-flight nonce and verifier do not — that is
  // what stops a replayed callback from exchanging the same code twice.
  assert.deepEqual(provider.clientInformation(), { client_id: "abc" });
  assert.equal(store.load("notion").state, undefined);
  assert.equal(store.load("notion").codeVerifier, undefined);
  assert.equal(store.load("notion").expiresAt, 1_000 + 60_000);

  // Expiry is reported, not acted on: the transport refreshes, this is display.
  const status = authStatus("notion", { dir: store.dir, now: () => 2_000_000 });
  assert.equal(status.authorized, true);
  assert.equal(status.expired, true);
  assert.equal(status.refreshable, false);

  assert.equal(clearAuth("notion", { dir: store.dir }), true);
  assert.equal(hasTokens("notion", { dir: store.dir }), false);
});

test("a missing verifier is a restartable message, not a crash", () => {
  const store = tempStore();
  const provider = new BoughOAuthProvider("notion", { dir: store.dir });
  assert.throws(
    () => provider.codeVerifier(),
    (error: unknown) =>
      error instanceof McpError && error.message.includes("press a on notion"),
  );
});

test("invalidateCredentials drops exactly its scope", () => {
  const store = tempStore();
  const provider = new BoughOAuthProvider("linear", { dir: store.dir });
  const seed = () =>
    store.write("linear", {
      client: { client_id: "c" },
      tokens: { access_token: "t", token_type: "Bearer" },
      codeVerifier: "v",
      discovery: { authorizationServerUrl: "http://as.invalid" },
    });

  seed();
  provider.invalidateCredentials("tokens");
  assert.equal(store.load("linear").tokens, undefined);
  // The registration is the expensive half — re-registering on every rejected
  // refresh leaves a trail of dead clients on the authorization server.
  assert.deepEqual(store.load("linear").client, { client_id: "c" });
  assert.equal(store.load("linear").codeVerifier, "v");

  seed();
  provider.invalidateCredentials("discovery");
  assert.equal(store.load("linear").discovery, undefined);
  assert.ok(store.load("linear").tokens);

  seed();
  provider.invalidateCredentials("all");
  assert.deepEqual(store.load("linear"), {});
});

test("the provider is a public client whose redirect is bough's own callback", () => {
  const prior = process.env.BOUGH_PORT;
  process.env.BOUGH_PORT = "9999";
  try {
    assert.equal(callbackUrl(), "http://127.0.0.1:9999/mcp/oauth/callback");
    // Boot wiring wins over the environment: the redirect URI has to name the port
    // the listener actually bound, or the browser comes back to nothing.
    configureOAuthCallback({ port: 4444 });
    assert.equal(callbackUrl(), "http://127.0.0.1:4444/mcp/oauth/callback");

    const provider = new BoughOAuthProvider("x", { dir: tempStore().dir });
    assert.equal(provider.clientMetadata.token_endpoint_auth_method, "none");
    assert.deepEqual(provider.clientMetadata.redirect_uris, [
      "http://127.0.0.1:4444/mcp/oauth/callback",
    ]);
    assert.equal(provider.redirectUrl, "http://127.0.0.1:4444/mcp/oauth/callback");
  } finally {
    configureOAuthCallback({ port: Number(prior ?? 4321) });
    if (prior === undefined) delete process.env.BOUGH_PORT;
    else process.env.BOUGH_PORT = prior;
  }
});

test("token files are private, one per server, under ~/.bough/mcp/tokens", async () => {
  await withHome(async (home) => {
    assert.equal(defaultTokensDir(), `${home}/mcp/tokens`);
    new BoughOAuthProvider("sec").saveTokens({ access_token: "t", token_type: "Bearer" });
    const file = `${home}/mcp/tokens/sec.json`;
    assert.equal(statSync(file).mode! & 0o777, 0o600);
    assert.equal(statSync(`${home}/mcp/tokens`).mode! & 0o777, 0o700);
    await Promise.resolve();
  });
});

test("a server name that is not a slug never becomes a path", () => {
  const store = tempStore();
  for (const name of ["../evil", "a/b", "Notion", ""]) {
    assert.throws(
      () => store.load(name),
      (error: unknown) => error instanceof McpError && error.status === 400,
      `expected ${JSON.stringify(name)} to be refused`,
    );
  }
});

// ---------------------------------------------------------------------------
// The flow
// ---------------------------------------------------------------------------

test("completeAuth validates the state round-trip before touching the network", async () => {
  const store = tempStore();
  const never = () => {
    throw new Error("the network must not be touched before the state check");
  };
  const opts = { dir: store.dir, fetchFn: never, serverUrlFor: () => "http://as.invalid/mcp" };

  await assert.rejects(
    () => completeAuth("nodot", "c", opts),
    (error: unknown) => error instanceof McpError && error.message.includes("malformed state"),
  );
  // Nothing stored for this server at all.
  await assert.rejects(
    () => completeAuth("notion.deadbeef", "c", opts),
    (error: unknown) => error instanceof McpError && error.message.includes("state mismatch"),
  );
  // A flow is in progress, but this is not its nonce.
  new BoughOAuthProvider("notion", { dir: store.dir }).state();
  await assert.rejects(
    () => completeAuth("notion.wrong", "c", opts),
    (error: unknown) => error instanceof McpError && error.message.includes("state mismatch"),
  );
  // The nonce matches, but the server is no longer a registered remote.
  const state = new BoughOAuthProvider("notion", { dir: store.dir }).state();
  await assert.rejects(
    () => completeAuth(state, "c", { ...opts, serverUrlFor: () => undefined }),
    (error: unknown) => error instanceof McpError && error.status === 404,
  );
});

test("beginAuth captures the authorization URL instead of navigating", async () => {
  const as = startAuthServer();
  const store = tempStore();
  const provider = new BoughOAuthProvider("acme", {
    dir: store.dir,
    redirectUrl: "http://127.0.0.1:4321/mcp/oauth/callback",
  });
  try {
    const started = await beginAuth("acme", as.mcpUrl(), { dir: store.dir, provider });
    assert.equal(started.status, "redirect");
    assert.equal(started.server, "acme");
    const url = new URL(started.authorizationUrl!);
    assert.equal(url.origin, as.base);
    assert.equal(url.pathname, "/authorize");
    assert.equal(url.searchParams.get("client_id"), "dyn-client");
    assert.equal(url.searchParams.get("code_challenge_method"), "S256");
    assert.equal(
      url.searchParams.get("redirect_uri"),
      "http://127.0.0.1:4321/mcp/oauth/callback",
    );
    // The nonce in the URL is the one that was stored, and it names the server.
    const state = url.searchParams.get("state")!;
    assert.equal(state, `acme.${store.load("acme").state}`);
    // A verifier is waiting for the callback — the flow is genuinely half-done.
    assert.ok(store.load("acme").codeVerifier);
  } finally {
    await as.close();
  }
});

// ---------------------------------------------------------------------------
// The HTTP surface
// ---------------------------------------------------------------------------

const CTX = {} as AppCtx; // these handlers read nothing off the ctx

test("the callback route is in the table at the path the redirect URI names", () => {
  assert.equal(CALLBACK_PATH, "/mcp/oauth/callback");
  const has = (method: string, path: string) =>
    routes.some((r) => r.method === method && r.pattern.exec({ pathname: path }) !== null);
  assert.ok(has("GET", "/mcp/oauth/callback"), "the browser must land on a real route");
  assert.ok(has("GET", "/mcp/servers/notion/auth"));
  assert.ok(has("POST", "/mcp/servers/notion/auth"));
  assert.ok(has("DELETE", "/mcp/servers/notion/auth"));
});

test("the callback refuses a request that is not a bough callback", async () => {
  const missing = await oauthCallbackH(new Request("http://127.0.0.1/mcp/oauth/callback"));
  assert.equal(missing.status, 400);
  assert.match(missing.headers.get("content-type") ?? "", /text\/html/);
  assert.match(await missing.text(), /not a bough callback/);

  const declined = await oauthCallbackH(
    new Request("http://127.0.0.1/mcp/oauth/callback?error=access_denied"),
  );
  assert.equal(declined.status, 400);
  const body = await declined.text();
  assert.match(body, /declined/);
  assert.match(body, /access_denied/);
});

test("the callback exchanges the code and stores the tokens", async () => {
  await withHome(async (home) => {
    const as = startAuthServer();
    try {
      saveRegistry({ servers: { acme: { url: as.mcpUrl() } } }, { file: mcpRegistryPath() });
      const store = new TokenStore();
      // A flow already begun: registered client, PKCE verifier, nonce.
      store.write("acme", {
        client: { client_id: "dyn-client", redirect_uris: [callbackUrl()] },
        codeVerifier: "verifier-1",
        state: "nonce-1",
      });

      const response = await oauthCallbackH(
        new Request("http://127.0.0.1/mcp/oauth/callback?code=the-code&state=acme.nonce-1"),
      );
      assert.equal(response.status, 200);
      assert.match(await response.text(), /Connected to acme/);

      // The tokens landed, under BOUGH_HOME and nowhere else.
      assert.equal(store.load("acme").tokens?.access_token, "granted-1");
      assert.equal(statSync(`${home}/mcp/tokens/acme.json`).mode! & 0o777, 0o600);
      // PKCE was actually proven, and the flow state is spent.
      assert.deepEqual(as.seen.grants, ["authorization_code"]);
      assert.deepEqual(as.seen.verifiers, ["verifier-1"]);
      assert.equal(store.load("acme").state, undefined);
      assert.equal(store.load("acme").codeVerifier, undefined);

      // Replaying the same callback is refused without a second exchange.
      const replay = await oauthCallbackH(
        new Request("http://127.0.0.1/mcp/oauth/callback?code=the-code&state=acme.nonce-1"),
      );
      assert.equal(replay.status, 400);
      assert.match(await replay.text(), /state mismatch/);
      assert.deepEqual(as.seen.grants, ["authorization_code"]);
    } finally {
      await as.close();
    }
  });
});

test("the /mcp auth verbs: status, start, and forget", async () => {
  await withHome(async () => {
    const as = startAuthServer();
    try {
      saveRegistry({
        servers: { acme: { url: as.mcpUrl() }, local: { command: "echo" } },
      }, { file: mcpRegistryPath() });

      const before = await (authStatusH(
        new Request("http://127.0.0.1/mcp/servers/acme/auth"),
        CTX,
        { name: "acme" },
      )).json();
      assert.equal(before.authorized, false);
      assert.equal(before.callback, callbackUrl());

      const started = await (await beginAuthH(
        new Request("http://127.0.0.1/mcp/servers/acme/auth", { method: "POST" }),
        CTX,
        { name: "acme" },
      )).json();
      assert.equal(started.status, "redirect");
      assert.ok(String(started.authorizationUrl).startsWith(`${as.base}/authorize`));
      // A URL for the human, and never a token in an API response.
      assert.equal(JSON.stringify(started).includes("access_token"), false);

      // A stdio server has no OAuth, and says so instead of half-working. Thrown,
      // not returned: the router's one catch renders it (`server/app.ts`).
      assert.throws(
        () => authStatusH(new Request("http://127.0.0.1/x"), CTX, { name: "local" }),
        (error: unknown) =>
          error instanceof McpError && error.status === 400 &&
          error.message.includes("local stdio server"),
      );
      // An unregistered one is a 404 naming how to register it.
      assert.throws(
        () => authStatusH(new Request("http://127.0.0.1/x"), CTX, { name: "nope" }),
        (error: unknown) => error instanceof McpError && error.status === 404,
      );

      new BoughOAuthProvider("acme").saveTokens({ access_token: "t", token_type: "Bearer" });
      assert.equal(hasTokens("acme"), true);
      const cleared = await (clearAuthH(
        new Request("http://127.0.0.1/x", { method: "DELETE" }),
        CTX,
        { name: "acme" },
      )).json();
      assert.deepEqual(cleared, { server: "acme", cleared: true });
      assert.equal(hasTokens("acme"), false);
    } finally {
      await as.close();
    }
  });
});

// ---- the docs URL is often not the flow's URL --------------------------------
// Linear publishes `https://mcp.linear.app/sse`; that endpoint's RFC9728 metadata
// declares its resource as `https://mcp.linear.app/mcp`, and the SDK refuses the
// mismatch. `beginAuthH` adopts the advertised URL rather than making the user read
// a 502 and edit the registry by hand — but only when it is safe to.

test("an advertised same-origin resource is adopted", () => {
  const err = new Error(
    "Protected resource https://mcp.linear.app/mcp does not match expected " +
      "https://mcp.linear.app/sse (or origin)",
  );
  assert.equal(
    declaredResource(err, "https://mcp.linear.app/sse"),
    "https://mcp.linear.app/mcp",
  );
});

test("a CROSS-ORIGIN redeclaration is refused", () => {
  // Following this would let a server point bough's registry at someone else's
  // endpoint — and the next flow would mint a token for that audience.
  const err = new Error(
    "Protected resource https://evil.example.com/mcp does not match expected " +
      "https://mcp.linear.app/sse (or origin)",
  );
  assert.equal(declaredResource(err, "https://mcp.linear.app/sse"), null);
});

test("an unrelated failure is not mistaken for a redeclaration", () => {
  // Everything else must keep surfacing as itself; only this one shape is retried.
  assert.equal(declaredResource(new Error("fetch failed"), "https://x.example/mcp"), null);
  assert.equal(declaredResource(new Error(""), "https://x.example/mcp"), null);
});

test("a resource identical to what was tried is not a correction", () => {
  // Retrying the same URL would loop.
  const err = new Error(
    "Protected resource https://x.example/mcp does not match expected " +
      "https://x.example/mcp (or origin)",
  );
  assert.equal(declaredResource(err, "https://x.example/mcp"), null);
});

// ---- a pre-registered OAuth client ------------------------------------------
// Dynamic registration is the default path and was the ONLY one: against an
// authorization server publishing `registration_endpoint: null` — Slack's does —
// the SDK went on to register, failed, and the flow died. These pin the fallback
// that lets a user supply an app they made themselves.

/** A registry file plus an env lookup, so nothing here reads the real ones. */
function tempRegistry(
  entry: Record<string, unknown>,
  env: Record<string, string> = {},
): { config: { file: string; env: (n: string) => string | undefined }; dir: string } {
  const file = join(mkdtempSync(join(tmpdir(), "bough-oauth-reg-")), "mcp.json");
  writeFileSync(file, JSON.stringify({ servers: { slack: entry }, activations: {} }));
  return {
    config: { file, env: (n: string) => env[n] },
    dir: mkdtempSync(join(tmpdir(), "bough-oauth-")),
  };
}

test("a static clientId is used when nothing was ever registered", () => {
  const { config, dir } = tempRegistry(
    {
      url: "https://mcp.slack.com/mcp",
      clientId: "1234.5678",
      clientSecret: "${SLACK_MCP_CLIENT_SECRET}",
    },
    { SLACK_MCP_CLIENT_SECRET: "shhh" },
  );
  const provider = new BoughOAuthProvider("slack", { dir, config });
  assert.deepEqual(provider.clientInformation(), {
    client_id: "1234.5678",
    client_secret: "shhh",
  });
});

test("an unset secret variable names itself rather than failing at the token endpoint", () => {
  // Without this the flow reaches the authorization server with an empty secret and
  // comes back as an opaque 401 — a failure that looks like the provider's fault.
  const { config, dir } = tempRegistry({
    url: "https://mcp.slack.com/mcp",
    clientId: "1234.5678",
    clientSecret: "${SLACK_MCP_CLIENT_SECRET}",
  });
  const provider = new BoughOAuthProvider("slack", { dir, config });
  assert.throws(
    () => provider.clientInformation(),
    (e: unknown) => e instanceof McpError && /SLACK_MCP_CLIENT_SECRET/.test(e.message),
  );
});

test("a clientId with no secret is a public pre-registered client", () => {
  const { config, dir } = tempRegistry({
    url: "https://mcp.slack.com/mcp",
    clientId: "1234.5678",
  });
  const provider = new BoughOAuthProvider("slack", { dir, config });
  assert.deepEqual(provider.clientInformation(), { client_id: "1234.5678" });
});

test("a DYNAMICALLY registered client shadows the static one", () => {
  // The registered client is the one the authorization server issued and knows;
  // the static id is only what to fall back on when there was no registration.
  const { config, dir } = tempRegistry(
    { url: "https://mcp.slack.com/mcp", clientId: "static", clientSecret: "${S}" },
    { S: "shhh" },
  );
  new TokenStore({ dir }).patch("slack", { client: { client_id: "registered" } });
  const provider = new BoughOAuthProvider("slack", { dir, config });
  assert.deepEqual(provider.clientInformation(), { client_id: "registered" });
});

test("an entry with no clientId still returns undefined, so DCR runs as before", () => {
  // The guard on the whole change: a server that never asked for this must reach
  // the SDK's registration path untouched.
  const { config, dir } = tempRegistry({ url: "https://mcp.slack.com/mcp" });
  assert.equal(new BoughOAuthProvider("slack", { dir, config }).clientInformation(), undefined);
});

test("a prefilled token is used only until this server has one of its own", () => {
  const store = tempStore();
  const provider = new BoughOAuthProvider("claude-ai", {
    dir: store.dir,
    prefill: "sk-ant-oat01-FROM-KEYCHAIN",
  });

  // Nothing stored: the connection is tried with the credential the machine
  // already has, so a freshly registered server works without anyone pressing `a`.
  assert.deepEqual(provider.tokens(), {
    access_token: "sk-ant-oat01-FROM-KEYCHAIN",
    token_type: "Bearer",
  });
  // …and prefill is NOT authorization: nothing was written to the token store, so
  // the one copy of that secret is still the keychain's.
  assert.equal(hasTokens("claude-ai", { dir: store.dir }), false);

  // Once a real flow completes, what the user authorized WINS. A credential that
  // merely happens to be on the machine must never displace a deliberate one.
  provider.saveTokens({ access_token: "from-oauth", token_type: "Bearer" });
  assert.equal(provider.tokens()?.access_token, "from-oauth");
  assert.equal(hasTokens("claude-ai", { dir: store.dir }), true);
});
