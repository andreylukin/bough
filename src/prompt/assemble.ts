/**
 * System-prompt assembly: which markdown sections a given turn gets, and in what
 * order.
 *
 * THE INVARIANT THIS HOLDS: **the prompt IS the capability grant.** A section that
 * documents a host function is included only when that host function is actually
 * bridged for this turn, and a bridged function always has its section. Spec §6
 * states it from the model's side — "a host function exists only when the prompt
 * grants it, the model never guesses at capabilities" — and this module is the only
 * place that can make it true. Get it wrong in one direction and the model calls a
 * verb that rejects with "unknown host function"; get it wrong in the other and a
 * granted capability is invisible and never used.
 *
 * The corollary, which is why the section list is data and not a template: adding a
 * host function means adding a section AND its condition, in one table, next to
 * every other one. There is no path by which a verb reaches the bridge without a
 * line here.
 *
 * WHY MARKDOWN FILES. `./<name>.md` IS the prompt — the single source of truth, and
 * the thing a human edits when the model misbehaves. There is deliberately no
 * inlined TypeScript copy of any section: the old tree kept one as a fallback, the
 * two copies drifted, and a deleted paragraph about an egress gate survived in the
 * builtin long after the .md was corrected — ready to reappear the moment a read
 * failed. A prompt that is WRONG is worse than one that is missing, so a missing
 * section is fatal (see `readSectionFile`).
 *
 * THE TWO TIERS. `system` is the stable prefix: byte-identical across sessions and
 * turns for a given (kind, capability) shape, so the provider's prompt cache can
 * share it. `systemVolatile` carries everything that interpolates a per-session
 * fact — the MCP catalog, skill bodies, and whatever notes the caller resolved
 * (workspace paths, running subagents). One volatile byte early in the prefix
 * defeats cross-session cache sharing, which is the whole reason for the split
 * (see `LlmParams` in `types.ts`).
 *
 * This module is PURE except for reading its own section files: no db, no clock, no
 * network. The caller resolves the runtime facts — which functions were bridged,
 * which MCP servers connected, whether the LSP backend answered — and passes them
 * in. That is what makes prompt assembly testable without a turn.
 */
import { readFileSync } from "node:fs";
import { createHash } from "node:crypto";
import type { HostFnName } from "../harness/protocol.ts";
import type { SessionKind } from "../schema/parts.ts";

// ---------------------------------------------------------------------------
// Inputs
// ---------------------------------------------------------------------------

/** One skill the user's message named, with `${SKILL_DIR}` already resolved. */
export interface PromptSkill {
  name: string;
  /** The SKILL.md body, verbatim. */
  body: string;
}

/** One tool on a connected MCP server, rendered as a single catalog line. */
export interface PromptMcpTool {
  name: string;
  /** Parameter shape, e.g. `({path, limit?})`. Defaults to `()`. */
  signature?: string;
  /** First line of the tool's description; longer text is the caller's to trim. */
  description?: string;
}

/**
 * One MCP server as the catalog renders it. A server that failed to connect is
 * listed WITH its error rather than omitted — silence invites the model to invent
 * tools that never came up (spec §10: answer from live state, not from memory).
 */
export interface PromptMcpServer {
  name: string;
  tools?: readonly PromptMcpTool[];
  error?: string;
}

/**
 * Everything assembly needs to know about a turn. All of it is resolved by the
 * caller: this module never asks the world anything.
 */
export interface PromptInput {
  /** Decides the delegation tier and the subagent framing (spec §6 table). */
  kind: SessionKind;
  /**
   * The host functions actually bridged for this turn. This is the capability
   * grant — every section that documents a verb is gated on its presence here.
   */
  granted: Iterable<HostFnName>;
  /** True when the LSP backend answered. Absent/false = no `lsp` section. */
  lsp?: boolean;
  /** Connected (or failed-to-connect) MCP servers. Empty = no MCP tools section. */
  mcpServers?: readonly PromptMcpServer[];
  /** Skills the user's message named. */
  skills?: readonly PromptSkill[];
  /**
   * Per-session notes the caller resolved — the workspace path, background
   * subagents still running, project rules. Appended verbatim to the VOLATILE
   * tier so they never poison the shared stable prefix. Each note is expected to
   * be a complete markdown section starting with its own heading.
   */
  notes?: readonly string[];
}

