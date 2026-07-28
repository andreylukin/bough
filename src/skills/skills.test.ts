/**
 * Skills: frontmatter, precedence, `${SKILL_DIR}`, and the path from a typed
 * `/name` to the bytes the provider receives.
 *
 * The load-bearing test is the last one: **the body of a skill the message named
 * reaches the assembled system prompt.** It is asserted through a real turn — a
 * scripted fake `LlmClient`, a fake program runner, an in-memory database and the
 * same `assemble` closure `server/main.ts` installs — because every other assertion
 * here would still pass if the wiring between discovery and the prompt did not
 * exist. Nothing binds a socket and nothing is on the network (plan §7).
 *
 * The second one worth naming is the malformed case. A SKILL.md that opens `---`
 * and never closes it used to be pasted into the prompt frontmatter and all; here it
 * must contribute NO body and a note saying so, because a prompt that is wrong is
 * worse than one that is missing (`skills/skills.ts`).
 *
 * Every source directory is a temp dir passed in through `sources` — no test reads
 * `~/.bough`, and the two bundled-skill tests read the repo's own folder, which is
 * checked in.
 *
 * Assertions come from `node:assert/strict` rather than `@std/assert`: jsr.io is not
 * reachable here, and a test that cannot run offline does not belong in
 * `deno task test`.
 */
import { test } from "bun:test";
import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Bus } from "../bus.ts";
import { openDb, type SqliteDb } from "../db/db.ts";
import { assemblePrompt } from "../prompt/assemble.ts";
import type { Message, Session } from "../schema/parts.ts";
import type { AppCtx, LlmBlock, LlmClient, LlmParams, LlmResult } from "../types.ts";
import { beginTurn, RUN_STEPS, STOP } from "../turn/runner.ts";
import { TurnRegistry } from "../turn/queue.ts";
import {
  activeSkills,
  BUNDLED_SKILLS_DIR,
  defaultSources,
  invokingText,
  listSkills,
  loadSkill,
  mentionIndex,
  parseFrontmatter,
  parseList,
  type SkillSource,
  turnSkills,
  widenGrant,
} from "./skills.ts";

// ---- fixtures ---------------------------------------------------------------

/** A temp source directory, plus a helper that writes skills into it. */
function tempSource(
  source: SkillSource["source"],
): SkillSource & { write: (name: string, text: string) => string } {
  const dir = mkdtempSync(join(tmpdir(), `bough-skills-${source}-`));
  return {
    source,
    dir,
    write(name, text) {
      const folder = join(dir, name);
      mkdirSync(folder, { recursive: true });
      writeFileSync(join(folder, "SKILL.md"), text);
      return folder;
    },
  };
}

function skillFile(fields: string, body: string): string {
  return `---\n${fields}\n---\n\n${body}\n`;
}

// ---- frontmatter ------------------------------------------------------------

test("frontmatter: fields are read, quotes stripped, and the body starts after the fence", () => {
  const fm = parseFrontmatter(
    skillFile(
      `name: review\ndescription: "Review a diff, carefully"\nmcp: linear, github`,
      "# Do this\n\nbody.",
    ),
  );
  assert.equal(fm.error, undefined);
  assert.equal(fm.fields.description, "Review a diff, carefully");
  assert.equal(fm.fields.mcp, "linear, github");
  assert.equal(fm.body, "# Do this\n\nbody.");
  assert.ok(!fm.body.includes("description:"), fm.body);
});

test("frontmatter: a file with no fence is all body", () => {
  const fm = parseFrontmatter("Just instructions.\n\nMore of them.\n");
  assert.equal(fm.error, undefined);
  assert.deepEqual(fm.fields, {});
  assert.equal(fm.body, "Just instructions.\n\nMore of them.");
});

test("frontmatter: an unterminated fence is an error and withholds the body", () => {
  const fm = parseFrontmatter("---\nname: broken\ndescription: no closing fence\n\nThe body.\n");
  assert.match(fm.error ?? "", /opens with `---` and never closes/);
  assert.equal(fm.body, "");
  assert.deepEqual(fm.fields, {});
});

test("frontmatter: a `---` inside the body does not truncate it", () => {
  // The old implementation split the whole file on "---", so a horizontal rule or a
  // fenced block containing one silently ate the rest of the skill.
  const fm = parseFrontmatter(skillFile("description: d", "before\n\n---\n\nafter the rule"));
  assert.ok(fm.body.startsWith("before"), fm.body);
  assert.ok(fm.body.endsWith("after the rule"), fm.body);
});

test("frontmatter: comments, blanks and junk lines are tolerated; first key wins", () => {
  const fm = parseFrontmatter(
    "---\n# a comment\n\ndescription: first\ndescription: second\nnot a field\n---\nbody\n",
  );
  assert.equal(fm.error, undefined);
  assert.equal(fm.fields.description, "first");
  assert.equal(fm.body, "body");
});

