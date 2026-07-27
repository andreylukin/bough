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
import assert from "node:assert/strict";
import { readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { McpError } from "../errors.ts";
import {
  activationsFor,
  childEnv,
  expandEnv,
  getServer,
  isStdio,
  loadRegistry,
  removeServer,
  requireServer,
  saveRegistry,
  ServerConfig,
  setActivation,
  ttlToExpires,
  upsertServer,
} from "./config.ts";

function tmpFile(): string {
  return join(Deno.makeTempDirSync({ prefix: "bough-mcp-config-" }), "mcp.json");
}

Deno.test("registry: empty when absent, round-trips, and a definition is not a grant", () => {
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

Deno.test("registry: a corrupt file contributes nothing rather than half a catalog", () => {
  const file = tmpFile();
  writeFileSync(file, "{ this is not json");
  assert.deepEqual(loadRegistry({ file }), { servers: {} });
  writeFileSync(file, JSON.stringify({ servers: { echo: { command: 42 } } }));
  assert.deepEqual(loadRegistry({ file }), { servers: {} });
});

Deno.test("registry: entry shapes are rejected with a sentence naming the fix", () => {
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
  assert.deepEqual(loadRegistry({ file }), { servers: {} }); // nothing was written
});

Deno.test("upsertServer replaces one entry without touching siblings; removeServer deletes", () => {
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

Deno.test("requireServer names the alternatives instead of saying 'not found'", () => {
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

Deno.test("saveRegistry preserves grants; removeServer revokes the ones it orphans", () => {
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

Deno.test("activations: per-session and global scopes, and a lapsed TTL fails closed", () => {
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

Deno.test("ttlToExpires parses the three forms and refuses anything else", () => {
  const now = Date.parse("2026-07-27T12:00:00Z");
  assert.equal(ttlToExpires("90m", now), new Date(now + 90 * 60_000).toISOString());
  assert.equal(ttlToExpires(" 2h ", now), new Date(now + 2 * 3_600_000).toISOString());
  assert.equal(ttlToExpires("7d", now), new Date(now + 7 * 86_400_000).toISOString());
  assert.throws(
    () => ttlToExpires("forever", now),
    (e: unknown) => e instanceof McpError && /"90m", "2h", "7d"/.test(e.message),
  );
});

Deno.test("expandEnv substitutes ${VAR} and refuses to start on a missing one", () => {
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

Deno.test("the secret reference is stored, never the secret", () => {
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

Deno.test("childEnv composes the child's whole environment: inherited names plus declared", () => {
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
