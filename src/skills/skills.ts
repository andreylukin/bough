/**
 * Skills: the `/name` instruction bundles a message pulls into one run (spec §16).
 *
 * A skill is a folder — `<dir>/<name>/SKILL.md` — with YAML-ish frontmatter
 * (`name`, `description`, optional `mcp:` server list) and a markdown body. When a
 * message names one, the body is appended to that turn's system prompt and the
 * servers it lists are granted to the turn. The folder name IS the invocation
 * token, which is why it wins over a `name:` field that disagrees with it: `/x`
 * loads `x/`, and a file that claims otherwise would send the user looking for a
 * skill that cannot be typed.
 *
 * THE INVARIANT THIS HOLDS: **a skill either arrives intact or is reported as
 * broken — never half-parsed into the prompt.** The old implementation split the
 * file on the literal `---` and, when the closing fence was missing, fell back to
 * "return the whole text", which pasted `---`, `name:` and `description:` into the
 * system prompt as if they were instructions. A prompt that is WRONG is worse than
 * one that is missing (the same rule `prompt/assemble.ts` holds about its sections),
 * so an unterminated fence withholds the body and produces a `note` instead: the
 * turn is told the named skill could not be loaded and why, and the model can say so
 * rather than silently behaving as though the user never typed it.
 *
 * SOURCES, FIRST NAME WINS: bundled (this module's own directory, which ships with
 * bough) then `~/.bough/skills` (the user's). Bundled first is spec §16's order and
 * it is deliberate in the direction people find surprising — a user folder cannot
 * shadow `history`, so the one skill the harness documents always means what the
 * documentation says.
 *
 * NOTHING IS CACHED. Every listing re-reads the directories and every load re-reads
 * the file, so a SKILL.md edited on disk takes effect on the very next turn with
 * nothing to invalidate. Discovery is a handful of small files; the read is cheaper
 * than the bug a stale cache produces.
 *
 * DI OVER GLOBALS: every entry point takes `{sources}` — a list of
 * `{source, dir}` pairs — and defaults to the real ones. A test passes two temp
 * directories and never touches `~/.bough`.
 *
 * PURE CORE: `parseFrontmatter`, `mentionIndex` and `activeSkills`'s selection are
 * pure functions over strings. Only `readSkill`/`listSkills` touch the filesystem,
 * and only through `Deno.readTextFileSync`/`readDirSync` — no db, no clock, no
 * network.
 *
 * Ported from `src/supervisor/skills.ts`. Deltas are marked `NOTE:`.
 */
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import type { PromptSkill } from "../prompt/assemble.ts";
import type { Message } from "../schema/parts.ts";
import type { Db } from "../types.ts";
import { userSkillsDir } from "../paths.ts";

// ---------------------------------------------------------------------------
// Sources
// ---------------------------------------------------------------------------

/** Where a skill came from. The panel shows it, so a user copy is never mistaken for the bundled one. */
export type SkillSourceName = "bundled" | "user";

/** One place skills are discovered from, in precedence order. */
export interface SkillSource {
  source: SkillSourceName;
  /** The directory that CONTAINS skill folders, not a skill folder itself. */
  dir: string;
}

/**
 * The bundled skills directory: this module's own folder.
 *
 * NOTE (port): the old tree resolved `../../skills` from `src/supervisor/`, i.e. a
 * repo-root `skills/`, and carried a `BOUGH_BUNDLED_SKILLS_DIR` override to make
 * that testable. Colocating the bundle with the module that reads it removes both —
 * the path cannot drift when the tree is renamed at cutover, and tests inject
 * `sources` rather than reaching for an env var (plan §0: DI over globals).
 */
export const BUNDLED_SKILLS_DIR: string = fileURLToPath(new URL(".", import.meta.url));

/** Bundled, then the user's. First name wins (spec §16). */
export function defaultSources(): SkillSource[] {
  return [
    { source: "bundled", dir: BUNDLED_SKILLS_DIR },
    { source: "user", dir: userSkillsDir() },
  ];
}

/** Every entry point takes this, so nothing here reads a global to find its files. */
export interface SkillOptions {
  sources?: readonly SkillSource[];
}

// ---------------------------------------------------------------------------
// Shapes
// ---------------------------------------------------------------------------

