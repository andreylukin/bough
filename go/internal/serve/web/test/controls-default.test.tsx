import { expect, mock, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
mock.module("dompurify", () => ({ default: { sanitize: (s: string) => s } }));
const { Controls } = await import("../src/app");
import type { Row } from "../src/types";

const row: Row = { id: "s1", title: "t", cwd: "/w", status: "idle", live: false, archived: false, entries: 1, modified: "2026-01-02T10:00:00Z", lastAt: "2026-01-02T10:00:00Z", mode: "local" };
const configured = { plugin: "llm-openrouter", model: "openai/gpt-6-astra", effort: "medium" };
const cat = {
  providers: [{ plugin: "llm-openrouter", models: [{ id: "openai/gpt-6-astra", efforts: ["low", "medium", "high"] }] }],
  efforts: ["low", "medium", "high"],
  // Serve's own llm row: not what a session in another cwd runs.
  default: { plugin: "llm-echo", model: "serve-default" },
};
const noop = () => {};
const render = (c: object | null, r: Row = { ...row, configured }) => renderToStaticMarkup(<Controls row={r} projects={[]} only="model" onModel={noop} onEffort={noop} onAssign={noop} catalogue={{ cat: c as never, failed: false, retry: noop }} />);

test("CD: an unpicked session shows its configured model and effort, not the word Default", () => {
  const html = render(cat);
  expect(html).toContain("gpt-6-astra");
  expect(html).not.toContain("Default model");
  expect(html).not.toContain("Default effort");
  expect(html).toContain("Medium");
});

test("CD: the configured row is the session's, never serve's /api/models default", () => {
  const html = render(cat, { ...row, configured: { plugin: "llm-control", model: "" } });
  expect(html).toContain("Next turn model: llm-control");
  expect(html).not.toContain("serve-default");
});

test("CD: with no configured effort the picker says the provider decides; with no configured row the old words stay", () => {
  expect(render(cat, { ...row, configured: { ...configured, effort: "" } })).toContain("Default");
  expect(render(cat, row)).toContain("Default model");
});

test("CD: until the catalogue loads the picker names nothing", () => {
  const html = render(null);
  expect(html).toContain("Next turn model: Loading");
  expect(html).not.toContain("gpt-6-astra");
});
