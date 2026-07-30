/**
 * Call-site extraction is the one derived signal the whole attribution story rests
 * on — "patch() was rejected four times" is what makes a failure evidence about
 * `patch-grammar.md` rather than a general impression of struggle. So the cases here
 * are the ones that would corrupt it silently: method calls that share a host
 * function's name, and results that arrive a round late.
 *
 * `node:assert` rather than `@std/assert`: jsr.io is denied by this environment's
 * egress policy (same constraint the rest of the tree documents).
 */

import { test } from "bun:test";
import { deepStrictEqual } from "node:assert";
import type { RoundRecord } from "../src/llm/trace.ts";
import { callSites, hostFnEvents } from "./materialize.ts";

test("a method that shares a host function's name is not a host function call", () => {
  // Every one of these names is BOTH a host function and an ordinary member.
  deepStrictEqual(callSites(`parts.join(",")`), []);
  deepStrictEqual(callSites(`res.write(x); el.fetch(); o.state(); a.view()`), []);
  deepStrictEqual(callSites(`obj.patch(1)`), []);
  deepStrictEqual(callSites(`myview(1); joined(2); rewrite(3)`), []);
});

test("bare calls are host function calls, repeats and all", () => {
  deepStrictEqual(
    callSites(`const a = await view("x");\nawait patch(p);\nawait patch(q);\nbash("ls")`),
    ["view", "patch", "patch", "bash"],
  );
  deepStrictEqual(callSites(`await join (h)`), ["join"], "whitespace before the paren");
});

const round = (n: number, code: string, result?: { isError: boolean; content: string }) => ({
  type: "round" as const,
  n,
  ts: 0,
  latencyMs: 1,
  model: "m",
  systemSha: "s",
  volatileSha: "v",
  request: {
    maxTokens: 1,
    tools: [],
    messages: result
      ? [{
        role: "user" as const,
        content: [{
          type: "tool_result" as const,
          toolUseId: `call-${n - 1}`,
          content: result.content,
          isError: result.isError,
        }],
      }]
      : [],
  },
  response: {
    content: [{ type: "tool_use" as const, id: `call-${n}`, name: "run_steps", input: { code } }],
    stopReason: "tool_use",
  },
}) as unknown as RoundRecord;

test("a call is paired with the result that lands in the NEXT round", () => {
  // The provider only shows a program's outcome back on the following round, so a
  // pairing that looked only at the round itself would report every call as
  // outcome-unknown — and a failed patch would read the same as a clean one.
  const events = hostFnEvents([
    round(1, `await patch(p)`),
    round(2, `await view("x")`, { isError: true, content: "no match for the context lines" }),
  ]);
  deepStrictEqual(events.map((e) => [e.fn, e.ok]), [["patch", false], ["view", null]]);
  deepStrictEqual(events[0].result, "no match for the context lines");
});

test("the last round's call has no result rather than a false success", () => {
  const events = hostFnEvents([round(1, `await bash("ls")`)]);
  deepStrictEqual(events, [{ round: 1, fn: "bash", ok: null, result: null }]);
});
