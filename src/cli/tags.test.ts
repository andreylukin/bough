/**
 * `bough tags`, driven end to end against a real in-memory database.
 *
 * The load-bearing property is that the default view is THE PRIMING NOTE'S OWN
 * ranking — not a second opinion about it. It reads through `rankedRepoTags`, the
 * same function `tagsNoteFor` calls, over the same queries, so a user checking why
 * the model favours a tag is looking at the arithmetic that actually ran. A test
 * that stubbed the ranking would prove nothing about that, which is why this one
 * seeds commands and asserts on the order that comes out.
 *
 * Parsing is pure and total, so it is tested as a function; rendering is tested
 * through the collectors, because a column that silently stopped printing is the
 * failure mode a return value would not catch.
 */
import { test } from "bun:test";
import assert from "node:assert/strict";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { openDb, type SqliteDb } from "../db/db.ts";
import { parseTagsArgs, runTags, USAGE } from "./tags.ts";
import { resetStatsMemo } from "../history/stats.ts";

const T0 = Date.UTC(2026, 7, 3, 12, 0, 0);
const DAY = 24 * 60 * 60 * 1000;

function collector() {
  const out: string[] = [];
  const err: string[] = [];
  return { out, err, deps: { out: (l: string) => out.push(l), err: (l: string) => err.push(l) } };
}

/** A memory with two repos, so distinctiveness has something to contrast against. */
function seeded(): SqliteDb {
  const db = openDb(":memory:");
  db.createSession({
    id: "s1",
    title: "t",
    kind: "root",
    parentId: null,
    createdAt: T0,
  });
  const rec = (repo: string, cmd: string, tags: string, exitCode: number, ts: number) =>
    db.recordCommand({
      sessionId: "s1",
      ts,
      repo,
      cmd,
      tags,
      tagList: tags === "" ? [] : tags.split(":"),
      dirs: [],
      exitCode,
      durationMs: 40,
      outputHead: "",
      spillPath: null,
      source: "live",
    });

  // `git` is used more than `composer` in THIS repo, and is also used in the other
  // one. That is the whole case the ranking exists for.
  for (let i = 0; i < 6; i++) rec("mine", `git status ${i}`, "git:status:worktree", 0, T0 - i);
  for (let i = 0; i < 3; i++) rec("mine", `bun test ${i}`, "bun:test:composer", 0, T0 - i);
  rec("mine", "bun test failing", "bun:test:composer", 1, T0 - 10);
  // …and `git` is in every OTHER repo the memory knows, which is what makes it a
  // tool name. Six of them, because with only two repos the damping is `ln 3 / ln 2`
  // and a tag used twice as often should still win — evidence that a word is
  // universal takes more than one other project to accumulate.
  for (let i = 0; i < 4; i++) rec("other", `git push ${i}`, "git:push:main", 0, T0 - i);
  for (let r = 2; r < 7; r++) rec(`other${r}`, "git log", "git:log:history", 0, T0 - r);
  // A day earlier, and an untagged leg — what `stats` reports as lost coverage.
  rec("mine", "rg todo", "rg:search:todo", 0, T0 - DAY);
  rec("mine", "echo untagged", "", 0, T0 - DAY);
  return db;
}

test("parsing is pure and total, and a bare word is a tag", async () => {
  assert.deepEqual(parseTagsArgs([]), {
    verb: "list",
    limit: 20,
    days: 30,
    json: false,
    allRepos: false,
    program: false,
  });
  const show = parseTagsArgs(["show", "git"]);
  assert.equal("usageError" in show ? "" : show.verb, "show");
  assert.equal("usageError" in show ? "" : show.tag, "git");
  // `bough tags git` is what a hand reaches for, and it means `show git`.
  const bare = parseTagsArgs(["git"]);
  assert.equal("usageError" in bare ? "" : bare.tag, "git");

  assert.equal("usageError" in parseTagsArgs(["-h"]), true);
  assert.match((parseTagsArgs(["--limit", "0"]) as { usageError: string }).usageError, /positive/);
  assert.match((parseTagsArgs(["--repo"]) as { usageError: string }).usageError, /needs a value/);
  assert.match((parseTagsArgs(["--nope"]) as { usageError: string }).usageError, /unknown option/);
  assert.match((parseTagsArgs(["show"]) as { usageError: string }).usageError, /exactly one TAG/);

  // `--all` after `--repo` is a correction, not a contradiction.
  const all = parseTagsArgs(["--repo", "x", "--all"]);
  assert.equal("usageError" in all ? "x" : all.repo, undefined);
});

test("--help is not a failure", async () => {
  const c = collector();
  assert.equal(await runTags(["--help"], { ...c.deps, db: seeded() }), 0);
  assert.equal(c.err[0], USAGE);
});

