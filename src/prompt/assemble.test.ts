/**
 * Prompt assembly is the only place the capability grant is decided, so these
 * tests read as the spec §6 table: which sections a turn gets, and why.
 *
 * Assertions come from `node:assert` rather than `@std/assert` for the same reason
 * as the rest of this tree: jsr.io is denied in the sandbox these tests run in, so
 * a jsr import cannot resolve. `node:assert` is built into the runtime.
 */
import { test } from "bun:test";
import { deepStrictEqual, ok, throws } from "node:assert";
import { HOST_FN_NAMES, type HostFnName } from "../harness/protocol.ts";
import {
  assemblePrompt,
  type PromptInput,
  readSectionFile,
  SECTION_FILES,
  type SectionId,
  scratchNote,
  sectionSha,
  workspaceNote,
} from "./assemble.ts";

const assert = (value: unknown, message?: string) => ok(value, message);
const assertEquals = <T>(actual: T, expected: T, message?: string) =>
  deepStrictEqual(actual, expected, message);
const assertStringIncludes = (haystack: string, needle: string, message?: string) =>
  ok(
    haystack.includes(needle),
    message ?? `expected the prompt to include ${JSON.stringify(needle)}`,
  );
/** Runs `fn`, asserts it threw, and hands back the error for message assertions. */
function captureError(fn: () => unknown): Error {
  let caught: Error | undefined;
  throws(fn, (err: unknown) => {
    caught = err as Error;
    return true;
  });
  return caught!;
}

const ALL: HostFnName[] = [...HOST_FN_NAMES];

/** What every turn bridges: shell + the one editing idiom. */
const CORE: HostFnName[] = [
  "bash",
  "sh",
  "bashBg",
  "bashOutput",
  "bashWait",
  "bashKill",
  "view",
  "patch",
  "write",
];

function without(...drop: HostFnName[]): HostFnName[] {
  return ALL.filter((n) => !drop.includes(n));
}

function build(input: Partial<PromptInput> = {}) {
  return assemblePrompt({ kind: "root", granted: ALL, ...input });
}

/** Whole prompt as one string — for asserting a phrase appears nowhere at all. */
function whole(p: { system: string; systemVolatile: string }): string {
  return p.system + "\n\n" + p.systemVolatile;
}

/**
 * Whitespace-collapsed and lowercased, for asserting on PROSE. The sections are
 * hard-wrapped, so a sentence-length phrase straddles a newline — matching the raw
 * text would make every assertion hostage to where the wrap happens to fall.
 */
function flat(text: string): string {
  return text.replace(/\s+/g, " ").toLowerCase();
}

// ---------------------------------------------------------------------------
// Delegation tier — the AC
// ---------------------------------------------------------------------------

test("a subagent gets the nested delegation section and not the top-level one", () => {
  const sub = build({ kind: "subagent" });
  assert(sub.sections.includes("delegation-nested"));
  assert(!sub.sections.includes("delegation"));
  assertStringIncludes(sub.system, "## Delegation (nested)");
  assert(!sub.system.includes("## Delegation to subagents"));
  // Blocking only: a subagent is never told about the detached verbs.
  assert(!sub.system.includes("await spawn("));
  assert(!sub.system.includes("await join("));
});

test("a top-level session gets the top-level section and not the nested one", () => {
  for (const kind of ["root", "fork", "compaction"] as const) {
    const p = build({ kind });
    assert(p.sections.includes("delegation"), `${kind} should delegate`);
    assert(!p.sections.includes("delegation-nested"), `${kind} is not nested`);
    assertStringIncludes(p.system, "await spawn(task, {name})");
  }
});

test("subagent framing rides on kind, delegation on the grant", () => {
  const sub = build({ kind: "subagent" });
  assertStringIncludes(sub.system, "## You are a subagent");

  const wfAgent = build({ kind: "workflow_agent" });
  assertStringIncludes(wfAgent.system, "## You are a subagent");
  // A workflow agent delegates nothing at all.
  assert(!wfAgent.sections.includes("delegation"));
  assert(!wfAgent.sections.includes("delegation-nested"));

  // Depth 2: still kind "subagent", but nothing is bridged — so nothing is granted.
  const deepest = assemblePrompt({ kind: "subagent", granted: CORE });
  assert(!deepest.sections.includes("delegation-nested"));
  assertStringIncludes(deepest.system, "## You are a subagent");

  const root = build({ kind: "root" });
  assert(!root.system.includes("## You are a subagent"));
});

