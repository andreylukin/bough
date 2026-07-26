/**
 * The supervisor's system prompt: the base contract plus the conditional sections
 * the turn runner appends (delegation, subagent reports, workspace/scratchpad
 * notes, AGENTS.md project rules). Pure text authoring — turn.ts resolves the
 * runtime facts (which capabilities are wired, which subagents run) and assembles
 * the pieces; nothing here touches the db or the loop.
 *
 * Cache contract for new sections: turn.ts assembles two tiers (see its comment
 * at the `system` / `systemVolatile` split). Constant text goes in the STABLE
 * tier; anything interpolating a per-session fact (a path, an id, a catalog)
 * must go in the VOLATILE tier, or it defeats cross-session prompt caching.
 *
 * Formatting contract (2026-07-20 reformat): sections join with "\n", carry a
 * "## Section" header, and separate discrete rules with blank lines — one rule
 * per block, CC-prompt style — instead of the old single run-on paragraph
 * (`.join(" ")`). The reformat was content-preserving (same sentences); per the
 * bench protocol, wording changes ride the next sweep, not this file's layout.
 * Appended sections (lspSection, workspaceNote, …) start with "\n\n#…" and
 * compose directly.
 */
import { join } from "node:path";
import { boughPath } from "../paths.ts";
import type { Session } from "../schema/parts.ts";
import { MAX_SPAWNS_PER_TURN, MAX_TREE_CONCURRENT } from "../subagent.ts";
import { SPEC_GUIDE } from "../server/jsonrender/catalog.ts";

// ---- prompt sections --------------------------------------------------------
// ./prompt/<name>.md IS the prompt — the single source of truth. Two optional
// layers may shadow a section, resolved per file at module load:
//   1. an explicit per-session dir (bough exec --prompt-dir / session.promptDir)
//   2. BOUGH_PROMPT_DIR — the tuner (bench/tune) points this at a variant's
//      section dir during a sweep, so a candidate is measured without editing the
//      repo. Unset in normal operation.
// An override dir may carry only the sections it changes; anything missing falls
// through to ./prompt.
//
// There used to be a third layer: a TS copy of every section inlined here as a
// string array, seeding ./prompt and standing in when a file was unreadable. It
// cost 217 lines and bought a drift bug — the two copies disagreed silently, and
// the deleted egress-gate paragraph survived in the builtin long after the .md
// was corrected, ready to reappear the moment a read failed. A prompt that is
// wrong is worse than a prompt that is missing, so a missing section is now fatal.
const PROMPT_DIR = Deno.env.get("BOUGH_PROMPT_DIR");
const DEFAULT_PROMPT_DIR = new URL("./prompt/", import.meta.url);

function promptSection(file: string, prefix = "", overrideDir?: string): string {
  const read = (path: string | URL) => prefix + Deno.readTextFileSync(path).trim();
  for (const dir of [overrideDir, PROMPT_DIR]) {
    if (dir) {
      try {
        return read(join(dir, file));
      } catch { /* not in this override dir — fall through to the next layer */ }
    }
  }
  try {
    return read(new URL(file, DEFAULT_PROMPT_DIR));
  } catch (e) {
    throw new Error(
      `cannot read the prompt section ${file} from ${DEFAULT_PROMPT_DIR.pathname} ` +
        `(${(e as Error).message}). bough cannot run without its prompt — this is a ` +
        `broken install or an incomplete checkout, not a recoverable condition.`,
    );
  }
}

export const SYSTEM = promptSection("system.md");

// Delegation section, appended only for sessions that may spawn (not subagents).

export const SYSTEM_DELEGATION = promptSection("delegation.md", "\n\n");

// Appended for every subagent turn: its final text is the report consumed by the
// spawner, so cap it — verbose reports bloat the parent's context.

export const SYSTEM_SUBAGENT = promptSection("subagent.md", "\n\n");

// Reduced delegation section for subagent turns: blocking only. A detached spawn
// could outlive this turn and mutate the branch after its report went upward.

export const SYSTEM_DELEGATION_NESTED = promptSection("delegation-nested.md", "\n\n");

/** The five supervisor prompt sections, resolved for a specific session's
 * `promptDir` override (undefined = the module-level exports above). The turn
 * runner calls this per turn so a prompt variant pinned on the session takes
 * effect with NO server restart. Passing undefined returns the process defaults,
 * byte-identical to the SYSTEM/… consts, so cache sharing is unaffected in normal
 * operation. */
