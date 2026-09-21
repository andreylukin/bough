import { expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { renderToStaticMarkup } from "react-dom/server";
import { FireInspection, FireRow } from "../src/hooks";

const css = readFileSync(new URL("../dist/index.html", import.meta.url), "utf8");
const fire = (n: number) => ({
  id: String(n), name: "rules", event: "post-result", at: `2026-06-01T12:00:0${n}Z`, ms: 0,
  description: "Append matching rules", input: { n }, output: null,
}) as any;

test("a closed folded row is one line and spells a sub-millisecond run once", () => {
  const all = [fire(3), fire(2), fire(1)];
  const html = renderToStaticMarkup(<FireRow f={all[0]} n={3} all={all} titles={{}} load={async () => ({ path: "", body: "" })} save={async () => {}} />);
  expect(html).toContain("&lt;1ms");
  expect(html).not.toContain("0ms");
  expect(html).not.toContain("hk-fold-body");
});

test("the inspection hides the purpose when told and marks a quiet output", () => {
  const html = renderToStaticMarkup(<FireInspection fire={fire(1)} showDescription={false} showDefinition={false} />);
  expect(html).not.toContain("hk-description");
  expect(html).not.toContain("Definition unavailable");
  expect(html).toContain("hk-io is-quiet");
  expect(html).toContain(">Copy<");
  expect(renderToStaticMarkup(<FireInspection fire={{ ...fire(1), output: { ok: true } }} />)).not.toContain("is-quiet");
});

test("hooks table: the column head sticks, the fold is prose, the open row is selected", () => {
  expect(css).toMatch(/\.hk-table>\.hk-cols\{position:sticky;top:var\(--stick-head\)/);
  expect(css).toMatch(/\.hk-table \.hk-fold-body\{[^}]*font:var\(--fs-sm\) var\(--sans\)/);
  expect(css).toMatch(/\.hk-table \.hk-fire\.hk-open>\.hk-main[^{]*\{background:var\(--sel\)\}/);
  expect(css).toMatch(/\.hk-table \.hk-dayrun>\.hk-day\{[^}]*text-transform:none/);
  expect(css).toMatch(/\.hk-table\{[^}]*overflow:clip/);
});