test("the default view is the priming note's ranking, arithmetic shown", async () => {
  resetStatsMemo();
  const c = collector();
  const code = await runTags(["--repo", "mine"], { ...c.deps, db: seeded(), now: () => T0 });
  assert.equal(code, 0);
  const text = c.out.join("\n");
  // The ranking itself: `git` outweighs `bun` here (6 successes to 3) and loses,
  // because it is used in both repos and `bun` in only this one. That inversion IS
  // the recommendation, and it is asserted rather than described.
  const order = ["git", "bun", "status", "test", "composer", "worktree"]
    .map((t) => ({ tag: t, at: text.indexOf(`\n  ${t} `) }))
    .filter((x) => x.at >= 0);
  const composer = order.find((x) => x.tag === "composer")?.at ?? -1;
  const git = order.find((x) => x.tag === "git")?.at ?? -1;
  assert.ok(composer >= 0 && git >= 0, text);
  assert.ok(composer < git, `this project's own word should outrank the tool:\n${text}`);
  // Every column the order depends on is printed: a table sorted by something it
  // does not show reads as a bug.
  assert.match(text, /tag\s+weight\s+repos\s+score/);
  assert.match(text, /how FEW repos use the tag/);
});

test("show answers what worked, newest first, with the exit code first", async () => {
  const c = collector();
  assert.equal(
    await runTags(["show", "bun", "--repo", "mine"], { ...c.deps, db: seeded(), now: () => T0 }),
    0,
  );
  const text = c.out.join("\n");
  assert.match(text, /4 commands tagged "bun"/);
  // The failing run is marked, because "what worked here" is the question.
  assert.match(text, /✓ .*bun test 0/);
  assert.match(text, /✗ .*bun test failing/);
});

test("show is scoped to this repo unless --all says otherwise", async () => {
  const mine = collector();
  await runTags(["show", "git", "--repo", "mine"], { ...mine.deps, db: seeded(), now: () => T0 });
  assert.equal(mine.out.join("\n").includes("git push"), false, "the other repo's commands");

  const all = collector();
  await runTags(["show", "git", "--all"], { ...all.deps, db: seeded(), now: () => T0 });
  assert.match(all.out.join("\n"), /git push/);
});

test("stats reports coverage and vocabulary per day", async () => {
  const c = collector();
  assert.equal(await runTags(["stats", "--repo", "mine"], { ...c.deps, db: seeded(), now: () => T0 }), 0);
  const text = c.out.join("\n");
  assert.match(text, /day\s+sessions\s+cmds\s+tagged\s+vocab\s+refs\s+uses/);
  // The day with the untagged leg is the one that reports less than 100% coverage —
  // which is the number the whole `sh`-leg problem shows up in.
  assert.match(text, /50%/);
  assert.match(text, /100%/);
});

test("--json is the same answer without the rendering", async () => {
  const c = collector();
  await runTags(["show", "bun", "--repo", "mine", "--json"], { ...c.deps, db: seeded(), now: () => T0 });
  const rows = JSON.parse(c.out.join("\n")) as { cmd: string; exitCode: number }[];
  assert.equal(rows.length, 4);
  assert.equal(rows.some((r) => r.exitCode === 1), true);
});

test("a recalled command reaches the PROGRAM that ran it", async () => {
  const db = openDb(":memory:");
  db.createSession({ id: "s1", title: "t", kind: "root", parentId: null, createdAt: T0 });
  const program = `const r = await bash("psql -f migrations/004.sql", "psql:migrate:demand");\nconsole.log(r);`;
  db.createMessage({
    id: "m1",
    sessionId: "s1",
    role: "supervisor",
    parts: [
      { type: "text", text: "running the migration" },
      { type: "tool_call", id: "c1", name: "run_steps", input: { code: program } },
    ],
    pending: false,
    createdAt: T0,
  });
  db.recordCommand({
    sessionId: "s1",
    ts: T0,
    repo: "mine",
    cmd: "psql -f migrations/004.sql",
    tags: "psql:migrate:demand:linear.eng-1234",
    tagList: ["psql", "migrate", "demand", "linear.eng-1234"],
    dirs: [],
    exitCode: 0,
    durationMs: 900,
    outputHead: "",
    spillPath: null,
    source: "live",
    messageId: "m1",
  });

  // The command is recalled by the REFERENCE as readily as by a word — same table,
  // same join, which is the whole point of keeping them in one namespace.
  const c = collector();
  await runTags(["show", "linear.eng-1234", "--repo", "mine"], {
    ...c.deps,
    db,
    now: () => T0,
  });
  const text = c.out.join("\n");
  assert.match(text, /psql -f migrations\/004\.sql/);
  // By default the program is a pointer, because a full program per row would bury
  // the commands the list is for.
  assert.match(text, /↳ program: 2 lines · --program to see it/);
  assert.equal(text.includes("console.log"), false);

  // …and `--program` prints it, which is the half that is actually reusable.
  const full = collector();
  await runTags(["show", "linear.eng-1234", "--repo", "mine", "--program"], {
    ...full.deps,
    db,
    now: () => T0,
  });
  assert.match(full.out.join("\n"), /│ console\.log\(r\);/);

  // A row with no message — every row written before the link existed — still
  // recalls, it just has no program to offer.
  db.recordCommand({
    sessionId: "s1",
    ts: T0 - 5,
    repo: "mine",
    cmd: "psql -c 'select 1'",
    tags: "psql:probe:demand",
    tagList: ["psql", "probe", "demand"],
    dirs: [],
    exitCode: 0,
    durationMs: 10,
    outputHead: "",
    spillPath: null,
    source: "live",
  });
  const old = collector();
  await runTags(["show", "probe", "--repo", "mine"], { ...old.deps, db, now: () => T0 });
  assert.match(old.out.join("\n"), /select 1/);
  assert.equal(old.out.join("\n").includes("↳ program"), false);
});