export function resolveSystemSections(overrideDir?: string) {
  if (!overrideDir) {
    return {
      SYSTEM,
      SYSTEM_DELEGATION,
      SYSTEM_DELEGATION_NESTED,
      SYSTEM_SUBAGENT,
    };
  }
  return {
    SYSTEM: promptSection("system.md", "", overrideDir),
    SYSTEM_DELEGATION: promptSection("delegation.md", "\n\n", overrideDir),
    SYSTEM_DELEGATION_NESTED: promptSection("delegation-nested.md", "\n\n", overrideDir),
    SYSTEM_SUBAGENT: promptSection("subagent.md", "\n\n", overrideDir),
  };
}

// ---- delegation fit gate ---------------------------------------------------
// Bench finding (predictions.jsonl, 2026-07-20): the supervisor NEVER delegates
// on its own initiative (0 self-initiated subagents across 240+ sessions), and
// an always-on prose nudge was refuted twice — inert on weak models, pure token
// cost on every turn ("prompt dilution"). So the push is gated STRUCTURALLY:
// a conservative detector on the triggering user message injects ONE short
// section — decision rule + literal spawn() code shape — only when the request
// is shaped like independent fan-out work. Cohesive requests see a byte-identical
// prompt. False positives are harmless by design: the note is a reminder that
// ends with "do it yourself", never a command.

/** Count words that make an independence adjective plural-scoped ("four
 * independent modules") rather than incidental ("the unrelated Report class"). */
const COUNT = /\b(\d+|two|three|four|five|six|seven|eight|nine|ten|several|multiple|many)\b/i;
/** Independence adjectives — only meaningful next to a count (same sentence). */
const INDEPENDENCE = /\b(independent|unrelated|separate)\b/i;
/** Explicit parallel intent stands on its own. */
const PARALLEL_INTENT = /\b(in parallel|concurrently|independently|simultaneously)\b/i;
/** "each … its own" — the distributive shape of per-part work. */
const EACH_ITS_OWN = /\beach\b[^.?!\n]{0,60}\bits own\b/i;
/** Fan-out survey verbs; deliberately excludes fix/find/update/rename/check,
 * which name cohesive single-cause work at least as often as fan-outs. */
const FANOUT_VERB =
  /\b(audit|review|research|investigate|survey|analy[sz]e|triage|summari[sz]e|compare)\b/i;
/** Distributive scope markers, only meaningful next to a fan-out verb. */
const DISTRIBUTIVE = /\b(across|each|every|all)\b/i;

/**
 * Does the request look decomposable into independent parts? Conservative on
 * purpose — calibrated against the full bench task bank (fires on fanout-bugs
 * and fanout-heavy, on nothing else; see prompt.test.ts). Signals:
 *   1. a count + an independence adjective in one sentence ("six independent modules")
 *   2. explicit parallel intent ("in parallel", "concurrently")
 *   3. "each … its own" ("each one has its own bug")
 *   4. a survey verb + a distributive marker in one sentence ("audit X across Y")
 *   5. three or more questions (a bundle of independent research questions)
 */
export function decomposableRequest(text: string): boolean {
  if (PARALLEL_INTENT.test(text)) return true;
  if (EACH_ITS_OWN.test(text)) return true;
  if ((text.match(/\?/g) ?? []).length >= 3) return true;
  // Same-sentence co-occurrence rules. Splitting on "." also splits filenames
  // (mod_a.py) — that only shrinks segments, i.e. errs conservative.
  const sentences = text.split(/[.?!\n]+/);
  return sentences.some((s) =>
    (INDEPENDENCE.test(s) && COUNT.test(s)) ||
    (FANOUT_VERB.test(s) && DISTRIBUTIVE.test(s))
  );
}

/**
 * The gated section, appended (root sessions only — spawn() exists there) when
 * the triggering request matches a decomposable shape. Carries the decision rule
 * and the literal code shape, so acting on it costs the model no invention.
 */
export function delegationHintNote(userText: string): string {
  if (!decomposableRequest(userText)) return "";
  return "\n\n# Delegation fit (this request)\n" +
    "This request looks decomposable. Decision rule: list the parts; if two or more " +
    "(a) touch disjoint files/areas and (b) need nothing from each other's results, " +
    "DELEGATE — spawn one subagent per part in your FIRST program instead of working " +
    "through them serially:\n" +
    "```js\n" +
    "const tasks = [\n" +
    '  "Fix the failing tests in src/parser/ (run: npm test parser). Touch only src/parser/. Report root cause + fix in 3 lines.",\n' +
    '  "Fix the failing tests in src/render/ (run: npm test render). Touch only src/render/. Report root cause + fix in 3 lines.",\n' +
    "];\n" +
    "const started = await Promise.allSettled(tasks.map((t) => spawn(t)));\n" +
    "```\n" +
    "Then work on anything left, or end your turn — each [subagent finished] report " +
    "arrives as a system note, wakes you, and its edits are already in the " +
    "workspace. Write each task string as a complete briefing (paths, commands, " +
    "acceptance criteria — the subagent sees nothing else). If the parts are NOT " +
    "truly independent, or the whole job fits in a program or two, ignore this note " +
    "and do it yourself.";
}

