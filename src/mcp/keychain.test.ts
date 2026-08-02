/**
 * Store-backed MCP credentials: the explicit `${keychain:…}` reference, the store it
 * resolves against, and the automatic prefill.
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
import { mkdtemp, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { McpError } from "../errors.ts";
import {
  CLAUDE_CODE_ITEM,
  claudeCodePrefill,
  claudeConfigDir,
  credentialsFileReader,
  credentialsPath,
  credentialStores,
  holdsPath,
  isCoveredHost,
  type KeychainReader,
  parseKeychainRef,
  readFromStores,
  readKeychainRef,
  securityReader,
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

test("a key with dots in it is still addressable", async () => {
  // Claude Code stores a per-server OAuth grant under `<serverName>|<hash>`, and a
  // server named for its host puts DOTS in that key — which a dotted path splits
  // straight through. Rejoining the remaining segments finds the literal key.
  const item = JSON.stringify({
    mcpOAuth: {
      "slack|a1b2": { accessToken: "plain-key-token" },
      "notion|mcp.notion.com|d4": { accessToken: "dotted-key-token" },
    },
  });
  const plain = parseKeychainRef("${keychain:x#mcpOAuth.slack|a1b2.accessToken}")!;
  assert.equal(await readKeychainRef(plain, { keychain: ok(item) }), "plain-key-token");
  const dotted = parseKeychainRef(
    "${keychain:x#mcpOAuth.notion|mcp.notion.com|d4.accessToken}",
  )!;
  assert.equal(await readKeychainRef(dotted, { keychain: ok(item) }), "dotted-key-token");
});

test("an exact key still wins over a rejoined one", async () => {
  // The fallback must never change what an existing reference resolves to.
  const item = JSON.stringify({ a: { b: { c: "nested" } }, "a.b": { c: "flat" } });
  const nested = parseKeychainRef("${keychain:x#a.b.c}")!;
  assert.equal(await readKeychainRef(nested, { keychain: ok(item) }), "nested");
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

// ---- the store the reference resolves against ---------------------------------

test("the config directory is CLAUDE_CONFIG_DIR when set, else ~/.claude", () => {
  assert.equal(claudeConfigDir({}, "/home/t"), "/home/t/.claude");
  assert.equal(claudeConfigDir({ CLAUDE_CONFIG_DIR: "/elsewhere" }, "/home/t"), "/elsewhere");
  // Blank is not a location. Treating it as one moves the read to a relative path.
  assert.equal(claudeConfigDir({ CLAUDE_CONFIG_DIR: "  " }, "/home/t"), "/home/t/.claude");
  assert.equal(credentialsPath({}, "/home/t"), "/home/t/.claude/.credentials.json");
});

/** Runs `fn` with `CLAUDE_CONFIG_DIR` pointed at a fresh directory. */
async function withConfigDir<T>(fn: (dir: string) => Promise<T>): Promise<T> {
  const dir = await mkdtemp(join(tmpdir(), "bough-creds-"));
  const before = process.env["CLAUDE_CONFIG_DIR"];
  process.env["CLAUDE_CONFIG_DIR"] = dir;
  try {
    return await fn(dir);
  } finally {
    if (before === undefined) delete process.env["CLAUDE_CONFIG_DIR"];
    else process.env["CLAUDE_CONFIG_DIR"] = before;
  }
}

test("off macOS the credential comes out of Claude Code's credentials file", async () => {
  // The whole reason this store exists: there is no login keychain on Linux, so a
  // `${keychain:…}` reference had nothing to resolve against and every adopted server
  // failed with a sentence about a macOS facility.
  await withConfigDir(async (dir) => {
    await writeFile(join(dir, ".credentials.json"), blob(), "utf8");
    const result = await credentialsFileReader(CLAUDE_CODE_ITEM);
    assert.equal(result.code, 0);
    assert.equal(result.store, "file");
    assert.equal(
      await readKeychainRef(
        { service: CLAUDE_CODE_ITEM, path: ["claudeAiOauth", "accessToken"] },
        { keychain: credentialsFileReader },
      ),
      "sk-ant-oat01-TOKEN",
    );
  });
});

