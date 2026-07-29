import assert from "node:assert/strict";
import { mkdtemp, writeFile } from "node:fs/promises";
import { mkdir } from "node:fs/promises";
import { homedir, tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "bun:test";
import { expandTilde, listDirEntries } from "./fs.ts";

test("expandTilde: `~` is the home directory, and nothing else is touched", () => {
  assert.equal(expandTilde("~"), homedir());
  assert.equal(expandTilde("~/repos"), join(homedir(), "repos"));
  assert.equal(expandTilde("/etc"), "/etc");
  // Not a home reference — `~foo` is another user's home, which we do not expand.
  assert.equal(expandTilde("~foo/bar"), "~foo/bar");
});

test("listDirEntries: one level, directories marked with a trailing slash", async () => {
  const dir = await mkdtemp(join(tmpdir(), "bough-fs-"));
  await mkdir(join(dir, "sub"));
  await writeFile(join(dir, "a.txt"), "x");
  await writeFile(join(dir, ".hidden"), "x");
  await writeFile(join(dir, "sub", "deep.txt"), "x");

  const entries = await listDirEntries(dir);
  // Dotfiles are included — the client filters by what was typed — and the nested
  // file is NOT, because browsing is one segment at a time.
  assert.deepEqual(entries, [".hidden", "a.txt", "sub/"]);
});

test("listDirEntries: a half-typed path answers empty, not an error", async () => {
  assert.deepEqual(await listDirEntries("/no/such/directory/anywhere"), []);
});
