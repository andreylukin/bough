/**
 * Tests for the remote (Streamable HTTP) MCP client, driven against a real
 * loopback fixture that speaks real JSON-RPC and a real OAuth 2.1 flow —
 * RFC 9728 discovery, dynamic client registration, PKCE, refresh grants.
 *
 * The happy path is here (handshake, paginated `tools/list`, a call round-trip),
 * but the tests that matter are the failure ones, because this module exists for
 * how it fails:
 *
 *   - a 401 becomes an AUTHORIZATION PROMPT — "not authorized — /mcp auth <name>" —
 *     carried in the catalog entry as a prompt, not as a fault and not as a hang;
 *   - an EXPIRED REFRESH TOKEN degrades to exactly the same prompt, because the
 *     human's move is the same;
 *   - a refresh that CAN succeed is invisible: the transport swaps the token
 *     mid-request and the caller sees a working connection;
 *   - a server that accepts a connection and never answers fails on a deadline.
 *
 * Every deadline here is in the hundreds of milliseconds, so a regression that
 * reintroduces a hang shows up as a failing test rather than as a suite that never
 * finishes.
 *
 * Hermetic: loopback only, no real `~/.bough` (the token store is injected at a
 * temp dir), no API keys, no outbound network.
 */
import assert from "node:assert/strict";
import { McpError } from "../errors.ts";
import { BoughOAuthProvider, TokenStore } from "./oauth.ts";
import { authPrompt, isAuthRequired, McpAuthRequiredError, McpRemoteClient } from "./remote.ts";

// ---------------------------------------------------------------------------
// The fixture
// ---------------------------------------------------------------------------

interface FixtureOptions {
  /** Bearer the MCP endpoint accepts. Absent = the endpoint needs no auth at all. */
  accept?: string;
  /** Refresh tokens the token endpoint honors, and what each one mints. */
  refresh?: Record<string, { access_token: string; refresh_token?: string }>;
  /** Authorization codes the token endpoint honors. */
  codes?: Record<string, { access_token: string; refresh_token?: string }>;
  /** Accept the POST and never answer it — the hang this module must not have. */
  stall?: boolean;
}

interface Fixture {
  url: string;
  base: string;
  seen: {
    /** Every `authorization` header the MCP endpoint received, in order. */
    bearers: (string | null)[];
    /** Every grant_type the token endpoint was asked for, in order. */
    grants: string[];
    /** How many times a client dynamically registered. */
    registrations: number;
    /** Bodies posted to /register, so the client metadata can be asserted. */
    registered: Record<string, unknown>[];
  };
  close: () => Promise<void>;
}

/**
 * One loopback server playing three roles at once: the MCP resource server, its
 * RFC 9728 metadata, and the authorization server. Real HTTP end to end — the
 * point is to exercise the SDK transport's own auth handling, which a mocked
 * `fetch` would step over.
 */