test("the credentials file answers for ONE item, not as a general vault", async () => {
  // A reference naming some other service must not be handed Claude Code's login.
  // Answering it would give one client's credential to a reference that asked for a
  // different one, which is the leak this whole module is arranged to prevent.
  await withConfigDir(async (dir) => {
    await writeFile(join(dir, ".credentials.json"), blob(), "utf8");
    const other = await credentialsFileReader("Some Other App");
    assert.equal(other.code, 44);
    assert.equal(other.value, "");
  });
});

test("an absent credentials file reads as absent, and says where it looked", async () => {
  await withConfigDir(async () => {
    const result = await credentialsFileReader(CLAUDE_CODE_ITEM);
    assert.equal(result.code, 44);
    const error = await readKeychainRef(
      { service: CLAUDE_CODE_ITEM, path: ["claudeAiOauth", "accessToken"] },
      { keychain: credentialsFileReader },
    ).then(() => null, (e: McpError) => e.message);
    // The advice has to be true for the store that actually answered: telling someone
    // on Linux to run `security find-generic-password` is a dead end.
    assert.match(String(error), /\.credentials\.json/);
    assert.match(String(error), /CLAUDE_CONFIG_DIR/);
    assert.equal(String(error).includes("generic-password"), false);
  });
});

// ---- both setups, on either platform ------------------------------------------

/** A store that HAS the item, tagged so the winner is identifiable. */
const has = (store: "keychain" | "file"): KeychainReader => () =>
  Promise.resolve({ value: blob(), code: 0, error: "", store });
/** A store that does not have it. */
const lacks = (store: "keychain" | "file", code = 44): KeychainReader => () =>
  Promise.resolve({ value: "", code, error: "", store });

test("either store can be the one that answers, whichever platform this is", async () => {
  // The point of the change: which store holds the credential is a property of the
  // MACHINE, not of its operating system. A Mac can have the keychain opted out and
  // the token in a file; a container can have the file mounted and a useless
  // `security` on PATH. Gating by platform got both of those wrong.
  const fileOnly = await readFromStores(CLAUDE_CODE_ITEM, [lacks("keychain"), has("file")]);
  assert.equal(fileOnly.store, "file");
  assert.equal(fileOnly.code, 0);

  const keychainOnly = await readFromStores(CLAUDE_CODE_ITEM, [lacks("file"), has("keychain")]);
  assert.equal(keychainOnly.store, "keychain");
  assert.equal(keychainOnly.code, 0);
});

test("ordering is by authority: the store the platform's Claude Code writes to is asked first", async () => {
  // Not availability, authority. Asking the keychain first on a Mac is what stops a
  // stale `.credentials.json` from an older install shadowing a live token. Off a Mac
  // the file is what gets written, so it goes first and the ordinary case costs no spawn.
  assert.equal(credentialStores("darwin")[0], securityReader);
  assert.equal(credentialStores("linux")[0], credentialsFileReader);
  assert.equal(credentialStores("win32")[0], credentialsFileReader);
  // Both stores are present in both orders: neither setup is out of reach.
  for (const platform of ["darwin", "linux", "win32"]) {
    const stores = credentialStores(platform);
    assert.equal(stores.length, 2, platform);
    assert.ok(stores.includes(securityReader) && stores.includes(credentialsFileReader), platform);
  }
});

/** The real split: the keychain kept the login, the file kept the MCP grants. */
const GRANT_KEY = "notion|eac663db915250e7";
const loginOnly = (store: "keychain" | "file"): KeychainReader => () =>
  Promise.resolve({ value: blob(), code: 0, error: "", store });