/** One discovered skill, body included. */
export interface Skill {
  /** The folder name — the token the user types after `/`. */
  name: string;
  /** `description:` from the frontmatter, or `""` when it has none. */
  description: string;
  /** `mcp:` servers this skill needs. Empty when it needs none. */
  mcp: string[];
  source: SkillSourceName;
  /** The skill's own folder — what `${SKILL_DIR}` resolves to in the body. */
  dir: string;
  /**
   * The body, frontmatter stripped and `${SKILL_DIR}` resolved. **Empty when
   * `error` is set** — a skill that could not be parsed contributes nothing to a
   * prompt rather than contributing its own frontmatter.
   */
  body: string;
  /** Why this skill cannot be loaded. Absent = it is fine. */
  error?: string;
}

/** What one message's `/name` tokens activate. */
export interface ActiveSkills {
  /** Bodies for `PromptInput.skills`, in the order the message named them. */
  skills: PromptSkill[];
  /** The union of the loaded skills' `mcp:` servers — the turn's added grant. */
  servers: string[];
  /** The names that loaded, in invocation order. For the UI and the logs. */
  names: string[];
  /**
   * Volatile prompt notes for skills the message named but that could not be
   * loaded. Each is a complete markdown section, as `PromptInput.notes` requires.
   * A named-and-broken skill must not fail silently: the turn happens either way,
   * and the model is the only thing that can tell the user their file is wrong.
   */
  notes: string[];
}

/** The token a body may use to point at its own folder. */
export const SKILL_DIR_TOKEN = "${SKILL_DIR}";

/**
 * A loadable skill name: one path segment, no separators, no leading dot.
 *
 * `GET /skills/:name` puts a request-supplied string into a path join, so this is
 * the guard on the server's own path construction (`paths.ts` `confine` says why
 * that is the case worth stopping). Names that come from `readDirSync` always pass.
 */
const NAME_RE = /^[A-Za-z0-9_][A-Za-z0-9._-]*$/;

// ---------------------------------------------------------------------------
// Frontmatter (pure)
// ---------------------------------------------------------------------------

const FENCE = "---";

/** The result of reading a SKILL.md's head. `error` set = the body is withheld. */
export interface Frontmatter {
  /** `key: value` pairs from the fenced block. Empty when there is no block. */
  fields: Record<string, string>;
  /** Everything after the closing fence — or the whole file when there is no block. */
  body: string;
  /** Set when the file opens a fence it never closes. `body` is then `""`. */
  error?: string;
}

/**
 * Parse a SKILL.md into its frontmatter fields and its body.
 *
 * Deliberately NOT a YAML parser (and deliberately not `@std/yaml`, which is a jsr
 * import this environment cannot reach): the frontmatter is three flat scalar
 * fields, and a real YAML dependency would accept nested documents this format has
 * no meaning for. What it does instead is line-based and total —
 *
 *   - no opening fence at all → the whole file is the body. A SKILL.md that is just
 *     instructions is a valid skill; its name comes from its folder either way.
 *   - an opening fence with no closing one → `error`, and NO body. This is the case
 *     the module header is about.
 *   - `key: value` lines, `#` comments and blank lines inside the block; anything
 *     else in there is skipped rather than fatal, because a stray line is not worth
 *     refusing a skill over.
 *
 * NOTE (port): the old version did `text.split("---")`, which mis-parsed any body
 * containing a horizontal rule or a `---` inside a code fence — the body was cut at
 * the wrong place and the tail was silently dropped. Scanning for a fence LINE is
 * what fixes that: only a line that is exactly `---` closes the block.
 */
export function parseFrontmatter(raw: string): Frontmatter {
  const text = raw.replace(/^\uFEFF/, "").replaceAll("\r\n", "\n");
  const lines = text.split("\n");

  let open = 0;
  while (open < lines.length && lines[open].trim() === "") open++;
  if (open >= lines.length || lines[open].trim() !== FENCE) {
    return { fields: {}, body: text.trim() };
  }

  let close = -1;
  for (let i = open + 1; i < lines.length; i++) {
    if (lines[i].trim() === FENCE) {
      close = i;
      break;
    }
  }
  if (close === -1) {
    return {
      fields: {},
      body: "",
      error: "its frontmatter opens with `---` and never closes. Add a `---` line " +
        "after the last field, or delete the opening one — until then the file has " +
        "no readable body and the skill cannot be loaded.",
    };
  }

  const fields: Record<string, string> = {};
  for (const line of lines.slice(open + 1, close)) {
    const trimmed = line.trim();
    if (trimmed === "" || trimmed.startsWith("#")) continue;
    const colon = trimmed.indexOf(":");
    if (colon <= 0) continue;
    const key = trimmed.slice(0, colon).trim();
    // First wins, so a duplicated key reads the way the file does top-down.
    if (key in fields) continue;
    fields[key] = unquote(trimmed.slice(colon + 1).trim());
  }
  return { fields, body: lines.slice(close + 1).join("\n").trim() };
}

