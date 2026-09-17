import { expect, test } from "bun:test";
import { isPathQuery } from "../src/palette";

// A pasted path used to start a session with the path as its first message.
test("a folder path is a place, not a message", () => {
  for (const q of ["~", "~/", "~/repos/bough", "/tmp", "  /Users/x "]) expect(isPathQuery(q)).toBe(true);
  for (const q of ["fix ~/x", "~bob", "repos/bough", "why does / fail", ""]) expect(isPathQuery(q)).toBe(false);
});

import { matchesOps, parseOps, markParts } from "../src/palette";

test("operators come out of the words and narrow the rows", () => {
  const o = parseOps("ENOSPC project:bough after:7d status:failed");
  expect(o.text).toBe("ENOSPC");
  expect(o.project).toBe("bough");
  expect(o.status).toBe("failed");
  expect(Math.abs((o.after ?? 0) - (Date.now() - 7 * 86_400_000))).toBeLessThan(5_000);
  const row = { id: "x", title: "t", repo: "/r/bough", status: "error", lastAt: new Date().toISOString() } as never;
  expect(matchesOps(row, o)).toBe(true);
  expect(matchesOps(row, parseOps("status:done"))).toBe(false);
  expect(matchesOps(row, parseOps("project:other"))).toBe(false);
  const old = { ...(row as object), lastAt: "2020-01-01T00:00:00Z" } as never;
  expect(matchesOps(old, parseOps("after:7d"))).toBe(false);
  expect(parseOps("see http://x").text).toBe("see http://x");
});

test("snippet matches are split out for <mark>", () => {
  expect(markParts("the Deploy failed on deploy", ["deploy"])).toEqual([
    { t: "the ", m: false }, { t: "Deploy", m: true }, { t: " failed on ", m: false }, { t: "deploy", m: true },
  ]);
  expect(markParts("plain", [])).toEqual([{ t: "plain", m: false }]);
});

import { recentSessions, startFolders, visit } from "../src/palette";

const r = (id: string, lastAt: string, cwd = "/r/" + id, extra = {}) => ({ id, title: id, cwd, lastAt, status: "done", ...extra }) as never;

test("visits keep most-recent-first order without repeats", () => {
  expect(visit(["b", "a"], "a")).toEqual(["a", "b"]);
  expect(visit([], "x")).toEqual(["x"]);
});

test("the switcher lists the previous session first, then the rest by activity", () => {
  const rows = [r("a", "2026-09-10T00:00:00Z"), r("b", "2026-09-12T00:00:00Z"), r("c", "2026-09-11T00:00:00Z"), r("d", "2026-09-13T00:00:00Z", "/x", { archived: true })];
  // At a, having come from c: Enter goes back to c.
  expect(recentSessions(rows, "a", ["a", "c"], 5).map((x: { id: string }) => x.id)).toEqual(["c", "b"]);
});

test("new session offers each known folder once, newest first, not home", () => {
  const rows = [r("a", "2026-09-10T00:00:00Z", "/r/one"), r("b", "2026-09-12T00:00:00Z", "/r/two"), r("c", "2026-09-11T00:00:00Z", "/r/one"), r("h", "2026-09-13T00:00:00Z", "/home")];
  expect(startFolders(rows, "/home")).toEqual(["/r/two", "/r/one"]);
});