test("references are recalled but never primed", async () => {
  // The ranking must not hand a ticket number the maximum rarity boost and open
  // every session reciting it. It is still in the table, still joinable, still
  // findable by name — just not in the vocabulary the model is shown.
  const c = collector();
  const db = seeded();
  db.recordCommand({
    sessionId: "s1",
    ts: T0,
    repo: "mine",
    cmd: "bun test src/tui",
    tags: "bun:test:linear.eng-1234",
    tagList: ["bun", "test", "linear.eng-1234"],
    dirs: [],
    exitCode: 0,
    durationMs: 10,
    outputHead: "",
    spillPath: null,
    source: "live",
  });
  await runTags(["--repo", "mine"], { ...c.deps, db, now: () => T0 });
  assert.equal(c.out.join("\n").includes("linear.eng-1234"), false, c.out.join("\n"));

  const shown = collector();
  await runTags(["show", "linear.eng-1234", "--repo", "mine"], { ...shown.deps, db, now: () => T0 });
  assert.match(shown.out.join("\n"), /bun test src\/tui/);
});

test("sql answers a SELECT and refuses everything else", async () => {
  // The guarantee that makes this a command instead of advice to run `sqlite3`:
  // a write cannot be expressed, against a file the server has open.
  const dir = mkdtempSync(join(tmpdir(), "bough-tags-sql-"));
  const file = join(dir, "bough.db");
  try {
    const db = openDb(file);
    db.createSession({ id: "s1", title: "wire the panel", kind: "root", parentId: null, createdAt: T0 });
    db.close();

    const ok = collector();
    assert.equal(
      await runTags(["sql", "SELECT title FROM sessions"], { ...ok.deps, dbFile: file, db: openDb(file) }),
      0,
    );
    assert.deepEqual(JSON.parse(ok.out.join("\n")), [{ title: "wire the panel" }]);

    for (
      const bad of [
        "DELETE FROM sessions",
        "UPDATE sessions SET title = 'x'",
        "DROP TABLE sessions",
        "PRAGMA writable_schema = ON",
      ]
    ) {
      const c = collector();
      assert.equal(await runTags(["sql", bad], { ...c.deps, dbFile: file, db: openDb(file) }), 2);
      assert.match(c.err.join("\n"), /must start with SELECT or WITH|read-only/);
    }

    // A malformed query answers with the driver's own words, which is what lets the
    // caller fix it — not a bare failure.
    const broken = collector();
    assert.equal(
      await runTags(["sql", "SELECT nope FROM sessions"], {
        ...broken.deps,
        dbFile: file,
        db: openDb(file),
      }),
      2,
    );
    assert.match(broken.err.join("\n"), /nope/);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("similar says why it cannot answer, and names what always works", async () => {
  // Graceful absence: the vector layer is optional by design, and the FTS path it
  // points at needs no extensions at all.
  const c = collector();
  assert.equal(
    await runTags(["similar", "get into the container"], {
      ...c.deps,
      db: seeded(),
      embed: () => null,
    }),
    1,
  );
  assert.match(c.err.join("\n"), /no local embedding layer/);
  assert.match(c.err.join("\n"), /bough tags sql/);

  // …and answers through the layer when there is one.
  const live = collector();
  assert.equal(
    await runTags(["similar", "docker"], {
      ...live.deps,
      db: seeded(),
      embed: () => ({
        similar: () => Promise.resolve([{ cmd: "docker exec -it web sh", distance: 0.2 }]),
        close: () => {},
      }),
    }),
    0,
  );
  assert.match(live.out.join("\n"), /docker exec -it web sh/);
});

test("an empty memory says so rather than printing an empty table", async () => {
  const c = collector();
  const empty = openDb(":memory:");
  assert.equal(await runTags([], { ...c.deps, db: empty, now: () => T0, cwd: "/nowhere" }), 0);
  assert.match(c.out.join("\n"), /no tagged commands yet/);
});