/**
 * Strip ONE matched pair of YAML quotes.
 *
 * Most hand-written skills quote their description, and the quotes leaked into both
 * the panel and the model's view of the skill. Only a matched pair goes — an
 * apostrophe inside the text has to survive.
 */
function unquote(value: string): string {
  const q = value[0];
  if ((q === '"' || q === "'") && value.length >= 2 && value.endsWith(q)) {
    return value.slice(1, -1);
  }
  return value;
}

/** `mcp: chrome-devtools, linear` or `mcp: [a, b]` → `["chrome-devtools", "linear"]`. */
export function parseList(value: string): string[] {
  const inner = value.trim().replace(/^\[/, "").replace(/\]$/, "");
  return inner.split(",").map((s) => unquote(s.trim())).filter((s) => s !== "");
}

// ---------------------------------------------------------------------------
// Discovery (filesystem)
// ---------------------------------------------------------------------------

/** Read one candidate folder. `null` = there is no SKILL.md, so it is not a skill. */
function readSkill(source: SkillSourceName, root: string, name: string): Skill | null {
  if (!NAME_RE.test(name)) return null;
  const dir = join(root, name);
  let text: string;
  try {
    text = Deno.readTextFileSync(join(dir, "SKILL.md"));
  } catch {
    return null;
  }
  const fm = parseFrontmatter(text);
  return {
    name,
    description: fm.fields.description ?? "",
    mcp: parseList(fm.fields.mcp ?? ""),
    source,
    dir,
    // `${SKILL_DIR}` resolves to the skill's OWN folder, so a body can point at a
    // helper script that lives next to its SKILL.md regardless of the session's
    // workspace (spec §16).
    body: fm.body.replaceAll(SKILL_DIR_TOKEN, dir),
    ...(fm.error ? { error: fm.error } : {}),
  };
}

/**
 * Every installed skill, sorted by name, first source wins on a collision.
 *
 * A directory that does not exist contributes nothing — a machine with no
 * `~/.bough/skills` is the normal case, not an error. Entries are walked in sorted
 * order so a listing does not depend on filesystem enumeration order.
 */
export function listSkills(opts: SkillOptions = {}): Skill[] {
  const out: Skill[] = [];
  const taken = new Set<string>();
  for (const { source, dir } of opts.sources ?? defaultSources()) {
    let entries: Deno.DirEntry[];
    try {
      entries = [...Deno.readDirSync(dir)];
    } catch {
      continue;
    }
    for (const entry of entries.sort((a, b) => a.name.localeCompare(b.name))) {
      if (!entry.isDirectory || taken.has(entry.name)) continue;
      const skill = readSkill(source, dir, entry.name);
      if (!skill) continue;
      taken.add(skill.name);
      out.push(skill);
    }
  }
  return out.sort((a, b) => a.name.localeCompare(b.name));
}

/** One skill by name, resolved in source order. `null` = no such skill. */
export function loadSkill(name: string, opts: SkillOptions = {}): Skill | null {
  if (!NAME_RE.test(name)) return null;
  for (const { source, dir } of opts.sources ?? defaultSources()) {
    const skill = readSkill(source, dir, name);
    if (skill) return skill;
  }
  return null;
}

// ---------------------------------------------------------------------------
// Invocation (pure)
// ---------------------------------------------------------------------------

/**
 * Where `message` names `/skill`, or -1.
 *
 * Anchored on a whitespace boundary before and a non-name character after, so
 * `/history` matches at the start of a line or mid-sentence, `/history-old` does
 * not match `history`, and a path like `/usr/bin/env` names nothing. The index is
 * returned rather than a boolean because it orders the activations: a message that
 * says `/review then /commit` gets them in that order, which is the order their
 * instructions are meant to be read in.
 */
export function mentionIndex(message: string, name: string): number {
  const escaped = name.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  return message.search(new RegExp(`(?:^|\\s)/${escaped}(?![\\w./-])`));
}

/**
 * Everything the message's `/name` tokens activate: the bodies for the prompt, the
 * union of their MCP servers, and a note for each named skill that is broken.
 *
 * The skill invocation IS the capability grant (spec §16): a skill that lists
 * `mcp: linear` gets that server connected for the turn without the user enabling
 * it separately, which is why `servers` is returned beside the bodies rather than
 * being something the caller has to dig out of the skill list.
 */