function startFixture(opts: FixtureOptions = {}): Fixture {
  const seen: Fixture["seen"] = { bearers: [], grants: [], registrations: 0, registered: [] };
  let base = "";
  const stalled: Array<() => void> = [];

  const json = (body: unknown, status = 200) =>
    new Response(JSON.stringify(body), {
      status,
      headers: { "content-type": "application/json" },
    });

  const handler = async (req: Request): Promise<Response> => {
    const { pathname } = new URL(req.url);

    // RFC 9728: which authorization server protects this resource.
    if (pathname.startsWith("/.well-known/oauth-protected-resource")) {
      return json({ resource: `${base}/mcp`, authorization_servers: [base] });
    }
    // RFC 8414 / OIDC discovery: the authorization server's endpoints.
    if (
      pathname.startsWith("/.well-known/oauth-authorization-server") ||
      pathname.startsWith("/.well-known/openid-configuration")
    ) {
      return json({
        issuer: base,
        authorization_endpoint: `${base}/authorize`,
        token_endpoint: `${base}/token`,
        registration_endpoint: `${base}/register`,
        response_types_supported: ["code"],
        grant_types_supported: ["authorization_code", "refresh_token"],
        code_challenge_methods_supported: ["S256"],
        token_endpoint_auth_methods_supported: ["none"],
      });
    }
    if (pathname === "/register" && req.method === "POST") {
      seen.registrations++;
      const metadata = await req.json() as { redirect_uris?: string[] };
      seen.registered.push(metadata);
      return json({
        client_id: "dyn-client",
        redirect_uris: metadata.redirect_uris ?? [],
        token_endpoint_auth_method: "none",
      }, 201);
    }
    if (pathname === "/token" && req.method === "POST") {
      const form = new URLSearchParams(await req.text());
      const grant = form.get("grant_type") ?? "";
      seen.grants.push(grant);
      const minted = grant === "refresh_token"
        ? opts.refresh?.[form.get("refresh_token") ?? ""]
        : opts.codes?.[form.get("code") ?? ""];
      if (!minted) return json({ error: "invalid_grant" }, 400);
      return json({ token_type: "Bearer", expires_in: 3600, ...minted });
    }

    if (pathname !== "/mcp") return new Response("not found", { status: 404 });
    // The transport opens a server→client SSE stream after initializing. This
    // fixture is JSON-response mode, so it declines — an expected, non-error case.
    if (req.method !== "POST") return new Response("method not allowed", { status: 405 });

    const authorization = req.headers.get("authorization");
    seen.bearers.push(authorization);
    if (opts.accept !== undefined && authorization !== `Bearer ${opts.accept}`) {
      return new Response(JSON.stringify({ error: "invalid_token" }), {
        status: 401,
        headers: {
          "content-type": "application/json",
          "www-authenticate":
            `Bearer resource_metadata="${base}/.well-known/oauth-protected-resource/mcp"`,
        },
      });
    }

    const msg = await req.json() as {
      id?: number;
      method?: string;
      params?: { cursor?: string; name?: string; arguments?: Record<string, unknown> };
    };
    if (opts.stall) {
      // Accepted and never answered. Exactly the shape of a hang.
      return await new Promise<Response>((resolve) => {
        stalled.push(() => resolve(new Response(null, { status: 204 })));
      });
    }
    if (msg.id === undefined) return new Response(null, { status: 202 }); // a notification

    const respond = (result: unknown) => json({ jsonrpc: "2.0", id: msg.id, result });

    if (msg.method === "initialize") {
      return respond({
        protocolVersion: "2025-06-18",
        capabilities: { tools: {} },
        serverInfo: { name: "http-fixture", version: "0" },
      });
    }
    if (msg.method === "tools/list") {
      return msg.params?.cursor === "p2"
        ? respond({
          tools: [{
            name: "boom",
            description: "Always fails.",
            inputSchema: { type: "object", properties: {} },
          }],
        })
        : respond({
          tools: [{
            name: "echo",
            description: "Echo the text back.",
            inputSchema: { type: "object", properties: { text: { type: "string" } } },
            annotations: { readOnlyHint: true },
          }],
          nextCursor: "p2",
        });
    }
    if (msg.method === "tools/call") {
      const { name, arguments: args = {} } = msg.params ?? {};
      if (name === "echo") {
        return respond({
          content: [{ type: "text", text: String(args.text) }],
          structuredContent: { echoed: args.text },
        });
      }
      return respond({ content: [{ type: "text", text: "kaboom" }], isError: true });
    }
    return json({ jsonrpc: "2.0", id: msg.id, error: { code: -32601, message: "no such method" } });
  };

  const server = Deno.serve({ port: 0, onListen: () => {} }, handler);
  const { port } = server.addr as Deno.NetAddr;
  base = `http://127.0.0.1:${port}`;
  return {
    url: `${base}/mcp`,
    base,
    seen,
    close: async () => {
      for (const release of stalled.splice(0)) release();
      await server.shutdown();
    },
  };
}

