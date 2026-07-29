/**
 * `bough sync-mcp`, driven end to end against a temporary registry.
 *
 * Nothing here reads `~/.claude.json`, `~/.bough` or the login keychain: the JSON
 * reads and the registry file are both injected, which is what lets the security
 * claims below be tested at all — "the Anthropic token is not sent to a stranger"
 * is a statement about what gets WRITTEN, and this asserts on the written file.
 */
import { test } from "bun:test";
import assert from "node:assert/strict";
import { mkdtemp, readFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { collectClaudeServers, looksSecret, parseSyncArgs, runSyncMcp } from "./sync_mcp.ts";

const HOME = "/home/t";

/** A fake filesystem of JSON documents, keyed by absolute path. */
function reader(files: Record<string, unknown>) {
  return (path: string) => (path in files ? files[path] : null);
}

async function registryFile(): Promise<string> {
  return join(await mkdtemp(join(tmpdir(), "bough-syncmcp-")), "mcp.json");
}

const silent = { out: () => {}, err: () => {} };

test("parseSyncArgs: flags only, and it never throws", () => {
  assert.deepEqual(parseSyncArgs([]), {
    args: { dirs: [], force: false, dryRun: false, help: false },
  });
  assert.deepEqual(
    parseSyncArgs(["--from", "/a", "-C", "/b", "-n", "--force"]).valueOf(),
    { args: { dirs: ["/a", "/b"], force: true, dryRun: true, help: false } },
  );
  assert.ok("usage" in parseSyncArgs(["--nope"]));
  assert.ok("usage" in parseSyncArgs(["--from"]));
  // A bare word is a mistake worth naming: this command takes no positionals, and
  // silently ignoring one means a typo'd flag looks like it worked.
  assert.ok("usage" in parseSyncArgs(["everything"]));
});

test("collect: user scope, project scope and a checked-in .mcp.json, later wins", () => {
  const files = {
    [`${HOME}/.claude.json`]: {
      mcpServers: { alpha: { command: "a" }, shared: { command: "user-version" } },
      projects: { "/w": { mcpServers: { beta: { command: "b" } } } },
    },
    "/w/.mcp.json": { mcpServers: { shared: { command: "team-version" } } },
  };
  const { found, errors } = collectClaudeServers(["/w"], reader(files), HOME);
  assert.deepEqual(errors, []);
  const byName = Object.fromEntries(found.map((f) => [f.name, f]));
  assert.deepEqual(Object.keys(byName).sort(), ["alpha", "beta", "shared"]);
  // Claude Code's own precedence: the checked-in file is the last word.
  assert.equal(byName.shared.server.command, "team-version");
  assert.equal(byName.beta.source, "~/.claude.json projects[/w]");
});

test("a stdio server keeps its command, args, env and cwd", async () => {
  const file = await registryFile();
  const files = {
    [`${HOME}/.claude.json`]: {
      mcpServers: {
        "chrome-devtools": {
          type: "stdio",
          command: "npx",
          args: ["chrome-devtools-mcp@latest"],
          env: { CHROME_PATH: "/Applications/Chrome.app" },
        },
      },
    },
  };
  const code = await runSyncMcp([], {
    ...silent,
    readJson: reader(files),
    home: HOME,
    cwd: "/w",
    config: { file },
  });
  assert.equal(code, 0);
  const doc = JSON.parse(await readFile(file, "utf8"));
  assert.deepEqual(doc.servers["chrome-devtools"], {
    command: "npx",
    args: ["chrome-devtools-mcp@latest"],
    env: { CHROME_PATH: "/Applications/Chrome.app" },
    headers: {},
  });
});

test("a claude.ai server gets a keychain REFERENCE, never a token", async () => {
  const file = await registryFile();
  const files = {
    [`${HOME}/.claude.json`]: {
      mcpServers: { gmail: { type: "http", url: "https://mcp.claude.ai/gmail" } },
    },
  };
  assert.equal(
    await runSyncMcp([], { ...silent, readJson: reader(files), home: HOME, cwd: "/w", config: { file } }),
    0,
  );
  const text = await readFile(file, "utf8");
  const entry = JSON.parse(text).servers.gmail;
  assert.equal(
    entry.headers.Authorization,
    "Bearer ${keychain:Claude Code-credentials#claudeAiOauth.accessToken}",
  );
  // The registry is served by GET /mcp/servers and rendered in the panel, so the
  // one thing that must never appear in it is the secret itself.
  assert.equal(text.includes("sk-"), false);
  assert.match(text, /\$\{keychain:/);
});

test("a THIRD-PARTY remote server is registered without Anthropic's token", async () => {
  // The generalization this refuses: "it is remote, so give it the bearer token"
  // would post the user's Anthropic OAuth credential to whatever host a config
  // file happens to name. A lookalike domain must not match either.
  const file = await registryFile();
  const files = {
    [`${HOME}/.claude.json`]: {
      mcpServers: {
        linear: { type: "http", url: "https://mcp.linear.app/sse" },
        lookalike: { type: "http", url: "https://claude.ai.evil.example/mcp" },
      },
    },
  };
  await runSyncMcp([], { ...silent, readJson: reader(files), home: HOME, cwd: "/w", config: { file } });
  const doc = JSON.parse(await readFile(file, "utf8"));
  assert.deepEqual(doc.servers.linear.headers, {});
  assert.deepEqual(doc.servers.lookalike.headers, {});
});

test("an existing entry is kept unless --force, and --dry-run writes nothing", async () => {
  const file = await registryFile();
  const files = {
    [`${HOME}/.claude.json`]: { mcpServers: { alpha: { command: "from-claude" } } },
  };
  const deps = { ...silent, readJson: reader(files), home: HOME, cwd: "/w", config: { file } };
  await runSyncMcp([], deps);
  // A hand-fixed local definition must survive a second sync.
  const files2 = {
    [`${HOME}/.claude.json`]: { mcpServers: { alpha: { command: "changed-upstream" } } },
  };
  await runSyncMcp([], { ...deps, readJson: reader(files2) });
  assert.equal(JSON.parse(await readFile(file, "utf8")).servers.alpha.command, "from-claude");

  await runSyncMcp(["--dry-run", "--force"], { ...deps, readJson: reader(files2) });
  assert.equal(JSON.parse(await readFile(file, "utf8")).servers.alpha.command, "from-claude");

  await runSyncMcp(["--force"], { ...deps, readJson: reader(files2) });
  assert.equal(JSON.parse(await readFile(file, "utf8")).servers.alpha.command, "changed-upstream");
});

test("an unusable entry is reported and the others still land", async () => {
  const file = await registryFile();
  const files = {
    [`${HOME}/.claude.json`]: {
      mcpServers: { broken: { type: "stdio" }, good: { command: "ok" } },
    },
  };
  const lines: string[] = [];
  const code = await runSyncMcp([], {
    out: (l) => lines.push(l),
    err: (l) => lines.push(l),
    readJson: reader(files),
    home: HOME,
    cwd: "/w",
    config: { file },
  });
  assert.equal(code, 1); // something failed…
  assert.ok(JSON.parse(await readFile(file, "utf8")).servers.good); // …and the rest synced
  assert.match(lines.join("\n"), /broken.*failed/);
});

test("a literal-looking secret in env is warned about, not silently republished", () => {
  assert.equal(looksSecret("GITHUB_TOKEN", "ghp_averyrealtokenvalue"), true);
  assert.equal(looksSecret("API_KEY", "${MY_KEY}"), false); // already a reference
  assert.equal(looksSecret("CHROME_PATH", "/Applications/Chrome.app"), false);
  assert.equal(looksSecret("TOKEN", "short"), false);
});

test("no servers anywhere is a true answer, not a failure", async () => {
  const lines: string[] = [];
  const code = await runSyncMcp([], {
    out: (l) => lines.push(l),
    err: () => {},
    readJson: () => null,
    home: HOME,
    cwd: "/w",
    config: { file: await registryFile() },
  });
  assert.equal(code, 0);
  assert.match(lines.join("\n"), /no MCP servers found/);
});
