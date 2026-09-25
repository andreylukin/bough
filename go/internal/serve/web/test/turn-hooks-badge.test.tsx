import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { TurnHooks } from "../src/app";

// go/tests/model/specs/hook_deny_effect_vs_ledger.fizz, DenialShowsFailure:
// the ledger writes "denied"/"blocked", and a refusal is the red badge,
// never the grey count. The badge once tested "deny"/"block", words no
// ledger entry carries, so "1 denied" sat in the quiet part.
const fire = (decision: string, output: unknown = { deny: "no" }) => ({
  seq: 1, at: "2026-09-24T12:00:00Z", kind: "hook", text: "",
  data: { event: "pre-code-exec", name: "a_first.js", decision, error: "", output },
}) as any;

for (const [decision, word] of [["denied", "denied"], ["blocked", "blocked"]]) {
  test(`a ${decision} fire is the red badge`, () => {
    const html = renderToStaticMarkup(<TurnHooks lines={[fire(decision)]} />);
    expect(html).toContain(`<span class="num toolrun-failed">1 ${word}</span>`);
    expect(html).not.toContain(`· 1 ${word}`);
  });
}

test("a rewrite stays in the quiet count", () => {
  const html = renderToStaticMarkup(<TurnHooks lines={[fire("rewrote", { args: {} })]} />);
  expect(html).toContain("1 fired · 1 rewrote");
  expect(html).not.toContain("toolrun-failed");
});