/** A fresh, throwaway token store. Nothing under the real `~/.bough` is touched. */
function tempStore(): TokenStore {
  return new TokenStore({ dir: Deno.makeTempDirSync({ prefix: "bough-mcp-tokens-" }) });
}

/**
 * What the layer above does with a connection attempt: one catalog entry per
 * granted server, an `error` sentence when it did not connect, and `authRequired`
 * when that sentence is a prompt rather than a fault. Restated here rather than
 * imported because the manager is T7.1's file; the shape is the contract this
 * module is written against.
 */
async function catalogEntry(opts: {
  name: string;
  url: string;
  store: TokenStore;
  provider?: BoughOAuthProvider;
}): Promise<{ name: string; tools: string[]; error?: string; authRequired?: boolean }> {
  try {
    const client = await McpRemoteClient.connect({
      name: opts.name,
      url: opts.url,
      dir: opts.store.dir,
      ...(opts.provider ? { authProvider: opts.provider } : {}),
      timeouts: { connectMs: 4_000, requestMs: 2_000 },
    });
    try {
      return { name: opts.name, tools: (await client.listTools()).map((t) => t.name) };
    } finally {
      await client.close();
    }
  } catch (error) {
    return {
      name: opts.name,
      tools: [],
      error: error instanceof Error ? error.message : String(error),
      ...(isAuthRequired(error) ? { authRequired: true } : {}),
    };
  }
}

// ---------------------------------------------------------------------------
// The happy path
// ---------------------------------------------------------------------------

Deno.test("remote client: connects, paginates tools, round-trips a call", async () => {
  const fixture = startFixture();
  const client = await McpRemoteClient.connect({
    name: "fix",
    url: fixture.url,
    authProvider: null, // this fixture needs no auth
    timeouts: { connectMs: 4_000, requestMs: 2_000, callMs: 2_000 },
  });
  try {
    const tools = await client.listTools();
    assert.deepEqual(tools.map((t) => t.name), ["echo", "boom"]);
    assert.equal(tools[0].annotations?.readOnlyHint, true);
    assert.deepEqual(tools[0].inputSchema?.properties, { text: { type: "string" } });
    assert.equal(client.serverInfo?.name, "http-fixture");

    const echoed = await client.callTool("echo", { text: "hi" });
    assert.deepEqual(echoed.structuredContent, { echoed: "hi" });
    // A tool that fails is DATA, not an exception.
    const boom = await client.callTool("boom", {});
    assert.equal(boom.isError, true);
  } finally {
    await client.close();
    await fixture.close();
  }
  assert.equal(client.alive, false);
});

Deno.test("remote client: static registry headers reach the server", async () => {
  const fixture = startFixture({ accept: "static-token" });
  const client = await McpRemoteClient.connect({
    name: "fix",
    url: fixture.url,
    headers: { authorization: "Bearer static-token" },
    authProvider: null,
    timeouts: { connectMs: 4_000, requestMs: 2_000 },
  });
  try {
    assert.deepEqual((await client.listTools()).map((t) => t.name), ["echo", "boom"]);
  } finally {
    await client.close();
    await fixture.close();
  }
});

// ---------------------------------------------------------------------------
// 401 — the authorization prompt
// ---------------------------------------------------------------------------

