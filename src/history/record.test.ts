/**
 * Tag normalization, directory attribution, and the recorder's best-effort
 * contract. `extractDirs` runs against a real temp directory: the whole heuristic
 * is "does this token resolve to something on disk", and faking the disk would
 * test the fake.
 */

import { test } from "bun:test";
import { deepStrictEqual, ok } from "node:assert";
import { spawnSync } from "node:child_process";
import { mkdirSync, writeFileSync } from "node:fs";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { SqliteDb } from "../db/db.ts";
import type { Db } from "../types.ts";
import {
  attributeCommand,
  createCommandRecorder,
  normalizeTags,
  repoIdentity,
  splitTags,
} from "./record.ts";

function eq(actual: unknown, expected: unknown, message?: string): void {
  deepStrictEqual(actual, expected, message);
}

// ---------------------------------------------------------------------------
// normalizeTags
// ---------------------------------------------------------------------------

test("normalizeTags lowercases, trims, slugifies and rejoins", () => {
  eq(normalizeTags("PSQL:Migrate"), "psql:migrate");
  eq(normalizeTags(" Git : PUSH "), "git:push");
  eq(normalizeTags("a!!:b c:d.e"), "a:b:c:d.e");
});

test("dashes and spaces are separators — no tag ever contains a dash", () => {
  eq(normalizeTags("repo-inspect"), "repo:inspect");
  eq(normalizeTags("git push"), "git:push");
  eq(normalizeTags("bun--test"), "bun:test");
  eq(normalizeTags("pre-commit-hook"), "pre:commit:hook");
});

test("normalizeTags returns '' when nothing survives — the caller's 'no tags' signal", () => {
  eq(normalizeTags(undefined), "");
  eq(normalizeTags(""), "");
  eq(normalizeTags("  "), "");
  eq(normalizeTags(":::"), "");
  eq(normalizeTags("!!!:???"), "");
});

test("normalizeTags caps the count", () => {
  eq(normalizeTags("a:b:c:d:e:f:g:h:i:j"), "a:b:c:d:e:f:g:h");
});

test("splitTags dedupes and treats '' as no tags", () => {
  eq(splitTags(""), []);
  eq(splitTags("a:b:a"), ["a", "b"]);
});

// ---------------------------------------------------------------------------
// extractDirs
// ---------------------------------------------------------------------------

async function withWorkspace(fn: (ws: string) => void | Promise<void>): Promise<void> {
  const ws = await mkdtemp(join(tmpdir(), "bough-hist-"));
  try {
    mkdirSync(join(ws, "src/tui"), { recursive: true });
    mkdirSync(join(ws, "migrations"), { recursive: true });
    mkdirSync(join(ws, "node_modules/pkg"), { recursive: true });
    writeFileSync(join(ws, "src/tui/composer.ts"), "x");
    writeFileSync(join(ws, "migrations/004.sql"), "x");
    await fn(ws);
  } finally {
    await rm(ws, { recursive: true, force: true });
  }
}

test("attributeCommand maps a command to the dirs of the paths it names", () =>
  withWorkspace((ws) => {
    eq(attributeCommand("bun test src/tui/composer.ts", ws).relDirs, ["src/tui"]);
    eq(attributeCommand("psql -f migrations/004.sql", ws).relDirs, ["migrations"]);
    // A directory token attributes to itself; a file to its dirname.
    eq(attributeCommand("ls -la src/tui", ws).relDirs, ["src/tui"]);
    // `--flag=path` and line refs both resolve.
    eq(attributeCommand("tool --input=src/tui/composer.ts", ws).relDirs, ["src/tui"]);
    eq(attributeCommand("rg -n foo src/tui/composer.ts:12", ws).relDirs, ["src/tui"]);
    // A non-git workspace's scope is its own path.
    eq(attributeCommand("ls -la src/tui", ws).repo, ws);
  }));

test("attributeCommand ignores non-paths and the trees nobody means", () =>
  withWorkspace((ws) => {
    eq(attributeCommand("git push origin main", ws).relDirs, []);
    eq(attributeCommand("curl https://example.com/a/b", ws).relDirs, []);
    // Outside every checkout and outside the workspace: touch-tracked, never a rel dir.
    eq(attributeCommand("cat /etc/hosts", ws).relDirs, []);
    eq(attributeCommand("ls node_modules/pkg", ws).relDirs, []);
    // A path that does not exist attributes nothing — the heuristic never guesses.
    eq(attributeCommand("bun test src/gone/nope.ts", ws).relDirs, []);
  }));