export function activeSkills(message: string, opts: SkillOptions = {}): ActiveSkills {
  const hits: { at: number; skill: Skill }[] = [];
  for (const skill of listSkills(opts)) {
    const at = mentionIndex(message, skill.name);
    if (at >= 0) hits.push({ at, skill });
  }
  hits.sort((a, b) => a.at - b.at);

  const skills: PromptSkill[] = [];
  const servers = new Set<string>();
  const names: string[] = [];
  const notes: string[] = [];
  for (const { skill } of hits) {
    if (skill.error || skill.body.trim() === "") {
      notes.push(brokenSkillNote(skill));
      continue;
    }
    names.push(skill.name);
    for (const server of skill.mcp) servers.add(server);
    skills.push({ name: skill.name, body: skill.body });
  }
  return { skills, servers: [...servers], names, notes };
}

/**
 * What the turn is told about a skill the user named and the harness could not
 * load.
 *
 * Addressed to the model because the model is the only thing in the loop that can
 * reach the user mid-turn. It says what was asked for, what is wrong with which
 * file, and what to do anyway — a turn must not stall on this, and it must not
 * pretend the `/name` was never typed.
 */
function brokenSkillNote(skill: Skill): string {
  const why = skill.error ?? "its SKILL.md has no body below the frontmatter.";
  return `## Skill /${skill.name} could not be loaded\n` +
    `The user's message named \`/${skill.name}\` and a skill folder exists at ` +
    `${skill.dir}, but ${why}\n\n` +
    `Its instructions are NOT in this prompt, so do not act as if you have them. ` +
    `Do the work the user asked for without it, and tell them the file needs fixing.`;
}

// ---------------------------------------------------------------------------
// The turn's skills
// ---------------------------------------------------------------------------

/**
 * The text of the newest USER message — the one whose `/name` tokens this turn
 * honors.
 *
 * Newest rather than "the message that started the turn" because a turn can also
 * begin from a queued drain or a system note (spec §5, §7), and in both cases the
 * user's latest instruction is still the one that decided which skills apply.
 * `system` notes are skipped deliberately: a subagent's report or a job exit is the
 * harness talking, and a `/name` quoted in one is not an invocation.
 */
export function invokingText(messages: readonly Message[]): string {
  for (let i = messages.length - 1; i >= 0; i--) {
    const message = messages[i];
    if (message.role !== "user") continue;
    return message.parts
      .filter((p): p is Extract<Message["parts"][number], { type: "text" }> => p.type === "text")
      .map((p) => p.text)
      .join("\n");
  }
  return "";
}

/**
 * The skills active for a session's current turn, read fresh from the database and
 * the filesystem.
 *
 * Takes the narrowest slice of `Db` it needs, so a test drives it with a literal
 * `{messagesFor: () => [...]}` and no database at all. The session's OWN messages
 * are what is read: a fork's seeded copies count (they are that branch's history),
 * an ancestor's do not (the skill applied to the turn that named it, not forever).
 */
export function turnSkills(
  db: Pick<Db, "messagesFor">,
  sessionId: string,
  opts: SkillOptions = {},
): ActiveSkills {
  return activeSkills(invokingText(db.messagesFor(sessionId)), opts);
}

/**
 * Widen a turn's OWN live MCP grant with the servers its skills asked for.
 *
 * `mcp/manager.ts` installs `mcpGrant` as a live getter over the session's
 * activations; this wraps that getter rather than replacing it with an array, for
 * the reason that module states — a frozen array would keep working after a human
 * revoked a grant mid-turn. The union is therefore recomputed on every read, and
 * the one read that matters for inheritance (a spawn) copies out the union, which
 * is exactly the snapshot spec §7 hands a subagent.
 *
 * **Never call this on an inherited grant.** A subagent may not widen what its
 * spawner had (`requireGranted` says so to the model); the caller checks
 * `ctx.mcpGrant === undefined` before binding, which is the only moment the two
 * cases are distinguishable.
 */
export function widenGrant<T extends { mcpGrant?: string[] }>(
  ctx: T,
  servers: readonly string[],
): T {
  if (servers.length === 0) return ctx;
  // The existing getter is read ONCE, here, and a plain value is snapshotted the
  // same way: reading `ctx.mcpGrant` from inside the new getter would call the new
  // getter, and the turn would hang on its own stack instead of resolving a grant.
  const existing = Object.getOwnPropertyDescriptor(ctx, "mcpGrant")?.get?.bind(ctx);
  const snapshot = existing ? [] : [...(ctx.mcpGrant ?? [])];
  const base = existing ?? (() => snapshot);
  Object.defineProperty(ctx, "mcpGrant", {
    get: () => [...new Set([...(base() ?? []), ...servers])],
    enumerable: true,
    configurable: true,
  });
  return ctx;
}