const withGrants = (store: "keychain" | "file"): KeychainReader => () =>
  Promise.resolve({
    value: JSON.stringify({
      mcpOAuth: { [GRANT_KEY]: { accessToken: "grant-token", serverName: "notion" } },
    }),
    code: 0,
    error: "",
    store,
  });

test("the store that has the FIELD wins, not the store that has the item", async () => {
  // The split this pins is real and observed: on this developer's Mac the keychain
  // item holds `claudeAiOauth` alone while the `mcpOAuth` grants live in
  // `.credentials.json`. Under "first store with bytes wins" the keychain answers
  // with a valid blob that cannot contain the reference's path, and the token in the
  // next store along is never reached.
  //
  // It is NOT the cause of the failure that led here — that machine had one readable
  // store and an empty grant. Pinned anyway because the arrangement is reachable.
  const ref = parseKeychainRef(`\${keychain:${CLAUDE_CODE_ITEM}#mcpOAuth.${GRANT_KEY}.accessToken}`)!;
  const picked = await readFromStores(
    CLAUDE_CODE_ITEM,
    [loginOnly("keychain"), withGrants("file")],
    (v) => holdsPath(v, ref.path),
  );
  assert.equal(picked.store, "file");
  assert.equal(JSON.parse(picked.value).mcpOAuth[GRANT_KEY].accessToken, "grant-token");
});

test("a whole-item reference still takes the first store with bytes", async () => {
  // No path means there is nothing to look inside for, so the authority ordering is
  // the whole rule and the second store must not get a say.
  const picked = await readFromStores(
    CLAUDE_CODE_ITEM,
    [loginOnly("keychain"), withGrants("file")],
    (v) => holdsPath(v, []),
  );
  assert.equal(picked.store, "keychain");
});

test("when no store holds the field, the error still names what was actually found", async () => {
  // Falling back to a bare "no such item" here would trade a diagnosis for a shrug:
  // "it holds an object with claudeAiOauth" is what tells the user the grant moved.
  const message = await readKeychainRef(
    { service: CLAUDE_CODE_ITEM, path: ["mcpOAuth", GRANT_KEY, "accessToken"] },
    {
      keychain: () =>
        readFromStores(
          CLAUDE_CODE_ITEM,
          [loginOnly("keychain"), loginOnly("file")],
          (v) => holdsPath(v, ["mcpOAuth", GRANT_KEY, "accessToken"]),
        ),
    },
  ).then(() => "", (e: McpError) => e.message);
  assert.match(message, /has no string at #mcpOAuth/);
  assert.match(message, /an object with claudeAiOauth/);
});

test("a specific failure beats a bare absence when neither store has it", async () => {
  // "You denied the prompt" and "that file is not readable" are both actionable. "No
  // such item" from the store that was never going to have it is not, so it loses.
  const denied = await readFromStores(CLAUDE_CODE_ITEM, [lacks("file"), lacks("keychain", 128)]);
  assert.equal(denied.code, 128);
  assert.equal(denied.store, "keychain");
});

test("not-found names BOTH stores, since both were tried", async () => {
  // A message naming only the store that answered last reads as though the other was
  // never looked at, and sends someone to check a location already checked.
  const message = await readKeychainRef(
    { service: CLAUDE_CODE_ITEM, path: [] },
    { keychain: () => readFromStores(CLAUDE_CODE_ITEM, [lacks("file"), lacks("keychain")]) },
  ).then(() => "", (e: McpError) => e.message);
  assert.match(message, /\.credentials\.json/);
  assert.match(message, /keychain/);
});

test("a missing `security` binary is an absent store, not an error", async () => {
  // It reports 44 so the OTHER store still gets asked. A bogus service name gives the
  // same 44 on a Mac where the binary does exist, so this holds on both platforms.
  const result = await securityReader("bough-test-no-such-item-58a98873");
  assert.equal(result.code, 44);
  assert.equal(result.store, "keychain");
  assert.equal(result.value, "");
});
