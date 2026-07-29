/**
 * Tests for the Changes rail, driven against a REAL temporary git repository.
 *
 * A fake here would test nothing worth testing. Every claim this task makes is a
 * claim about what git does — that `git diff <base>` plus `ls-files --others` is the
 * whole change set, that `git checkout <base> -- <path>` restores one file without
 * touching its sibling, that a directory outside a repo answers rather than errors —
 * so the repo is real, created under `mkdtemp` and removed afterwards.
 * Nothing here touches `~/.bough`, the network, or an API key; the database is
 * in-memory and no socket is bound (plan §7).
 *
 * The acceptance criteria (task T8.5), each with a named test below:
 *
 *   - **base is recorded at creation.** It was never populated before this task —
 *     `POST /sessions` always stored null, so no session had a change set at all.
 *   - **the diff after edits** covers tracked edits AND untracked files.
 *   - **per-path revert leaves a sibling edit intact.** This is the one that makes
 *     the rail safe to use: reverting one file out of three must not cost the other
 *     two.
 *   - **a non-repo workspace answers cleanly** — 200 with a stated reason, not a
 *     throw and not an empty diff (spec §13).
 *
 * Plus the enforcement the port added: a revert may only touch paths the rail
 * showed. A path outside the change set is skipped and left on disk.
 *
 * Assertions come from `node:assert/strict` — jsr.io is unreachable from this
 * environment and a test that cannot run offline does not belong in `deno task test`.
 */
