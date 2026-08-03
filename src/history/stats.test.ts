/**
 * The popularity math and the two injection surfaces built on it. The Db is
 * stubbed — the SQL behind `commandTagRows` is pinned in db.test.ts; what matters
 * here is the weighting, the per-session freezing, and the divergence rule.
 */

import { afterEach, test } from "bun:test";
import { deepStrictEqual, ok } from "node:assert";
import type { CommandTagRow, Db } from "../types.ts";
import {
  dirTagHints,
  rankTags,
  resetStatsMemo,
  tagsNoteFor,
  tagWeights,
  topRepoTags,
} from "./stats.ts";

function eq(actual: unknown, expected: unknown, message?: string): void {
  deepStrictEqual(actual, expected, message);
}

afterEach(() => resetStatsMemo());

const DAY = 24 * 60 * 60 * 1000;

/**
 * `spread` is what `rankTags` contrasts against. The default is ONE repo, which is
 * the fresh-install case and makes every idf identical — so these tests read as
 * pure popularity, which is what they were written to pin. The distinctiveness
 * ranking has its own tests below.
 */
function stubDb(
  rows: CommandTagRow[],
  dirRows: Record<string, CommandTagRow[]> = {},
  spread: { repos: number; byTag: Map<string, number> } = { repos: 1, byTag: new Map() },
): Db {
  return {
    commandTagRows: (_repo: string, opts: { dir?: string } = {}) =>
      opts.dir !== undefined ? dirRows[opts.dir] ?? [] : rows,
    tagSpread: () => spread,
  } as unknown as Db;
}

// ---------------------------------------------------------------------------
// Weighting
// ---------------------------------------------------------------------------

test("a failing command's tag weighs a quarter of a passing one", () => {
  const now = 1_000_000;
  const w = tagWeights([
    { tag: "ok", ts: now, exitCode: 0 },
    { tag: "bad", ts: now, exitCode: 1 },
    { tag: "unknown", ts: now, exitCode: null },
  ], now);
  eq(w.get("ok"), 1);
  eq(w.get("bad"), 0.25);
  eq(w.get("unknown"), 0.5);
});

test("weight halves every 30 days, so one old habit loses to a new one", () => {
  const now = 400 * DAY;
  const w = tagWeights([
    { tag: "old", ts: now - 30 * DAY, exitCode: 0 },
    { tag: "fresh", ts: now, exitCode: 0 },
  ], now);
  ok(Math.abs(w.get("old")! - 0.5) < 1e-9);
  eq(w.get("fresh"), 1);
});

test("topRepoTags ranks by summed weight: ten failures lose to two successes", () => {
  const now = 1_000_000;
  const rows: CommandTagRow[] = [
    ...Array.from({ length: 10 }, () => ({ tag: "flaky", ts: now, exitCode: 1 })),
    { tag: "solid", ts: now, exitCode: 0 },
    { tag: "solid", ts: now, exitCode: 0 },
    { tag: "rare", ts: now, exitCode: 0 },
  ];
  // 10×0.25 = 2.5 for flaky vs 2.0 for solid — failures still count, just less.
  eq(topRepoTags(stubDb(rows), "/ws", now, 2), ["flaky", "solid"]);
});

// ---------------------------------------------------------------------------
// The priming note
// ---------------------------------------------------------------------------

test("the note prefers this project's OWN words over the tools every project uses", () => {
  const now = 1_000 * DAY;
  // `git` is used twice as much as `composer` here — and in all twelve repos the
  // memory knows, which is what makes it a tool name rather than this project's
  // vocabulary. A popularity ranking spent its slots on exactly these.
  const rows: CommandTagRow[] = [
    ...Array.from({ length: 8 }, () => ({ tag: "git", ts: now, exitCode: 0 })),
    ...Array.from({ length: 8 }, () => ({ tag: "bun", ts: now, exitCode: 0 })),
    ...Array.from({ length: 4 }, () => ({ tag: "composer", ts: now, exitCode: 0 })),
    ...Array.from({ length: 3 }, () => ({ tag: "retention", ts: now, exitCode: 0 })),
  ];
  const spread = {
    repos: 12,
    byTag: new Map([["git", 12], ["bun", 9], ["composer", 1], ["retention", 1]]),
  };
  eq(topRepoTags(stubDb(rows, {}, spread), "/w", now, 4), [
    "composer",
    "retention",
    "bun",
    "git",
  ]);

  // The whole ranking inverts back to popularity when there is nothing to contrast
  // against — one repo means every idf is ln 2, which is the honest answer on a
  // fresh install and needs no special case.
  const alone = { repos: 1, byTag: new Map([["git", 1], ["composer", 1]]) };
  eq(topRepoTags(stubDb(rows, {}, alone), "/w", now, 2), ["bun", "git"]);
});

