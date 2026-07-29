/**
 * The scratchpad: creation, and the sweep that keeps its root from becoming an
 * archive. Every path is injected — nothing here touches the real `~/.bough`.
 */
import { test } from "bun:test";
import assert from "node:assert/strict";
import { existsSync, mkdirSync, mkdtempSync, utimesSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { MAX_AGE_MS, sweepScratch } from "./scratch.ts";

function root(): string {
  return mkdtempSync(join(tmpdir(), "bough-scratch-test-"));
}

/** A scratch directory with one file, last touched `ageMs` ago. */
function aged(root: string, name: string, ageMs: number, now: number): string {
  const dir = join(root, name);
  mkdirSync(dir, { recursive: true });
  writeFileSync(join(dir, "note.txt"), "x");
  const when = new Date(now - ageMs);
  utimesSync(dir, when, when);
  return dir;
}

test("the sweep removes what is stale and keeps what is not", () => {
  const now = Date.parse("2026-07-29T12:00:00Z");
  const r = root();
  const fresh = aged(r, "fresh-session", 60_000, now);
  const yesterday = aged(r, "yesterday", 24 * 60 * 60_000, now);
  const ancient = aged(r, "ancient", MAX_AGE_MS + 60_000, now);

  assert.deepEqual(sweepScratch({ root: r, now: () => now }), ["ancient"]);
  assert.equal(existsSync(ancient), false);
  // A conversation can be months old and still be the one you are working in, so
  // the question is when anything was last WRITTEN, not how old the session is.
  assert.equal(existsSync(fresh), true);
  assert.equal(existsSync(yesterday), true);
});

test("a missing root is not an error — nothing has ever been written", () => {
  assert.deepEqual(sweepScratch({ root: join(root(), "never-created") }), []);
});

test("a file loose in the root is left alone", () => {
  // Not ours to delete: the sweep's rule is about directories it created, and a
  // recursive delete of anything it finds is how a bug here becomes data loss.
  const now = Date.now();
  const r = root();
  writeFileSync(join(r, "stray.txt"), "x");
  assert.deepEqual(sweepScratch({ root: r, now: () => now + MAX_AGE_MS * 2 }), []);
  assert.equal(existsSync(join(r, "stray.txt")), true);
});
