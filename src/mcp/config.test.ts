/**
 * Tests for the MCP registry and its grants.
 *
 * Hermetic twice over: every call passes `{file}` pointing at a temp path, and
 * every `${VAR}` lookup is an injected function — nothing here reads the real
 * environment or writes under the real `~/.bough`.
 *
 * The properties that matter, in order of how much damage getting them wrong
 * does: a definition is not a grant, a lapsed grant fails closed, a secret
 * reference is never expanded into the stored file, and writing one part of the
 * document never silently erases the other.
 */
import { test } from "bun:test";
import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { McpError } from "../errors.ts";
import {
  activationsFor,
  childEnv,
  expandEnv,
  getServer,
  isStdio,
  loadRegistry,
  promoteSessionGrants,
  removeServer,
  revokeEverywhere,
  requireServer,
  saveRegistry,
  ServerConfig,
  setActivation,
  ttlToExpires,
  upsertServer,
} from "./config.ts";

function tmpFile(): string {
  return join(mkdtempSync(join(tmpdir(), "bough-mcp-config-")), "mcp.json");
}

test("registry: empty when absent, round-trips, and a definition is not a grant", () => {
  const file = tmpFile();
  assert.deepEqual(loadRegistry({ file }), { servers: {} });

  saveRegistry({ servers: { echo: { command: "deno", args: ["run", "srv.ts"] } } }, { file });
  const registry = loadRegistry({ file });
  assert.deepEqual(Object.keys(registry.servers), ["echo"]);
  assert.equal(registry.servers.echo.command, "deno");
  assert.deepEqual(registry.servers.echo.args, ["run", "srv.ts"]);
  assert.ok(isStdio(registry.servers.echo));

  // Registering granted nothing: no session sees it until something activates it.
  assert.deepEqual(activationsFor("s1", { file }), []);
});

test("registry: a corrupt file contributes nothing rather than half a catalog", () => {
  const file = tmpFile();
  writeFileSync(file, "{ this is not json");
  assert.deepEqual(loadRegistry({ file }), { servers: {} });
  writeFileSync(file, JSON.stringify({ servers: { echo: { command: 42 } } }));
  assert.deepEqual(loadRegistry({ file }), { servers: {} });
});

test("registry: entry shapes are rejected with a sentence naming the fix", () => {
  const file = tmpFile();
  // Neither transport, and both, are the same mistake reported the same way.
  for (const bad of [{}, { command: "x", url: "https://y.example" }]) {
    assert.throws(
      () => saveRegistry({ servers: { bad } }, { file }),
      (e: unknown) =>
        e instanceof McpError && e.status === 400 && /exactly one of `command`/.test(e.message),
    );
  }
  // Names are slugs — both on the whole-registry path and the per-server one.
  assert.throws(
    () => saveRegistry({ servers: { "Bad Name": { command: "x" } } }, { file }),
    McpError,
  );
  assert.throws(() => upsertServer("Bad Name", { command: "x" }, { file }), McpError);
  // Transport-specific keys on the wrong transport.
  assert.throws(
    () => upsertServer("remote", { url: "https://y.example", args: ["--x"] }, { file }),
    (e: unknown) => e instanceof McpError && /remote server takes/.test(e.message),
  );
  assert.throws(
    () => upsertServer("local", { command: "x", headers: { a: "b" } }, { file }),
    (e: unknown) => e instanceof McpError && /stdio server takes/.test(e.message),
  );
  // A PRE-REGISTERED OAuth client belongs to a remote server and to nothing else.
  assert.throws(
    () => upsertServer("local", { command: "x", clientId: "abc" }, { file }),
    (e: unknown) => e instanceof McpError && /stdio server takes/.test(e.message),
  );
  // A secret identifies nothing on its own.
  assert.throws(
    () =>
      upsertServer("remote", {
        url: "https://y.example",
        clientSecret: "${SOME_VAR}",
      }, { file }),
    (e: unknown) => e instanceof McpError && /needs the `clientId`/.test(e.message),
  );
  // THE ONE THAT MATTERS: a literal secret is refused. This file is served by
  // GET /mcp/servers and rendered in the panel, so a literal would sit in a
  // response body and in the model's context.
  assert.throws(
    () =>
      upsertServer("remote", {
        url: "https://y.example",
        clientId: "abc",
        clientSecret: "xoxb-the-actual-secret",
      }, { file }),
    (e: unknown) => e instanceof McpError && /must be a `\$\{VAR\}` reference/.test(e.message),
  );
  assert.deepEqual(loadRegistry({ file }), { servers: {} }); // nothing was written
});

