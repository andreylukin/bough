/**
 * The file verbs are the pure patch engine plus exactly one piece of state: the
 * text this session last saw. So these tests are about that state and the IO
 * around it — the engine's own conflict math is exhausted in `patch.test.ts` and
 * is not re-proved here, only spot-checked where it crosses the filesystem.
 *
 * The acceptance criterion (plan T3.3) is the first test below: view → patch with
 * an EMPTY tag → succeeds and echoes a new tag → a SECOND patch chains onto that
 * echoed tag without viewing again.
 *
 * Hermetic and offline: every test owns a fresh `mkdtemp()` workspace it
 * deletes afterwards, and every fixture injects its own `SnapshotStore`, so no
 * test can see another's snapshots and nothing here touches `~/.bough` or the
 * network.
 *
 * Assertions come from `node:assert` rather than `@std/assert`: jsr.io is denied
 * by this environment's egress policy, so the jsr import declared in `deno.json`
 * cannot resolve. `node:assert` is built into the runtime and needs no fetch.
 * (Same constraint `patch.test.ts` and `bus.test.ts` document.)
 */

import { test } from "bun:test";
import { match, notStrictEqual, ok, strictEqual } from "node:assert";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { NotFoundError, PatchError } from "../errors.ts";
import {
  createFileHostFns,
  type FileHostFns,
  SnapshotStore,
  takeSessionWrites,
} from "./files.ts";
import { tagOf } from "./patch.ts";

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/** File text from lines, with the trailing newline a real file has. */
const doc = (...lines: string[]) => lines.join("\n") + "\n";

interface Workspace {
  /** The temp directory the verbs resolve relative paths against. */
  dir: string;
  /** Verbs for the default session ("s1"), over the fixture's own store. */
  fns: FileHostFns;
  snapshots: SnapshotStore;
  /** Verbs for another session over the SAME store — the isolation tests use this. */
  session(id: string): FileHostFns;
  read(path: string): Promise<string>;
  put(path: string, text: string): Promise<void>;
}

/**
 * Run `body` against a throwaway workspace seeded with `files`, then delete it.
 * `storeOpts` reaches the injected `SnapshotStore` so the eviction test can shrink
 * the bounds instead of writing 65 files.
 */
