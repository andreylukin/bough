/**
 * Tests for the pushed command memory (`history/echo.ts`).
 *
 * The behaviours worth pinning are the ones a future edit could quietly invert:
 *
 *   - **A first failure says nothing.** The echo is history, not commentary. If it
 *     ever fired on a command's own failure, every failing command in every round
 *     would carry a paragraph and the signal would be worth nothing.
 *   - **The guard is byte-exact and session-scoped.** It refuses to run something,
 *     which is the one thing here that can be wrong in a way that costs the user a
 *     turn. A changed flag, another session, or an older failure must all still run.
 *   - **A skipped command is not recorded.** Nothing ran, so nothing may enter the
 *     memory — least of all as another failure of itself, which would make the
 *     threshold self-reinforcing.
 *
 * Hermetic: `:memory:`, an injected clock, and a workspace path that is not a git
 * checkout, so `repoIdentity` resolves to the path and no subprocess runs.
 */
import { test } from "bun:test";
import assert from "node:assert/strict";
import { openDb } from "../db/db.ts";
import { createCommandEcho } from "./echo.ts";
import type { Db } from "../types.ts";

const WS = "/nonexistent-workspace-for-echo-tests";
const SESSION = "s1";
const T0 = 1_700_000_000_000;
/** What every failing command in these tests printed. */
const ERR = 'invalid argument "merged"\n[exit code 1]';

function fail(db: Db, cmd: string, opts: { ts: number; session?: string; out?: string }): void {
  db.recordCommand({
    sessionId: opts.session ?? SESSION,
    ts: opts.ts,
    repo: WS,
    cmd,
    tags: "",
    tagList: [],
    dirs: [],
    exitCode: 1,
    durationMs: 5,
    outputHead: opts.out ?? 'invalid argument "merged"\n[exit code 1]',
    spillPath: null,
    source: "live",
  });
}

/** A database with the sessions the history rows below hang off. */
function freshDb(): Db {
  const db = openDb(":memory:");
  for (const id of [SESSION, "someone-else"]) {
    db.createSession({ id, parentId: null, title: id, kind: "root", createdAt: T0 - 1_000_000 });
  }
  return db;
}

function echoOver(db: Db, now: () => number) {
  return createCommandEcho({ db, sessionId: SESSION, workspace: WS, now });
}

test("a command with no failing history gets no note", () => {
  const db = freshDb();
  const echo = echoOver(db, () => T0);
  assert.equal(echo.note("gh search prs --state merged", 1, ERR), null);
  db.close();
});

