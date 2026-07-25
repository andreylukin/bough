import { assertEquals } from "jsr:@std/assert@1";
import { Diff, type FileDiff, parseGitDiff } from "./changes.ts";

Deno.test("Diff schema round-trips", () => {
  const d = {
    source: "repo",
    files: [
      {
        path: "a.txt",
        status: "modified",
        hunks: [{ header: "@@ -1,1 +1,2 @@", lines: [" hello", "+world"] }],
      },
    ],
  };
  assertEquals(Diff.parse(d), d);
});

Deno.test("parseGitDiff: modified, added, deleted in one payload", () => {
  const text = [
    "diff --git a/a.txt b/a.txt",
    "index ce01362..94954ab 100644",
    "--- a/a.txt",
    "+++ b/a.txt",
    "@@ -1,1 +1,2 @@",
    " hello",
    "+world",
    "diff --git a/b.txt b/b.txt",
    "new file mode 100644",
    "index 0000000..3e75765",
    "--- /dev/null",
    "+++ b/b.txt",
    "@@ -0,0 +1,1 @@",
    "+new",
    "diff --git a/c.txt b/c.txt",
    "deleted file mode 100644",
    "index 2fa992c..0000000",
    "--- a/c.txt",
    "+++ /dev/null",
    "@@ -1 +0,0 @@",
    "-gone",
    "",
  ].join("\n");

  const files: FileDiff[] = parseGitDiff(text);
  assertEquals(files.length, 3);

  assertEquals(files[0].path, "a.txt");
  assertEquals(files[0].status, "modified");
  assertEquals(files[0].hunks, [{ header: "@@ -1,1 +1,2 @@", lines: [" hello", "+world"] }]);

  assertEquals(files[1].path, "b.txt");
  assertEquals(files[1].status, "added");
  assertEquals(files[1].hunks[0].lines, ["+new"]);

  assertEquals(files[2].path, "c.txt");
  assertEquals(files[2].status, "deleted");
  assertEquals(files[2].hunks[0].lines, ["-gone"]);
});

Deno.test("parseGitDiff: stripPrefix recovers a clean path", () => {
  const text = [
    "diff --git a/root/orig/.zshrc b/root/snap/root/orig/.zshrc",
    "--- a/root/orig/.zshrc",
    "+++ b/root/snap/root/orig/.zshrc",
    "@@ -1 +1,2 @@",
    " x",
    "+y",
  ].join("\n");
  const files = parseGitDiff(text, (p) => p.replace(/^root\/snap\//, ""));
  assertEquals(files[0].path, "root/orig/.zshrc");
});

Deno.test("parseGitDiff: empty input yields no files", () => {
  assertEquals(parseGitDiff(""), []);
});

Deno.test("parseGitDiff: multiple hunks in one file", () => {
  const text = [
    "diff --git a/f b/f",
    "--- a/f",
    "+++ b/f",
    "@@ -1,2 +1,2 @@",
    " a",
    "-b",
    "+B",
    "@@ -10,2 +10,3 @@",
    " j",
    "+k",
    " l",
  ].join("\n");
  const files = parseGitDiff(text);
  assertEquals(files[0].hunks.length, 2);
  assertEquals(files[0].hunks[1].header, "@@ -10,2 +10,3 @@");
});