test("frontmatter: CRLF line endings parse the same as LF", () => {
  const fm = parseFrontmatter("---\r\ndescription: windows\r\n---\r\nbody\r\n");
  assert.equal(fm.error, undefined);
  assert.equal(fm.fields.description, "windows");
  assert.equal(fm.body, "body");
});

test("mcp lists parse as a comma list or a bracketed one", () => {
  assert.deepEqual(parseList("linear, github"), ["linear", "github"]);
  assert.deepEqual(parseList("[a, b]"), ["a", "b"]);
  assert.deepEqual(parseList(""), []);
});

// ---- discovery and precedence ----------------------------------------------

test("a name in two sources resolves to the bundled one — first source wins", () => {
  const bundled = tempSource("bundled");
  const user = tempSource("user");
  try {
    bundled.write("history", skillFile("description: the bundled one", "BUNDLED BODY"));
    user.write("history", skillFile("description: the shadow", "USER BODY"));
    user.write("mine", skillFile("description: only the user has this", "MINE"));

    const sources = [bundled, user];
    const listed = listSkills({ sources });
    assert.deepEqual(listed.map((s) => s.name), ["history", "mine"]);

    const history = listed.find((s) => s.name === "history")!;
    assert.equal(history.source, "bundled");
    assert.equal(history.description, "the bundled one");
    assert.equal(history.body, "BUNDLED BODY");
    // Exactly one row per name: the shadowed copy is resolved away, not listed twice.
    assert.equal(listed.filter((s) => s.name === "history").length, 1);

    assert.equal(loadSkill("history", { sources })!.body, "BUNDLED BODY");
    assert.equal(loadSkill("mine", { sources })!.source, "user");
    assert.equal(loadSkill("absent", { sources }), null);
  } finally {
    rmSync(bundled.dir, { recursive: true, force: true });
    rmSync(user.dir, { recursive: true, force: true });
  }
});

test("a folder without a SKILL.md is not a skill, and a missing source dir is not an error", () => {
  const user = tempSource("user");
  try {
    mkdirSync(join(user.dir, "scratch"), { recursive: true });
    writeFileSync(join(user.dir, "loose.md"), "not a skill");
    user.write("real", skillFile("description: d", "body"));
    const sources: SkillSource[] = [
      { source: "bundled", dir: join(user.dir, "does-not-exist") },
      user,
    ];
    assert.deepEqual(listSkills({ sources }).map((s) => s.name), ["real"]);
  } finally {
    rmSync(user.dir, { recursive: true, force: true });
  }
});

test("a traversing name never becomes a path", () => {
  const user = tempSource("user");
  try {
    assert.equal(loadSkill("../../etc", { sources: [user] }), null);
    assert.equal(loadSkill("a/b", { sources: [user] }), null);
    assert.equal(loadSkill(".hidden", { sources: [user] }), null);
  } finally {
    rmSync(user.dir, { recursive: true, force: true });
  }
});

// ---- ${SKILL_DIR} -----------------------------------------------------------

test("${SKILL_DIR} resolves to the skill's own folder, everywhere it appears", () => {
  const user = tempSource("user");
  try {
    const folder = user.write(
      "helper",
      skillFile(
        "description: d",
        "Run `python3 ${SKILL_DIR}/run.py` then read ${SKILL_DIR}/notes.md",
      ),
    );
    const skill = loadSkill("helper", { sources: [user] })!;
    assert.equal(skill.dir, folder);
    assert.equal(skill.body, `Run \`python3 ${folder}/run.py\` then read ${folder}/notes.md`);
    assert.ok(!skill.body.includes("SKILL_DIR"), skill.body);
  } finally {
    rmSync(user.dir, { recursive: true, force: true });
  }
});

// ---- invocation -------------------------------------------------------------

test("a skill is named at a word boundary, and only there", () => {
  assert.ok(mentionIndex("/history what did I do", "history") >= 0);
  assert.ok(mentionIndex("please /history now", "history") > 0);
  assert.ok(mentionIndex("look it up with /history", "history") > 0);
  assert.ok(mentionIndex("/history, then summarize", "history") >= 0);
  // Not an invocation: a longer token, a path, or a bare word.
  assert.equal(mentionIndex("/history-old", "history"), -1);
  assert.equal(mentionIndex("/usr/bin/history", "history"), -1);
  assert.equal(mentionIndex("history of the repo", "history"), -1);
  assert.equal(mentionIndex("x/history", "history"), -1);
});

