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
import {
  boughName,
  collectClaudeServers,
  collectPluginServers,
  looksSecret,
  parseSyncArgs,
  runSyncMcp,
} from "./sync_mcp.ts";

const HOME = "/home/t";
const CONFIG_DIR = `${HOME}/.claude`;

/** A fake filesystem of JSON documents, keyed by absolute path. */
function reader(files: Record<string, unknown>) {
  return (path: string) => (path in files ? files[path] : null);
}

async function registryFile(): Promise<string> {
  return join(await mkdtemp(join(tmpdir(), "bough-syncmcp-")), "mcp.json");
}

/**
 * `security` reporting "no such item" (44).
 *
 * The DEFAULT for every test that is not about grants, and not an incidental one:
 * without it the command falls through to `securityReader` and a test run reads the
 * developer's real login keychain — which can raise the system's "allow access?"
 * dialog and hang a suite on a machine nobody is watching.
 */
const noKeychain = async () => ({ value: "", code: 44, error: "" });

const silent = { out: () => {}, err: () => {}, keychain: noKeychain };

test("parseSyncArgs: flags only, and it never throws", () => {
  assert.deepEqual(parseSyncArgs([]), {
    args: { dirs: [], force: false, dryRun: false, help: false, plugins: true },
  });
  assert.deepEqual(
    parseSyncArgs(["--from", "/a", "-C", "/b", "-n", "--force"]).valueOf(),
    { args: { dirs: ["/a", "/b"], force: true, dryRun: true, help: false, plugins: true } },
  );
  // Plugin servers are the default source, so the flag that matters is the opt-OUT.
  assert.deepEqual(
    parseSyncArgs(["--no-plugins"]).valueOf(),
    { args: { dirs: [], force: false, dryRun: false, help: false, plugins: false } },
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

test("collect: the config directory's .claude.json is read, and wins", () => {
  // The bug this covers, verbatim from a machine with three servers working in Claude
  // Code: `~/.claude.json` does not exist on a current install (the file is inside the
  // config directory), so reading only the old path took ENOENT for "no servers
  // configured" and reported that as a fact about the user's setup.
  const files = {
    [`${CONFIG_DIR}/.claude.json`]: {
      mcpServers: { alpha: { command: "from-config-dir" } },
      projects: { "/w": { mcpServers: { beta: { command: "b" } } } },
    },
  };
  const { found, errors } = collectClaudeServers(["/w"], reader(files), HOME, CONFIG_DIR);
  assert.deepEqual(errors, []);
  const byName = Object.fromEntries(found.map((f) => [f.name, f]));
  assert.deepEqual(Object.keys(byName).sort(), ["alpha", "beta"]);
  assert.equal(byName.alpha.source, "~/.claude/.claude.json");
  assert.equal(byName.beta.source, "~/.claude/.claude.json projects[/w]");

  // With BOTH present the config-directory file is the live one and takes the name.
  const both = {
    [`${HOME}/.claude.json`]: { mcpServers: { alpha: { command: "legacy" } } },
    ...files,
  };
  const merged = collectClaudeServers([], reader(both), HOME, CONFIG_DIR).found;
  assert.equal(merged.find((f) => f.name === "alpha")?.server.command, "from-config-dir");
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

// ---- installed plugins' own servers -------------------------------------------

/** Where a plugin lands when Claude Code installs it. */
const SLACK_ROOT = `${CONFIG_DIR}/plugins/cache/official/slack/1.1.0`;
const CDT_ROOT = `${CONFIG_DIR}/plugins/cache/cdt-plugins/chrome-devtools-mcp/1.4.0`;

/** `installed_plugins.json` plus the two definition shapes that exist in the wild. */
const PLUGIN_FILES: Record<string, unknown> = {
  [`${CONFIG_DIR}/plugins/installed_plugins.json`]: {
    version: 2,
    plugins: {
      "slack@official": [{ scope: "user", installPath: SLACK_ROOT, version: "1.1.0" }],
      "chrome-devtools-mcp@cdt-plugins": [{ scope: "user", installPath: CDT_ROOT }],
    },
  },
  // Shape one: a `.mcp.json` beside the plugin, wrapped in `mcpServers`.
  [`${SLACK_ROOT}/.mcp.json`]: {
    mcpServers: {
      slack: {
        type: "http",
        url: "https://mcp.slack.com/mcp",
        oauth: { clientId: "1601185624273.8899143856786", callbackPort: 3118 },
      },
    },
  },
  [`${SLACK_ROOT}/.claude-plugin/plugin.json`]: { name: "slack", version: "1.1.0" },
  // Shape two: declared inline in the manifest, no `.mcp.json` at all.
  [`${CDT_ROOT}/.claude-plugin/plugin.json`]: {
    name: "chrome-devtools-mcp",
    mcpServers: { "chrome-devtools": { command: "npx", args: ["chrome-devtools-mcp@1.4.0"] } },
  },
};

test("collect: a plugin's servers are found in EITHER shape, namespaced as Claude Code does", () => {
  // Both shapes are live on the machine that reported this: slack ships a `.mcp.json`,
  // chrome-devtools declares its server in the manifest. Neither is in any file
  // this command used to read. That is why it found nothing while three servers worked.
  const { found, errors } = collectPluginServers(CONFIG_DIR, [], reader(PLUGIN_FILES));
  assert.deepEqual(errors, []);
  const byName = Object.fromEntries(found.map((f) => [f.name, f]));
  assert.deepEqual(
    Object.keys(byName).sort(),
    ["plugin:chrome-devtools-mcp:chrome-devtools", "plugin:slack:slack"],
  );
  // The namespaced name is kept verbatim because it is the key the OAuth grant is
  // stored under. Renaming happens later, in `boughName`.
  assert.equal(byName["plugin:slack:slack"].server.url, "https://mcp.slack.com/mcp");
  assert.equal(byName["plugin:chrome-devtools-mcp:chrome-devtools"].server.command, "npx");
});

test("collect: a bare server map is read too, not just the mcpServers wrapper", () => {
  // Several official plugins ship `{ "<name>": { … } }` with no wrapper (terraform,
  // linear, github). Reading only the documented shape found nothing in them and said
  // nothing about it, which is the worst of the two failures.
  const root = `${CONFIG_DIR}/plugins/cache/o/linear/1.0.0`;
  const { found } = collectPluginServers(CONFIG_DIR, [], reader({
    [`${CONFIG_DIR}/plugins/installed_plugins.json`]: {
      plugins: { "linear@o": [{ scope: "user", installPath: root }] },
    },
    [`${root}/.mcp.json`]: { linear: { type: "http", url: "https://mcp.linear.app/mcp" } },
  }));
  assert.deepEqual(found.map((f) => f.name), ["plugin:linear:linear"]);
  assert.equal(found[0].server.url, "https://mcp.linear.app/mcp");
});

test("collect: ${CLAUDE_PLUGIN_ROOT} becomes the directory the plugin is installed in", () => {
  // Claude Code sets that variable when it spawns the child. Copied verbatim, bough
  // would spawn in a directory literally named `${CLAUDE_PLUGIN_ROOT}`.
  const root = `${CONFIG_DIR}/plugins/cache/o/discord/1.0.0`;
  const { found } = collectPluginServers(CONFIG_DIR, [], reader({
    [`${CONFIG_DIR}/plugins/installed_plugins.json`]: {
      plugins: { "discord@o": [{ scope: "user", installPath: root }] },
    },
    [`${root}/.mcp.json`]: {
      mcpServers: {
        discord: {
          command: "bun",
          args: ["run", "--cwd", "${CLAUDE_PLUGIN_ROOT}", "start"],
          env: { ROOT: "${CLAUDE_PLUGIN_ROOT}/lib" },
        },
      },
    },
  }));
  assert.deepEqual(found[0].server.args, ["run", "--cwd", root, "start"]);
  assert.deepEqual(found[0].server.env, { ROOT: `${root}/lib` });
});

test("collect: only INSTALLED plugins, and a project-scoped one only for its project", () => {
  // The marketplace cache holds a `.mcp.json` for every plugin ever indexed (dozens
  // of them), so `installed_plugins.json` is what decides. And a plugin scoped to one
  // checkout is scoped precisely so it does not follow you into unrelated ones.
  const root = `${CONFIG_DIR}/plugins/cache/m/frontend/1.2.0`;
  const files = {
    [`${CONFIG_DIR}/plugins/installed_plugins.json`]: {
      plugins: {
        "frontend@m": [{ scope: "project", projectPath: "/repos/theirs", installPath: root }],
      },
    },
    [`${root}/.mcp.json`]: { mcpServers: { fe: { command: "fe" } } },
  };
  assert.deepEqual(collectPluginServers(CONFIG_DIR, ["/repos/mine"], reader(files)).found, []);
  assert.deepEqual(
    collectPluginServers(CONFIG_DIR, ["/repos/theirs"], reader(files)).found.map((f) => f.name),
    ["plugin:frontend:fe"],
  );
  // No registry file at all: no plugins, and not an error either.
  assert.deepEqual(collectPluginServers(CONFIG_DIR, [], () => null), { found: [], errors: [] });
});

test("a plugin server is synced, renamed, and given its plugin's OAuth client", async () => {
  const file = await registryFile();
  const lines: string[] = [];
  const code = await runSyncMcp([], {
    out: (l) => lines.push(l),
    err: (l) => lines.push(l),
    readJson: reader(PLUGIN_FILES),
    keychain: keychain({
      "plugin:slack:slack|38801a": {
        serverName: "plugin:slack:slack",
        serverUrl: "https://mcp.slack.com/mcp",
        accessToken: "slack-secret-token",
        expiresAt: Date.now() + 3_600_000,
      },
    }),
    home: HOME,
    configDir: CONFIG_DIR,
    cwd: "/w",
    config: { file },
  });
  assert.equal(code, 0);
  const text = await readFile(file, "utf8");
  const servers = JSON.parse(text).servers;
  assert.deepEqual(Object.keys(servers).sort(), ["chrome-devtools", "slack"]);
  // Its own grant, referenced and not copied.
  assert.equal(
    servers.slack.headers.Authorization,
    "Bearer ${keychain:Claude Code-credentials#mcpOAuth.plugin:slack:slack|38801a.accessToken}",
  );
  assert.equal(text.includes("slack-secret-token"), false);
  // Slack publishes `registration_endpoint: null`, so without the plugin's client id
  // this entry becomes un-reauthorizable the moment the adopted grant expires.
  assert.equal(servers.slack.clientId, "1601185624273.8899143856786");
  assert.deepEqual(servers["chrome-devtools"].args, ["chrome-devtools-mcp@1.4.0"]);
  assert.match(lines.join("\n"), /renamed from plugin:slack:slack/);
});

test("--no-plugins leaves a plugin's servers alone", async () => {
  const lines: string[] = [];
  const code = await runSyncMcp(["--no-plugins"], {
    out: (l) => lines.push(l),
    err: () => {},
    keychain: noKeychain,
    readJson: reader(PLUGIN_FILES),
    home: HOME,
    configDir: CONFIG_DIR,
    cwd: "/w",
    config: { file: await registryFile() },
  });
  assert.equal(code, 0);
  // Plugins were the only source with anything in it, so opting out leaves nothing.
  assert.match(lines.join("\n"), /no MCP servers found/);
});

test("syncing twice is the same as syncing once, stdio servers included", async () => {
  // The bug, on the second real run: every plugin server needs a rename, so
  // `mcp-search` came back "taken" by the entry the FIRST run had just written,
  // `boughName` fell through to slugifying the whole name, and the registry grew
  // `plugin-claude-mem-mcp-search` beside the `mcp-search` that was already there.
  // A URL identified a server; a subprocess did not, so nothing caught it.
  const file = await registryFile();
  const deps = {
    ...silent,
    readJson: reader({
      ...PLUGIN_FILES,
      [`${CONFIG_DIR}/plugins/installed_plugins.json`]: {
        plugins: {
          "claude-mem@t": [{ scope: "user", installPath: `${CONFIG_DIR}/plugins/cache/t/cm/1` }],
        },
      },
      [`${CONFIG_DIR}/plugins/cache/t/cm/1/.mcp.json`]: {
        mcpServers: { "mcp-search": { command: "node", args: ["-e", "boot"] } },
      },
    }),
    home: HOME,
    configDir: CONFIG_DIR,
    cwd: "/w",
    config: { file },
  };
  await runSyncMcp([], deps);
  assert.deepEqual(Object.keys(JSON.parse(await readFile(file, "utf8")).servers), ["mcp-search"]);
  await runSyncMcp([], deps);
  assert.deepEqual(Object.keys(JSON.parse(await readFile(file, "utf8")).servers), ["mcp-search"]);
});

test("a user's own definition beats the plugin's under the same name", async () => {
  // A plugin's definition is the default; somebody who has written their own entry for
  // that server is saying they want theirs, and a source read later must not clobber it.
  const file = await registryFile();
  await runSyncMcp([], {
    ...silent,
    readJson: reader({
      ...PLUGIN_FILES,
      [`${CONFIG_DIR}/.claude.json`]: {
        mcpServers: { "plugin:slack:slack": { type: "http", url: "https://slack.mine/mcp" } },
      },
    }),
    home: HOME,
    configDir: CONFIG_DIR,
    cwd: "/w",
    config: { file },
  });
  assert.equal(JSON.parse(await readFile(file, "utf8")).servers.slack.url, "https://slack.mine/mcp");
});

// ---- the credential store's own grants ----------------------------------------

/** A `Claude Code-credentials` item, shaped like the real one. */
function keychain(mcpOAuth: Record<string, unknown>) {
  return async () => ({
    value: JSON.stringify({
      mcpOAuth,
      claudeAiOauth: { accessToken: "account-token", expiresAt: Date.now() + 3_600_000 },
    }),
    code: 0,
    error: "",
  });
}

const SLACK_GRANT = {
  "slack|a1b2c3": {
    serverName: "slack",
    serverUrl: "https://slack.example.com/mcp",
    accessToken: "slack-secret-token",
    refreshToken: "slack-refresh",
    redirectUri: "http://localhost:1/callback",
    expiresAt: Date.now() + 3_600_000,
    scope: "read",
    discoveryState: { authorizationServerUrl: "https://slack.example.com", oauthMetadataFound: true },
  },
};

test("a server that exists ONLY as a keychain grant is synced — the Slack case", async () => {
  // What made this the reported bug: a connector authorized through Claude Code
  // leaves NOTHING in ~/.claude.json, so there was no definition to copy; and the
  // account token is deliberately withheld from third parties, so there was no
  // credential to point at either. The grant supplies both.
  const file = await registryFile();
  const code = await runSyncMcp([], {
    ...silent,
    readJson: () => null, // no config files at all
    keychain: keychain(SLACK_GRANT),
    home: HOME,
    cwd: "/w",
    config: { file },
  });
  assert.equal(code, 0);
  const text = await readFile(file, "utf8");
  const slack = JSON.parse(text).servers.slack;
  assert.equal(slack.url, "https://slack.example.com/mcp");
  assert.equal(
    slack.headers.Authorization,
    "Bearer ${keychain:Claude Code-credentials#mcpOAuth.slack|a1b2c3.accessToken}",
  );
  // Its OWN grant, never the account token — Slack would reject that one anyway.
  assert.equal(text.includes("claudeAiOauth"), false);
  // And no secret is written down, which is the invariant the whole command holds.
  assert.equal(text.includes("slack-secret-token"), false);
});

test("a plugin-namespaced name is renamed to a slug bough accepts", async () => {
  // The real failure, verbatim: `plugin:slack:slack  failed — invalid server name`.
  // Claude Code namespaces a plugin's server; bough's registry takes slugs. The
  // name is what you type in /mcp, so `slack` is the right answer and the rename
  // has to be said out loud.
  const file = await registryFile();
  const lines: string[] = [];
  const code = await runSyncMcp([], {
    out: (l) => lines.push(l),
    err: (l) => lines.push(l),
    readJson: () => null,
    keychain: keychain({
      "plugin:slack:slack|a1b2c3": {
        serverName: "plugin:slack:slack",
        serverUrl: "https://slack.example.com/mcp",
        accessToken: "t",
        expiresAt: Date.now() + 3_600_000,
      },
    }),
    home: HOME,
    cwd: "/w",
    config: { file },
  });
  assert.equal(code, 0);
  const servers = JSON.parse(await readFile(file, "utf8")).servers;
  assert.deepEqual(Object.keys(servers), ["slack"]);
  // The grant is keyed by CLAUDE CODE's name — the rename is bough's business only.
  assert.equal(
    servers.slack.headers.Authorization,
    "Bearer ${keychain:Claude Code-credentials#mcpOAuth.plugin:slack:slack|a1b2c3.accessToken}",
  );
  assert.match(lines.join("\n"), /renamed from plugin:slack:slack/);
});

test("a rename never lands on top of another server", () => {
  assert.equal(boughName("slack", new Set()), "slack"); // already valid: untouched
  assert.equal(boughName("plugin:slack:slack", new Set()), "slack");
  // `slack` is spoken for, so the whole name is slugified rather than merged.
  assert.equal(boughName("plugin:slack:slack", new Set(["slack"])), "plugin-slack-slack");
  assert.equal(
    boughName("plugin:slack:slack", new Set(["slack", "plugin-slack-slack"])),
    null,
  );
  assert.equal(boughName("Weird Name!", new Set()), "weird-name");
});

test("a configured server is matched to its grant by name, then by url", async () => {
  const file = await registryFile();
  const files = {
    [`${HOME}/.claude.json`]: {
      mcpServers: {
        // Same name as the grant…
        slack: { type: "http", url: "https://slack.example.com/mcp" },
        // …and a different name, same endpoint but for a trailing slash.
        notes: { type: "http", url: "https://notion.example.com/mcp/" },
      },
    },
  };
  await runSyncMcp([], {
    ...silent,
    readJson: reader(files),
    keychain: keychain({
      ...SLACK_GRANT,
      "notion|d4e5": {
        serverName: "notion",
        serverUrl: "https://notion.example.com/mcp",
        accessToken: "t",
        expiresAt: Date.now() + 3_600_000,
      },
    }),
    home: HOME,
    cwd: "/w",
    config: { file },
  });
  const servers = JSON.parse(await readFile(file, "utf8")).servers;
  assert.match(servers.slack.headers.Authorization, /mcpOAuth\.slack\|a1b2c3/);
  assert.match(servers.notes.headers.Authorization, /mcpOAuth\.notion\|d4e5/);
  // Matched, so the grant does not ALSO land as a second server under its own name.
  assert.equal("notion" in servers, false);
});

test("an expired grant is synced and said out loud, not silently sent", async () => {
  const file = await registryFile();
  const lines: string[] = [];
  await runSyncMcp([], {
    out: (l) => lines.push(l),
    err: (l) => lines.push(l),
    readJson: () => null,
    keychain: keychain({
      "slack|a1b2c3": {
        serverName: "slack",
        serverUrl: "https://slack.example.com/mcp",
        accessToken: "stale",
        expiresAt: Date.now() - 60_000,
      },
    }),
    home: HOME,
    cwd: "/w",
    config: { file },
  });
  assert.ok(JSON.parse(await readFile(file, "utf8")).servers.slack, "still registered");
  assert.match(lines.join("\n"), /expired/i);
  assert.match(lines.join("\n"), /claude/i); // …and how to refresh it
});

test("the same endpoint under a different name is ONE server, not two", async () => {
  // The screenshot that reported this: `slack` and `plugin-slack-slack` side by
  // side, same URL, and `linear` (authorized, working) beside a `linear-server`
  // that says "needs auth". The registry is keyed by name, so nothing downstream
  // would ever have noticed the duplicate.
  const file = await registryFile();
  const deps = { ...silent, readJson: () => null, home: HOME, cwd: "/w", config: { file } };
  await runSyncMcp([], { ...deps, keychain: keychain(SLACK_GRANT) });
  // Now the same endpoint arrives under Claude Code's namespaced name.
  await runSyncMcp([], {
    ...deps,
    keychain: keychain({
      "plugin:slack:slack|zz99": {
        serverName: "plugin:slack:slack",
        serverUrl: "https://slack.example.com/mcp",
        accessToken: "t",
        expiresAt: Date.now() + 3_600_000,
      },
    }),
  });
  assert.deepEqual(Object.keys(JSON.parse(await readFile(file, "utf8")).servers), ["slack"]);
});

test("with a duplicate already there, the credential lands on the better name", async () => {
  // The reported screen: `slack` AND `plugin-slack-slack`, same URL, both without
  // credentials. `boughName` refuses a name that is taken — and here the taker IS
  // this same server, so name-first logic renamed it into a duplicate of itself and
  // credentialed the ugly one. The endpoint decides first.
  const file = await registryFile();
  await Bun.write(
    file,
    JSON.stringify({
      servers: {
        slack: { url: "https://mcp.slack.com/mcp", args: [], env: {}, headers: {} },
        "plugin-slack-slack": { url: "https://mcp.slack.com/mcp", args: [], env: {}, headers: {} },
      },
      activations: {},
    }),
  );
  const lines: string[] = [];
  await runSyncMcp([], {
    out: (l) => lines.push(l),
    err: (l) => lines.push(l),
    readJson: () => null,
    keychain: keychain({
      "plugin:slack:slack|a1b2": {
        serverName: "plugin:slack:slack",
        serverUrl: "https://mcp.slack.com/mcp",
        accessToken: "t",
        expiresAt: Date.now() + 3_600_000,
      },
    }),
    home: HOME,
    cwd: "/w",
    config: { file },
  });
  const servers = JSON.parse(await readFile(file, "utf8")).servers;
  assert.match(servers.slack.headers.Authorization, /mcpOAuth\.plugin:slack:slack\|a1b2/);
  assert.deepEqual(servers["plugin-slack-slack"].headers, {});
  // …and the duplicate that was already there is named, since silence is how it
  // survives. Removing it is the user's call, not this command's.
  assert.match(lines.join("\n"), /are the same server/);
});

test("an entry with no credential GETS one when a grant exists for it", async () => {
  // Directly the reported complaint — "it should copy over everything, I shouldn't
  // need to auth". A server registered before this command could read grants sits
  // there with no Authorization at all, so the panel says "needs auth" and pressing
  // `a` fails against a provider with no dynamic registration. The credential is
  // right there on the machine.
  const file = await registryFile();
  const files = {
    [`${HOME}/.claude.json`]: {
      mcpServers: { slack: { type: "http", url: "https://slack.example.com/mcp" } },
    },
  };
  // First sync with no keychain at all: registered, unauthenticated.
  await runSyncMcp([], { ...silent, readJson: reader(files), home: HOME, cwd: "/w", config: { file } });
  assert.deepEqual(JSON.parse(await readFile(file, "utf8")).servers.slack.headers, {});

  // Second sync, now that the grant is readable — WITHOUT --force.
  const lines: string[] = [];
  await runSyncMcp([], {
    out: (l) => lines.push(l),
    err: (l) => lines.push(l),
    readJson: reader(files),
    keychain: keychain(SLACK_GRANT),
    home: HOME,
    cwd: "/w",
    config: { file },
  });
  const slack = JSON.parse(await readFile(file, "utf8")).servers.slack;
  assert.match(slack.headers.Authorization, /mcpOAuth\.slack\|a1b2c3/);
  assert.match(lines.join("\n"), /added the missing credential/);
});

test("an entry that already has a credential is left alone", async () => {
  // The complement: adding a header where there was none is not a clobber, but
  // REPLACING one is exactly what --force exists for.
  const file = await registryFile();
  const files = {
    [`${HOME}/.claude.json`]: {
      mcpServers: {
        slack: {
          type: "http",
          url: "https://slack.example.com/mcp",
          headers: { Authorization: "Bearer ${MY_OWN_TOKEN}" },
        },
      },
    },
  };
  const deps = { ...silent, readJson: reader(files), home: HOME, cwd: "/w", config: { file } };
  await runSyncMcp([], deps);
  await runSyncMcp([], { ...deps, keychain: keychain(SLACK_GRANT) });
  assert.equal(
    JSON.parse(await readFile(file, "utf8")).servers.slack.headers.Authorization,
    "Bearer ${MY_OWN_TOKEN}",
  );
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
    keychain: noKeychain,
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
    keychain: noKeychain,
    readJson: () => null,
    home: HOME,
    cwd: "/w",
    config: { file: await registryFile() },
  });
  assert.equal(code, 0);
  assert.match(lines.join("\n"), /no MCP servers found/);
});
