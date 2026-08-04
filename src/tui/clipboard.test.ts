import { test } from "bun:test";
import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { clipboardFromText, clipboardImagePath } from "./clipboard.ts";

test("a clipboard naming an image file is a picture, however it names it", () => {
  assert.deepEqual(clipboardImagePath("/tmp/shot.png"), {
    path: "/tmp/shot.png",
    mediaType: "image/png",
  });
  assert.deepEqual(clipboardImagePath("  /tmp/a.JPEG\n"), {
    path: "/tmp/a.JPEG",
    mediaType: "image/jpeg",
  });
  assert.deepEqual(clipboardImagePath("file:///tmp/with%20space.webp"), {
    path: "/tmp/with space.webp",
    mediaType: "image/webp",
  });
});

test("anything that is not one absolute image path stays text", () => {
  // A relative path, a directory, a non-image, prose that merely mentions one, and
  // a multi-line paste whose first line is a path: all words, all pasted as words.
  for (
    const text of [
      "",
      "shot.png",
      "/tmp/notes.txt",
      "/tmp/pictures",
      "look at /tmp/shot.png",
      "/tmp/a.png\n/tmp/b.png",
      "file://",
    ]
  ) {
    assert.equal(clipboardImagePath(text), null, text);
  }
});

test("the named file is read as bytes, and a missing one falls back to the text", async () => {
  const dir = mkdtempSync(join(tmpdir(), "bough-clip-"));
  const path = join(dir, "shot.png");
  writeFileSync(path, new Uint8Array([137, 80, 78, 71]));

  const pasted = await clipboardFromText(path);
  assert.ok("image" in pasted!, "an existing image path attaches");
  assert.equal(pasted.image.type, "image/png");
  assert.equal(pasted.image.size, 4);

  const viaUrl = await clipboardFromText(pathToFileURL(path).href);
  assert.ok(viaUrl && "image" in viaUrl, "a file:// URL names the same file");
  assert.equal(viaUrl.image.size, 4);

  const gone = await clipboardFromText(join(dir, "gone.png"));
  assert.deepEqual(gone, { text: join(dir, "gone.png") }, "a path to nothing is text");
  assert.deepEqual(await clipboardFromText(dir + "/x.png"), { text: dir + "/x.png" });
});
