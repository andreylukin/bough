import { expect, mock, test } from "bun:test";
mock.module("dompurify", () => ({ default: { sanitize: (s: string) => s } }));
import { renderToStaticMarkup } from "react-dom/server";
import { MePage, groupSignals, projectLines, standupText } from "../src/me";
import type { MeData, MeSignal, WikiBlock } from "../src/wiki";
import type { Row } from "../src/types";

const at = (h: number) => new Date(Date.now() - h * 3_600_000).toISOString();
const block = (kind: WikiBlock["kind"], text: string, over: Partial<WikiBlock> = {}): WikiBlock =>
  ({ kind, text, line: 1, end: 1, raw: text, bullet: kind === "claim", state: kind === "claim" ? "cited" : undefined, cites: [], ...over });

const blocks: WikiBlock[] = [
  block("lede", "Today is about the demand fix.", { bullet: false, cites: [{ session: "", seq: 0, source: "gh", ref: "asi/uni-nes#7801", url: "https://github.com/asi/uni-nes/issues/7801", label: "gh", excerpt: "asi/uni-nes#7801" }] }),
  block("heading", "Since yesterday", { bullet: false }),
  block("claim", "Put up the settlement fix.", { cites: [{ session: "", seq: 0, source: "gh", ref: "asi/uni-nes#7801", url: "https://github.com/asi/uni-nes/issues/7801", label: "gh", excerpt: "" }] }),
  block("heading", "Today", { bullet: false }),
  block("claim", "Chasing the review.", { cites: [{ session: "01a0c046-e48e-716b-9f1c-26c93cbbc95d", seq: 12, label: "input", excerpt: "…" }] }),
  block("heading", "Waiting on", { bullet: false }),
  block("claim", "Priya — the review.", { cites: [{ session: "", seq: 0, source: "linear", ref: "NME-1462", label: "linear", excerpt: "" }] }),
];

const signals: MeSignal[] = [
  { kind: "moving", source: "thread", title: "fix the broken ci", project: "git-ai-enrichment", at: at(1), session: "s1" },
  { kind: "needs-you", source: "gh", title: "Review comment on the demand fix", note: "Priya", project: "smart-scheduler", at: at(2), url: "https://github.com/x" },
  { kind: "done", source: "git", title: "Collector release", at: at(20) },
];

const data: MeData = {
  date: "2026-09-21", hasProfile: true, path: "topics/me/briefs/2026-09-21.md", asOf: at(0.2), days: ["2026-09-21", "2026-09-18"],
  page: { path: "topics/me/briefs/2026-09-21.md", topic: "me", title: "Brief, Mon Sep 21", summary: "", updated: "", counts: { cited: 3, inferred: 0, uncited: 0, unsupported: 0, superseded: 0 }, blocks, sessions: [], linkedFrom: [], body: "" },
  signals: { asOf: at(0.2), items: signals, sources: [{ name: "gh", ok: true, at: at(0.2) }, { name: "slack", ok: false, error: "not connected" }] },
};

const row = (id: string, project: string, over: Partial<Row> = {}): Row =>
  ({ id, title: id, cwd: "/w", status: "idle", live: false, archived: false, entries: 2, modified: at(3), lastAt: at(3), mode: "project", project, ...over });

const noop = () => {};
const page = (over: Partial<Parameters<typeof MePage>[0]> = {}) =>
  renderToStaticMarkup(<MePage data={data} rows={[row("a", "smart-scheduler", { status: "needs-you" }), row("b", "smart-scheduler", { status: "running", live: true }), row("c", "nas-event-log")]}
                               projectNames={{ "smart-scheduler": "SMART scheduler", "nas-event-log": "nas-event-log" }} onRefresh={noop} onRetry={noop} onOpenSession={noop} onOpenProject={noop} onOpenPage={noop} {...over} />);

test("the brief is prose first, then the rows by kind, then the projects", () => {
  const html = page();
  expect(html.indexOf("me-brief")).toBeLessThan(html.indexOf("me-group"));
  expect(html.indexOf("me-group")).toBeLessThan(html.indexOf("me-rail"));
  // Needs you before moving before done; waiting is absent, so no group says so.
  expect(html.indexOf('data-kind="needs-you"')).toBeLessThan(html.indexOf('data-kind="moving"'));
  expect(html.indexOf('data-kind="moving"')).toBeLessThan(html.indexOf('data-kind="done"'));
  expect(html).not.toContain('data-kind="waiting"');
  expect(html).toContain("Copy standup");
  expect(html).toContain("Refresh");
});

test("an external citation is a link out, a session citation is a chip, and a source with no address is a name", () => {
  const html = page();
  expect(html).toContain('href="https://github.com/asi/uni-nes/issues/7801"');
  expect(html).toContain(">gh:asi/uni-nes#7801<");
  expect(html).toContain(">#12<");
  expect(html).toContain('<span class="wk-cite wk-cite-ext" title="linear NME-1462">linear:NME-1462</span>');
});

test("a signal opens its session when it has one, else its url, else nothing", () => {
  const html = page();
  expect(html).toContain('<button type="button" class="me-sig">');
  expect(html).toContain('<a class="me-sig" href="https://github.com/x"');
  expect(html).toContain('<div class="me-sig">');
});

test("the rail counts each project from the fleet and names it", () => {
  const html = page();
  expect(html).toContain("SMART scheduler");
  expect(html).toContain("1 needs you");
  expect(html).toContain("1 running");
  expect(html).toContain("nas-event-log");
  expect(html).toContain(">quiet<");
  // Sources: the one that failed says why.
  expect(html).toContain("not connected");
  // Earlier briefs, not today's.
  expect(html).not.toContain("Sep 21</button>");
  expect(html).toContain("Sep 18</button>");
});

test("no profile: one explanation and no Refresh; a profile and no brief: one sentence", () => {
  const none = page({ data: { date: "2026-09-21", hasProfile: false, days: [] } });
  expect(none).toContain("Tell the brief whose work this is");
  expect(none).not.toContain(">Refresh<");
  const empty = page({ data: { date: "2026-09-21", hasProfile: true, days: [] } });
  expect(empty).toContain("Refresh writes it now");
  expect(empty).not.toContain("me-brief");
});

test("a stale brief says which day it is from", () => {
  const html = page({ data: { ...data, date: "2026-09-22", stale: true } });
  expect(html).toContain("Last brief is from");
});

test("standupText is the Since yesterday and Today sections in the thread's shape", () => {
  expect(standupText(blocks)).toBe("Yesterday:\n• Put up the settlement fix.\nToday:\n• Chasing the review.");
  expect(standupText([])).toBe("");
});

test("groupSignals keeps the page's order and drops empty kinds; projectLines skips archived rows", () => {
  expect(groupSignals(signals).map((g) => g.kind)).toEqual(["needs-you", "moving", "done"]);
  const lines = projectLines([row("a", "p1", { status: "error" }), row("b", "p1", { archived: true, status: "running" }), row("c", "p2", { status: "needs-you" })]);
  expect(lines.map((l) => l.slug)).toEqual(["p2", "p1"]);
  // With the project list in hand, a slug no project answers to is left out.
  expect(projectLines([row("a", "p1"), row("d", "gone")], { p1: "One" }).map((l) => l.slug)).toEqual(["p1"]);
  expect(lines[1].error).toBe(1);
  expect(lines[1].running).toBe(0);
});
