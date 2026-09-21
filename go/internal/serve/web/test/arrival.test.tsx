import { expect, mock, test } from "bun:test";
mock.module("dompurify", () => ({ default: { sanitize: (s: string) => s } }));
import { arrivalPick } from "../src/app";
import type { Row } from "../src/types";

const now = Date.parse("2026-09-21T12:00:00Z");
const at = (h: number) => new Date(now - h * 3_600_000).toISOString();
const row = (id: string, over: Partial<Row> = {}): Row =>
  ({ id, title: id, cwd: "/w", status: "done", live: false, archived: false, entries: 3, modified: at(1), lastAt: at(1), ...over });

test("arrival opens what needs you from the last day, never an old or background thread", () => {
  // A week-old interrupted background agent outranks on signal; it is not what you came for.
  const stale = row("stale", { status: "interrupted", background: true, lastAt: at(24 * 6) });
  const recent = row("recent", { lastAt: at(2) });
  const asking = row("asking", { status: "needs-you", lastAt: at(5) });
  expect(arrivalPick([stale, recent, asking], now)?.id).toBe("asking");
  expect(arrivalPick([stale, recent], now)?.id).toBe("recent");
  // Nothing from today: nothing opens, and the overview stays.
  expect(arrivalPick([stale, row("old", { lastAt: at(30) })], now)).toBeUndefined();
  // A person's session from today beats a background one from today.
  expect(arrivalPick([row("bg", { background: true, lastAt: at(0.1) }), row("me", { lastAt: at(3) })], now)?.id).toBe("me");
  expect(arrivalPick([row("e", { empty: true }), row("a", { archived: true })], now)).toBeUndefined();
});