/** What the turn runner hands to `LlmClient.run`. */
export interface AssembledPrompt {
  /** The cacheable prefix. */
  system: string;
  /** The per-session suffix; `""` when there is nothing session-specific. */
  systemVolatile: string;
  /**
   * The ids included, in order — stable tier then volatile. Exposed because
   * "which sections did this turn get" is the thing tests and the UI want to
   * assert on, and reconstructing it by substring-matching the prose is exactly
   * the brittleness this avoids.
   */
  sections: SectionId[];
  /**
   * Each included section's id paired with the sha of the exact text that went
   * into the prefix, in the same order as `sections`.
   *
   * This exists for prompt attribution: an experiment that edits `shell.md` can
   * only be credited or blamed on turns whose prefix actually CONTAINED that
   * text, and inclusion here is conditional (see the section table), so "the
   * file was edited" and "the turn ran with the edit" are different facts. The
   * sha is over the section text rather than the file so a volatile section —
   * rendered, never read from disk — is fingerprinted on the same terms.
   */
  shas: SectionSha[];
}

/** One included section's identity: what it was, and the exact bytes it contributed. */
export interface SectionSha {
  id: SectionId;
  /** sha256 of the section text, truncated — collision-free at this scale, readable in a log. */
  sha: string;
}

// ---------------------------------------------------------------------------
// The section table
// ---------------------------------------------------------------------------

/** Ids of the file-backed (stable-tier) sections, in prompt order. */
export type SectionId =
  | "identity"
  | "shell"
  | "files"
  | "patch-grammar"
  | "ask"
  | "state"
  | "schedule"
  | "image"
  | "fetch"
  | "artifact"
  | "mcp-status"
  | "delegation"
  | "delegation-nested"
  | "workflow"
  | "subagent"
  | "printing"
  | "searching"
  | "lsp"
  | "network"
  | "ending"
  // volatile, rendered rather than read from a file
  | "mcp-tools"
  | "skills"
  | "notes";

/** The resolved facts a condition asks about. */
interface Facts {
  kind: SessionKind;
  has(fn: HostFnName): boolean;
  lsp: boolean;
}

interface SectionSpec {
  id: SectionId;
  file: string;
  /** Included when this returns true. */
  when(f: Facts): boolean;
}

/** A session that may `spawn()` and start workflows: everything but a delegate. */
const TOP_LEVEL_KINDS: readonly SessionKind[] = ["root", "fork", "compaction"];

const ALWAYS = () => true;

/**
 * The stable tier, in prompt order. This table IS spec §6's inclusion table; read
 * the two side by side.
 *
 * Note what the conditions are made of: a session kind, or a bridged host function
 * — never a flag someone remembered to set. `delegation` is gated on `spawn`
 * because top-level delegation is precisely the tier where detaching is legal, and
 * `delegation-nested` on `agent` because a depth-2 subagent (still `kind:
 * "subagent"`) is bridged nothing and must therefore be told nothing.
 */
const SECTIONS: readonly SectionSpec[] = [
  { id: "identity", file: "identity.md", when: ALWAYS },
  { id: "shell", file: "shell.md", when: (f) => f.has("bash") },
  { id: "files", file: "files.md", when: (f) => f.has("view") },
  { id: "patch-grammar", file: "patch-grammar.md", when: (f) => f.has("patch") },
  { id: "ask", file: "ask.md", when: (f) => f.has("ask") },
  { id: "state", file: "state.md", when: (f) => f.has("state") },
  { id: "schedule", file: "schedule.md", when: (f) => f.has("schedule") },
  { id: "image", file: "image.md", when: (f) => f.has("image") },
  { id: "fetch", file: "fetch.md", when: (f) => f.has("fetch") },
  { id: "artifact", file: "artifact.md", when: (f) => f.has("artifact") },
  { id: "mcp-status", file: "mcp-status.md", when: (f) => f.has("mcpStatus") },
  {
    id: "delegation",
    file: "delegation.md",
    when: (f) => TOP_LEVEL_KINDS.includes(f.kind) && f.has("spawn"),
  },
  {
    id: "delegation-nested",
    file: "delegation-nested.md",
    when: (f) => f.kind === "subagent" && f.has("agent"),
  },
  {
    id: "workflow",
    file: "workflow.md",
    when: (f) => TOP_LEVEL_KINDS.includes(f.kind) && f.has("workflow"),
  },
  {
    id: "subagent",
    file: "subagent.md",
    when: (f) => f.kind === "subagent" || f.kind === "workflow_agent",
  },
  { id: "printing", file: "printing.md", when: ALWAYS },
  { id: "searching", file: "searching.md", when: ALWAYS },
  { id: "lsp", file: "lsp.md", when: (f) => f.lsp && f.has("lsp") },
  { id: "network", file: "network.md", when: ALWAYS },
  { id: "ending", file: "ending.md", when: ALWAYS },
];

/** Every stable section's id and file — exported so a test can walk the whole set. */
export const SECTION_FILES: readonly { id: SectionId; file: string }[] = SECTIONS.map((
  s,
) => ({ id: s.id, file: s.file }));

// ---------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------

const SECTION_DIR = new URL("./", import.meta.url);
const cache = new Map<string, string>();

