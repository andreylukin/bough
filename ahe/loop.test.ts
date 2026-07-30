/**
 * `settle` decides whether an edit survives, so its bias is a design choice and not
 * an implementation detail. The cases below pin that bias: an edit earns its keep by
 * moving something it NAMED, and loses it by moving nothing, by being right for the
 * wrong reason, or by costing more elsewhere than it gained.
 *
 * The last one is the paper's own reported blind spot — self-attribution is reliable
 * for fixes and blind to regressions — so it gets a test rather than a comment.
 */

import { test } from "bun:test";
import { deepStrictEqual, strictEqual } from "node:assert";
import type { ChangeEntry } from "./agents.ts";
import { settle } from "./loop.ts";
import type { SweepResult } from "./sweep.ts";

const result = (byTask: Record<string, [number, number]>): SweepResult => ({
  rows: [],
  byTask: Object.fromEntries(
    Object.entries(byTask).map(([t, [pass, of]]) => [t, { pass, of }]),
  ),
  passRate: 0,
  costUsd: 0,
});

const change = (predicted_pass: string[]): ChangeEntry => ({
  file: "patch-grammar.md",
  failure_evidence: "e",
  root_cause: "r",
  targeted_fix: "f",
  predicted_pass,
  predicted_at_risk: [],
});

test("an edit whose named task improves, with no net loss, is kept", () => {
  const v = settle(
    [change(["alpha"])],
    result({ alpha: [0, 3], beta: [3, 3] }),
    result({ alpha: [2, 3], beta: [3, 3] }),
  );
  strictEqual(v[0].held, true);
  strictEqual(v[0].reverted, false);
  deepStrictEqual(v[0].flipped_to_pass, ["alpha"]);
});

test("an edit that moves nothing is reverted, not left in place", () => {
  // Inert prompt text is not free: it is growth that dilutes everything around it,
  // and this model's instruction-following measurably degrades as the prompt grows.
  const v = settle(
    [change(["alpha"])],
    result({ alpha: [0, 3] }),
    result({ alpha: [0, 3] }),
  );
  strictEqual(v[0].held, false);
});

test("an edit is not credited for a task it did not predict", () => {
  // Right outcome, wrong reason. Keeping this would mean the loop learns from a
  // coincidence and carries a wrong theory into the next round's edits.
  const v = settle(
    [change(["alpha"])],
    result({ alpha: [0, 3], beta: [0, 3] }),
    result({ alpha: [0, 3], beta: [3, 3] }),
  );
  strictEqual(v[0].held, false);
  deepStrictEqual(v[0].flipped_to_pass, ["beta"]);
});

test("a predicted gain that costs more elsewhere is still reverted", () => {
  const v = settle(
    [change(["alpha"])],
    result({ alpha: [0, 3], beta: [3, 3], gamma: [3, 3] }),
    result({ alpha: [2, 3], beta: [0, 3], gamma: [1, 3] }),
  );
  strictEqual(v[0].held, false, "one gain does not pay for two regressions");
  deepStrictEqual(v[0].flipped_to_fail, ["beta", "gamma"]);
});

test("a gain that exactly offsets a regression is kept, and the loss is on the record", () => {
  // Deliberately at the boundary: net zero holds, because a bank this small cannot
  // tell a one-task trade from noise, and the flip lists are what a human reads.
  const v = settle(
    [change(["alpha"])],
    result({ alpha: [0, 3], beta: [3, 3] }),
    result({ alpha: [3, 3], beta: [0, 3] }),
  );
  strictEqual(v[0].held, true);
  deepStrictEqual(v[0].flipped_to_fail, ["beta"]);
});