test("a pre-registered OAuth client round-trips, and the secret stays a reference", () => {
  // The registry keeps the REFERENCE. Expansion happens where the value is used
  // (`expandEnv`, and `BoughOAuthProvider.clientInformation` through it), never on
  // the way in — otherwise the resolved secret would be what is written to disk and
  // served.
  const file = tmpFile();
  upsertServer("slack", {
    url: "https://mcp.slack.com/mcp",
    clientId: "1234.5678",
    clientSecret: "${SLACK_MCP_CLIENT_SECRET}",
  }, { file });
  const entry = loadRegistry({ file }).servers.slack!;
  assert.equal(entry.clientId, "1234.5678");
  assert.equal(entry.clientSecret, "${SLACK_MCP_CLIENT_SECRET}");
  assert.equal(
    readFileSync(file, "utf8").includes("${SLACK_MCP_CLIENT_SECRET}"),
    true,
    "the reference, not a resolved value, is what is stored",
  );
});

test("upsertServer replaces one entry without touching siblings; removeServer deletes", () => {
  const file = tmpFile();
  saveRegistry({ servers: { exa: { command: "npx", args: ["exa-mcp"] } } }, { file });
  upsertServer("echo", { command: "deno", args: ["run", "srv.ts"] }, { file });
  assert.deepEqual(Object.keys(loadRegistry({ file }).servers).sort(), ["echo", "exa"]);
  assert.deepEqual(loadRegistry({ file }).servers.exa.args, ["exa-mcp"]); // sibling untouched

  upsertServer("echo", { url: "https://mcp.example.com/mcp" }, { file });
  assert.equal(getServer("echo", { file })?.url, "https://mcp.example.com/mcp");
  assert.equal(isStdio(getServer("echo", { file })!), false);

  assert.equal(removeServer("echo", { file }), true);
  assert.equal(removeServer("echo", { file }), false);
  assert.deepEqual(Object.keys(loadRegistry({ file }).servers), ["exa"]);
});

test("requireServer names the alternatives instead of saying 'not found'", () => {
  const file = tmpFile();
  saveRegistry({ servers: { exa: { command: "npx" }, linear: { url: "https://l.example" } } }, {
    file,
  });
  assert.equal(requireServer("exa", { file }).command, "npx");
  assert.throws(
    () => requireServer("linaer", { file }),
    (e: unknown) =>
      e instanceof McpError && e.status === 404 &&
      /Registered servers: exa, linear/.test(e.message) &&
      /PUT \/mcp\/servers\/linaer/.test(e.message),
  );
});

test("saveRegistry preserves grants; removeServer revokes the ones it orphans", () => {
  const file = tmpFile();
  saveRegistry({ servers: { echo: { command: "deno" }, exa: { command: "npx" } } }, { file });
  setActivation("s1", "echo", true, { file });
  setActivation(undefined, "exa", true, { file });

  // Renaming/rewriting the registry must not revoke every grant as a side effect.
  saveRegistry({ servers: { echo: { command: "deno", args: ["-A"] }, exa: { command: "npx" } } }, {
    file,
  });
  assert.deepEqual(activationsFor("s1", { file }), ["echo", "exa"]);

  // Deleting the server deletes its grant, so re-registering the name starts ungranted.
  removeServer("echo", { file });
  assert.deepEqual(activationsFor("s1", { file }), ["exa"]);
  upsertServer("echo", { command: "deno" }, { file });
  assert.deepEqual(activationsFor("s1", { file }), ["exa"]);
});

test("promoteSessionGrants lifts old per-conversation grants to the global scope", () => {
  // Every grant written before the panel's ⏎ became install-wide is scoped to the
  // conversation it was made in. Those servers then read `off` everywhere else —
  // and on the new-conversation screen, which has no session at all, `off` full
  // stop, which is what a person sees after upgrading and reports as "broken".
  const file = tmpFile();
  saveRegistry({
    servers: {
      echo: { command: "deno" },
      exa: { command: "npx" },
      linear: { url: "https://l.example" },
    },
  }, { file });
  setActivation("s1", "echo", true, { file });
  setActivation("s2", "echo", true, { file }); // the same server in two conversations
  setActivation("s2", "exa", true, { file });
  setActivation(undefined, "linear", true, { file }); // already global: kept, not doubled
  // A TTL grant is a deliberate limit and must not become permanent.
  setActivation("s1", "linear", true, { file, expires: ttlToExpires("2h") });

  assert.deepEqual(promoteSessionGrants({ file }).sort(), ["echo", "exa"]);
  assert.deepEqual(activationsFor(undefined, { file }), ["echo", "exa", "linear"]);
  // The session rows are gone, so a conversation resolves exactly the global set…
  assert.deepEqual(activationsFor("s1", { file }), ["echo", "exa", "linear"]);
  // …and running it again is a no-op, which is what makes it safe at every boot.
  assert.deepEqual(promoteSessionGrants({ file }), []);
});

test("revokeEverywhere clears the global scope AND every session that holds it", () => {
  // The panel's ⏎ grants globally, so its opposite has to mean what it says.
  // Clearing only the global row left a server granted in whichever conversations
  // had been granted it one at a time — which is how every grant was made before ⏎
  // became global — so the screen said "off in every conversation" while the next
  // turn in an older one could still call it.
  const file = tmpFile();
  saveRegistry({ servers: { echo: { command: "deno" }, exa: { command: "npx" } } }, { file });
  setActivation("s1", "echo", true, { file });
  setActivation("s2", "echo", true, { file });
  setActivation(undefined, "echo", true, { file });
  setActivation("s1", "exa", true, { file }); // a sibling grant, which must survive

  revokeEverywhere("echo", { file });
  assert.deepEqual(activationsFor("s1", { file }), ["exa"]);
  assert.deepEqual(activationsFor("s2", { file }), []);
  assert.deepEqual(activationsFor(undefined, { file }), []);
});

