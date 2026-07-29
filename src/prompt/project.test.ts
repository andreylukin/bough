/**
 * `AGENTS.md` loading. The regression these pin is not a formatting one: before this
 * module existed, NOTHING in the tree read a project instruction file, so every rule
 * a user wrote in `AGENTS.md` was silently ignored while the file sat in the
 * checkout looking obeyed.
 */
import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, beforeEach, test } from "bun:test";

import { findProjectRules, projectRulesNote } from "./project.ts";

let root = "";

beforeEach(() => {
  root = mkdtempSync(join(tmpdir(), "bough-agentsmd-"));
});
afterEach(() => {
  rmSync(root, { recursive: true, force: true });
});

const write = (path: string, body: string) => {
  mkdirSync(join(path, ".."), { recursive: true });
  writeFileSync(path, body);
};

test("the workspace's own AGENTS.md is found", () => {
  const ws = join(root, "repo");
  mkdirSync(join(ws, ".git"), { recursive: true });
  write(join(ws, "AGENTS.md"), "always use tabs");

  const files = findProjectRules(ws);
  assert.equal(files.length, 1);
  assert.equal(files[0].body, "always use tabs");
});

test("a monorepo cascades root then package, nearest last", () => {
  const repo = join(root, "mono");
  const pkg = join(repo, "packages", "web");
  mkdirSync(join(repo, ".git"), { recursive: true });
  mkdirSync(pkg, { recursive: true });
  write(join(repo, "AGENTS.md"), "house style");
  write(join(pkg, "AGENTS.md"), "web rules");

  const files = findProjectRules(pkg);
  assert.deepEqual(files.map((f) => f.body), ["house style", "web rules"]);
});

test("the walk stops at the git root", () => {
  const repo = join(root, "outer", "repo");
  mkdirSync(join(repo, ".git"), { recursive: true });
  write(join(root, "outer", "AGENTS.md"), "not mine");
  write(join(repo, "AGENTS.md"), "mine");

  assert.deepEqual(findProjectRules(repo).map((f) => f.body), ["mine"]);
});

test("outside a git checkout only the workspace directory is read", () => {
  const ws = join(root, "loose", "dir");
  mkdirSync(ws, { recursive: true });
  write(join(root, "loose", "AGENTS.md"), "parent");
  write(join(ws, "AGENTS.md"), "self");

  assert.deepEqual(findProjectRules(ws).map((f) => f.body), ["self"]);
});

test("the global tier comes first and is never confused with the project's", () => {
  const home = join(root, "home");
  const ws = join(root, "repo");
  mkdirSync(join(ws, ".git"), { recursive: true });
  write(join(home, "AGENTS.md"), "global");
  write(join(ws, "AGENTS.md"), "project");

  assert.deepEqual(findProjectRules(ws, home).map((f) => f.body), ["global", "project"]);
});

test("a missing, empty or unreadable file is a skip, never a throw", () => {
  const ws = join(root, "empty");
  mkdirSync(join(ws, ".git"), { recursive: true });
  assert.deepEqual(findProjectRules(ws, join(root, "nope")), []);

  write(join(ws, "AGENTS.md"), "   \n\n");
  assert.deepEqual(findProjectRules(ws), []);
});

test("a directory named AGENTS.md is not a rule file", () => {
  const ws = join(root, "weird");
  mkdirSync(join(ws, ".git"), { recursive: true });
  mkdirSync(join(ws, "AGENTS.md"), { recursive: true });
  assert.deepEqual(findProjectRules(ws), []);
});

test("the note says the rules WIN, and names its sources relative to the workspace", () => {
  const note = projectRulesNote(
    [
      { path: "/w/AGENTS.md", body: "house" },
      { path: "/w/pkg/AGENTS.md", body: "pkg" },
    ],
    "/w",
  );
  assert.ok(note !== null);
  assert.match(note, /THEY WIN/);
  assert.match(note, /### AGENTS\.md/);
  assert.match(note, /### pkg\/AGENTS\.md/);
  // Order is the resolution order, so the nearer block is the later text.
  assert.ok(note.indexOf("house") < note.indexOf("pkg"));
  assert.match(note, /Later blocks are nearer/);
});

test("no rules yields no note at all, not an empty heading", () => {
  assert.equal(projectRulesNote([], "/w"), null);
});

test("a global file outside the workspace is shown by absolute path", () => {
  const note = projectRulesNote([{ path: "/home/u/.bough/AGENTS.md", body: "g" }], "/w");
  assert.match(note ?? "", /### \/home\/u\/\.bough\/AGENTS\.md/);
  // One source: nothing to disambiguate, so no cascade footnote.
  assert.doesNotMatch(note ?? "", /Later blocks/);
});
