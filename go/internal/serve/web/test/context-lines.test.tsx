import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { ContextView, type ContextData, type ContextFile } from "../src/context";

const file = (path: string, lines: number, rest: Partial<ContextFile> = {}): ContextFile =>
  ({ path, lines, found: true, dropped: 0, same: "", ...rest });

const view = (contextFiles: ContextFile[]) =>
  renderToStaticMarkup(
    <ContextView data={{ cwd: "/w", rules: [], contextFiles, skills: [] } as ContextData}
                 load={async () => ""} save={async () => {}} setOff={async () => {}} />,
  );

// A context file is prepended to every turn, so its length is the cost
// the row has to show.
test("a context file row counts its lines", () => {
  const html = view([file("/w/AGENTS.md", 84)]);
  expect(html).toContain("84 lines");
  expect(html).toContain("in full");
  expect(html).not.toContain("ctx-lines-long");
});

test("past 200 lines the count turns amber and says why", () => {
  const html = view([file("/w/AGENTS.md", 201)]);
  expect(html).toContain("ctx-lines-long");
  expect(html).toContain("injected every turn");
  expect(html).not.toContain("ctx-lines-max");
});

test("past 400 lines the count turns red", () => {
  const html = view([file("/w/AGENTS.md", 401)]);
  expect(html).toContain("ctx-lines-max");
});

// The length and the de-duplication are one line, not two: the row is
// 28-32px and the grid has three columns.
test("the dropped-sections note keeps its place beside the count", () => {
  const html = view([file("/w/CLAUDE.md", 96, { dropped: 3, same: "/w/AGENTS.md" })]);
  expect(html).toContain("96 lines");
  expect(html).toContain("3 sections dropped, identical to AGENTS.md");
  expect(html).not.toContain("in full");
});

// A file bough looked for and did not find is listed by path alone.
test("a missing file gets no line count", () => {
  const html = view([file("/w/AGENTS.md", 12), file("/h/CLAUDE.md", 0, { found: false })]);
  expect(html).toContain("1 file not found");
  expect(html).not.toContain("0 lines");
});
