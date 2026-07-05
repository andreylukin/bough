import { assertEquals, assertRejects, assertStringIncludes } from "jsr:@std/assert@1";
import { BoughOAuthProvider, callbackUrl, clearAuth, completeAuth, hasTokens } from "./oauth.ts";

function withMcpDir(fn: () => void | Promise<void>) {
  const dir = Deno.makeTempDirSync({ prefix: "bough-mcp-oauth-" });
  Deno.env.set("BOUGH_MCP_DIR", dir);
  const done = fn();
  const cleanup = () => Deno.env.delete("BOUGH_MCP_DIR");
  return done instanceof Promise ? done.finally(cleanup) : cleanup();
}

Deno.test("provider persists registration, tokens, verifier; saveTokens clears state", () => {
  withMcpDir(() => {
    const p = new BoughOAuthProvider("notion");
    assertEquals(p.clientInformation(), undefined);
    assertEquals(hasTokens("notion"), false);

    p.saveClientInformation({ client_id: "abc" });
    p.saveCodeVerifier("ver1");
    const state = p.state();
    assertStringIncludes(state, "notion.");

    assertEquals(p.clientInformation(), { client_id: "abc" });
    assertEquals(p.codeVerifier(), "ver1");

    p.saveTokens({ access_token: "tok", token_type: "bearer" });
    assertEquals(hasTokens("notion"), true);
    assertEquals(p.tokens()?.access_token, "tok");
    // registration survives token save; the flow nonce does not
    assertEquals(p.clientInformation(), { client_id: "abc" });

    clearAuth("notion");
    assertEquals(hasTokens("notion"), false);
    assertEquals(p.tokens(), undefined);
  });
});

Deno.test("provider metadata: public client, callback on bough's own port", () => {
  Deno.env.set("BOUGH_PORT", "9999");
  try {
    assertEquals(callbackUrl(), "http://127.0.0.1:9999/mcp/oauth/callback");
    const p = new BoughOAuthProvider("x");
    assertEquals(p.clientMetadata.token_endpoint_auth_method, "none");
    assertEquals(p.clientMetadata.redirect_uris, ["http://127.0.0.1:9999/mcp/oauth/callback"]);
  } finally {
    Deno.env.delete("BOUGH_PORT");
  }
});

Deno.test("completeAuth validates the state round-trip before touching the network", async () => {
  await withMcpDir(async () => {
    await assertRejects(() => completeAuth("nodot", "c", () => "u"), Error, "malformed state");
    // no nonce stored for this server
    await assertRejects(
      () => completeAuth("notion.deadbeef", "c", () => "u"),
      Error,
      "state mismatch",
    );
    // stored nonce differs
    new BoughOAuthProvider("notion").state();
    await assertRejects(
      () => completeAuth("notion.wrong", "c", () => "u"),
      Error,
      "state mismatch",
    );
    // valid state but the server is not registered as remote
    const state = new BoughOAuthProvider("notion").state();
    await assertRejects(
      () => completeAuth(state, "c", () => undefined),
      Error,
      "not a registered remote",
    );
  });
});

Deno.test("token files are private (0600)", () => {
  withMcpDir(() => {
    new BoughOAuthProvider("sec").saveTokens({ access_token: "t", token_type: "bearer" });
    const path = `${Deno.env.get("BOUGH_MCP_DIR")}/tokens/sec.json`;
    const mode = Deno.statSync(path).mode! & 0o777;
    assertEquals(mode, 0o600);
  });
});