test("named skills load in invocation order, with their servers unioned", () => {
  const user = tempSource("user");
  try {
    user.write("alpha", skillFile("description: a\nmcp: linear, github", "ALPHA BODY"));
    user.write("beta", skillFile("description: b\nmcp: github", "BETA BODY"));
    user.write("gamma", skillFile("description: c", "GAMMA BODY"));

    const active = activeSkills("first /beta then /alpha", { sources: [user] });
    assert.deepEqual(active.names, ["beta", "alpha"]);
    assert.deepEqual(active.skills.map((s) => s.body), ["BETA BODY", "ALPHA BODY"]);
    assert.deepEqual(active.servers.sort(), ["github", "linear"]);
    assert.deepEqual(active.notes, []);

    // Nothing named = nothing loaded, and gamma stays out of it.
    assert.deepEqual(activeSkills("no skills here", { sources: [user] }).skills, []);
  } finally {
    rmSync(user.dir, { recursive: true, force: true });
  }
});

test("a named skill that cannot be parsed contributes a note, never a body", () => {
  const user = tempSource("user");
  try {
    user.write("broken", "---\nname: broken\ndescription: unterminated\n\nThe instructions.\n");
    const active = activeSkills("please /broken this", { sources: [user] });
    assert.deepEqual(active.skills, []);
    assert.deepEqual(active.names, []);
    assert.equal(active.notes.length, 1);
    assert.match(active.notes[0], /^## Skill \/broken could not be loaded/);
    assert.match(active.notes[0], /never closes/);
    // The frontmatter itself must not have leaked into what the model is told.
    assert.ok(!active.notes[0].includes("The instructions."), active.notes[0]);
    // It is still LISTED, with its error — the panel is where the user finds out.
    const listed = listSkills({ sources: [user] });
    assert.equal(listed.length, 1);
    assert.match(listed[0].error ?? "", /never closes/);
  } finally {
    rmSync(user.dir, { recursive: true, force: true });
  }
});

test("the invoking text is the newest USER message, not a system note", () => {
  const message = (role: Message["role"], text: string, at: number): Message => ({
    id: crypto.randomUUID(),
    sessionId: "s",
    role,
    parts: [{ type: "text", text }],
    pending: false,
    createdAt: at,
  });
  assert.equal(
    invokingText([
      message("user", "old /alpha", 1),
      message("user", "new /beta", 2),
      message("system", "[subagent finished] mentioned /alpha", 3),
    ]),
    "new /beta",
  );
  assert.equal(invokingText([]), "");
});

test("turnSkills reads the session's own newest user message", () => {
  const user = tempSource("user");
  try {
    user.write("alpha", skillFile("description: a", "ALPHA BODY"));
    const db = openDb(":memory:");
    const session = db.createSession({
      id: crypto.randomUUID(),
      title: "t",
      kind: "root",
      createdAt: 1,
      parentId: null,
    });
    db.createMessage({
      id: crypto.randomUUID(),
      sessionId: session.id,
      role: "user",
      parts: [{ type: "text", text: "use /alpha please" }],
      pending: false,
      createdAt: 2,
    });
    assert.deepEqual(turnSkills(db, session.id, { sources: [user] }).names, ["alpha"]);
    db.close();
  } finally {
    rmSync(user.dir, { recursive: true, force: true });
  }
});

// ---- the bundled skill ------------------------------------------------------

test("the bundled `history` skill is discoverable from the default sources", () => {
  assert.equal(defaultSources()[0].dir, BUNDLED_SKILLS_DIR);
  const skill = loadSkill("history")!;
  assert.ok(skill, "the history skill ships bundled (spec §16)");
  assert.equal(skill.source, "bundled");
  assert.ok(skill.description.length > 0);
  assert.equal(skill.error, undefined);
  // It documents the CURRENT schema — the tables it names must be the real ones.
  const schema = readFileSync(new URL("../db/schema.sql", import.meta.url), "utf8");
  for (const table of ["messages_fts", "sessions", "messages", "turns"]) {
    assert.ok(skill.body.includes(table), `history skill should mention ${table}`);
    assert.ok(schema.includes(table), `${table} should exist in the schema`);
  }
  // And nothing that no longer exists (spec §17: no semantic recall, no embeddings).
  for (const gone of ["recall(", "message_embeddings", "archived_at", "deprecated_at"]) {
    assert.ok(!skill.body.includes(gone), `history skill must not mention ${gone}`);
  }
  // Frontmatter is stripped, not appended to the prompt.
  assert.ok(!skill.body.includes("description:"), skill.body.slice(0, 200));
});

// ---- the MCP grant ----------------------------------------------------------

test("widenGrant unions the skill's servers into a live grant without freezing it", () => {
  let activations = ["already-granted", "linear"];
  const ctx: { mcpGrant?: string[] } = {};
  Object.defineProperty(ctx, "mcpGrant", { get: () => [...activations], configurable: true });

  widenGrant(ctx, ["linear"]);
  // Deduped: a server the human had already enabled is not listed twice.
  assert.deepEqual(ctx.mcpGrant!.sort(), ["already-granted", "linear"]);

  // Still LIVE: a revocation between calls is visible immediately, and the skill's
  // own server survives it (the invocation is what granted that one).
  activations = [];
  assert.deepEqual(ctx.mcpGrant, ["linear"]);

  // A plain-array grant is snapshotted rather than read back through the new getter,
  // which would recurse forever.
  const inherited: { mcpGrant?: string[] } = { mcpGrant: ["from-spawner"] };
  widenGrant(inherited, ["extra"]);
  assert.deepEqual(inherited.mcpGrant!.sort(), ["extra", "from-spawner"]);

  // Nothing to add = nothing touched.
  const untouched: { mcpGrant?: string[] } = {};
  assert.equal(widenGrant(untouched, []), untouched);
  assert.equal(untouched.mcpGrant, undefined);
});

// ---- the body reaches the prompt --------------------------------------------

test("activeSkills' bodies land in the assembled prompt's volatile tier", () => {
  const user = tempSource("user");
  try {
    user.write("alpha", skillFile("description: a", "ALPHA INSTRUCTIONS"));
    const active = activeSkills("go /alpha", { sources: [user] });
    const prompt = assemblePrompt({ kind: "root", granted: ["bash"], skills: active.skills });
    assert.ok(prompt.sections.includes("skills"), prompt.sections.join(","));
    assert.ok(prompt.systemVolatile.includes("## Skill: alpha"), prompt.systemVolatile);
    assert.ok(prompt.systemVolatile.includes("ALPHA INSTRUCTIONS"), prompt.systemVolatile);
    // The stable tier stays byte-identical to a turn with no skills — one volatile
    // byte in the shared prefix would cost every other session the prompt cache.
    const bare = assemblePrompt({ kind: "root", granted: ["bash"] });
    assert.equal(prompt.system, bare.system);
  } finally {
    rmSync(user.dir, { recursive: true, force: true });
  }
});

test("a real turn sends the named skill's body to the provider", async () => {
  const user = tempSource("user");
  const db: SqliteDb = openDb(":memory:");
  try {
    user.write(
      "deploy",
      skillFile("description: ship it\nmcp: linear", "STEP ONE: read ${SKILL_DIR}/checklist.md"),
    );

    const bus = new Bus();
    const session: Session = db.createSession({
      id: crypto.randomUUID(),
      title: "t",
      kind: "root",
      createdAt: 1_000,
      parentId: null,
    });
    db.createMessage({
      id: crypto.randomUUID(),
      sessionId: session.id,
      role: "user",
      parts: [{ type: "text", text: "/deploy the api" }],
      pending: false,
      createdAt: 2_000,
    });

    const calls: LlmParams[] = [];
    const stop: LlmBlock = { type: "tool_use", id: "s1", name: STOP, input: {} };
    const llm: LlmClient = {
      run(params): Promise<LlmResult> {
        calls.push(structuredClone(params));
        return Promise.resolve({
          content: [{ type: "text", text: "Deployed." }, stop],
          stopReason: "end_turn",
        });
      },
    };
    const ctx: AppCtx = { db, bus, llm, model: "claude-opus-4-8" };

    // The same closure `server/main.ts` installs: the session id is captured at start
    // and the skills are resolved at assemble time.
    const outcome = await beginTurn(ctx, session.id, {
      registry: new TurnRegistry(),
      program: () => Promise.resolve({ ok: true, logs: [] }),
      assemble: (input) => {
        const active = turnSkills(ctx.db, session.id, { sources: [user] });
        return assemblePrompt({
          ...input,
          skills: active.skills,
          notes: [...(input.notes ?? []), ...active.notes],
        });
      },
    }).done;

    assert.equal(outcome.status, "done");
    assert.equal(calls.length, 1);
    const sent = calls[0].systemVolatile ?? "";
    assert.ok(sent.includes("## Skill: deploy"), sent);
    assert.ok(sent.includes("STEP ONE"), sent);
    // `${SKILL_DIR}` was resolved before it ever reached the provider.
    assert.ok(sent.includes(join(user.dir, "deploy")), sent);
    assert.ok(!sent.includes("SKILL_DIR"), sent);
    // The tool surface is untouched: a skill is instructions, not a tool.
    assert.deepEqual(calls[0].tools?.map((t) => t.name), [RUN_STEPS, STOP]);
  } finally {
    db.close();
    rmSync(user.dir, { recursive: true, force: true });
  }
});
