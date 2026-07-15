/**
 * Skills: named, reusable
 * instruction bundles the human pulls into a run by typing `/<name>` in their
 * message (e.g. `/commit tidy this up`). A skill is a folder
 * `<dir>/<name>/SKILL.md` with YAML-ish frontmatter (`name`, `description`, and
 * optionally `mcp:` — a comma list of MCP server names the skill needs) and
 * a markdown body of instructions. When a message names an installed skill, the
 * harness appends that skill's body to the supervisor's system prompt for the
 * run and connects its MCP servers (see turn.ts). Three sources, first name wins:
 *   1. BUILTINS — inline in this file;
 *   2. bundled — the repo's skills/ dir (ships with bough; `deno desktop`
 *      packaging must --include it);
 *   3. installed — ~/.bough/skills (the user's own).
 * `${SKILL_DIR}` in a file-based skill body resolves to the skill's folder, so
 * instructions can reference helper scripts that live next to the SKILL.md.
 *
 * Overrides: BOUGH_SKILLS_DIR (installed dir, tests), BOUGH_BUNDLED_SKILLS_DIR
 * (bundled dir, tests/packaging).
 */
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { homedir } from "node:os";

export interface Skill {
  name: string;
  description: string;
  /**
   * MCP servers (registry names — see mcp/config.ts) this skill needs. Invoking
   * the skill is what grants them for the turn: the turn runner connects each
   * one, injects its tool catalog, and bridges the mcp() host function.
   */
  mcp?: string[];
}

function dir(): string {
  return Deno.env.get("BOUGH_SKILLS_DIR") ?? join(homedir(), ".bough", "skills");
}

/** The repo's skills/ dir — skills that ship with bough as files, not code. */
function bundledDir(): string {
  return Deno.env.get("BOUGH_BUNDLED_SKILLS_DIR") ??
    join(dirname(fileURLToPath(import.meta.url)), "..", "..", "skills");
}

/**
 * Skills shipped with bough, available without an install. `/init` mirrors opencode's:
 * analyze the project and write an authoritative AGENTS.md (which future turns then load
 * automatically — see turn.ts readAgentsFile).
 */