/**
 * Read one section file, trimmed and memoized.
 *
 * A missing or unreadable section is FATAL and says so: bough cannot run on a
 * partial prompt, and the failure mode of guessing (a model told about a verb it
 * does not have, or not told about one it does) is worse than not starting. This
 * is a broken install or an incomplete checkout, not a recoverable condition.
 */
export function readSectionFile(file: string): string {
  const hit = cache.get(file);
  if (hit !== undefined) return hit;
  let text: string;
  try {
    text = readFileSync(new URL(file, SECTION_DIR), "utf8").trim();
  } catch (err) {
    throw new Error(
      `cannot read the prompt section ${file} from ${SECTION_DIR.pathname} ` +
        `(${err instanceof Error ? err.message : String(err)}). bough cannot run ` +
        `without its prompt — this is a broken install or an incomplete checkout, ` +
        `not a recoverable condition.`,
    );
  }
  if (!text) {
    throw new Error(
      `the prompt section ${file} is empty. An empty section silently drops a ` +
        `capability grant from the prompt — restore the file or delete its entry ` +
        `from the section table.`,
    );
  }
  cache.set(file, text);
  return text;
}

/**
 * Fingerprint one section's text. Truncated sha256: 16 hex chars is 64 bits, so a
 * collision across the few hundred distinct section texts a campaign ever sees is
 * not a thing that happens, and the value stays readable in a trace line.
 */
export function sectionSha(text: string): string {
  return createHash("sha256").update(text).digest("hex").slice(0, 16);
}

// ---------------------------------------------------------------------------
// Volatile rendering
// ---------------------------------------------------------------------------

/** Per-server budget for the rendered tool list, so a chatty server can't crowd out the task. */
const SERVER_CHARS = 4_000;

function mcpToolLine(tool: PromptMcpTool): string {
  const desc = (tool.description ?? "").split("\n")[0].trim();
  return `- ${tool.name}${tool.signature ?? "()"}${desc ? ` — ${desc}` : ""}`;
}

function mcpServerBlock(server: PromptMcpServer): string {
  if (server.error) return `server "${server.name}": UNAVAILABLE — ${server.error}`;
  const tools = server.tools ?? [];
  const lines = [`server "${server.name}" (${tools.length} tools):`];
  let used = 0;
  let shown = 0;
  for (const tool of tools) {
    const line = mcpToolLine(tool);
    if (used + line.length > SERVER_CHARS) break;
    lines.push(line);
    used += line.length;
    shown++;
  }
  const omitted = tools.length - shown;
  if (omitted > 0) lines.push(`…(${omitted} more tools omitted)`);
  return lines.join("\n");
}

/**
 * The MCP tools section: the calling convention, then a compact per-server
 * catalog. Compact by design — no JSON Schema dumps.
 *
 * "Only the servers and tools listed here exist" is the load-bearing sentence: the
 * catalog is this turn's grant, and it changes between turns.
 */
function mcpToolsSection(servers: readonly PromptMcpServer[]): string {
  return "## MCP tools\n" +
    "This turn has MCP servers connected. Inside your program, call\n" +
    "`await mcp(server, tool, args)` — `args` is a plain object matching the tool's\n" +
    "parameters. The call returns the tool's result (an object, or its text output)\n" +
    "and throws on failure, with the server's own error text. Only the servers and\n" +
    "tools listed here exist; a tool you do not see is not one to guess at.\n\n" +
    servers.map(mcpServerBlock).join("\n\n");
}

/** Skill bodies, appended verbatim under a heading naming the skill (spec §16). */
function skillsSection(skills: readonly PromptSkill[]): string {
  return skills
    .map((s) => `## Skill: ${s.name}\n\n${s.body.trim()}`)
    .join("\n\n");
}

/**
 * Where this turn's verbs actually operate — the one note every turn gets.
 *
 * Rendered here rather than kept as a `.md` because it interpolates a per-session
 * path, which is exactly what the volatile tier is for: one session's workspace in
 * the stable prefix would defeat cross-session prompt caching for every other
 * session. It is a `notes` entry, so `turn/runner.ts` passes it in like any other
 * caller-resolved fact and this module still asks the world nothing.
 *
 * TWO THINGS IT HAS TO SAY, and the second is the one that was learned the hard way:
 *
 *  1. The workspace path at all. Without it the model has no cwd information and
 *    invents a container layout (`cd /workspace || cd /home`), walking itself out of
 *    the real project. That is the note the old tree carried and the reason it did.
 *
 *  2. That the PROGRAM's own cwd is not the workspace. `bash`, `sh` and the file
 *    verbs are given the workspace explicitly (`hostfn/shell.ts`, `hostfn/files.ts`),
 *    but the program worker is a thread of the server process and inherits the
 *    SERVER's working directory — and it cannot simply `chdir`, because cwd is a
 *    process attribute and one turn changing it would move every concurrent turn's
 *    shells with it. So `Bun.file("x").text()` and `view("x")` in the same program
 *    name two different files. `files.md` sends the model to `Bun.file` for
 *    raw content (spec §6: "there is no read()"), so this is a reachable trap on the
 *    documented path, not a corner case — say it plainly and give the fix.
 */