test("workflows are offered only to a session that may start one", () => {
  assert(build({ kind: "root" }).sections.includes("workflow"));
  assert(!build({ kind: "root", granted: without("workflow") }).sections.includes("workflow"));
  assert(!build({ kind: "subagent" }).sections.includes("workflow"));
});

// ---------------------------------------------------------------------------
// The capability grant — the other half of the AC
// ---------------------------------------------------------------------------

/** Section → the host function it grants, and a phrase only that section carries. */
const GRANTS: { id: SectionId; fn: HostFnName; phrase: string }[] = [
  { id: "shell", fn: "bash", phrase: "await bashBg(name, cmd)" },
  { id: "files", fn: "view", phrase: "await view(path)" },
  { id: "patch-grammar", fn: "patch", phrase: "INS.HEAD:" },
  { id: "ask", fn: "ask", phrase: "await ask(question" },
  { id: "state", fn: "state", phrase: "await state.get(key)" },
  { id: "schedule", fn: "schedule", phrase: "await schedule.list()" },
  { id: "artifact", fn: "artifact", phrase: "await artifact(name, content)" },
  // The probe is `spawn`'s own line, not `adopt`'s. It used to be the latter, which meant
  // this test — whose subject is "the delegation section appears iff spawn is granted" —
  // was pinned to a sentence about a DIFFERENT verb, and removing that undocumented verb
  // failed it for no reason connected to what it checks.
  { id: "delegation", fn: "spawn", phrase: "await spawn(task, {name})" },
  { id: "workflow", fn: "workflow", phrase: "await workflow.start(" },
];

test("a section granting a host function is absent when the capability is absent", () => {
  for (const { id, fn, phrase } of GRANTS) {
    const granted = build();
    assert(granted.sections.includes(id), `${id} should be present when ${fn} is granted`);
    assertStringIncludes(granted.system, phrase);

    const revoked = build({ granted: without(fn) });
    assert(!revoked.sections.includes(id), `${id} must be absent when ${fn} is not granted`);
    assert(
      !whole(revoked).includes(phrase),
      `no section may document ${fn} when it is not granted (found ${JSON.stringify(phrase)})`,
    );
  }
});

test("a core-only turn gets exactly the always-on sections", () => {
  const p = assemblePrompt({ kind: "root", granted: CORE });
  assertEquals(p.sections, [
    "identity",
    "shell",
    // The memory is a `bough tags` invocation now, not a host verb — so a turn that
    // can run a command can reach it, and the section rides with `bash`.
    "history",
    "files",
    "patch-grammar",
    "printing",
    "searching",
    "network",
    "ending",
  ]);
  assertEquals(p.systemVolatile, "");
});

// ---------------------------------------------------------------------------
// The volatile tier
// ---------------------------------------------------------------------------