const BUILTINS: Record<string, Skill & { body: string }> = {
  init: {
    name: "init",
    description: "Analyze the project and write an AGENTS.md at its root",
    body: [
      "Analyze this workspace and write a concise `AGENTS.md` at the repo root that a coding",
      "agent can treat as authoritative. Include: the build / test / lint / run commands (read",
      "them from package.json, deno.json, Makefile, etc. — don't invent), the project layout in",
      'a sentence or two, key conventions to follow, and what "done" means (which check must',
      "pass). Keep it tight — a page, not an essay. If an AGENTS.md already exists, update it",
      "rather than replacing it wholesale. Verify the commands you list actually exist.",
    ].join(" "),
  },
  "net-plugin": {
    name: "net-plugin",
    description: "Create and LIVE-TEST a Claw Patrol classifier plugin for a provider's API",
    body: `Create a Claw Patrol classifier plugin — a small declarative table that teaches
bough's egress gate one provider's verb vocabulary, so specific DESTRUCTIVE operations are
held or denied per-operation while reads keep flowing — and then PROVE it works with live
probes. The bough API is at http://127.0.0.1:\${BOUGH_PORT:-4321} and is reachable from your
shell (loopback bypasses the egress proxy).

## 1. Scope
Pin down from the user's request: the target host(s), which operations count as destructive,
and whether the plugin should expire (ttl). Ask only if genuinely ambiguous.

## 2. Ground the table in real traffic
\`curl -s localhost:4321/net/requests\` lists recent egress rows ({host, action, verdict}) —
the traffic your table must classify. Combine with what you know of the provider's API paths.

## 3. Draft the spec
JSON, exactly this shape:
{"meta":{"name":"<lowercase-slug>","description":"<one line>","hosts":["api.example.com"]},
 "ops":[{"match":"GET *","kind":"read"},
        {"match":"POST /v1/refunds*","kind":"write","verb":"<name>:refund"}],
 "fixtures":[{"req":{"method":"POST","path":"/v1/refunds"},"expect":{"kind":"write","verb":"<name>:refund"}},
             {"req":{"method":"GET","path":"/v1/charges"},"expect":{"kind":"read"}}]}
Rules: the FIRST matching op wins, so specific rows go before catch-alls. A request matching
no row classifies "unknown" and FAILS CLOSED (denied in read_only mode, held in review) — do
not add a permissive catch-all beyond "GET *" → read. Give every destructive op a stable verb
"<name>:<op>" so the rule set can target it. Fixtures are mandatory and must cover every
destructive op plus at least one read.

## 4. Install to the library, then enable for THIS branch
Plugin files are a shared library; they gate nothing until enabled per scope.
- Install: POST /net/plugins/install with {"plugin": <spec>}. The server re-validates and
  re-runs the fixtures; on a 400, fix the spec and retry. If it answers "already exists",
  the plugin is already in the library — skip straight to enabling it.
- Enable for this branch: POST "/net/plugins/<name>/enable?session=$BOUGH_SESSION" with
  body {"ttl": "2h"} — ttl only when the user wants expiry ("90m" | "2h" | "7d" forms).
  $BOUGH_SESSION is in your shell env. The TTL belongs to THIS activation: the same plugin
  can be enabled elsewhere with a different ttl or none. When it lapses, this branch's
  hosts fail closed again. Omit ?session= ONLY if the user explicitly wants it global.
  Disable later with POST "/net/plugins/<name>/disable?session=$BOUGH_SESSION".

Enabling is also the trust decision: while the activation is live, the plugin's hosts skip
the allowHosts gate and every request is judged by your ops table instead (unmatched ops
still fail closed as "unknown"). No separate allowHosts edit is needed.

## 5. Live-test — this is the point; never skip it
Your shell's egress runs through the gate, so probe the REAL host:
- one read probe: expect it to pass and the feed to show the right verb;
- one probe per destructive op, always with obviously-fake IDs (so even a misclassified
  probe that reaches the origin cannot damage anything): expect the proxy to answer
  403 "blocked by Claw Patrol" (deny) or to park the request (hold). Use \`curl --max-time 5\`
  — a held probe blocks until a human resolves it, and timing out is fine: the feed row is
  the evidence.
Then \`curl -s localhost:4321/net/requests\` and check each probe's row for the plugin's verb
and the expected verdict. A destructive probe that came back ALLOWED means the table is
wrong — fix the plugin and re-test before reporting. To force a verb to be gated regardless
of mode, merge it into the branch's holdVerbs/denyVerbs (same GET → edit → PUT on
"/net/policy?session=$BOUGH_SESSION").

## 6. Report
Show the ops table, each probe with its verb + verdict, the plugin file path, and the expiry
if one was set.`,
  },
  mcp: {
    name: "mcp",
    description:
      "Manage this session's MCP servers: status, register, enable/disable, prove they run",
    body: `Manage MCP servers for this session via the bough API at
http://127.0.0.1:\${BOUGH_PORT:-4321} (loopback is reachable from your shell and bypasses
the egress proxy). $BOUGH_SESSION in your shell env is this session's id; carry
?session=$BOUGH_SESSION on every call below and omit it ONLY when the user explicitly
wants the global scope. Do what the user's message asks — status / register / enable /
disable / restart / auth — and ALWAYS finish with step 3: a setup you didn't prove
running is not done. Never read ~/.bough/mcp/tokens or paste its contents anywhere.

## 1. Ground first
\`await mcpStatus()\` (host function, always available) returns {registry, auth, active,
connections}: the configured servers, OAuth state for remote ones, which names are
enabled for this scope, and live connections (alive, toolCount, stderrTail when
something wrote to stderr). The same JSON is at GET /mcp/servers?session=$BOUGH_SESSION
if you need it from the shell.

## 2. Act
- status: nothing more to do — skip to step 3's report using what you have.
- register / update ONE server: PUT "/mcp/servers/<name>" with just that entry —
  never round-trip the whole registry. Stdio shape:
  \`curl -s -X PUT localhost:4321/mcp/servers/<name> -H 'content-type: application/json'
  -d '{"command":"npx","args":["-y","--prefer-offline","some-mcp"],"env":{"TOKEN":"\${SOME_VAR}"}}'\`
  — env values may reference \${VAR} from bough's own environment (that is where
  secrets belong; they reach the server child only — never put a literal secret in the
  entry). Node-based servers also want "NODE_USE_ENV_PROXY":"1" so their fetch
  respects the egress proxy. Remote shape: '{"url":"https://mcp.example.com/mcp"}'
  (then auth, below). A 400 names the problem — fix and re-PUT. Registering grants
  nothing by itself: enable (or a skill's \`mcp:\` frontmatter) is what grants.
- unregister: DELETE "/mcp/servers/<name>" — removes the entry and its connections.
- enable <name>: POST "/mcp/servers/<name>/enable?session=$BOUGH_SESSION", body
  {"ttl":"2h"} only when the user wants expiry ("90m" | "2h" | "7d" forms; a lapsed
  grant fails closed). The mcp() host function appears at the START of the next turn,
  but you can and must still verify the server NOW — step 3.
- disable <name>: POST "/mcp/servers/<name>/disable?session=$BOUGH_SESSION" — removes
  the activation and drops any live connection.
- restart <name>: POST "/mcp/servers/<name>/restart?session=$BOUGH_SESSION" — drops and
  respawns a currently-connected server. For a server that isn't connected, use
  /connect (step 3) instead.
- auth <name> (remote servers): POST "/mcp/servers/<name>/auth". {"status":"authorized"}
  → tokens already valid, say so. {"status":"redirect","authorizationUrl":...} → SHOW
  the human that URL and tell them to open it and approve; the browser lands on bough's
  /mcp/oauth/callback, which stores the tokens. Poll GET /mcp/servers (sleep 2 between
  tries) until auth.<name>.authorized is true, then step 3.
- logout <name>: DELETE "/mcp/servers/<name>/auth" — forgets tokens and drops the
  server's connections everywhere; the next use needs auth again.

## 3. Prove it runs — this is the point; never skip it
POST "/mcp/servers/<name>/connect?session=$BOUGH_SESSION" connects (or reuses) the
server for THIS session right now, under the same sandbox/proxy confinement a turn
uses, and returns {connected, status, tools}. connected:false comes with the error and
stderrTail — a typo'd command, missing binary, or unset \${VAR} surfaces HERE, not on
some later turn. Fix the entry, re-PUT, re-connect until it lists tools.
Do NOT call the server's tools from your shell to test it: tool calls belong to the
mcp() host function, which appears next turn and passes the egress gate (write-kind
tools may park on human approval — expected, not an error).

## 4. Report
Report each relevant server: registered command/url, enabled scope(s) + expiry,
authorized flag for remote ones, and the proof — connected with N tools (name a few),
or the exact failure line from stderrTail. If you enabled a server this turn, say its
mcp() tools become callable on the next message.`,
  },
};

