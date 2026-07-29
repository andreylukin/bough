import assert from "node:assert/strict";
import { mkdtemp, writeFile } from "node:fs/promises";
import { mkdir } from "node:fs/promises";
import { homedir, tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "bun:test";
import { branchH, expandTilde, listDirEntries } from "./fs.ts";

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

test("branchH: names the branch, and says nothing rather than erroring", async () => {
  const ctx = {} as never;
  const call = async (dir: string) => {
    const res = await branchH(new Request(`http://x/fs/branch?dir=${encodeURIComponent(dir)}`), ctx, {});
    return await res.json() as { branch: string };
  };

  // This repo is a checkout on a branch, so the answer is a non-empty name.
  const here = await call(new URL("../..", import.meta.url).pathname);
  assert.ok(here.branch.length > 0, `expected a branch name, got ${JSON.stringify(here)}`);
  // A directory that is not a repository has no branch to name. Not an error: the
  // meter simply says less, and a status bar is not a place to raise one.
  const outside = await call(await mkdtemp(join(tmpdir(), "bough-nogit-")));
  assert.equal(outside.branch, "");
});

test("listDirEntries: a half-typed path answers empty, not an error", async () => {
  assert.deepEqual(await listDirEntries("/no/such/directory/anywhere"), []);
});
