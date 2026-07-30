/**
 * The trace exists to be READ BACK by an experiment, so these tests read it back
 * rather than asserting on the writer's intentions: every case parses the JSONL
 * off a real temp directory and checks the facts an attribution pass depends on —
 * that the prefix reconstructs from its sha, that a failed attempt is on the
 * record, and that tracing off costs nothing.
 *
 * `node:assert` rather than `@std/assert`: jsr.io is denied by this environment's
 * egress policy (same constraint the other test files document).
 */

import { test } from "bun:test";
import { deepStrictEqual, ok, strictEqual } from "node:assert";
import { mkdtempSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { LlmClient, LlmParams, LlmResult } from "../types.ts";
import { sectionSha } from "../prompt/assemble.ts";
import { type RoundRecord, traceLabel, tracePath, withTrace } from "./trace.ts";

const label = () => ({
  dir: mkdtempSync(join(tmpdir(), "bough-trace-")),
  sessionId: "s1",
  turnId: "t1",
});

const PARAMS: LlmParams = {
  model: "claude-haiku-4-5",
  system: "STABLE PREFIX",
  systemVolatile: "VOLATILE",
  maxTokens: 100,
  messages: [{ role: "user", content: [{ type: "text", text: "hi" }] }],
  tools: [{ name: "run_steps", description: "d", inputSchema: {} }],
};

const RESULT: LlmResult = {
  content: [{ type: "text", text: "ok" }],
  stopReason: "end_turn",
  usage: { inputTokens: 1, outputTokens: 2, costUsd: 0.5 },
};

const stub = (impl?: () => Promise<LlmResult>): LlmClient => ({
  run: impl ?? (() => Promise.resolve(RESULT)),
});

function lines(l: ReturnType<typeof label>): Record<string, unknown>[] {
  return readFileSync(tracePath(l), "utf8").trim().split("\n").map((s) => JSON.parse(s));
}

test("a round records the request, the response and the prefix it ran with", async () => {
  const l = label();
  await withTrace(stub(), l).run(PARAMS, () => {});

  const [system, volatile, round] = lines(l) as [
    Record<string, unknown>,
    Record<string, unknown>,
    RoundRecord,
  ];
  strictEqual(system.tier, "system");
  strictEqual(system.text, "STABLE PREFIX");
  strictEqual(volatile.text, "VOLATILE");

  // The fact the whole loop rests on: the prefix a round ran with is recoverable
  // from the file, byte for byte, not merely named by it.
  strictEqual(round.systemSha, sectionSha(system.text as string));
  strictEqual(round.volatileSha, sectionSha(volatile.text as string));

  strictEqual(round.n, 1);
  strictEqual(round.model, "claude-haiku-4-5");
  deepStrictEqual(round.request.tools, ["run_steps"]);
  deepStrictEqual(round.request.messages, PARAMS.messages);
  strictEqual(round.response?.stopReason, "end_turn");
  // Cost is present because tracing wraps pricing, not the other way round.
  strictEqual(round.response?.usage?.costUsd, 0.5);
  ok(round.latencyMs >= 0);
});

test("an unchanged prefix is written once and referenced by sha afterwards", async () => {
  const l = label();
  const client = withTrace(stub(), l);
  await client.run(PARAMS, () => {});
  await client.run(PARAMS, () => {});

  const all = lines(l);
  strictEqual(all.filter((r) => r.type === "prompt").length, 2, "one per tier, not per round");
  const rounds = all.filter((r) => r.type === "round") as unknown as RoundRecord[];
  deepStrictEqual(rounds.map((r) => r.n), [1, 2]);
  strictEqual(rounds[0].systemSha, rounds[1].systemSha);
});

test("a failed attempt is recorded and still throws", async () => {
  const l = label();
  const client = withTrace(stub(() => Promise.reject(new TypeError("boom"))), l);
  await client.run(PARAMS, () => {}).then(
    () => ok(false, "should have rejected"),
    (err) => strictEqual((err as Error).message, "boom"),
  );

  const round = lines(l).find((r) => r.type === "round") as unknown as RoundRecord;
  // The retry wrapper sits OUTSIDE this one precisely so a swallowed attempt is
  // still evidence: an experiment reading only successes would misread a flaky
  // round as a clean one.
  strictEqual(round.error?.message, "boom");
  strictEqual(round.error?.name, "TypeError");
  strictEqual(round.response, undefined);
});

test("tracing off returns the client untouched", () => {
  const inner = stub();
  strictEqual(withTrace(inner, null), inner);
  strictEqual(traceLabel("s", "t", () => undefined), null);
  strictEqual(traceLabel("s", "t", () => "  "), null, "a blank dir is not a directory");
  deepStrictEqual(traceLabel("s", "t", () => "/tmp/x"), {
    dir: "/tmp/x",
    sessionId: "s",
    turnId: "t",
  });
});