async function withWorkspace(
  files: Record<string, string>,
  body: (ws: Workspace) => Promise<void>,
  storeOpts?: { maxSessions?: number; maxPerSession?: number },
): Promise<void> {
  const dir = await mkdtemp(join(tmpdir(), "bough-files-test-"));
  const snapshots = new SnapshotStore(storeOpts);
  const put = async (path: string, text: string) => {
    const full = join(dir, path);
    await mkdir(dirname(full), { recursive: true });
    await writeFile(full, text);
  };
  try {
    for (const [path, text] of Object.entries(files)) await put(path, text);
    await body({
      dir,
      snapshots,
      fns: createFileHostFns({ workspace: dir, sessionId: "s1" }, { snapshots }),
      session: (id) => createFileHostFns({ workspace: dir, sessionId: id }, { snapshots }),
      read: (path) => readFile(join(dir, path), "utf8"),
      put,
    });
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
}

/** The `[path#TAG]` a verb echoed. Fails loudly rather than returning null. */
function echoedTag(output: string, path: string): string {
  const m = new RegExp(`\\[${path.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}#([0-9A-F]{4})\\]`)
    .exec(output);
  ok(m, `expected a [${path}#TAG] in:\n${output}`);
  return m![1];
}

/** Assert the thunk rejects, and hand the error back for claims about its text. */
async function rejects(fn: () => Promise<unknown>): Promise<Error> {
  try {
    await fn();
  } catch (e) {
    return e as Error;
  }
  throw new Error("expected a rejection, but the call resolved");
}

// ---------------------------------------------------------------------------
// The acceptance criterion
// ---------------------------------------------------------------------------

test("AC: view → empty-tag patch → echoed tag chains a second patch, no re-view", async () => {
  await withWorkspace({ "a.ts": doc("one", "two", "three", "four") }, async (ws) => {
    // 1. view records the version the ops will be written against.
    const listing = await ws.fns.view("a.ts");
    const viewedTag = echoedTag(listing, "a.ts");

    // 2. an EMPTY tag means "the version I just viewed" — the normal case.
    const first = await ws.fns.patch(`[a.ts#]\nSWAP 2:\n+TWO\n`);
    strictEqual(await ws.read("a.ts"), doc("one", "TWO", "three", "four"));

    // …and it echoes the file's NEW tag, which is a real tag of the new text.
    const firstTag = echoedTag(first, "a.ts");
    strictEqual(firstTag, tagOf(await ws.read("a.ts")));
    notStrictEqual(firstTag, viewedTag);
    match(first, /patched — 1 operation, now 4 lines/);

    // 3. a SECOND patch chains onto the echoed tag with no view() in between.
    //    Its line numbers are in the coordinates of the version that tag names.
    const second = await ws.fns.patch(`[a.ts#${firstTag}]\nINS.POST 4:\n+five\nDEL 1\n`);
    strictEqual(await ws.read("a.ts"), doc("TWO", "three", "four", "five"));

    // …and that echo chains again, indefinitely.
    const secondTag = echoedTag(second, "a.ts");
    strictEqual(secondTag, tagOf(await ws.read("a.ts")));
    notStrictEqual(secondTag, firstTag);
    await ws.fns.patch(`[a.ts#${secondTag}]\nSWAP 1:\n+2\n`);
    strictEqual(await ws.read("a.ts"), doc("2", "three", "four", "five"));

    // …as does an empty tag, which now names what the last patch wrote.
    await ws.fns.patch(`[a.ts#]\nINS.HEAD:\n+// header\n`);
    strictEqual(await ws.read("a.ts"), doc("// header", "2", "three", "four", "five"));
  });
});

// ---------------------------------------------------------------------------
// view
// ---------------------------------------------------------------------------

test("view: [path#TAG] header then numbered lines, padded to a common width", async () => {
  const text = doc(...Array.from({ length: 12 }, (_, i) => `line ${i + 1}`));
  await withWorkspace({ "a.ts": text }, async (ws) => {
    const out = await ws.fns.view("a.ts");
    const lines = out.split("\n");
    strictEqual(lines[0], `[a.ts#${tagOf(text)}]`);
    strictEqual(lines[1], " 1:line 1");
    strictEqual(lines[9], " 9:line 9");
    strictEqual(lines[10], "10:line 10");
    strictEqual(lines[12], "12:line 12");
    strictEqual(lines.length, 13); // header + 12 lines, no trailing blank
  });
});

test("view: the path is echoed as WRITTEN, but recorded as RESOLVED", async () => {
  await withWorkspace({ "sub/a.ts": doc("x") }, async (ws) => {
    const out = await ws.fns.view("./sub/a.ts");
    match(out, /^\[\.\/sub\/a\.ts#[0-9A-F]{4}\]/);
    // "./sub/a.ts" and "sub/a.ts" are one file, so they must be one record.
    await ws.fns.patch(`[sub/a.ts#]\nSWAP 1:\n+y\n`);
    strictEqual(await ws.read("sub/a.ts"), doc("y"));
    strictEqual(ws.snapshots.size("s1"), 1);
  });
});

test("view: an empty file says so instead of rendering nothing", async () => {
  await withWorkspace({ "empty.ts": "" }, async (ws) => {
    const out = await ws.fns.view("empty.ts");
    strictEqual(out.split("\n")[0], `[empty.ts#${tagOf("")}]`);
    match(out, /this file is empty/);
    match(out, /INS\.HEAD:/);
    // …and it is on record, so INS.HEAD against it works.
    await ws.fns.patch(`[empty.ts#]\nINS.HEAD:\n+first\n`);
    strictEqual(await ws.read("empty.ts"), doc("first"));
  });
});

test("view: a missing file names the path it looked at and how to create it", async () => {
  await withWorkspace({}, async (ws) => {
    const err = await rejects(() => ws.fns.view("nope/a.ts"));
    ok(err instanceof NotFoundError, `expected NotFoundError, got ${err.name}`);
    match(err.message, /no such file/);
    match(err.message, /nope\/a\.ts/);
    match(err.message, /write\("nope\/a\.ts"/);
  });
});

test("view: a directory is named as one, not reported as unreadable", async () => {
  await withWorkspace({ "sub/a.ts": doc("x") }, async (ws) => {
    const err = await rejects(() => ws.fns.view("sub"));
    match(err.message, /it is a directory/);
    match(err.message, /bash\("ls -la sub"\)/);
  });
});

test("view: a binary file is refused before it can be lossily rewritten", async () => {
  await withWorkspace({}, async (ws) => {
    await writeFile(join(ws.dir, "b.bin"), new Uint8Array([0x89, 0x00, 0x01, 0x02]));
    const err = await rejects(() => ws.fns.view("b.bin"));
    match(err.message, /NUL bytes/);
    // Nothing on record, so a patch against it is refused too.
    strictEqual(ws.snapshots.size("s1"), 0);
  });
});

test("view: an oversized file is refused with a way to read part of it", async () => {
  await withWorkspace({}, async (ws) => {
    await writeFile(join(ws.dir, "big.txt"), "x".repeat(2 * 1024 * 1024 + 1));
    const err = await rejects(() => ws.fns.view("big.txt"));
    match(err.message, /over the 2097152-byte view limit/);
    match(err.message, /rg -n PATTERN big\.txt/);
  });
});

test("view: an empty path is refused by name rather than resolving to the workspace", async () => {
  await withWorkspace({}, async (ws) => {
    const err = await rejects(() => ws.fns.view("   "));
    match(err.message, /view\(\) needs a path/);
  });
});

// ---------------------------------------------------------------------------
// write
// ---------------------------------------------------------------------------

test("write: creates parent directories, echoes the tag, and records it", async () => {
  await withWorkspace({}, async (ws) => {
    const content = doc("a", "b", "c");
    const out = await ws.fns.write("deep/er/new.ts", content);
    strictEqual(await ws.read("deep/er/new.ts"), content);
    strictEqual(echoedTag(out, "deep/er/new.ts"), tagOf(content));
    match(out, /wrote 3 lines \(6 bytes\)/);

    // A file this session wrote is a file it has seen: patch it without viewing.
    await ws.fns.patch(`[deep/er/new.ts#]\nSWAP 2:\n+B\n`);
    strictEqual(await ws.read("deep/er/new.ts"), doc("a", "B", "c"));
  });
});

test("write: replaces an existing file wholesale and re-anchors it", async () => {
  await withWorkspace({ "a.ts": doc("old", "old", "old") }, async (ws) => {
    await ws.fns.view("a.ts");
    const out = await ws.fns.write("a.ts", doc("new"));
    strictEqual(await ws.read("a.ts"), doc("new"));
    // The snapshot is the WRITE, not the earlier view — otherwise an empty tag
    // would resolve to a version the file no longer has.
    strictEqual(echoedTag(out, "a.ts"), tagOf(doc("new")));
    await ws.fns.patch(`[a.ts#]\nINS.TAIL:\n+tail\n`);
    strictEqual(await ws.read("a.ts"), doc("new", "tail"));
  });
});

test("write: an empty file is 0 lines, not one blank one", async () => {
  await withWorkspace({}, async (ws) => {
    const out = await ws.fns.write("e.ts", "");
    strictEqual(await ws.read("e.ts"), "");
    match(out, /wrote 0 lines \(0 bytes\)/);
  });
});

// ---------------------------------------------------------------------------
// patch: what the snapshot store is for
// ---------------------------------------------------------------------------

test("patch: a file this session never viewed is refused, and told to view it", async () => {
  await withWorkspace({ "a.ts": doc("one", "two") }, async (ws) => {
    const err = await rejects(() => ws.fns.patch(`[a.ts#]\nSWAP 1:\n+ONE\n`));
    ok(err instanceof PatchError, `expected PatchError, got ${err.name}`);
    match(err.message, /no viewed version of a\.ts is on record/);
    match(err.message, /call view\("a\.ts"\)/);
    strictEqual(await ws.read("a.ts"), doc("one", "two")); // untouched
  });
});

test("patch: snapshots are per session — a sibling's view is not mine", async () => {
  await withWorkspace({ "a.ts": doc("one", "two") }, async (ws) => {
    await ws.session("spawner").view("a.ts");
    // A subagent is its own session and must anchor to what IT read.
    const err = await rejects(() => ws.session("subagent").patch(`[a.ts#]\nSWAP 1:\n+ONE\n`));
    match(err.message, /no viewed version of a\.ts is on record/);
    strictEqual(await ws.read("a.ts"), doc("one", "two"));

    // The session that did view it is unaffected.
    await ws.session("spawner").patch(`[a.ts#]\nSWAP 1:\n+ONE\n`);
    strictEqual(await ws.read("a.ts"), doc("ONE", "two"));
  });
});

test("patch: a missing file says to write() it, and nothing else in the patch lands", async () => {
  await withWorkspace({ "a.ts": doc("one") }, async (ws) => {
    await ws.fns.view("a.ts");
    const err = await rejects(() =>
      ws.fns.patch(`[a.ts#]\nSWAP 1:\n+ONE\n\n[gone.ts#]\nSWAP 1:\n+x\n`)
    );
    match(err.message, /cannot patch gone\.ts: no such file/);
    match(err.message, /write\("gone\.ts"/);
    match(err.message, /all its files or none/);
    strictEqual(await ws.read("a.ts"), doc("one")); // the readable file is untouched
  });
});

test("patch: a stale explicit tag names the current tag and the empty-tag escape", async () => {
  await withWorkspace({ "a.ts": doc("one", "two") }, async (ws) => {
    await ws.fns.view("a.ts");
    const err = await rejects(() => ws.fns.patch(`[a.ts#0000]\nSWAP 1:\n+ONE\n`));
    match(err.message, /stale tag/);
    match(err.message, new RegExp(`now #${tagOf(doc("one", "two"))}`));
    match(err.message, /empty tag "\[a\.ts#\]"/);
    strictEqual(await ws.read("a.ts"), doc("one", "two"));
  });
});

// ---------------------------------------------------------------------------
// patch: concurrency, the reason any of this exists
// ---------------------------------------------------------------------------

test("patch: a concurrent edit OUTSIDE the patched lines rebases and both land", async () => {
  await withWorkspace({ "a.ts": doc("l1", "l2", "l3", "l4") }, async (ws) => {
    await ws.fns.view("a.ts");
    // Someone else (another subagent, the user's editor) prepends a line.
    await ws.put("a.ts", doc("added", "l1", "l2", "l3", "l4"));

    // Written in the VIEWED coordinates: line 3 is "l3".
    const out = await ws.fns.patch(`[a.ts#]\nSWAP 3:\n+L3\n`);
    strictEqual(await ws.read("a.ts"), doc("added", "l1", "l2", "L3", "l4"));
    strictEqual(echoedTag(out, "a.ts"), tagOf(await ws.read("a.ts")));
  });
});

test("patch: a concurrent edit INSIDE the patched lines is a named conflict", async () => {
  await withWorkspace({ "a.ts": doc("l1", "l2", "l3", "l4") }, async (ws) => {
    await ws.fns.view("a.ts");
    const theirs = doc("l1", "l2", "THEIRS", "l4");
    await ws.put("a.ts", theirs);

    const err = await rejects(() => ws.fns.patch(`[a.ts#]\nSWAP 3:\n+MINE\n`));
    ok(err instanceof PatchError, `expected PatchError, got ${err.name}`);
    match(err.message, /patch conflict in a\.ts/);
    match(err.message, /lines 3\.=3 were rewritten/);
    match(err.message, /Someone else changed a\.ts/);
    match(err.message, /Re-view a\.ts/);
    // Their edit survives untouched — this is the whole point.
    strictEqual(await ws.read("a.ts"), theirs);
  });
});

test("patch: multi-file — all of them or none", async () => {
  const a = doc("a1", "a2");
  const b = doc("b1", "b2");
  await withWorkspace({ "a.ts": a, "b.ts": b }, async (ws) => {
    await ws.fns.view("a.ts");
    await ws.fns.view("b.ts");

    // b's anchor is out of range, so NEITHER file may be written.
    const err = await rejects(() =>
      ws.fns.patch(`[a.ts#]\nSWAP 1:\n+A1\n\n[b.ts#]\nSWAP 99:\n+B\n`)
    );
    match(err.message, /b\.ts: line 99 is out of range/);
    strictEqual(await ws.read("a.ts"), a);
    strictEqual(await ws.read("b.ts"), b);

    // Corrected, both land in one call, and both tags are echoed.
    const out = await ws.fns.patch(`[a.ts#]\nSWAP 1:\n+A1\n\n[b.ts#]\nSWAP 2:\n+B2\n`);
    strictEqual(await ws.read("a.ts"), doc("A1", "a2"));
    strictEqual(await ws.read("b.ts"), doc("b1", "B2"));
    strictEqual(echoedTag(out, "a.ts"), tagOf(await ws.read("a.ts")));
    strictEqual(echoedTag(out, "b.ts"), tagOf(await ws.read("b.ts")));
    strictEqual(out.split("\n").length, 2);
  });
});

test("patch: two spellings of one path in one patch are refused, not merged", async () => {
  await withWorkspace({ "a.ts": doc("one", "two") }, async (ws) => {
    await ws.fns.view("a.ts");
    // Both sections would be computed against the pre-patch text, so the second
    // write would silently discard the first.
    const err = await rejects(() =>
      ws.fns.patch(`[a.ts#]\nSWAP 1:\n+ONE\n\n[./a.ts#]\nSWAP 2:\n+TWO\n`)
    );
    ok(err instanceof PatchError, `expected PatchError, got ${err.name}`);
    match(err.message, /name the same file/);
    match(err.message, /single "\[a\.ts#\]" section/);
    strictEqual(await ws.read("a.ts"), doc("one", "two"));
  });
});

test("patch: an absolute path outside the workspace is an ordinary target", async () => {
  const outside = await mkdtemp(join(tmpdir(), "bough-files-outside-"));
  const target = join(outside, "cfg.txt");
  try {
    await writeFile(target, doc("k=1"));
    await withWorkspace({}, async (ws) => {
      // The workspace is the ORIGIN for relative paths, never a boundary (spec §2).
      await ws.fns.view(target);
      await ws.fns.patch(`[${target}#]\nSWAP 1:\n+k=2\n`);
      strictEqual(await readFile(target, "utf8"), doc("k=2"));
    });
  } finally {
    await rm(outside, { recursive: true, force: true });
  }
});

test("patch: CRLF and a missing trailing newline survive the round trip", async () => {
  await withWorkspace({}, async (ws) => {
    await writeFile(join(ws.dir, "crlf.ts"), "one\r\ntwo\r\nthree");
    await ws.fns.view("crlf.ts");
    await ws.fns.patch(`[crlf.ts#]\nSWAP 2:\n+TWO\n`);
    strictEqual(await ws.read("crlf.ts"), "one\r\nTWO\r\nthree");
  });
});

// ---------------------------------------------------------------------------
// the store's bounds
// ---------------------------------------------------------------------------

test("snapshots: the oldest path is evicted, and a dropped one costs a re-view", async () => {
  await withWorkspace(
    { "a.ts": doc("a"), "b.ts": doc("b"), "c.ts": doc("c") },
    async (ws) => {
      await ws.fns.view("a.ts");
      await ws.fns.view("b.ts");
      await ws.fns.view("c.ts"); // evicts a.ts
      strictEqual(ws.snapshots.size("s1"), 2);

      const err = await rejects(() => ws.fns.patch(`[a.ts#]\nSWAP 1:\n+A\n`));
      match(err.message, /no viewed version of a\.ts is on record/);
      strictEqual(await ws.read("a.ts"), doc("a"));

      // Re-viewing puts it back — the eviction costs a round, never an edit.
      await ws.fns.view("a.ts");
      await ws.fns.patch(`[a.ts#]\nSWAP 1:\n+A\n`);
      strictEqual(await ws.read("a.ts"), doc("A"));
    },
    { maxPerSession: 2 },
  );
});

test("snapshots: the least recently active session is evicted whole", async () => {
  await withWorkspace({ "a.ts": doc("a") }, async (ws) => {
    await ws.session("s-old").view("a.ts");
    await ws.session("s-mid").view("a.ts");
    await ws.session("s-new").view("a.ts"); // evicts s-old
    strictEqual(ws.snapshots.size("s-old"), 0);
    strictEqual(ws.snapshots.size("s-mid"), 1);
    strictEqual(ws.snapshots.size("s-new"), 1);

    const err = await rejects(() => ws.session("s-old").patch(`[a.ts#]\nSWAP 1:\n+A\n`));
    match(err.message, /no viewed version of a\.ts is on record/);
  }, { maxSessions: 2 });
});

test("snapshots: recording is keyed by session, so two sessions hold two versions", async () => {
  await withWorkspace({ "a.ts": doc("one") }, async (ws) => {
    const one = ws.session("one");
    const two = ws.session("two");
    await one.view("a.ts");
    await two.view("a.ts");
    // `one` patches first; `two` is now anchored to a version that has moved on,
    // but its lines were not touched, so it rebases rather than conflicting.
    await one.patch(`[a.ts#]\nINS.HEAD:\n+header\n`);
    await two.patch(`[a.ts#]\nINS.TAIL:\n+footer\n`);
    strictEqual(await ws.read("a.ts"), doc("header", "one", "footer"));
  });
});

test("the write verbs record what a session wrote, and the record is read once", async () => {
  // Every delegated report read `Changed files: not reported`. Git cannot answer it —
  // subagents share their spawner's checkout, so a diff at the end is the union of every
  // concurrent sibling's work — but the write verbs know exactly what they wrote.
  await withWorkspace({}, async (ws) => {
    // The store is module-level and keyed by session id, so earlier tests in this file have
    // already written under "s1". Draining first is what a real caller does anyway.
    takeSessionWrites("s1");
    await ws.fns.write("lib/alpha.py", doc("def a(): pass"));
    await ws.fns.write("lib/beta.py", doc("def b(): pass"));
    // A patch counts too: it is the other way a file changes.
    const shown = await ws.fns.view("lib/alpha.py");
    const tag = /\[lib\/alpha\.py#([^\]]+)\]/.exec(shown)![1];
    await ws.fns.patch(`[lib/alpha.py#${tag}]\nSWAP 1.=1:\n+def a(): return 1`);

    const wrote = takeSessionWrites("s1").sort();
    strictEqual(wrote.join(","), "lib/alpha.py,lib/beta.py");
    // READ ONCE: the only caller builds a report once, and a store that only grows in a
    // process running for weeks is a leak with extra steps.
    strictEqual(takeSessionWrites("s1").length, 0);
    // Another session's writes are its own.
    strictEqual(takeSessionWrites("s2").length, 0);
  });
});
