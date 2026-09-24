import { afterAll, beforeAll, expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { Sidebar } from "../src/app";
import { ProjectPage, threadGroup } from "../src/project";
import type { ProjectDetail, Row } from "../src/types";

// Sidebar reads window.navigation at mount; a bare object is enough for a static render.
const g = globalThis as unknown as { window?: object };
beforeAll(() => { g.window ??= {}; });
afterAll(() => { delete g.window; });

const at = (h: number) => new Date(Date.now() - h * 3_600_000).toISOString();
function row(id: string, title: string, over: Partial<Row> = {}): Row {
  return {
    id, title, cwd: "/w/bough", status: "done", live: false, archived: false, entries: 3,
    modified: at(1), lastAt: at(1), mode: "local", project: "bough", ...over,
  };
}

const fresh = row("u1", "Finished while away", { unseen: true });
const seen = row("d1", "Looked at already");

test("a clean finish nobody has seen is its own group; trouble keeps its own", () => {
  expect(threadGroup(fresh)).toBe("unseen");
  expect(threadGroup(seen)).toBe("done");
  // A failed finish is Error, never also unseen.
  expect(threadGroup(row("e1", "x", { unseen: true, trouble: "tests failed", testsFailed: true }))).toBe("error");
});

const detail: ProjectDetail = { slug: "bough", name: "bough", main: "", orbs: [], threads: [seen, fresh] };
const noop = () => {};
const page = (open: string) => renderToStaticMarkup(
  <ProjectPage detail={detail} files={{}} open={open} onOpen={noop} onBack={noop} onStopOrb={noop} onRetry={noop}
               onSave={async () => {}} onMessage={async () => {}} conversation={<div className="thread">c</div>} />,
);

test("the project page lists unseen finishes above Done, each with the blue dot", () => {
  const html = page("x");
  const unseenAt = html.indexOf('prj-group-label">Finished — unseen<'), doneAt = html.indexOf('prj-group-label">Done<');
  expect(unseenAt).toBeGreaterThan(-1);
  expect(doneAt).toBeGreaterThan(unseenAt);
  expect(html.match(/class="unseen-dot"/g)?.length).toBe(1);
  expect(html).toContain("not seen yet");
  // The open thread is being read: no dot while its ack is on the way.
  expect(page("u1")).not.toContain("unseen-dot");
});

const side = (selected: string | null) => renderToStaticMarkup(
  <Sidebar rows={[fresh, seen]} selected={selected} onSelect={noop} query="" onQuery={noop} showArchived={false} onToggleArchived={noop} />,
);

test("the sidebar marks an unseen finish with the same dot, and says it in words", () => {
  const html = side(null);
  expect(html.match(/class="unseen-dot"/g)?.length).toBe(1);
  expect(html).toContain("Done, not seen yet");
  expect(side("u1")).not.toContain("unseen-dot");
});

test("the row kept as your place after going back still shows its dot until the ack lands", () => {
  // Home marks the session you left (selected), but nothing is on screen
  // (viewing ""): leaving before the ack answered must not hide the dot.
  const html = renderToStaticMarkup(
    <Sidebar rows={[fresh, seen]} selected="u1" viewing="" onSelect={noop} query="" onQuery={noop} showArchived={false} onToggleArchived={noop} />,
  );
  expect(html.match(/class="unseen-dot"/g)?.length).toBe(1);
});