test("attributeCommand dedupes and caps", () =>
  withWorkspace((ws) => {
    eq(
      attributeCommand("diff src/tui/composer.ts src/tui/composer.ts migrations/004.sql", ws)
        .relDirs,
      ["src/tui", "migrations"],
    );
  }));

test("a command about ANOTHER checkout is scoped to that checkout, not the workspace", () =>
  withWorkspace((ws) => {
    // `ws` plays the home dir; a separate checkout lives at ws/repos/proj.
    const proj = join(ws, "repos/proj");
    mkdirSync(join(proj, ".git"), { recursive: true });
    mkdirSync(join(proj, "src"), { recursive: true });
    writeFileSync(join(proj, "src/a.ts"), "x");
    // Touching the checkout's root scopes the row to it, with no dir rows.
    const atRoot = attributeCommand(`cd ${proj} && ls -la`, ws);
    eq(atRoot.repo, proj);
    eq(atRoot.relDirs, []);
    eq(atRoot.absDirs, [proj]);
    // Touching a file inside it attributes REPO-ROOT-relative dirs, so sessions
    // rooted anywhere agree on what "src" means.
    const inside = attributeCommand(`sed -n 1p ${proj}/src/a.ts`, ws);
    eq(inside.repo, proj);
    eq(inside.relDirs, ["src"]);
  }));

// ---------------------------------------------------------------------------
// repoIdentity
// ---------------------------------------------------------------------------

test("repoIdentity is the origin URL in a git checkout, else the path", async () => {
  const ws = await mkdtemp(join(tmpdir(), "bough-repoid-"));
  try {
    // Non-git: the path is the identity.
    eq(repoIdentity(ws), ws);
    // The cache is per-workspace, so a second dir sees fresh state.
    const git = await mkdtemp(join(tmpdir(), "bough-repoid-git-"));
    try {
      spawnSync("git", ["init", "-q"], { cwd: git });
      spawnSync("git", ["remote", "add", "origin", "https://example.com/me/repo.git"], {
        cwd: git,
      });
      eq(repoIdentity(git), "https://example.com/me/repo.git");
    } finally {
      await rm(git, { recursive: true, force: true });
    }
  } finally {
    await rm(ws, { recursive: true, force: true });
  }
});

// ---------------------------------------------------------------------------
// createCommandRecorder
// ---------------------------------------------------------------------------

test("the recorder writes a full row and never throws for a broken db", async () => {
  const ws = await mkdtemp(join(tmpdir(), "bough-rec-"));
  try {
    mkdirSync(join(ws, "src"), { recursive: true });
    writeFileSync(join(ws, "src/a.ts"), "x");
    const db = new SqliteDb(":memory:");
    db.createSession({ id: "s1", parentId: null, title: "s1", kind: "root", createdAt: 1 });
    const record = createCommandRecorder({ db, sessionId: "s1", workspace: ws, now: () => 42 });
    record({ command: "bun test src/a.ts", tags: "bun:test", exitCode: 0, durationMs: 7, outputHead: "ok", spillPath: null });
    const rows = db.commandTagRows(ws);
    eq(rows, [
      { tag: "bun", ts: 42, exitCode: 0 },
      { tag: "test", ts: 42, exitCode: 0 },
    ]);
    eq(db.commandTagRows(ws, { dir: "src" }).length, 2);
    db.close();

    // A recorder over a session the db has never seen (FK violation) swallows it.
    const db2 = new SqliteDb(":memory:");
    const broken = createCommandRecorder({ db: db2 as Db, sessionId: "ghost", workspace: ws });
    broken({ command: "true", tags: "t", exitCode: 0, durationMs: 1, outputHead: "", spillPath: null });
    eq(db2.commandTagRows(ws), []);
    db2.close();
  } finally {
    await rm(ws, { recursive: true, force: true });
  }
});

test("the recorder swallows a db whose recordCommand itself throws", () => {
  const db = { recordCommand: () => { throw new Error("disk full"); } } as unknown as Db;
  const record = createCommandRecorder({ db, sessionId: "s", workspace: "/nope" });
  // Not throwing IS the assertion.
  record({ command: "true", tags: "t", exitCode: 0, durationMs: 1, outputHead: "", spillPath: null });
  ok(true);
});
