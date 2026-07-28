/**
 * Keychain-backed MCP credentials: the explicit `${keychain:…}` reference and the
 * automatic prefill.
 *
 * Every test injects the reader, so nothing here spawns `security`, reads the
 * developer's login keychain, or raises an access dialog on a machine running the
 * suite. What is asserted is the part that can actually be wrong: which server a
 * secret is allowed to reach, which credential wins when there are two, and whether
 * a failure is loud or silent — those are opposite answers for the two paths and
 * getting them the wrong way round is either a leak or a broken server.
 */
import { test } from "bun:test";
import assert from "node:assert/strict";
import { McpError } from "../errors.ts";
import {
  CLAUDE_CODE_ITEM,
  claudeCodePrefill,
  isCoveredHost,
  type KeychainReader,
  parseKeychainRef,
  readKeychainRef,
} from "./keychain.ts";
import { expandHeaders } from "./config.ts";

const ok = (value: string): KeychainReader => () => Promise.resolve({ value, code: 0, error: "" });
const fails = (code: number, error = ""): KeychainReader => () =>
  Promise.resolve({ value: "", code, error });

/** What Claude Code stores, in the shape it stores it. */
const blob = (over: Record<string, unknown> = {}) =>
  JSON.stringify({
    claudeAiOauth: {
      accessToken: "sk-ant-oat01-TOKEN",
      refreshToken: "sk-ant-ort01-REFRESH",
      expiresAt: Date.now() + 3_600_000,
      scopes: ["user:inference", "user:profile"],
      subscriptionType: "max",
      ...over,
    },
  });

// ---- the reference ----------------------------------------------------------

test("a reference names an item, and optionally a field inside it", () => {
  assert.deepEqual(parseKeychainRef("${keychain:Claude Code-credentials}"), {
    service: "Claude Code-credentials",
    path: [],
  });
  assert.deepEqual(parseKeychainRef("${keychain:Claude Code-credentials#a.b}"), {
    service: "Claude Code-credentials",
    path: ["a", "b"],
  });
  // Not a reference: an ordinary header value, and an env reference, both of which
  // must fall through to the expansion that already existed.
  assert.equal(parseKeychainRef("Bearer abc"), null);
  assert.equal(parseKeychainRef("${TOKEN}"), null);
  assert.equal(parseKeychainRef("${keychain:}"), null);
});

test("a field is read out of a JSON item; a plain item is used whole", async () => {
  const ref = parseKeychainRef("${keychain:x#claudeAiOauth.accessToken}")!;
  assert.equal(await readKeychainRef(ref, { keychain: ok(blob()) }), "sk-ant-oat01-TOKEN");
  const whole = parseKeychainRef("${keychain:x}")!;
  assert.equal(await readKeychainRef(whole, { keychain: ok("plain-secret") }), "plain-secret");
});

test("`security -w` newline is not part of the secret", async () => {
  // A token with a trailing newline makes a header the remote end rejects for
  // reasons it will not explain.
  const ref = parseKeychainRef("${keychain:x}")!;
  assert.equal(await readKeychainRef(ref, { keychain: ok("token\n") }), "token");
});

test("every failure names the item and says what to do", async () => {
  const ref = parseKeychainRef("${keychain:Claude Code-credentials}")!;
  const message = async (reader: KeychainReader): Promise<string> => {
    try {
      await readKeychainRef(ref, { keychain: reader });
    } catch (error) {
      assert.ok(error instanceof McpError, `${error}`);
      return error.message;
    }
    throw new Error("expected a throw");
  };

  // 44 is "no such item" — a setup problem, and the name is the thing to check.
  const missing = await message(fails(44));
  assert.match(missing, /no generic-password item with that service name/);
  assert.match(missing, /Claude Code-credentials/);
  // 128 is the access dialog being refused, which is a decision the user just made
  // and must not read as a bug.
  assert.match(await message(fails(128)), /prompt was denied or cancelled/);
  // Anything else reports the code rather than guessing.
  assert.match(await message(fails(1, "boom")), /security exited 1 — boom/);
});

