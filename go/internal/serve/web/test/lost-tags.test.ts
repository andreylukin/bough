import { expect, test } from "bun:test";
import { readFileSync } from "fs";
import { join } from "path";
const { lostTags } = await import("../src/prompt");

// A tag whose content is gone (a failed upload, a paste from another
// draft) is never sent as its placeholder, by Send or by Queue.
test("lostTags names the tags whose content is missing", () => {
  expect(lostTags("[Image #1] [File #2] [Pasted text #1 +20 lines]", ["/a.png", ""], ["body"])).toEqual(["[File #2]"]);
  expect(lostTags("[Image #1] and [Pasted text #2 +20 lines]", [""], ["body"])).toEqual(["[Image #1]", "[Pasted text #2 +20 lines]"]);
  expect(lostTags("plain text", [], [])).toEqual([]);
});

// Found by the attachments_paste MBT walk: Queue on a failed upload
// queued "[Image #1]" as text, because enqueue skipped send's check.
test("Send and Queue both refuse a lost tag", () => {
  const src = readFileSync(join(import.meta.dir, "../src/app.tsx"), "utf8");
  const body = (name: string) => src.slice(src.indexOf(`const ${name} = `), src.indexOf("};", src.indexOf(`const ${name} = `)));
  expect(body("send")).toContain("lostTags(");
  expect(body("enqueue")).toContain("lostTags(");
});