test("rankTags carries the arithmetic it sorted by", () => {
  // `bough tags` shows these columns, so a user can see WHY the note says what it
  // says rather than being told to trust it.
  const weights = new Map([["git", 8], ["composer", 4]]);
  const ranked = rankTags(weights, { repos: 12, byTag: new Map([["git", 12], ["composer", 1]]) }, 5);
  eq(ranked.map((r) => r.tag), ["composer", "git"]);
  eq(ranked[0].repos, 1);
  eq(ranked[0].weight, 4);
  ok(ranked[0].score > ranked[1].score, "the score is what the order is by");
  // A tag the spread has never seen is treated as this project's alone, not as
  // ubiquitous — an unknown must not be damped into last place.
  eq(rankTags(new Map([["new", 1]]), { repos: 4, byTag: new Map() }, 5)[0].repos, 1);
});

test("tagsNoteFor names the top tags once and freezes per session", () => {
  const now = 1_000_000;
  const db = stubDb([
    { tag: "git", ts: now, exitCode: 0 },
    { tag: "bun", ts: now, exitCode: 0 },
  ]);
  const first = tagsNoteFor(db, "sess", "/ws", now);
  ok(first !== null && first.includes("git") && first.includes("bun"));
  // A session's note never drifts, even when the stats underneath it change.
  const drifted = tagsNoteFor(stubDb([{ tag: "other", ts: now, exitCode: 0 }]), "sess", "/ws", now);
  eq(drifted, first);
});

test("tagsNoteFor is null — and stays null — for a project with no history", () => {
  eq(tagsNoteFor(stubDb([]), "sess", "/ws", 1), null);
  eq(tagsNoteFor(stubDb([]), "sess", "/ws", 1), null);
});

// ---------------------------------------------------------------------------
// Directory hints
// ---------------------------------------------------------------------------

test("a directory hints only when its profile diverges from the primed set", () => {
  const now = 1_000_000;
  const db = stubDb(
    [{ tag: "bun", ts: now, exitCode: 0 }],
    {
      "migrations": [{ tag: "psql", ts: now, exitCode: 0 }, { tag: "bun", ts: now, exitCode: 0 }],
      "src/tui": [{ tag: "bun", ts: now, exitCode: 0 }],
    },
  );
  // Prime the session so the divergence rule has a baseline.
  tagsNoteFor(db, "sess", "/ws", now);
  const lines = dirTagHints(db, "sess", "/ws", ["/ws/migrations", "/ws/src/tui"], now);
  // migrations diverges (psql); src/tui is covered by the primed set — silent.
  eq(lines.length, 1);
  ok(lines[0].includes("migrations/") && lines[0].includes("psql"));
  ok(!lines[0].includes("bun"), "already-primed tags never repeat in a hint");
  // Once per directory, ever.
  eq(dirTagHints(db, "sess", "/ws", ["/ws/migrations"], now), []);
});

test("hints stop at the per-session cap", () => {
  const now = 1_000_000;
  const dirRows: Record<string, CommandTagRow[]> = {};
  const dirs: string[] = [];
  for (let i = 0; i < 6; i++) {
    dirRows[`d${i}`] = [{ tag: `t${i}`, ts: now, exitCode: 0 }];
    dirs.push(`/ws/d${i}`);
  }
  const db = stubDb([], dirRows);
  const lines = dirTagHints(db, "sess", "/ws", dirs, now);
  eq(lines.length, 4);
});

test("touching a FOREIGN checkout surfaces that repo's own profile", async () => {
  const { mkdtemp, rm } = await import("node:fs/promises");
  const { mkdirSync } = await import("node:fs");
  const { tmpdir } = await import("node:os");
  const { join } = await import("node:path");
  const home = await mkdtemp(join(tmpdir(), "bough-xrepo-"));
  try {
    const proj = join(home, "repos/proj");
    mkdirSync(join(proj, ".git"), { recursive: true });
    const now = 1_000_000;
    // Repo-aware stub: only the foreign checkout's identity has history.
    const db = {
      commandTagRows: (repo: string, opts: { dir?: string } = {}) =>
        repo === proj && opts.dir === undefined
          ? [{ tag: "docs:read", ts: now, exitCode: 0 }]
          : [],
    } as unknown as Db;
    // The workspace (home) has no history of its own — priming is empty.
    tagsNoteFor(db, "sess", home, now);
    const lines = dirTagHints(db, "sess", home, [proj], now);
    eq(lines.length, 1);
    ok(lines[0].includes("repos/proj/"), lines[0]);
    ok(lines[0].includes("docs:read"), lines[0]);
    // The workspace's OWN root never hints — its profile is the priming set.
    eq(dirTagHints(db, "sess", home, [home], now), []);
  } finally {
    await rm(home, { recursive: true, force: true });
  }
});
