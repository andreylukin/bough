import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import type { Row } from "../src/types";
const { ChangesBody } = await import("../src/changes");

const row = { id: "s1", title: "t", cwd: "/repo", status: "done", live: false, archived: false, entries: 3 } as unknown as Row;
const read = (files: any[]) => ({ files, repo: true, failed: false, at: 1 });
const data = { session: read([{ path: "src/math.ts", add: -1, del: -1, patch: false }]), tree: read([]), retry: () => {} } as any;

test("a session edit with no checkpoint says the diff is unavailable and points at the working tree", () => {
  const pop = renderToStaticMarkup(<ChangesBody row={row} data={data} scope="session" onScope={() => {}} />);
  expect(pop).not.toContain("Patch not recorded");
  expect(pop).toContain("Diff unavailable");
  // MB-PAGES: the page says it once, as a callout, and the file row only says "No diff".
  const page = renderToStaticMarkup(<ChangesBody row={row} data={data} scope="session" onScope={() => {}} cards />);
  expect(page.split("No checkpoint for this session").length - 1).toBe(1);
  expect(page).not.toContain("Diff unavailable");
  expect(page).toContain("See working tree");
  const one = renderToStaticMarkup(<ChangesBody row={row} data={{ ...data, session: read([
    { path: "a.ts", add: -1, del: -1, patch: false }, { path: "b.ts", add: 1, del: 0, patch: true }]) }} scope="session" onScope={() => {}} />);
  expect(one).toContain("Diff unavailable");
});

// R2-G: a tools.write has no printed patch; its diff is the content, all added, numbered from 1.
test("a write reads as a file row of numbered additions", async () => {
  const { callEdits, FileEdit } = await import("../src/changes");
  const [w] = callEdits('tools.write("src/main.js", `import { add } from "./math.js";\nconsole.log(add(2, 3));\n`)');
  const html = renderToStaticMarkup(<FileEdit edit={w} />);
  expect(html).toContain("main.js");
  expect(html).toContain("+2");
  expect((html.match(/dl-add/g) ?? []).length).toBe(2);
  expect(html).toMatch(/dl-n[^>]*>2</);
  expect(html).not.toContain("dl-del");
});

test("the turn scope is a tab only when a turn is named", () => {
  const plain = renderToStaticMarkup(<ChangesBody row={row} data={data} scope="session" onScope={() => {}} />);
  expect(plain).not.toContain("This turn");
  const turn = renderToStaticMarkup(<ChangesBody row={row} data={{ ...data, turn: read([{ path: "a.ts", add: 2, del: 0, patch: true }]) }} scope="turn" onScope={() => {}} />);
  expect(turn).toContain("This turn");
  expect(turn).toContain("a.ts");
});

// R3-I: a fresh load of #/s/<id>/changes?turn=4 lost ?turn before the router had read the hash.
test("the router never rewrites the hash before it has read it, and keeps a sub-page's query", async () => {
  const { hashToReplace } = await import("../src/app");
  const link = "#/s/s1/changes?turn=4";
  expect(hashToReplace(link, "#/", null, true)).toBeNull();
  expect(hashToReplace(link, "#/s/s1/changes", "changes", false)).toBeNull();
  expect(hashToReplace("#/s/s1/changes", "#/s/s1", null, false)).toBe("#/s/s1");
});

// MB-PAGES: the page's scope switch is a segmented control with counts; an empty scope is disabled, not "None".
test("the Changes page scopes show counts and disable an empty scope", () => {
  const html = renderToStaticMarkup(<ChangesBody row={row} data={data} scope="session" onScope={() => {}} cards />);
  expect(html).not.toContain("None");
  expect(html).toMatch(/Working tree <span class="num seg-count">0<\/span>/);
  expect(html).toMatch(/<button[^>]*disabled=""[^>]*>Working tree/);
});
