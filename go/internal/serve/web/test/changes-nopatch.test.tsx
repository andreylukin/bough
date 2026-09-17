import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import type { Row } from "../src/types";
const { ChangesBody } = await import("../src/changes");

const row = { id: "s1", title: "t", cwd: "/repo", status: "done", live: false, archived: false, entries: 3 } as unknown as Row;
const read = (files: any[]) => ({ files, repo: true, failed: false, at: 1 });
const data = { session: read([{ path: "src/math.ts", add: -1, del: -1, patch: false }]), tree: read([]), retry: () => {} } as any;

test("a session edit with no checkpoint says the diff is unavailable and points at the working tree", () => {
  for (const cards of [false, true]) {
    const html = renderToStaticMarkup(<ChangesBody row={row} data={data} scope="session" onScope={() => {}} cards={cards} />);
    expect(html).not.toContain("Patch not recorded");
    expect(html).toContain("Diff unavailable");
  }
  const one = renderToStaticMarkup(<ChangesBody row={row} data={{ ...data, session: read([
    { path: "a.ts", add: -1, del: -1, patch: false }, { path: "b.ts", add: 1, del: 0, patch: true }]) }} scope="session" onScope={() => {}} />);
  expect(one).toContain("Diff unavailable");
});