Deno.test("a 401 surfaces as an authorization prompt in the catalog, not an error", async () => {
  const fixture = startFixture({ accept: "never-issued" });
  const store = tempStore();
  const provider = new BoughOAuthProvider("notion", {
    dir: store.dir,
    redirectUrl: "http://127.0.0.1:4321/mcp/oauth/callback",
  });
  try {
    const entry = await catalogEntry({
      name: "notion",
      url: fixture.url,
      store,
      provider,
    });

    // The catalog entry a turn renders: no tools, one sentence, flagged as a
    // prompt. NOT an exception thrown into the turn, and not a hang.
    assert.deepEqual(entry.tools, []);
    assert.equal(entry.authRequired, true);
    assert.ok(
      entry.error?.includes(authPrompt("notion")),
      `catalog error must carry the prompt, got: ${entry.error}`,
    );
    assert.ok(entry.error?.includes("not authorized — /mcp auth notion"));

    // And the human's next step actually exists: the flow got as far as PKCE, so
    // there is a URL to open and a verifier waiting for the callback.
    const url = provider.authorizationUrl;
    assert.ok(url, "the flow must capture an authorization URL for the human");
    assert.equal(url.searchParams.get("code_challenge_method"), "S256");
    assert.equal(
      url.searchParams.get("redirect_uri"),
      "http://127.0.0.1:4321/mcp/oauth/callback",
    );
    assert.ok(url.searchParams.get("state")?.startsWith("notion."));
    assert.equal(fixture.seen.registrations, 1);
    assert.equal(fixture.seen.registered[0].token_endpoint_auth_method, "none");
  } finally {
    await fixture.close();
  }
});

Deno.test("the 401 prompt survives an auth flow that fails after the 401", async () => {
  // A server that answers 401 but publishes no OAuth metadata at all: discovery
  // and registration both fail, and the error that escapes the SDK is about THAT.
  // It must still read as "nobody has authorized this yet", because that is what
  // the user has to fix.
  const server = Deno.serve({ port: 0, onListen: () => {} }, (req: Request) => {
    if (req.method !== "POST") return new Response(null, { status: 405 });
    return new Response("nope", { status: 401 });
  });
  const { port } = server.addr as Deno.NetAddr;
  const store = tempStore();
  try {
    const error = await McpRemoteClient.connect({
      name: "bare",
      url: `http://127.0.0.1:${port}/mcp`,
      dir: store.dir,
      timeouts: { connectMs: 4_000, requestMs: 1_000 },
    }).then(() => undefined, (e: unknown) => e);

    assert.ok(error instanceof McpAuthRequiredError, `expected an auth prompt, got ${error}`);
    assert.equal(error.status, 401);
    assert.equal(error.server, "bare");
    assert.ok(error.message.includes(authPrompt("bare")));
    assert.ok(isAuthRequired(error));
  } finally {
    await server.shutdown();
  }
});

// ---------------------------------------------------------------------------
// Refresh
// ---------------------------------------------------------------------------

Deno.test("an expired access token is refreshed inside the transport, invisibly", async () => {
  const fixture = startFixture({
    accept: "fresh-1",
    refresh: { "r-good": { access_token: "fresh-1", refresh_token: "r-2" } },
  });
  const store = tempStore();
  // A server authorized in some previous session: registration and a stale pair.
  store.write("linear", {
    client: { client_id: "dyn-client" },
    tokens: { access_token: "stale", token_type: "Bearer", refresh_token: "r-good" },
    expiresAt: 1,
  });

  const client = await McpRemoteClient.connect({
    name: "linear",
    url: fixture.url,
    dir: store.dir,
    timeouts: { connectMs: 4_000, requestMs: 2_000 },
  });
  try {
    // The caller sees a working connection; the 401 and the refresh happened under it.
    assert.deepEqual((await client.listTools()).map((t) => t.name), ["echo", "boom"]);
    assert.deepEqual(
      (await client.callTool("echo", { text: "yo" })).structuredContent,
      { echoed: "yo" },
    );
  } finally {
    await client.close();
    await fixture.close();
  }

  // Exactly one refresh grant, and the new pair is what is persisted — including
  // the rotated refresh token, or the NEXT expiry starts the whole flow over.
  assert.deepEqual(fixture.seen.grants, ["refresh_token"]);
  const stored = store.load("linear");
  assert.equal(stored.tokens?.access_token, "fresh-1");
  assert.equal(stored.tokens?.refresh_token, "r-2");
  // Registration was reused rather than repeated.
  assert.equal(fixture.seen.registrations, 0);
  // The stale token was presented first, the fresh one after — the retry is real.
  assert.deepEqual(fixture.seen.bearers.slice(0, 2), ["Bearer stale", "Bearer fresh-1"]);
});

