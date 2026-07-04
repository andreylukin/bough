/**
 * Skills: named, reusable
 * instruction bundles the human pulls into a run by typing `/<name>` in their
 * message (e.g. `/commit tidy this up`). A skill is a folder
 * `~/.bough/skills/<name>/SKILL.md` with YAML-ish frontmatter (`name`,
 * `description`) and a markdown body of instructions. When a message names an
 * installed skill, the harness appends that skill's body to the supervisor's
 * system prompt for the run (see turn.ts).
 *
 * Override the directory with BOUGH_SKILLS_DIR (tests).
 */
import { join } from "node:path";
import { homedir } from "node:os";

export interface Skill {
  name: string;
  description: string;
}

function dir(): string {
  return Deno.env.get("BOUGH_SKILLS_DIR") ?? join(homedir(), ".bough", "skills");
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
};

function skillFile(name: string): string {
  return join(dir(), name, "SKILL.md");
}

/** Installed skills (name + one-line description) plus builtins, for discovery/UI. */
export function listSkills(): Skill[] {
  const skills: Skill[] = Object.values(BUILTINS).map((b) => ({
    name: b.name,
    description: b.description,
  }));
  let entries: Deno.DirEntry[];
  try {
    entries = [...Deno.readDirSync(dir())];
  } catch {
    entries = [];
  }
  for (const e of entries) {
    if (!e.isDirectory || e.name in BUILTINS) continue;
    try {
      const text = Deno.readTextFileSync(skillFile(e.name));
      skills.push({ name: e.name, description: descriptionOf(text) });
    } catch {
      // folder without a SKILL.md — not a skill
    }
  }
  return skills.sort((a, b) => a.name.localeCompare(b.name));
}

/** The markdown body of a skill (instructions), frontmatter stripped. Builtins first. */
export function loadBody(name: string): string | null {
  if (name in BUILTINS) return BUILTINS[name].body;
  try {
    return stripFrontmatter(Deno.readTextFileSync(skillFile(name)));
  } catch {
    return null;
  }
}

/**
 * The supervisor-prompt section for every installed skill the message invokes via
 * `/<name>`. Empty when none are named (or installed).
 */
export function activeFor(message: string): string {
  const sections: string[] = [];
  for (const skill of listSkills()) {
    if (!mentions(message, skill.name)) continue;
    const body = loadBody(skill.name);
    if (body === null) continue;
    sections.push(
      `\n\n# Active skill: /${skill.name}\n` +
        `The human invoked the \`/${skill.name}\` skill for this task. Follow its ` +
        "instructions, which are authoritative for how to do the work (but never " +
        "override the safety and sandbox rules above):\n\n" +
        body.trim(),
    );
  }
  return sections.join("");
}

/** True when `message` contains the token `/<name>` at a word boundary. */
function mentions(message: string, name: string): boolean {
  const escaped = name.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  return new RegExp(`(^|\\s)/${escaped}(\\s|$)`).test(message);
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