/** Bundled first, then installed — matching the name-resolution order in loadBody. */
function skillDirs(name: string): string[] {
  return [join(bundledDir(), name), join(dir(), name)];
}

/** Bundled + installed skills from one source dir; helper for listSkills. */
function listDir(root: string, taken: Set<string>, out: Skill[]): void {
  let entries: Deno.DirEntry[];
  try {
    entries = [...Deno.readDirSync(root)];
  } catch {
    return;
  }
  for (const e of entries) {
    if (!e.isDirectory || taken.has(e.name)) continue;
    try {
      const text = Deno.readTextFileSync(join(root, e.name, "SKILL.md"));
      const mcp = mcpOf(text);
      out.push({ name: e.name, description: descriptionOf(text), ...(mcp.length ? { mcp } : {}) });
      taken.add(e.name);
    } catch {
      // folder without a SKILL.md — not a skill
    }
  }
}

/** All skills (builtins + bundled + installed; first name wins), for discovery/UI. */
export function listSkills(): Skill[] {
  const skills: Skill[] = Object.values(BUILTINS).map((b) => ({
    name: b.name,
    description: b.description,
    ...(b.mcp?.length ? { mcp: b.mcp } : {}),
  }));
  const taken = new Set(Object.keys(BUILTINS));
  listDir(bundledDir(), taken, skills);
  listDir(dir(), taken, skills);
  return skills.sort((a, b) => a.name.localeCompare(b.name));
}