export function workspaceNote(workspace: string): string {
  return "## Workspace\n" +
    `The workspace is ${workspace} — the user's REAL checkout. bash(), sh() and the\n` +
    "file verbs (view/patch/write) all start there, and a relative path you give\n" +
    "THEM resolves against it.\n\n" +
    "Your program's own working directory is NOT the workspace: the runtime inherits\n" +
    "the server's directory, so a raw `Bun.file(\"src/x.ts\").text()`,\n" +
    "`readdir(\".\")` or `process.cwd()` reads somewhere else entirely. When you\n" +
    "reach past the host functions to the runtime, pass an ABSOLUTE path — join it\n" +
    "onto the workspace above — or go through bash(), which is already there.\n\n" +
    "Your edits are immediately real: nothing is copied, staged or confined, and git\n" +
    "is the source of truth for what changed (`git status`, `git diff`). Deliver work\n" +
    "with plain git through bash — `git commit`, `git push` — but ONLY when the user\n" +
    "asks; never as a routine end-of-task step.";
}

/**
 * Where temporary files go.
 *
 * NAMED, ABSOLUTE, AND PER SESSION — all three, because the version of this that
 * does not work is well documented: told only "use a scratch directory" (which is
 * what `AGENTS.md` said and all this used to be), a model keeps reaching for `/tmp`,
 * and the instruction reads as advice rather than as an address. So the path is
 * spelled out, and the reason is stated in the one form that transfers: a file
 * written into the checkout is a file the human has to review or revert.
 *
 * The permission sentence matters as much as the path. bough runs programs with the
 * user's full authority and no sandbox (spec §2), so "you may write here freely" is
 * not a grant — it is a statement about which writes are NOISE-FREE, and it is what
 * stops a model asking itself whether a debug dump is worth the intrusion.
 */
export function scratchNote(dir: string): string {
  return "## Scratchpad\n" +
    `Temporary files go in ${dir} — this session's own directory, outside the\n` +
    "workspace. Intermediate results, debug dumps, a script you are about to run\n" +
    "once, anything you would otherwise put in /tmp.\n\n" +
    "Write there freely: nothing in it is reviewed, diffed or reverted. A temp file\n" +
    "written into the workspace instead is one the human has to read in the changes\n" +
    "rail and decide about, which is a cost you are imposing on them for your own\n" +
    "convenience. Use /tmp only if the user asks for it.";
}

// ---------------------------------------------------------------------------
// Assembly
// ---------------------------------------------------------------------------

/**
 * Build a turn's system prompt.
 *
 * Sections join with a blank line, each already carrying its own `##` heading, so
 * the result is one flat markdown document and adding a section never requires
 * touching the joiner.
 */
export function assemblePrompt(input: PromptInput): AssembledPrompt {
  const granted = new Set<HostFnName>(input.granted);
  const facts: Facts = {
    kind: input.kind,
    has: (fn) => granted.has(fn),
    lsp: input.lsp === true,
  };

  const sections: SectionId[] = [];
  const shas: SectionSha[] = [];
  const stable: string[] = [];
  /** Record one section in all three parallel outputs, so they cannot drift apart. */
  const include = (id: SectionId, text: string, tier: string[]): void => {
    sections.push(id);
    shas.push({ id, sha: sectionSha(text) });
    tier.push(text);
  };

  for (const spec of SECTIONS) {
    if (!spec.when(facts)) continue;
    include(spec.id, readSectionFile(spec.file), stable);
  }

  const volatile: string[] = [];
  const servers = input.mcpServers ?? [];
  if (servers.length > 0 && granted.has("mcp")) {
    include("mcp-tools", mcpToolsSection(servers), volatile);
  }
  const skills = input.skills ?? [];
  if (skills.length > 0) {
    include("skills", skillsSection(skills), volatile);
  }
  const notes = (input.notes ?? []).map((n) => n.trim()).filter((n) => n !== "");
  if (notes.length > 0) {
    // The notes join into ONE section: they are separate strings only because the
    // caller resolves them separately, and a per-note id would not name anything
    // an experiment can edit.
    include("notes", notes.join("\n\n"), volatile);
  }

  return {
    system: stable.join("\n\n"),
    systemVolatile: volatile.join("\n\n"),
    sections,
    shas,
  };
}