import { test } from "bun:test";
import { mkdir, mkdtemp, readFile, rm, stat, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import assert from "node:assert/strict";
import { Bus } from "../bus.ts";
import { openDb, type SqliteDb } from "../db/db.ts";
import type { Session } from "../schema/parts.ts";
import type { AppCtx } from "../types.ts";
import { EMPTY_TREE, git, parseGitDiff } from "../vcs/repodiff.ts";
// `./app.ts` first — the documented evaluation-order rule for this package: `app.ts`
// imports the handler modules, and they import its response helpers back.
import { createHandler, type Route, route } from "./app.ts";
import { getChangesH, revertChangesH, type SessionChangeSet } from "./changes.ts";
import { createSession } from "./sessions.ts";

/** The three entries this test drives, isolated from whatever else the table holds. */
const TABLE: Route[] = [
  route("POST", "/sessions", createSession),
  route("GET", "/sessions/:id/changes", getChangesH),
  route("POST", "/sessions/:id/changes/revert", revertChangesH),
];

interface Fixture {
  call: (req: Request) => Promise<Response>;
  db: SqliteDb;
  ctx: AppCtx;
  [Symbol.dispose](): void;
}

function fixture(): Fixture {
  const db = openDb(":memory:");
  const ctx: AppCtx = { db, bus: new Bus({ onListenerError: () => {} }), model: "test-model" };
  return {
    call: createHandler(ctx, { routes: TABLE }),
    db,
    ctx,
    [Symbol.dispose]() {
      db.close();
    },
  };
}

const url = (path: string) => `http://127.0.0.1:4321${path}`;
const get = (path: string) => new Request(url(path));
const post = (path: string, body?: unknown) =>
  new Request(url(path), {
    method: "POST",
    ...(body === undefined ? {} : { body: JSON.stringify(body) }),
  });

/** Whether git is usable at all. Absent git skips the repo tests rather than failing. */
const gitAvailable = await (async () => {
  try {
    return (await Bun.spawn(["git", "--version"], { stdout: "ignore", stderr: "ignore" })
      .exited) === 0;
  } catch {
    return false;
  }
})();

/**
 * A repo with one commit. Identity and signing are forced per command so the test
 * passes on a machine with no git config and on one that signs every commit.
 */
async function tempRepo(opts: { commit?: boolean } = {}): Promise<string> {
  const dir = await mkdtemp(join(tmpdir(), "bough-changes-"));
  await run(dir, ["init", "-q", "."]);
  if (opts.commit === false) return dir;
  await writeFile(`${dir}/README.md`, "base\n");
  await writeFile(`${dir}/vendor.txt`, "untouched\n");
  await writeFile(`${dir}/.gitignore`, "ignored.txt\n");
  await run(dir, ["add", "-A"]);
  await run(dir, [
    "-c",
    "user.email=t@t",
    "-c",
    "user.name=t",
    "-c",
    "commit.gpgsign=false",
    "commit",
    "-qm",
    "init",
  ]);
  return dir;
}

async function run(dir: string, args: string[]): Promise<void> {
  const r = await git(dir, args);
  if (!r.ok) throw new Error(`git ${args.join(" ")} failed: ${r.err}`);
}

/** Create the session over HTTP — the path that must record the base. */
async function startSession(f: Fixture, workspace: string): Promise<Session> {
  const res = await f.call(post("/sessions", { title: "s", workspace }));
  assert.equal(res.status, 201, await res.clone().text());
  return await res.json() as Session;
}

async function changesOf(f: Fixture, id: string): Promise<SessionChangeSet> {
  const res = await f.call(get(`/sessions/${id}/changes`));
  assert.equal(res.status, 200, await res.clone().text());
  return await res.json() as SessionChangeSet;
}

async function exists(path: string): Promise<boolean> {
  try {
    await stat(path);
    return true;
  } catch {
    return false;
  }
}

// ---- the parser (pure) -------------------------------------------------------

test("parseGitDiff: statuses, hunk bodies and the no-newline marker", () => {
  const files = parseGitDiff(
    [
      "diff --git a/keep.ts b/keep.ts",
      "index 111..222 100644",
      "--- a/keep.ts",
      "+++ b/keep.ts",
      "@@ -1,2 +1,2 @@",
      " const a = 1;",
      "-const b = 2;",
      "+const b = 3;",
      "\\ No newline at end of file",
      "diff --git a/gone.ts b/gone.ts",
      "deleted file mode 100644",
      "--- a/gone.ts",
      "+++ /dev/null",
      "@@ -1 +0,0 @@",
      "-was here",
      "diff --git a/fresh.ts b/fresh.ts",
      "new file mode 100644",
      "--- /dev/null",
      "+++ b/fresh.ts",
      "@@ -0,0 +1 @@",
      "+brand new",
      "",
    ].join("\n"),
  );

  assert.deepEqual(files.map((f) => [f.path, f.status]), [
    ["keep.ts", "modified"],
    ["gone.ts", "deleted"],
    ["fresh.ts", "added"],
  ]);
  assert.equal(files[0].hunks.length, 1);
  assert.deepEqual(files[0].hunks[0].lines, [
    " const a = 1;",
    "-const b = 2;",
    "+const b = 3;",
    "\\ No newline at end of file",
  ]);
  assert.equal(files[2].hunks[0].header, "@@ -0,0 +1 @@");
});

test("parseGitDiff: empty input is an empty change set, not a throw", () => {
  assert.deepEqual(parseGitDiff(""), []);
});

// ---- AC: base recorded at creation -------------------------------------------

test.skipIf(!gitAvailable)("AC: POST /sessions records the workspace's HEAD as the session's base", async () => {
  using f = fixture();
  const repo = await tempRepo();
  try {
    const head = (await git(repo, ["rev-parse", "HEAD"])).out.trim();
    const s = await startSession(f, repo);
    // On the wire AND in the row: the response is what the database kept.
    assert.equal(s.base, head);
    assert.equal(f.db.getSessionRuntime(s.id).base, head);
  } finally {
    await rm(repo, { recursive: true, force: true });
  }
});

test.skipIf(!gitAvailable)("base: a repo with no commits records the empty tree, so it still has a diff", async () => {
  using f = fixture();
  const repo = await tempRepo({ commit: false });
  try {
    const s = await startSession(f, repo);
    assert.equal(s.base, EMPTY_TREE);

    // Everything the session writes is its work — there is no earlier state.
    await writeFile(`${repo}/first.txt`, "hello\n");
    const set = await changesOf(f, s.id);
    assert.equal(set.available, true);
    assert.deepEqual(set.files.map((file) => file.path), ["first.txt"]);
    assert.equal(set.files[0].status, "added");
  } finally {
    await rm(repo, { recursive: true, force: true });
  }
});

test.skipIf(!gitAvailable)("base: a workspace that is not a repository records nothing", async () => {
  using f = fixture();
  const dir = await mkdtemp(join(tmpdir(), "bough-plain-"));
  try {
    const s = await startSession(f, dir);
    assert.equal(s.base ?? null, null);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

// ---- AC: the diff after edits ------------------------------------------------

test.skipIf(!gitAvailable)("AC: the change set is `git diff <base>` plus untracked files", async () => {
  using f = fixture();
  const repo = await tempRepo();
  try {
    const s = await startSession(f, repo);
    // The agent works in the checkout, so "an edit" is just writing there.
    await writeFile(`${repo}/README.md`, "base\nmore\n");
    await writeFile(`${repo}/new.txt`, "hi\n");

    const set = await changesOf(f, s.id);
    assert.equal(set.available, true);
    assert.equal(set.base, f.db.getSessionRuntime(s.id).base);
    assert.equal(set.workspace, repo);

    const byPath = new Map(set.files.map((file) => [file.path, file]));
    assert.deepEqual([...byPath.keys()].sort(), ["README.md", "new.txt"]);
    assert.equal(byPath.get("README.md")!.status, "modified");
    // Untracked ⇒ all-added, with real content so the rail can render it.
    assert.equal(byPath.get("new.txt")!.status, "added");
    assert.deepEqual(byPath.get("new.txt")!.hunks[0].lines, ["+hi"]);
    // vendor.txt was committed and never touched: not this session's work.
    assert.equal(byPath.has("vendor.txt"), false);

    // A staged edit is still the same change set — `git diff <commit>` covers the
    // index and the worktree both, so nothing is double-counted or lost.
    await run(repo, ["add", "new.txt"]);
    const staged = await changesOf(f, s.id);
    assert.deepEqual(staged.files.map((file) => file.path).sort(), ["README.md", "new.txt"]);
  } finally {
    await rm(repo, { recursive: true, force: true });
  }
});

test.skipIf(!gitAvailable)("changes: a deleted tracked file is part of the change set", async () => {
  using f = fixture();
  const repo = await tempRepo();
  try {
    const s = await startSession(f, repo);
    await rm(`${repo}/vendor.txt`);
    const set = await changesOf(f, s.id);
    assert.deepEqual(set.files.map((file) => [file.path, file.status]), [[
      "vendor.txt",
      "deleted",
    ]]);
  } finally {
    await rm(repo, { recursive: true, force: true });
  }
});

test.skipIf(!gitAvailable)("changes: build/cache noise is filtered from what the rail shows", async () => {
  using f = fixture();
  const repo = await tempRepo();
  try {
    const s = await startSession(f, repo);
    await writeFile(`${repo}/real.py`, "x = 1\n");
    await mkdir(`${repo}/__pycache__`, { recursive: true });
    await writeFile(`${repo}/__pycache__/real.cpython-312.pyc`, "junk\n");
    await writeFile(`${repo}/mod.pyc`, "junk\n");
    await writeFile(`${repo}/.DS_Store`, "junk\n");

    const set = await changesOf(f, s.id);
    assert.deepEqual(set.files.map((file) => file.path), ["real.py"]);
  } finally {
    await rm(repo, { recursive: true, force: true });
  }
});

// ---- AC: per-path revert -----------------------------------------------------

test.skipIf(!gitAvailable)("AC: per-path revert restores one file and leaves its siblings' edits intact", async () => {
  using f = fixture();
  const repo = await tempRepo();
  try {
    const s = await startSession(f, repo);
    await writeFile(`${repo}/README.md`, "clobbered\n"); // tracked edit
    await mkdir(`${repo}/sub`, { recursive: true });
    await writeFile(`${repo}/sub/created.txt`, "made by the agent\n"); // untracked
    await writeFile(`${repo}/kept.txt`, "keep me\n"); // the sibling

    const before = await changesOf(f, s.id);
    assert.deepEqual(before.files.map((file) => file.path).sort(), [
      "README.md",
      "kept.txt",
      "sub/created.txt",
    ]);

    const res = await f.call(
      post(`/sessions/${s.id}/changes/revert`, { paths: ["README.md", "sub/created.txt"] }),
    );
    assert.equal(res.status, 200, await res.clone().text());
    const outcome = await res.json() as { reverted: string[]; skipped: string[]; failed: [] };
    assert.deepEqual(outcome.reverted.sort(), ["README.md", "sub/created.txt"]);
    assert.deepEqual(outcome.skipped, []);
    assert.deepEqual(outcome.failed, []);

    // The tracked file is back at its base content; the created one is gone along
    // with the directory that existed only to hold it…
    assert.equal(await readFile(`${repo}/README.md`, "utf8"), "base\n");
    assert.equal(await exists(`${repo}/sub/created.txt`), false);
    assert.equal(await exists(`${repo}/sub`), false);
    // …and the sibling edit the reviewer did not pick is untouched.
    assert.equal(await readFile(`${repo}/kept.txt`, "utf8"), "keep me\n");

    const after = await changesOf(f, s.id);
    assert.deepEqual(after.files.map((file) => file.path), ["kept.txt"]);
  } finally {
    await rm(repo, { recursive: true, force: true });
  }
});

test.skipIf(!gitAvailable)("revert tells the MODEL, or the next turn re-applies the edit", async () => {
  // The bug this pins, measured end to end: reverting wrote nothing the agent could
  // read — no message, no event, no session state — so the next turn replayed its own
  // successful patch, its green test run and its summary, found the old code back,
  // and concluded its write had failed. It then "fixed the regression" by applying
  // the edit again. The one gesture that means "no, not that" was the one the model
  // was guaranteed to undo.
  using f = fixture();
  const repo = await tempRepo();
  try {
    const s = await startSession(f, repo);
    await writeFile(`${repo}/README.md`, "the agent's edit\n");
    const before = f.db.messagesFor(s.id).length;

    const res = await f.call(post(`/sessions/${s.id}/changes/revert`, { paths: ["README.md"] }));
    assert.equal(res.status, 200, await res.clone().text());

    const messages = f.db.messagesFor(s.id);
    assert.equal(messages.length, before + 1, "a revert must leave a record the model replays");
    const note = messages.at(-1)!;
    assert.equal(note.role, "system");
    const text = note.parts.map((p) => (p.type === "text" ? p.text : "")).join("");
    // It must name the file, and say the two things that stop a re-apply: this was
    // deliberate, and it is not yours to repair.
    assert.match(text, /README\.md/);
    assert.match(text, /revert/i);
    assert.match(text, /not.*(re-?apply|repair|regression)/is);
  } finally {
    await rm(repo, { recursive: true, force: true });
  }
});

test.skipIf(!gitAvailable)("revert: a revert that changed nothing says nothing to the model", async () => {
  // Skipped paths revert nothing, and a note about an edit that is still on disk
  // would be a lie the model acts on — the mirror image of the bug above.
  using f = fixture();
  const repo = await tempRepo();
  try {
    const s = await startSession(f, repo);
    await writeFile(`${repo}/README.md`, "the agent's edit\n");
    const before = f.db.messagesFor(s.id).length;
    const res = await f.call(post(`/sessions/${s.id}/changes/revert`, { paths: ["nope.txt"] }));
    assert.equal(res.status, 200, await res.clone().text());
    const outcome = await res.json() as { reverted: string[]; skipped: string[] };
    assert.deepEqual(outcome.reverted, []);
    assert.deepEqual(outcome.skipped, ["nope.txt"]);
    assert.equal(f.db.messagesFor(s.id).length, before);
  } finally {
    await rm(repo, { recursive: true, force: true });
  }
});

test.skipIf(!gitAvailable)("revert: an ABSENT `paths` reverts everything the rail is showing", async () => {
  using f = fixture();
  const repo = await tempRepo();
  try {
    const s = await startSession(f, repo);
    await writeFile(`${repo}/README.md`, "clobbered\n");
    await writeFile(`${repo}/new.txt`, "hi\n");

    const res = await f.call(post(`/sessions/${s.id}/changes/revert`, {}));
    assert.equal(res.status, 200, await res.clone().text());
    const outcome = await res.json() as { reverted: string[] };
    assert.deepEqual(outcome.reverted.sort(), ["README.md", "new.txt"]);

    assert.equal(await readFile(`${repo}/README.md`, "utf8"), "base\n");
    assert.equal(await exists(`${repo}/new.txt`), false);
    assert.deepEqual((await changesOf(f, s.id)).files, []);
  } finally {
    await rm(repo, { recursive: true, force: true });
  }
});

// THE REGRESSION THIS PINS. `{paths: []}` used to mean "revert the whole change
// set", identically to omitting the field — and an empty list is what a caller
// produces by ACCIDENT: a selection loop that matched no rows, a rail with nothing
// highlighted, a variable that came back empty. That made the one request nobody
// types on purpose the most destructive request in the API, against a change set
// that is every uncommitted file in the checkout (`base` is the sha the session
// started from, so work that was already there is in it too). It cost a real tree.
//
// Absent still means everything — the test above pins that, and it is what
// `api.revertChanges(id)` sends. Explicitly empty is now refused, loudly, with the
// move that gets revert-all.
test.skipIf(!gitAvailable)("revert: an EXPLICIT empty `paths` is refused, not read as a wildcard", async () => {
  using f = fixture();
  const repo = await tempRepo();
  try {
    const s = await startSession(f, repo);
    await writeFile(`${repo}/README.md`, "clobbered\n");
    await writeFile(`${repo}/new.txt`, "hi\n");

    const res = await f.call(post(`/sessions/${s.id}/changes/revert`, { paths: [] }));
    assert.equal(res.status, 400, await res.clone().text());
    const { error } = await res.json() as { error: string };
    // The message has to carry the move, or the caller just retries the same body.
    assert.match(error, /empty/i);
    assert.match(error, /omit `paths`/);

    // Nothing was touched — this is the whole point.
    assert.equal(await readFile(`${repo}/README.md`, "utf8"), "clobbered\n");
    assert.equal(await readFile(`${repo}/new.txt`, "utf8"), "hi\n");
    assert.deepEqual((await changesOf(f, s.id)).files.map((file) => file.path).sort(), [
      "README.md",
      "new.txt",
    ]);
  } finally {
    await rm(repo, { recursive: true, force: true });
  }
});

test.skipIf(!gitAvailable)("revert: a path outside the change set is SKIPPED, never restored or deleted", async () => {
  using f = fixture();
  const repo = await tempRepo();
  try {
    const s = await startSession(f, repo);
    await writeFile(`${repo}/real.py`, "x = 1\n");
    // Two paths the rail deliberately does not show: build noise, and a file the
    // repository ignores. Neither is the session's reviewable work, so neither is
    // revertable — and `git checkout <base> -- vendor.txt` would happily rewrite a
    // file nobody in this session ever opened.
    await writeFile(`${repo}/mod.pyc`, "junk\n");
    await writeFile(`${repo}/ignored.txt`, "user's own\n");

    const res = await f.call(post(`/sessions/${s.id}/changes/revert`, {
      paths: ["real.py", "mod.pyc", "ignored.txt", "vendor.txt", "../escape.txt"],
    }));
    assert.equal(res.status, 200, await res.clone().text());
    const outcome = await res.json() as { reverted: string[]; skipped: string[] };
    assert.deepEqual(outcome.reverted, ["real.py"]);
    assert.deepEqual(outcome.skipped.sort(), [
      "../escape.txt",
      "ignored.txt",
      "mod.pyc",
      "vendor.txt",
    ]);

    assert.equal(await exists(`${repo}/real.py`), false);
    assert.equal(await readFile(`${repo}/mod.pyc`, "utf8"), "junk\n");
    assert.equal(await readFile(`${repo}/ignored.txt`, "utf8"), "user's own\n");
    assert.equal(await readFile(`${repo}/vendor.txt`, "utf8"), "untouched\n");
  } finally {
    await rm(repo, { recursive: true, force: true });
  }
});

// ---- AC: a non-repo workspace ------------------------------------------------

test.skipIf(!gitAvailable)("AC: a workspace that is not a repository answers cleanly — 200 with a reason", async () => {
  using f = fixture();
  const dir = await mkdtemp(join(tmpdir(), "bough-plain-"));
  try {
    const s = await startSession(f, dir);
    await writeFile(`${dir}/work.txt`, "the agent still works here\n");

    const set = await changesOf(f, s.id);
    // Not an empty diff and not a throw: an ANSWER, with the reason spelled out
    // (spec §13). The distinction is the whole point — "not a repository" and
    // "you changed nothing" are different facts.
    assert.equal(set.available, false);
    assert.match(set.reason ?? "", /not a git repository/);
    assert.deepEqual(set.files, []);
    assert.equal(set.base, null);
    assert.equal(set.workspace, dir);

    // Revert is unavailable, and says why in the same words.
    const res = await f.call(post(`/sessions/${s.id}/changes/revert`, {}));
    assert.equal(res.status, 400);
    assert.match((await res.json() as { error: string }).error, /not a git repository/);
    // The file the agent wrote is still there — a refused revert deletes nothing.
    assert.equal(await exists(`${dir}/work.txt`), true);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test("a session with no workspace has no change set, and says so", async () => {
  using f = fixture();
  const res = await f.call(post("/sessions", { title: "no workspace" }));
  const s = await res.json() as Session;

  const set = await changesOf(f, s.id);
  assert.equal(set.available, false);
  assert.equal(set.workspace, null);
  assert.match(set.reason ?? "", /no workspace/);
  assert.equal((await f.call(post(`/sessions/${s.id}/changes/revert`, {}))).status, 400);
});

test("both routes 404 on an unknown session", async () => {
  using f = fixture();
  assert.equal((await f.call(get("/sessions/nope/changes"))).status, 404);
  assert.equal((await f.call(post("/sessions/nope/changes/revert", {}))).status, 404);
});

test.skipIf(!gitAvailable)("a session whose base was never recorded reports that, rather than the whole tree", async () => {
  using f = fixture();
  const repo = await tempRepo();
  try {
    const s = await startSession(f, repo);
    // The pre-T8.5 state: a real repo workspace with a null base. The tree must not
    // be reported as this session's work.
    f.db.createSession({
      id: "legacy",
      title: "legacy",
      kind: "root",
      parentId: null,
      createdAt: Date.now(),
      workspace: repo,
    });
    await writeFile(`${repo}/README.md`, "base\nmore\n");

    const legacy = await changesOf(f, "legacy");
    assert.equal(legacy.available, false);
    assert.match(legacy.reason ?? "", /no starting commit/);
    assert.deepEqual(legacy.files, []);
    // …while the session that DID record one sees the same edit fine.
    assert.deepEqual((await changesOf(f, s.id)).files.map((file) => file.path), ["README.md"]);
  } finally {
    await rm(repo, { recursive: true, force: true });
  }
});