test("an expired token is reported, not refreshed", async () => {
  const ref = parseKeychainRef("${keychain:Claude Code-credentials#claudeAiOauth.accessToken}")!;
  const stale = blob({ expiresAt: Date.now() - 1_000 });
  try {
    await readKeychainRef(ref, { keychain: ok(stale) });
    throw new Error("expected a throw");
  } catch (error) {
    assert.ok(error instanceof McpError);
    // bough does not mint a new token from a refresh token it did not obtain: that
    // is impersonating the client that owns it, and the fix belongs to the user.
    assert.match(error.message, /expired at/);
    assert.match(error.message, /does not refresh a credential it did not obtain/);
    assert.match(error.message, /run `claude` once/);
  }
});

test("an error never contains the secret — only the item's shape", async () => {
  const ref = parseKeychainRef("${keychain:x#nope.missing}")!;
  try {
    await readKeychainRef(ref, { keychain: ok(blob()) });
    throw new Error("expected a throw");
  } catch (error) {
    assert.ok(error instanceof McpError);
    assert.match(error.message, /has no string at #nope\.missing/);
    assert.match(error.message, /an object with claudeAiOauth/);
    assert.equal(error.message.includes("sk-ant-"), false, error.message);
  }
});

// ---- headers ----------------------------------------------------------------

test("headers resolve at send time: keychain, env, and plain text", async () => {
  const headers = await expandHeaders({
    Authorization: "Bearer ${keychain:Claude Code-credentials#claudeAiOauth.accessToken}",
    "X-Token": "${keychain:Claude Code-credentials#claudeAiOauth.refreshToken}",
    "X-Env": "${SOME_TOKEN}",
    "X-Static": "1",
  }, {
    keychain: ok(blob()),
    env: (name) => (name === "SOME_TOKEN" ? "from-env" : undefined),
  });
  assert.deepEqual(headers, {
    Authorization: "Bearer sk-ant-oat01-TOKEN",
    "X-Token": "sk-ant-ort01-REFRESH",
    "X-Env": "from-env",
    "X-Static": "1",
  });
});

// ---- prefill ----------------------------------------------------------------

test("prefill is confined to hosts the credential belongs to", () => {
  // The question prefill has to answer is not "would a token help here" but "may
  // this server be told this secret". An MCP server sees the Authorization header
  // verbatim, so prefilling a third party hands them an Anthropic credential as a
  // side effect of registering a server.
  assert.equal(isCoveredHost("https://mcp.claude.ai/mcp"), true);
  assert.equal(isCoveredHost("https://claude.ai/api/mcp"), true);
  assert.equal(isCoveredHost("https://api.anthropic.com/v1/mcp"), true);
  assert.equal(isCoveredHost("https://mcp.linear.app/sse"), false);
  assert.equal(isCoveredHost("https://claude.ai.evil.example/mcp"), false);
  assert.equal(isCoveredHost("not a url"), false);
});

test("prefill answers for a covered host and stays silent everywhere else", async () => {
  const keychain = ok(blob());
  assert.equal(
    await claudeCodePrefill("https://mcp.claude.ai/mcp", { keychain }),
    "sk-ant-oat01-TOKEN",
  );
  assert.equal(await claudeCodePrefill("https://mcp.linear.app/sse", { keychain }), undefined);
});

test("a missing or stale prefill is SILENT — the ordinary path must still work", async () => {
  // The opposite rule from the explicit reference above, deliberately: a machine
  // that never ran Claude Code has no such item, and turning that into an error
  // would break every server with its own perfectly good OAuth flow.
  const url = "https://mcp.claude.ai/mcp";
  assert.equal(await claudeCodePrefill(url, { keychain: fails(44) }), undefined);
  assert.equal(await claudeCodePrefill(url, { keychain: ok("not json") }), undefined);
  assert.equal(
    await claudeCodePrefill(url, { keychain: ok(blob({ expiresAt: Date.now() - 1 })) }),
    undefined,
  );
  // …and the item it looks in is the one Claude Code actually writes.
  assert.equal(CLAUDE_CODE_ITEM, "Claude Code-credentials");
});
