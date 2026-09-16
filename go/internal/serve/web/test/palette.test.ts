import { expect, test } from "bun:test";
import { isPathQuery } from "../src/palette";

// A pasted path used to start a session with the path as its first message.
test("a folder path is a place, not a message", () => {
  for (const q of ["~", "~/", "~/repos/bough", "/tmp", "  /Users/x "]) expect(isPathQuery(q)).toBe(true);
  for (const q of ["fix ~/x", "~bob", "repos/bough", "why does / fail", ""]) expect(isPathQuery(q)).toBe(false);
});
