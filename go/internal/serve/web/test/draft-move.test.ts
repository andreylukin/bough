import { expect, test } from "bun:test";
import { moveDraft } from "../src/app";

const store = (init: Record<string, string>) => {
  const m = new Map(Object.entries(init));
  return {
    m,
    getItem: (k: string) => m.get(k) ?? null,
    setItem: (k: string, v: string) => void m.set(k, v),
    removeItem: (k: string) => void m.delete(k),
  };
};

// specs/draft_move_start_project.fizz: the draft moves as it is, tags as
// tags with their pastes and slots, and the old session keeps no key.
// Expanding it at pick time moved a paste as raw wrapper text and an
// image still uploading as a tag whose path was then written nowhere.
test("Start project session moves the draft with its tags, keeping nothing behind", () => {
  const atts = JSON.stringify({ pastes: ["a\nb"], images: [""] });
  const s = store({
    "bough:draft:old": "[Pasted text #1 +2 lines] [Image #1] ",
    "bough:draft-atts:old": atts,
    "bough:draft-ask:old": "",
  });
  moveDraft("old", "new", s);
  expect(Object.fromEntries(s.m)).toEqual({
    "bough:draft:new": "[Pasted text #1 +2 lines] [Image #1] ",
    "bough:draft-atts:new": atts,
  });
});

test("an empty draft moves nothing and still clears the old keys", () => {
  const s = store({ "bough:draft-ask:old": "" });
  moveDraft("old", "new", s);
  expect(s.m.size).toBe(0);
});
