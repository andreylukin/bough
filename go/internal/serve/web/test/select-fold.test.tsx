import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { foldState, SelectView, type Option } from "../src/select";

// The model picker lists every model of every provider (hundreds on
// OpenRouter): it opens as one heading per provider, a click unfolds one,
// and a search shows every match.
const all: Option[] = [
  { value: "cur", label: "claude-opus-5-5", group: "Next turn" },
  { value: "a1", label: "claude-sonnet-5", group: "Anthropic" },
  { value: "a2", label: "claude-haiku-4-5", group: "Anthropic" },
  { value: "o1", label: "openai/gpt-6-astra", group: "Openrouter" },
  { value: "o2", label: "anthropic/claude-opus-5.5", group: "Openrouter" },
  { value: "o3", label: "google/gemini-4-pro", group: "Openrouter" },
];

const view = (unfolded: string[], on = true) => {
  const fold = foldState(all, on, "Next turn", new Set(unfolded));
  const shown = all.filter((o) => !fold.folded(o.group));
  const noop = () => {};
  return { shown, html: renderToStaticMarkup(<SelectView open save={null} shown={shown} all={all} fold={fold} at={0} value="cur"
    listId="l" optId={(i) => `o${i}`} label="Model" placeholder="Choose" searchable align="end" disabled={false} pos={{}} q=""
    onKey={noop} onButton={noop} onRetrySave={noop} onQ={noop} onHover={noop} onPick={noop} />) };
};

test("SF: every provider opens folded under a heading with its count; the current model stays", () => {
  const { shown, html } = view([]);
  expect(shown.map((o) => o.value)).toEqual(["cur"]);
  expect(html).toContain("claude-opus-5-5");
  expect(html).not.toContain("gpt-6-astra");
  expect(html).toMatch(/aria-expanded="false"[^>]*>.*?Anthropic.*?>2</);
  expect(html).toMatch(/aria-expanded="false"[^>]*>.*?Openrouter.*?>3</);
});

test("SF: unfolding one provider shows its models and leaves the others folded", () => {
  const { shown, html } = view(["Openrouter"]);
  expect(shown.map((o) => o.value)).toEqual(["cur", "o1", "o2", "o3"]);
  expect(html).toContain("gpt-6-astra");
  expect(html).not.toContain("claude-sonnet-5");
  // Option ids stay contiguous over what is shown, so aria-activedescendant and ↓↑ agree.
  expect(html).toContain('id="o3"');
  expect(html).not.toContain('id="o4"');
});

test("SF: a search (fold off) shows every match with plain headings", () => {
  const { shown, html } = view([], false);
  expect(shown).toHaveLength(all.length);
  expect(html).not.toContain("aria-expanded=\"false\"");
  expect(html).not.toContain("sel-fold");
});

test("SF: a list with only one foldable group never folds it", () => {
  const one = all.filter((o) => o.group !== "Anthropic");
  const fold = foldState(one, true, "Next turn", new Set());
  expect(fold.folded("Openrouter")).toBe(false);
});
