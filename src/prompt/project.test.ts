/**
 * `AGENTS.md` loading. The regression these pin is not a formatting one: before this
 * module existed, NOTHING in the tree read a project instruction file, so every rule
 * a user wrote in `AGENTS.md` was silently ignored while the file sat in the
 * checkout looking obeyed.
 */
import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { isAbsolute, join } from "node:path";
import { afterEach, beforeEach, test } from "bun:test";

import {
  drainProjectRuleNotes,
  findProjectRules,
  noteProjectRules,
  projectRulesNote,
  resetProjectRulesMemo,
  ruleSummaries,
} from "./project.ts";

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

// ---------------------------------------------------------------------------
// Reporting what was injected
// ---------------------------------------------------------------------------

test("the summary is the prompt's own order, labelled relative to the workspace", () => {
  const repo = join(root, "mono");
  const pkg = join(repo, "packages", "api");
  mkdirSync(join(repo, ".git"), { recursive: true });
  mkdirSync(pkg, { recursive: true });
  write(join(repo, "AGENTS.md"), "house style");
  write(join(pkg, "AGENTS.md"), "package rules");
  write(join(root, "home", "AGENTS.md"), "global");

  const files = findProjectRules(pkg, join(root, "home"));
  const summary = ruleSummaries(files, pkg);

  // Global first, then the repo root, then the workspace's own — the order they
  // concatenate in, so a reader of the row knows which one wins without opening any
  // of them. The labelling rule is `projectRulesNote`'s, deliberately: anything
  // outside the workspace is shown by absolute path rather than as `../../`, so the
  // row and the note the model got can never name the same file differently.
  assert.deepEqual(summary.map((r) => r.label), [
    join(root, "home", "AGENTS.md"),
    join(repo, "AGENTS.md"),
    "AGENTS.md",
  ]);
  assert.deepEqual(summary.map((r) => r.bytes), [6, 11, 13]);
  assert.ok(summary.every((r) => isAbsolute(r.path)));
});

test("the first turn reports what is in the prompt; an unchanged second turn says nothing", () => {
  resetProjectRulesMemo();
  const ws = join(root, "repo");
  mkdirSync(join(ws, ".git"), { recursive: true });
  write(join(ws, "AGENTS.md"), "always use tabs");

  noteProjectRules("s1", findProjectRules(ws), ws);
  const first = drainProjectRuleNotes("s1");
  assert.equal(first.length, 1);
  assert.match(first[0], /^\[rules\] AGENTS\.md \(15\) in this turn's prompt — /);

  // Drained, so the same turn cannot say it twice on a second round.
  assert.deepEqual(drainProjectRuleNotes("s1"), []);

  noteProjectRules("s1", findProjectRules(ws), ws);
  // Nothing changed, so nothing is said. A line repeated every turn is a line
  // nobody reads by the third one.
  assert.deepEqual(drainProjectRuleNotes("s1"), []);
});

test("an edit, an addition and a removal each say so on the turn they land in", () => {
  resetProjectRulesMemo();
  const repo = join(root, "repo");
  const pkg = join(repo, "pkg");
  mkdirSync(join(repo, ".git"), { recursive: true });
  mkdirSync(pkg, { recursive: true });
  write(join(repo, "AGENTS.md"), "short");

  noteProjectRules("s1", findProjectRules(pkg), pkg);
  drainProjectRuleNotes("s1"); // the opening report

  // Edited mid-session — the case the whole surface exists for: this used to take
  // effect on the next turn with no sign whatsoever that it had.
  write(join(repo, "AGENTS.md"), "considerably longer rules");
  noteProjectRules("s1", findProjectRules(pkg), pkg);
  const edited = drainProjectRuleNotes("s1");
  assert.equal(edited.length, 1);
  assert.match(edited[0], /changed \(5 → 25\)/);

  // A new nearer file: added, not "changed".
  write(join(pkg, "AGENTS.md"), "pkg");
  noteProjectRules("s1", findProjectRules(pkg), pkg);
  const added = drainProjectRuleNotes("s1");
  assert.equal(added.length, 1);
  assert.match(added[0], /^\[rules\] \+ AGENTS\.md \(3\)/);

  rmSync(join(pkg, "AGENTS.md"));
  noteProjectRules("s1", findProjectRules(pkg), pkg);
  const removed = drainProjectRuleNotes("s1");
  assert.equal(removed.length, 1);
  assert.match(removed[0], /gone, no longer in the prompt/);
});

test("a project with no rules says nothing at all, ever", () => {
  resetProjectRulesMemo();
  const ws = join(root, "bare");
  mkdirSync(join(ws, ".git"), { recursive: true });

  noteProjectRules("s1", findProjectRules(ws), ws);
  assert.deepEqual(drainProjectRuleNotes("s1"), []);
  noteProjectRules("s1", findProjectRules(ws), ws);
  assert.deepEqual(drainProjectRuleNotes("s1"), []);
});

test("sessions do not share a memo — a second conversation gets its own report", () => {
  resetProjectRulesMemo();
  const ws = join(root, "repo");
  mkdirSync(join(ws, ".git"), { recursive: true });
  write(join(ws, "AGENTS.md"), "rules");

  noteProjectRules("s1", findProjectRules(ws), ws);
  assert.equal(drainProjectRuleNotes("s1").length, 1);
  // A session that has never seen these files is on its first turn, whatever the
  // session before it saw.
  noteProjectRules("s2", findProjectRules(ws), ws);
  assert.equal(drainProjectRuleNotes("s2").length, 1);
});