test("a repeat failure is echoed with the count and the last error", () => {
  const db = freshDb();
  fail(db, "gh search prs --state merged", { ts: T0 - 60_000 });
  fail(db, "gh search prs --state merged", { ts: T0 - 30_000 });
  const note = echoOver(db, () => T0).note("gh search prs --state merged", 1, ERR);
  assert.ok(note, "expected a note");
  assert.match(note, /already failed here 2×/);
  assert.match(note, /invalid argument "merged"/);
  // The `[exit code N]` trailer is the harness talking, not the error.
  assert.doesNotMatch(note, /\[exit code/);
  db.close();
});

test("a successful sibling command is offered alongside the failure", () => {
  const db = freshDb();
  fail(db, "gh search prs --state merged", { ts: T0 - 30_000 });
  db.recordCommand({
    sessionId: SESSION,
    ts: T0 - 20_000,
    repo: WS,
    cmd: "gh search prs --state closed --json number",
    tags: "",
    tagList: [],
    dirs: [],
    exitCode: 0,
    durationMs: 5,
    outputHead: "[]",
    spillPath: null,
    source: "live",
  });
  const note = echoOver(db, () => T0).note("gh search prs --state merged", 1, ERR);
  assert.ok(note);
  assert.match(note, /this exited 0 here: gh search prs --state closed --json number/);
  db.close();
});

test("a success is never echoed, however bad its history", () => {
  const db = freshDb();
  fail(db, "flaky", { ts: T0 - 10_000 });
  fail(db, "flaky", { ts: T0 - 5_000 });
  assert.equal(echoOver(db, () => T0).note("flaky", 0, ERR), null);
  db.close();
});

test("the guard fires only at the threshold, and quotes what it skipped", () => {
  const db = freshDb();
  const cmd = "gh search prs --state merged";
  const echo = echoOver(db, () => T0);
  fail(db, cmd, { ts: T0 - 3_000 });
  assert.equal(echo.guard(cmd), null, "one failure is not a loop");
  fail(db, cmd, { ts: T0 - 2_000 });
  assert.equal(echo.guard(cmd), null, "two failures are not a loop");
  fail(db, cmd, { ts: T0 - 1_000 });
  const skip = echo.guard(cmd);
  assert.ok(skip, "three identical failures in seconds is a loop");
  assert.match(skip, /^\[not run\]/);
  assert.match(skip, /failed 3 times in this session/);
  assert.match(skip, /invalid argument "merged"/);
  db.close();
});

test("the guard ignores older failures, other sessions, and edited commands", () => {
  const db = freshDb();
  const cmd = "gh search prs --state merged";
  const old = { ts: T0 - 10 * 60_000 };
  fail(db, cmd, old);
  fail(db, cmd, old);
  fail(db, cmd, old);
  const echo = echoOver(db, () => T0);
  assert.equal(echo.guard(cmd), null, "ten minutes ago is history, not a loop");

  for (const ts of [T0 - 3_000, T0 - 2_000, T0 - 1_000]) {
    fail(db, cmd, { ts, session: "someone-else" });
  }
  assert.equal(echo.guard(cmd), null, "another session's loop is not this one's");

  for (const ts of [T0 - 3_000, T0 - 2_000, T0 - 1_000]) fail(db, cmd, { ts });
  assert.ok(echo.guard(cmd), "this session's own loop does fire");
  assert.equal(
    echo.guard(`${cmd} --json number`),
    null,
    "any edit makes it a different command",
  );
  db.close();
});

test("a command containing LIKE wildcards cannot widen its own success lookup", () => {
  const db = freshDb();
  fail(db, "rg %_ src", { ts: T0 - 30_000 });
  db.recordCommand({
    sessionId: SESSION,
    ts: T0 - 20_000,
    repo: WS,
    cmd: "rg unrelated-thing src",
    tags: "",
    tagList: [],
    dirs: [],
    exitCode: 0,
    durationMs: 5,
    outputHead: "",
    spillPath: null,
    source: "live",
  });
  const note = echoOver(db, () => T0).note("rg %_ src", 1, ERR);
  assert.ok(note);
  assert.doesNotMatch(note, /unrelated-thing/, "`%_` must be escaped, not matched as wildcards");
  db.close();
});

test("a broken lookup is silent, never a thrown round", () => {
  const exploding = {
    priorFailures() {
      throw new Error("db is gone");
    },
    lastSuccessLike() {
      throw new Error("db is gone");
    },
  } as unknown as Db;
  const echo = echoOver(exploding, () => T0);
  assert.equal(echo.note("anything", 1, ERR), null);
  assert.equal(echo.guard("anything"), null);
});

// ---------------------------------------------------------------------------
// The error path — the incident this was actually built for
// ---------------------------------------------------------------------------

test("THE REAL INCIDENT: a hundred distinct commands, one mistake", () => {
  // Reconstructed from the rows that motivated this module. Every command differs
  // (one ticket each), every command fails identically. Command-identity matching
  // sees nothing here — that is the whole point of the error path.
  const db = freshDb();
  const tickets = ["NMC-5630", "NMFB-1811", "NMC-5602", "NMC-5881"];
  const cmdFor = (t: string) =>
    `gh search prs "${t}" --owner uni-intelligence --state merged --json number --limit 20`;
  tickets.forEach((t, i) => fail(db, cmdFor(t), { ts: T0 - (40 - i) * 1_000 }));
  const echo = echoOver(db, () => T0);
  const next = cmdFor("NMC-9999");

  // The byte-exact matchers are blind to it, exactly as they were in production.
  assert.equal(echo.guard(next), null, "distinct commands are not a stuck loop");
  const note = echo.note(next, 1, ERR);
  assert.ok(note, "the error path must see what command identity cannot");
  assert.match(note, /4 other commands here failed the same way/);
  assert.match(note, /invalid argument "merged"/);
  assert.match(note, /The command has been changing; the mistake has not/);
  assert.doesNotMatch(note, /this exact command/, "no command ran twice");
  db.close();
});

test("one other command with the same error is not yet a pattern", () => {
  const db = freshDb();
  fail(db, "gh pr list --state merged", { ts: T0 - 5_000 });
  assert.equal(echoOver(db, () => T0).note("gh search prs --state merged", 1, ERR), null);
  db.close();
});

test("a different error does not group, however many commands failed", () => {
  const db = freshDb();
  for (let i = 0; i < 6; i++) {
    fail(db, `cmd-${i}`, { ts: T0 - 5_000, out: `connection refused on port ${i}\n[exit code 1]` });
  }
  // Same repo, plenty of failures, unrelated mistake.
  assert.equal(echoOver(db, () => T0).note("gh search prs --state merged", 1, ERR), null);
  db.close();
});

test("both matchers can speak at once, command first", () => {
  const db = freshDb();
  const cmd = "gh search prs --state merged";
  fail(db, cmd, { ts: T0 - 9_000 });
  fail(db, "gh search prs --owner x --state merged", { ts: T0 - 8_000 });
  fail(db, "gh search prs --owner y --state merged", { ts: T0 - 7_000 });
  const note = echoOver(db, () => T0).note(cmd, 1, ERR);
  assert.ok(note);
  const exact = note.indexOf("this exact command");
  const spread = note.indexOf("other commands here failed the same way");
  assert.ok(exact >= 0 && spread >= 0, "both lines present");
  assert.ok(exact < spread, "the certain fact comes before the inferred pattern");
  db.close();
});

test("a command that printed nothing has no signature to group on", () => {
  const db = freshDb();
  for (let i = 0; i < 4; i++) fail(db, `q-${i}`, { ts: T0 - 5_000, out: "" });
  assert.equal(echoOver(db, () => T0).note("q-new", 1, ""), null);
  db.close();
});