Deno.test("an expired refresh token degrades to the same authorization prompt", async () => {
  // The token endpoint rejects the refresh with invalid_grant. The SDK drops the
  // tokens and starts a fresh authorization, which is a REDIRECT — so the human
  // gets the same one-command prompt as a server that was never authorized.
  const fixture = startFixture({ accept: "never-issued", refresh: {} });
  const store = tempStore();
  store.write("linear", {
    client: { client_id: "dyn-client" },
    tokens: { access_token: "stale", token_type: "Bearer", refresh_token: "r-dead" },
  });
  const provider = new BoughOAuthProvider("linear", {
    dir: store.dir,
    redirectUrl: "http://127.0.0.1:4321/mcp/oauth/callback",
  });
  try {
    const entry = await catalogEntry({ name: "linear", url: fixture.url, store, provider });
    assert.deepEqual(entry.tools, []);
    assert.equal(entry.authRequired, true);
    assert.ok(entry.error?.includes(authPrompt("linear")), `got: ${entry.error}`);

    // The dead pair is gone, so nothing keeps re-presenting it, and there is a URL
    // for the human to open.
    assert.equal(store.load("linear").tokens, undefined);
    assert.ok(provider.authorizationUrl);
    // The registration survived the token clear — re-registering on every expiry
    // would leave a trail of dead clients on the authorization server.
    assert.deepEqual(store.load("linear").client, { client_id: "dyn-client" });
    assert.deepEqual(fixture.seen.grants, ["refresh_token"]);
  } finally {
    await fixture.close();
  }
});

// ---------------------------------------------------------------------------
// Bounded failure — never a hang
// ---------------------------------------------------------------------------

Deno.test("a server that accepts and never answers fails on the deadline", async () => {
  const fixture = startFixture({ stall: true });
  try {
    const error = await McpRemoteClient.connect({
      name: "wedged",
      url: fixture.url,
      authProvider: null,
      timeouts: { connectMs: 400, requestMs: 300 },
    }).then(() => undefined, (e: unknown) => e);

    assert.ok(error instanceof McpError, `expected an McpError, got ${error}`);
    assert.ok(!isAuthRequired(error), "a wedged server is a fault, not an auth prompt");
    assert.equal(error.status, 504);
    assert.ok(error.message.includes('"wedged"'));
  } finally {
    await fixture.close();
  }
});

Deno.test("an unreachable server fails by name, and is not an auth prompt", async () => {
  const error = await McpRemoteClient.connect({
    name: "dead",
    url: "http://127.0.0.1:1/mcp",
    authProvider: null,
    timeouts: { connectMs: 2_000, requestMs: 1_000 },
  }).then(() => undefined, (e: unknown) => e);

  assert.ok(error instanceof McpError, `expected an McpError, got ${error}`);
  assert.ok(!isAuthRequired(error));
  assert.ok(error.message.includes('"dead"'));
  assert.ok(error.message.includes("http://127.0.0.1:1/mcp"));
});

Deno.test("an unusable url is refused before anything is opened", async () => {
  const error = await McpRemoteClient.connect({
    name: "bad",
    url: "not a url",
    authProvider: null,
  }).then(() => undefined, (e: unknown) => e);

  assert.ok(error instanceof McpError);
  assert.equal(error.status, 400);
  assert.ok(error.message.includes("unusable `url`"));
});

Deno.test("a closed connection refuses further calls instead of hanging", async () => {
  const fixture = startFixture();
  const client = await McpRemoteClient.connect({
    name: "fix",
    url: fixture.url,
    authProvider: null,
    timeouts: { connectMs: 4_000, requestMs: 2_000 },
  });
  await client.close();
  await fixture.close();

  const error = await client.listTools().then(() => undefined, (e: unknown) => e);
  assert.ok(error instanceof McpError);
  assert.ok(error.message.includes("disconnected"));
});