test("MCP tools appear only when servers are connected, and stay out of the stable tier", () => {
  const none = build();
  assert(!none.sections.includes("mcp-tools"));
  assertEquals(none.systemVolatile, "");

  const p = build({
    mcpServers: [
      {
        name: "files",
        tools: [{ name: "read_file", signature: "({path})", description: "Read a file\nmore" }],
      },
      { name: "broken", error: "exited before handshake" },
    ],
  });
  assert(p.sections.includes("mcp-tools"));
  assertStringIncludes(p.systemVolatile, "## MCP tools");
  assertStringIncludes(p.systemVolatile, "bough mcp call SERVER TOOL");
  assertStringIncludes(p.systemVolatile, "- read_file({path}) — Read a file");
  // A failed server is named with its error, not silently dropped.
  assertStringIncludes(p.systemVolatile, 'server "broken": UNAVAILABLE — exited before handshake');
  // Nor is a granted server whose tools are not known yet rendered as `(0 tools)`,
  // which reads as "this server has nothing" — the opposite of what is true.
  const pending = build({
    mcpServers: [{ name: "notion", note: "granted, not connected yet — call it to connect" }],
  });
  assertStringIncludes(pending.systemVolatile, 'server "notion": granted, not connected yet');
  assert(!pending.systemVolatile.includes("0 tools"));
  // The catalog is per-session; it must never reach the cacheable prefix.
  assert(!p.system.includes("MCP tools"));
  // NO SHELL, NO CATALOG. A tool is called by running `bough mcp call`, so a turn
  // that cannot run a command cannot reach one — and listing tools to it would be a
  // list of things it has no way to use. (This used to be gated on the `mcp` host
  // function, which no longer exists.)
  assert(
    !build({
      granted: without("bash"),
      mcpServers: [{ name: "files", tools: [] }],
    }).sections.includes("mcp-tools"),
  );
});

test("skills and caller notes land in the volatile tier only", () => {
  const p = build({
    skills: [{ name: "history", body: "Query ~/.bough/bough.db with sqlite3." }],
    notes: ["# Workspace\nbash starts in /repo.", "   ", ""],
  });
  assertEquals(p.sections.slice(-2), ["skills", "notes"]);
  assertStringIncludes(p.systemVolatile, "## Skill: history");
  assertStringIncludes(p.systemVolatile, "Query ~/.bough/bough.db");
  assertStringIncludes(p.systemVolatile, "bash starts in /repo.");
  assert(!p.system.includes("/repo"));
  // Blank notes are dropped rather than joined into stray separators.
  assert(!p.systemVolatile.includes("\n\n\n"));
});

test("the stable tier is byte-identical for the same shape", () => {
  const a = build({ mcpServers: [{ name: "x", tools: [] }], notes: ["# A\nfirst"] });
  const b = build({ mcpServers: [{ name: "y", tools: [] }], notes: ["# B\nsecond"] });
  assertEquals(a.system, b.system);
  assert(a.systemVolatile !== b.systemVolatile);
});

// ---------------------------------------------------------------------------
// Content: the prompt has to match THIS spec
// ---------------------------------------------------------------------------

test("the prompt grants view + patch + write and nothing else for files", () => {
  const text = whole(build());
  for (const gone of ["await read(", "await edit(", "await extract(", "await recall("]) {
    assert(!text.includes(gone), `${gone} was removed from the spec`);
  }
  assertStringIncludes(flat(text), "there is no read() and no edit().");
  assertStringIncludes(text, "await write(path, content)");
});

test("there is no done-gate and no committed check", () => {
  const text = flat(whole(build()));
  for (const gone of ["done-gate", "committed check", "checkpassed", "re-runs the committed"]) {
    assert(!text.includes(gone), `"${gone}" belongs to the old acceptance gate`);
  }
  assertStringIncludes(text, "there is no acceptance gate in this harness");
  // `done` survives as a report, and stop is what ends a turn.
  assertStringIncludes(text, "it is a report, not a gate");
  assertStringIncludes(text, "call the stop tool");
});

test("the network section states plainly that nothing filters egress", () => {
  const p = build();
  assert(p.sections.includes("network"));
  assertStringIncludes(p.system, "## Network");
  const text = flat(p.system);
  assertStringIncludes(text, "you have network access, and nothing filters it");
  assertStringIncludes(
    text,
    "there is no egress proxy, no allowlist, no credential gate, and no review step",
  );
  assert(!text.includes("egress gate"), "there is no egress gate to describe");
});

// ---------------------------------------------------------------------------
// The section files themselves
// ---------------------------------------------------------------------------

test("every section in the table has a readable, headed file", () => {
  for (const { id, file } of SECTION_FILES) {
    const text = readSectionFile(file);
    assert(text.startsWith("## "), `${id} (${file}) must start with its own "## " heading`);
    assert(text.length > 100, `${id} (${file}) looks truncated`);
  }
});