/**
 * The markdown body of a skill (instructions), frontmatter stripped. Resolution
 * order: builtins, bundled, installed. `${SKILL_DIR}` in a file-based body is
 * replaced with the skill's folder so instructions can point at helper scripts
 * that live next to the SKILL.md regardless of the session's workspace.
 */
export function loadBody(name: string): string | null {
  if (name in BUILTINS) return BUILTINS[name].body;
  for (const folder of skillDirs(name)) {
    try {
      const text = Deno.readTextFileSync(join(folder, "SKILL.md"));
      return stripFrontmatter(text).replaceAll("${SKILL_DIR}", folder);
    } catch {
      // not in this source — try the next
    }
  }
  return null;
}

/**
 * Everything the message's `/<name>` invocations activate: the prompt sections for
 * each named skill, and the union of their `mcp:` server references (the turn
 * runner connects those and bridges the mcp() host function — the skill invocation
 * IS the capability grant). Empty when none are named (or installed).
 */
export function activeSkills(message: string): { sections: string; servers: string[] } {
  const sections: string[] = [];
  const servers = new Set<string>();
  for (const skill of listSkills()) {
    if (!mentions(message, skill.name)) continue;
    const body = loadBody(skill.name);
    if (body === null) continue;
    for (const server of skill.mcp ?? []) servers.add(server);
    sections.push(
      `\n\n# Active skill: /${skill.name}\n` +
        `The human invoked the \`/${skill.name}\` skill for this task. Follow its ` +
        "instructions, which are authoritative for how to do the work (but never " +
        "override the safety and sandbox rules above):\n\n" +
        body.trim(),
    );
  }
  return { sections: sections.join(""), servers: [...servers] };
}

/** The supervisor-prompt sections alone (see activeSkills). */
export function activeFor(message: string): string {
  return activeSkills(message).sections;
}

/** True when `message` contains the token `/<name>` at a word boundary. */
function mentions(message: string, name: string): boolean {
  const escaped = name.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  return new RegExp(`(^|\\s)/${escaped}(\\s|$)`).test(message);
}

/** `mcp:` from the frontmatter as a list — "chrome-devtools, linear" forms. */
function mcpOf(text: string): string[] {
  for (const line of frontmatterLines(text)) {
    const idx = line.indexOf(":");
    if (idx > 0 && line.slice(0, idx).trim() === "mcp") {
      return line.slice(idx + 1).split(",").map((s) => s.trim()).filter(Boolean);
    }
  }
  return [];
}

/** `description:` from the frontmatter, or "" if absent. */
function descriptionOf(text: string): string {
  for (const line of frontmatterLines(text)) {
    const idx = line.indexOf(":");
    if (idx > 0 && line.slice(0, idx).trim() === "description") {
      return line.slice(idx + 1).trim();
    }
  }
  return "";
}

function frontmatterLines(text: string): string[] {
  if (!text.trimStart().startsWith("---")) return [];
  const parts = text.split("---");
  return parts.length >= 2 ? parts[1].split("\n") : [];
}

function stripFrontmatter(text: string): string {
  if (!text.trimStart().startsWith("---")) return text;
  const parts = text.split("---");
  return parts.length >= 3 ? parts.slice(2).join("---").trimStart() : text;
}
