import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { Thread } from "../src/app";
import type { Line, Row } from "../src/types";

const row = { id: "s2", cwd: "/tmp/x", status: "done", title: "New session" } as unknown as Row;
// The session just left: its transcript is still in the parent's state for one render.
const old = [
  { seq: 1, kind: "input", text: "old session prompt", at: "2026-09-16T00:00:00Z" },
  { seq: 2, kind: "done", text: "", at: "2026-09-16T00:00:01Z" },
] as unknown as Line[];
const props = {
  onSend: async () => null, onAnswer: async () => null, onInterrupt: () => {}, onArchive: () => {}, onRename: async () => {},
  onModel: () => {}, onEffort: () => {}, onAssign: () => {}, onBack: () => {}, onContext: () => {}, onAck: () => {},
  projects: [], busy: false, jump: null,
};

test("while a session loads, no other transcript renders under its title", () => {
  const html = renderToStaticMarkup(<Thread row={row} lines={old} loading {...props} />);
  expect(html).toContain("New session");
  expect(html).not.toContain("old session prompt");
});

test("while a session loads, the header names no status it would revise a moment later", () => {
  const html = renderToStaticMarkup(<Thread row={row} lines={[]} loading {...props} />);
  expect(html).not.toMatch(/thread-head[\s\S]*>Done</);
  const loaded = renderToStaticMarkup(<Thread row={row} lines={old} {...props} />);
  expect(loaded).toContain("old session prompt");
  expect(loaded).toMatch(/thread-head[\s\S]*>Done</);
});

import { sidebarSelected } from "../src/app";

test("a session row is only highlighted while a session is what is on screen", () => {
  expect(sidebarSelected("sessions", null, "s1", "s0")).toBe("s1");
  expect(sidebarSelected("sessions", null, null, "s0")).toBe("s0");
  for (const v of ["wiki", "hooks", "projects"] as const) expect(sidebarSelected(v, null, "s1", "s0")).toBeNull();
  expect(sidebarSelected("sessions", "nope", null, "s0")).toBeNull();
});