/**
 * A system-prompt section listing this session's background subagents that are
 * still running, so the model stays aware of in-flight delegated work across
 * turns — it can join() one or simply not re-delegate the same task. Empty when
 * nothing is running. (The caller resolves which sessions are running — this
 * module stays db-free.)
 */
export function runningSubagentsNote(running: Session[]): string {
  if (running.length === 0) return "";
  return "\n\n# Background subagents currently running\n" +
    running.map((s) =>
      `- "${s.title}" (${s.id}) — join("${s.id}") to wait for its result, or end your turn and its report will arrive as a system note.`
    ).join("\n");
}

/**
 * Tell the model where its tools actually operate. Without this it has zero cwd
 * information and tends to invent a container layout (`cd /workspace || cd /home`),
 * walking itself out of the real project.
 */
export function workspaceNote(cwd: string): string {
  return `\n\n# Workspace\nbash starts in ${cwd} and relative file paths resolve ` +
    "against it. This is the user's REAL checkout: your edits are immediately real, " +
    "nothing is copied or confined, and git is the source of truth for what changed " +
    "(`git status`, `git diff`). Deliver work with plain git through bash — " +
    "`git commit`, `git push`, `gh pr create` — but ONLY when the user asks; never " +
    "commit as a routine end-of-task step.";
}

/**
 * Point the agent at the per-session scratchpad for throwaway files. The workspace is
 * the user's own checkout, so a stray `./probe.json` really lands in their repo and shows
 * up in `git status`. The scratch dir is outside the repo and OS-reaped — the right home
 * for anything not meant to keep.
 */
export function scratchpadNote(scratchDir: string): string {
  return `\n\n# Scratchpad\n${scratchDir} is a writable per-session temp dir OUTSIDE ` +
    "the workspace. Put ALL throwaway files there — intermediate data, temp scripts, " +
    "probe outputs, downloads — NOT in the workspace and NOT in /tmp. Files written " +
    "into the workspace are real changes to the user's repo: they show up in " +
    "`git status` and in the session's diff. Use absolute paths " +
    "under the scratchpad; it's already created.";
}

/** Read one instructions file (capped, trimmed), or null if absent/empty. */
async function readOneAgentsFile(path: string): Promise<string | null> {
  try {
    const text = await Deno.readTextFile(path);
    if (!text.trim()) return null;
    // Cap so a huge file can't crowd out the task; the model can read the rest itself.
    return text.length > 12_000 ? text.slice(0, 12_000).trim() + "\n…(truncated)" : text.trim();
  } catch {
    return null;
  }
}

/**
 * Build the "Project rules" system-prompt section from two AGENTS.md files:
 * a global one at ~/.bough/AGENTS.md (applies to every workspace) and the
 * workspace root's own. The name is always exactly AGENTS.md — never other
 * tools' files (no CLAUDE.md, no AGENT.md). Both are included when present;
 * the project file is authoritative and overrides the global on conflict.
 * Returns null if neither exists. (BOUGH_GLOBAL_AGENTS overrides the global
 * path, chiefly for tests.)
 */
export async function readAgentsFile(cwd: string): Promise<string | null> {
  const globalPath = Deno.env.get("BOUGH_GLOBAL_AGENTS") ?? boughPath("AGENTS.md");
  const [global, project] = await Promise.all([
    readOneAgentsFile(globalPath),
    readOneAgentsFile(join(cwd, "AGENTS.md")),
  ]);
  if (!global && !project) return null;
  let out = "\n\n# Project rules (AGENTS.md)\nTreat the following as authoritative for " +
    'build/test commands, conventions, and what "done" means.';
  if (global) {
    out += "\n\n## Global rules (~/.bough/AGENTS.md) — apply to every workspace\n\n" + global;
  }
  if (project) {
    out +=
      "\n\n## Workspace rules (AGENTS.md) — this project, override the global on conflict\n\n" +
      project;
  }
  return out;
}
