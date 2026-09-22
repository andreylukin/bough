import { expect, mock, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
mock.module("dompurify", () => ({ default: { sanitize: (s: string) => s } }));
const { Controls } = await import("../src/app");
import type { Row } from "../src/types";

const row: Row = { id: "s1", title: "t", cwd: "/w", status: "idle", live: false, archived: false, entries: 1, modified: "2026-01-02T10:00:00Z", lastAt: "2026-01-02T10:00:00Z", mode: "local" };
const cat = {
  providers: [{ plugin: "llm-openrouter", models: [{ id: "openai/gpt-6-astra", efforts: ["low", "medium", "high"] }] }],
  efforts: ["low", "medium", "high"],
  default: { plugin: "llm-openrouter", model: "openai/gpt-6-astra", effort: "medium" },
};
const noop = () => {};
const render = (c: object) => renderToStaticMarkup(<Controls row={row} projects={[]} only="model" onModel={noop} onEffort={noop} onAssign={noop} catalogue={{ cat: c as never, failed: false, retry: noop }} />);

test("CD: an unpicked session shows the configured model and effort, not the word Default", () => {
  const html = render(cat);
  expect(html).toContain("gpt-6-astra");
  expect(html).not.toContain("Default model");
  expect(html).not.toContain("Default effort");
  expect(html).toContain("Medium");
});

test("CD: with no configured effort the picker says the provider decides; with no default at all the old words stay", () => {
  expect(render({ ...cat, default: { ...cat.default, effort: "" } })).toContain("Default");
  const bare = render({ providers: cat.providers, efforts: cat.efforts });
  expect(bare).toContain("Default model");
});