test("a missing section file is fatal and says why", () => {
  const err = captureError(() => readSectionFile("no-such-section.md"));
  assertStringIncludes(err.message, "no-such-section.md");
  assertStringIncludes(err.message, "not a recoverable condition");
});

// ---------------------------------------------------------------------------
// The workspace note
// ---------------------------------------------------------------------------

test("the workspace note names the path, and rides the VOLATILE tier", () => {
  const note = workspaceNote("/home/u/proj");
  assert(note.startsWith("## Workspace"), "a note is a complete section with its own heading");
  assertStringIncludes(note, "/home/u/proj");

  const p = build({ notes: [note] });
  assert(p.sections.includes("notes"));
  // The stable prefix is shared across sessions and cached by the provider; one
  // session's workspace path in it would defeat that for every other session.
  assert(!p.system.includes("/home/u/proj"), "a per-session path must never enter the stable tier");
  assertStringIncludes(p.systemVolatile, "/home/u/proj");
});

test("the scratchpad note names an absolute path and stays out of the stable tier", () => {
  // The version of this that does NOT work is documented: told only "use a scratch
  // directory", a model keeps reaching for /tmp, because that is advice and not an
  // address. So the assertion is that the path itself is in the text.
  const note = scratchNote("/home/u/.bough/scratch/abc123");
  assert(note.startsWith("## Scratchpad"));
  assertStringIncludes(note, "/home/u/.bough/scratch/abc123");
  const text = flat(note);
  assertStringIncludes(text, "/tmp"); // …and says what it replaces
  // The reason, in the form that transfers: a temp file in the checkout is work the
  // human has to review.
  assertStringIncludes(text, "changes");

  const p = build({ notes: [scratchNote("/home/u/.bough/scratch/abc123")] });
  // Per-session, so it must never enter the prefix every other session shares.
  assert(!p.system.includes("abc123"), "a per-session path must never enter the stable tier");
  assertStringIncludes(p.systemVolatile, "abc123");
});

test("the workspace note warns that the program's own cwd is NOT the workspace", () => {
  // The trap this closes: bash() and view() are handed the workspace explicitly,
  // but the program worker inherits the SERVER's directory, so
  // `Bun.file("x").text()` and `view("x")` in one program name two different
  // files — and files.md sends the model to Bun.file for raw content.
  const text = flat(workspaceNote("/w"));
  assertStringIncludes(text, "your program's own working directory is not the workspace");
  assertStringIncludes(text, "bun.file");
  assertStringIncludes(text, "absolute");
});

test("the workspace note is not gated on a capability — every kind edits a real checkout", () => {
  for (const kind of ["root", "fork", "compaction", "subagent", "workflow_agent"] as const) {
    const p = assemblePrompt({ kind, granted: CORE, notes: [workspaceNote("/w/" + kind)] });
    assertStringIncludes(p.systemVolatile, "/w/" + kind);
  }
});

// ---------------------------------------------------------------------------
// Section fingerprints — what makes prompt attribution possible
// ---------------------------------------------------------------------------

test("every included section is fingerprinted, in prompt order", () => {
  const p = build({ skills: [{ name: "s", body: "B" }], notes: ["## N\nnote"] });
  assertEquals(p.shas.map((s) => s.id), p.sections, "shas parallel sections exactly");
  assert(p.shas.every((s) => /^[0-9a-f]{16}$/.test(s.sha)), "each sha is truncated sha256");
});

test("a section's sha is over the text that actually went into the prefix", () => {
  const p = build();
  const identity = p.shas.find((s) => s.id === "identity")!;
  assertEquals(identity.sha, sectionSha(readSectionFile("identity.md")));
  // The point of the exercise: an edit to one .md moves exactly one sha, so a
  // flipped task can be attributed to a file rather than to "the prompt".
  const shell = p.shas.find((s) => s.id === "shell")!;
  assert(shell.sha !== identity.sha);
});

test("a turn without a capability carries no fingerprint for its section", () => {
  const p = build({ granted: without("artifact") });
  assertEquals(p.shas.some((s) => s.id === "artifact"), false);
  // An experiment editing artifact.md must not count this turn as exposed to it.
});
