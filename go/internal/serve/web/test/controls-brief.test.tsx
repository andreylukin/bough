import { expect, mock, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
mock.module("dompurify", () => ({ default: { sanitize: (s: string) => s } }));
const { Controls } = await import("../src/app");
import type { Project, Row } from "../src/types";

// A running child keeps the MEMORY.md it started with (serve reads the
// project directory only at a child's start), so the Project setting of
// a live session says which one is in force, and when a move takes effect.
const row: Row = { id: "s1", title: "t", cwd: "/w", status: "idle", live: true, archived: false, entries: 1, modified: "2026-01-02T10:00:00Z", lastAt: "2026-01-02T10:00:00Z", mode: "local" };
const projects = [{ slug: "web", name: "Web" }, { slug: "api", name: "API" }] as Project[];
const noop = () => {};
const note = (r: Partial<Row>) => {
  const html = renderToStaticMarkup(<Controls row={{ ...row, ...r }} projects={projects} only="rest" onModel={noop} onEffort={noop} onAssign={noop} />);
  const m = html.match(/<p class="ctl-brief"[^>]*>(.*?)<\/p>/);
  return m ? m[1].replace(/&#x27;|&#39;/g, "'") : null;
};

test("CB: a live session filed where it started runs with that project's MEMORY.md", () => {
  expect(note({ project: "web", startedIn: "web" })).toBe("Running with Web’s MEMORY.md.");
  expect(note({})).toBe("Running with no project MEMORY.md.");
});

test("CB: a move while it runs says the new filing waits for its next start", () => {
  expect(note({ project: "api", startedIn: "web" })).toBe("Running with Web’s MEMORY.md. API’s applies from its next start.");
  expect(note({ project: "api" })).toBe("Running with no project MEMORY.md. API’s applies from its next start.");
  expect(note({ startedIn: "web" })).toBe("Running with Web’s MEMORY.md. Taking it out applies from its next start.");
});

test("CB: nothing is said with no child running, or of a project session", () => {
  expect(note({ live: false, project: "web" })).toBeNull();
  expect(note({ mode: "project", project: "web" })).toBeNull();
});
