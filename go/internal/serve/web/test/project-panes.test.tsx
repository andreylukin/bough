import { expect, test } from "bun:test";
import { useContext } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { ProjectPage, StartThreadCtx } from "../src/project";
import type { ProjectDetail, Row } from "../src/types";

const at = (h: number) => new Date(Date.now() - h * 3_600_000).toISOString();
const row = (id: string, title: string, over: Partial<Row> = {}): Row => ({
  id, title, cwd: "/w/bough", status: "idle", live: false, archived: false, entries: 3, modified: at(2), lastAt: at(2), mode: "project", project: "bough", ...over,
});
const detail: ProjectDetail = { slug: "bough", name: "Control room", main: "main-1", orbs: [], threads: [row("t1", "Port the PTY suite")] };
const noop = () => {};
const page = (over: Partial<Parameters<typeof ProjectPage>[0]> = {}) =>
  renderToStaticMarkup(
    <ProjectPage detail={detail} files={{}} open="" onOpen={noop} onBack={noop} onStopOrb={noop} onRetry={noop} onSave={async () => {}} onMessage={async () => {}}
                 mainRow={row("main-1", "Main thread")} conversation={<div className="thread">conversation</div>} {...over} />,
  );

test("PP: beside a conversation the project panel opens closed; on the home it is half the page", () => {
  const html = page({ open: "t1" });
  expect(html).not.toContain('class="prj-panel"');
  expect(html).toContain('class="btn btn-sm prj-panel-btn" aria-expanded="false"');
  expect(page()).toContain('class="prj-panel"');
});

test("PP: the thread column carries its fold chevron and lists threads beside a conversation", () => {
  const html = page({ open: "t1" });
  expect(html).toContain('aria-label="Hide threads"');
  expect(html).toContain("prj-threads-list");
  expect(html).not.toContain("prj-threads-folded");
});

test("PP: only the main thread's conversation is offered Start thread", () => {
  const Probe = () => <p>{useContext(StartThreadCtx) ? "start:yes" : "start:no"}</p>;
  const start = async () => {};
  expect(page({ open: "main-1", conversation: <Probe />, onStartThread: start })).toContain("start:yes");
  expect(page({ open: "t1", conversation: <Probe />, onStartThread: start })).toContain("start:no");
  expect(page({ open: "main-1", conversation: <Probe /> })).toContain("start:no");
});
