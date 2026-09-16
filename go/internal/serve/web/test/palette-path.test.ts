import { expect, test } from "bun:test";
import { isTypingTarget, pathRows } from "../src/palette";

const places = { folder: { path: "/tmp", exists: true }, dirs: [{ path: "/tmp/x", exists: true }], home: "" };

test("a path-shaped query puts New session in first; a plain word does not", () => {
  const rows = pathRows("/tmp", places, () => {});
  expect(rows[0].label.startsWith("New session in")).toBe(true);
  expect(pathRows("tmp", places, () => {})).toEqual([]);
});

test("a missing folder says so on its row", () => {
  const rows = pathRows("/nope", { folder: { path: "/nope", exists: false }, dirs: [], home: "" }, () => {});
  expect(rows[0].label).toBe("New session in /nope");
  expect(rows[0].hint).toBe("Folder not found");
});

test("single-key shortcuts leave typing targets alone", () => {
  expect(isTypingTarget({ tagName: "INPUT" } as unknown as EventTarget)).toBe(true);
  expect(isTypingTarget({ tagName: "TEXTAREA" } as unknown as EventTarget)).toBe(true);
  expect(isTypingTarget({ tagName: "DIV", isContentEditable: true } as unknown as EventTarget)).toBe(true);
  expect(isTypingTarget({ tagName: "DIV" } as unknown as EventTarget)).toBe(false);
  expect(isTypingTarget(null)).toBe(false);
});