test("activations: per-session and global scopes, and a lapsed TTL fails closed", () => {
  const file = tmpFile();
  const now = Date.parse("2026-07-27T12:00:00Z");
  saveRegistry({ servers: { echo: { command: "deno" }, linear: { url: "https://l.example" } } }, {
    file,
  });

  assert.deepEqual(activationsFor("s1", { file, now }), []);
  setActivation("s1", "echo", true, { file });
  setActivation(undefined, "linear", true, { file }); // global
  assert.deepEqual(activationsFor("s1", { file, now }), ["echo", "linear"]);
  assert.deepEqual(activationsFor("s2", { file, now }), ["linear"]); // only the global one
  assert.deepEqual(activationsFor(undefined, { file, now }), ["linear"]);

  // A TTL is stored absolute and read against the injected clock.
  const expires = ttlToExpires("2h", now);
  setActivation("s1", "echo", true, { file, expires });
  assert.deepEqual(activationsFor("s1", { file, now: now + 3_600_000 }), ["echo", "linear"]);
  assert.deepEqual(activationsFor("s1", { file, now: now + 7_200_001 }), ["linear"]);

  // Re-enabling replaces the lapsed grant rather than sitting beside it.
  setActivation("s1", "echo", true, { file });
  assert.deepEqual(activationsFor("s1", { file, now: now + 7_200_001 }), ["echo", "linear"]);
  setActivation("s1", "echo", false, { file });
  setActivation(undefined, "linear", false, { file });
  assert.deepEqual(activationsFor("s1", { file, now }), []);
});

test("ttlToExpires parses the three forms and refuses anything else", () => {
  const now = Date.parse("2026-07-27T12:00:00Z");
  assert.equal(ttlToExpires("90m", now), new Date(now + 90 * 60_000).toISOString());
  assert.equal(ttlToExpires(" 2h ", now), new Date(now + 2 * 3_600_000).toISOString());
  assert.equal(ttlToExpires("7d", now), new Date(now + 7 * 86_400_000).toISOString());
  assert.throws(
    () => ttlToExpires("forever", now),
    (e: unknown) => e instanceof McpError && /"90m", "2h", "7d"/.test(e.message),
  );
});

test("expandEnv substitutes ${VAR} and refuses to start on a missing one", () => {
  const env = (name: string) => ({ TOK: "s3cr3t" } as Record<string, string>)[name];
  assert.deepEqual(
    expandEnv({ TOKEN: "${TOK}", MIXED: "Bearer ${TOK}", PLAIN: "as-is" }, { env }),
    { TOKEN: "s3cr3t", MIXED: "Bearer s3cr3t", PLAIN: "as-is" },
  );
  assert.throws(
    () => expandEnv({ TOKEN: "${NOPE}" }, { env }),
    (e: unknown) => e instanceof McpError && e.status === 400 && /\$\{NOPE\}/.test(e.message),
  );
});

test("the secret reference is stored, never the secret", () => {
  const file = tmpFile();
  upsertServer("linear", { url: "https://l.example", headers: {} }, { file });
  upsertServer("gh", { command: "gh-mcp", env: { TOKEN: "${GH_TOKEN}" } }, { file });
  const onDisk = readFileSync(file, "utf-8");
  assert.match(onDisk, /\$\{GH_TOKEN\}/);
  assert.doesNotMatch(onDisk, /s3cr3t/);
  // The value only appears when a child is about to be spawned.
  const composed = childEnv(getServer("gh", { file })!, {
    env: (n) => ({ GH_TOKEN: "s3cr3t", PATH: "/usr/bin" } as Record<string, string>)[n],
  });
  assert.equal(composed.TOKEN, "s3cr3t");
});

test("childEnv composes the child's whole environment: inherited names plus declared", () => {
  const host: Record<string, string> = {
    PATH: "/usr/bin",
    HOME: "/home/u",
    HTTPS_PROXY: "http://proxy:8080",
    ANTHROPIC_API_KEY: "sk-do-not-leak",
    RANDOM_THING: "no",
  };
  const server = ServerConfig.parse({ command: "srv", env: { PATH: "/opt/bin", X: "1" } });
  const composed = childEnv(server, { env: (n) => host[n] });

  assert.equal(composed.HOME, "/home/u");
  assert.equal(composed.HTTPS_PROXY, "http://proxy:8080");
  assert.equal(composed.X, "1");
  // Declared values win on a collision — a server that overrides PATH meant to.
  assert.equal(composed.PATH, "/opt/bin");
  // Everything else stays behind: a third-party binary gets no provider keys.
  assert.equal(composed.ANTHROPIC_API_KEY, undefined);
  assert.equal(composed.RANDOM_THING, undefined);
});
