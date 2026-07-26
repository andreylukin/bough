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
    text = Deno.readTextFileSync(new URL(file, SECTION_DIR)).trim();
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
  const stable: string[] = [];
  for (const spec of SECTIONS) {
    if (!spec.when(facts)) continue;
    sections.push(spec.id);
    stable.push(readSectionFile(spec.file));
  }

  const volatile: string[] = [];
  const servers = input.mcpServers ?? [];
  if (servers.length > 0 && granted.has("mcp")) {
    sections.push("mcp-tools");
    volatile.push(mcpToolsSection(servers));
  }
  const skills = input.skills ?? [];
  if (skills.length > 0) {
    sections.push("skills");
    volatile.push(skillsSection(skills));
  }
  const notes = (input.notes ?? []).map((n) => n.trim()).filter((n) => n !== "");
  if (notes.length > 0) {
    sections.push("notes");
    volatile.push(...notes);
  }

  return {
    system: stable.join("\n\n"),
    systemVolatile: volatile.join("\n\n"),
    sections,
  };
}
