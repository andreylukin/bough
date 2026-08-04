/**
 * The compaction scout.
 *
 * Two things are worth pinning here and the rest is the loop's plumbing:
 *
 *   1. WHAT IT IS POINTED AT. `touchedPaths` mines paths out of transcript TEXT — the
 *      only place a path appears in this harness, since file verbs are calls inside a
 *      `run_steps` program — so the regex necessarily over-matches and the filesystem
 *      is the gate. The tests assert both halves: a real file is found, a plausible
 *      one that does not exist is not, and a real path OUTSIDE the workspace is not,
 *      because scope is the whole point of scoping to the touched directories.
 *   2. THAT IT CANNOT FAIL A COMPACTION. Every failure returns null. A test drives an
 *      LLM that throws and asserts null rather than a rejection, because the one thing
 *      this module must never do is take down the operation a user reaches for when a
 *      conversation has grown too long to continue.
 */
import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "bun:test";
import { exploreModel, exploreSpan, touchedDirs, touchedPaths } from "./explore.ts";
import type { Message, Part } from "../schema/parts.ts";
import type { LlmClient, LlmParams, LlmResult } from "../types.ts";

function workspace(): string {
  const dir = mkdtempSync(join(tmpdir(), "bough-explore-"));
  mkdirSync(join(dir, "src", "history"), { recursive: true });
  writeFileSync(join(dir, "src", "history", "compact.ts"), "export const x = 1;\n");
  writeFileSync(join(dir, "README.md"), "# hi\n");
  return dir;
}

function span(...texts: string[]): Message[] {
  return texts.map((text, i) => ({
    id: `m${i}`,
    sessionId: "s1",
    role: "supervisor" as const,
    parts: [{ type: "text", text } satisfies Part],
    pending: false,
    createdAt: 1_000 + i,
  }));
}

test("touchedPaths keeps the paths that exist and drops the ones that only look real", () => {
  const dir = workspace();
  const paths = touchedPaths(
    span(
      "I edited src/history/compact.ts and README.md",
      "then src/history/nosuch.ts, and bumped to v1.2.4 — see docs/plan.md",
    ),
    dir,
  );
  assert.ok(paths.includes("src/history/compact.ts"), paths.join(","));
  assert.ok(paths.includes("README.md"), paths.join(","));
  // Named in the transcript, absent from the tree: nothing to explore.
  assert.equal(paths.includes("src/history/nosuch.ts"), false);
  assert.equal(paths.includes("docs/plan.md"), false);
  // The version number is exactly the kind of token the loose regex matches and the
  // filesystem throws away.
  assert.equal(paths.some((p) => p.includes("1.2.4")), false);
});

test("touchedPaths refuses a real path outside the workspace", () => {
  const dir = workspace();
  const outside = workspace();
  writeFileSync(join(outside, "elsewhere.txt"), "x");
  const paths = touchedPaths(span(`I also read ${join(outside, "elsewhere.txt")}`), dir);
  assert.deepEqual(paths, []);
});

test("touchedDirs is the directories, deduped and in order", () => {
  assert.deepEqual(
    touchedDirs(["src/history/compact.ts", "src/history/branch.ts", "README.md"]),
    ["src/history", "."],
  );
});

test("the scout runs its bash calls and returns the notes it ends with", async () => {
  const dir = workspace();
  const commands: string[] = [];
  let round = 0;
  const llm: LlmClient = {
    run(params: LlmParams): Promise<LlmResult> {
      round++;
      if (round === 1) {
        // The brief must name the directory the span's files live in — that is the
        // scoping this whole module exists for.
        const first = params.messages[0].content[0];
        assert.ok(first.type === "text" && first.text.includes("src/history"), "no scope in brief");
        return Promise.resolve({
          content: [{ type: "tool_use", id: "t1", name: "bash", input: { command: "echo SCOUTED" } }],
          stopReason: "tool_use",
        });
      }
      // The tool result must have come back as a user message, or the scout is
      // reasoning about a command it never saw the output of.
      const last = params.messages.at(-1);
      const result = last?.content[0];
      assert.ok(result?.type === "tool_result", "the command's output never reached the scout");
      commands.push(result.content.trim());
      return Promise.resolve({
        content: [{ type: "text", text: "notes: the file is there" }],
        stopReason: "end_turn",
      });
    },
  };
  const notes = await exploreSpan(
    { sessionId: "s1", workspace: dir, llm, model: "test-model" },
    span("I edited src/history/compact.ts"),
  );
  assert.equal(notes, "notes: the file is there");
  assert.deepEqual(commands, ["SCOUTED"]);
});

test("a span that touched nothing that exists is not scouted at all", async () => {
  const dir = workspace();
  const llm: LlmClient = {
    run(): Promise<LlmResult> {
      throw new assert.AssertionError({ message: "the scout must not run with no paths" });
    },
  };
  assert.equal(
    await exploreSpan({ sessionId: "s1", workspace: dir, llm, model: "m" }, span("we talked")),
    null,
  );
});

test("a scout that throws yields null, never a rejection", async () => {
  const dir = workspace();
  const llm: LlmClient = {
    run(): Promise<LlmResult> {
      return Promise.reject(new Error("401 no key for that provider"));
    },
  };
  assert.equal(
    await exploreSpan(
      { sessionId: "s1", workspace: dir, llm, model: "m" },
      span("I edited src/history/compact.ts"),
    ),
    null,
  );
});

test("the scout model is pinned, and overridable for a user with a different key", () => {
  assert.equal(exploreModel({} as NodeJS.ProcessEnv), "gpt-5.6-luna");
  assert.equal(
    exploreModel({ BOUGH_COMPACT_EXPLORE_MODEL: " claude-opus-4-8 " } as NodeJS.ProcessEnv),
    "claude-opus-4-8",
  );
});
