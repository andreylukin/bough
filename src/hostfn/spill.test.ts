/**
 * Spilling oversized command output to the scratchpad. The filesystem is injected,
 * so nothing here touches a real directory.
 */
import { test } from "bun:test";
import assert from "node:assert/strict";
import {
  planSpill,
  spill,
  type SpillDeps,
  SPILL_HEAD_CHARS,
  SPILL_OVER_CHARS,
  type SpillSink,
  SPILL_TAIL_CHARS,
  streamSpill,
} from "./spill.ts";

/** A fake filesystem recording every write. */
function fakeFs() {
  const files = new Map<string, string>();
  const dirs: string[] = [];
  const deps: SpillDeps = {
    exists: (p) => files.has(p),
    mkdirp: (d) => {
      dirs.push(d);
    },
    write: (p, t) => {
      files.set(p, t);
    },
    append: (p, t) => {
      files.set(p, (files.get(p) ?? "") + t);
    },
  };
  return { deps, files, dirs };
}

/** `n` characters of recognizable filler. */
function big(n: number, ch = "x"): string {
  return ch.repeat(n);
}

// ---------------------------------------------------------------------------
// planSpill — pure
// ---------------------------------------------------------------------------

test("output at or under the threshold does not spill", () => {
  // The common case by a wide margin: git status, a targeted rg, a passing test
  // run. None of them should be affected by any of this.
  assert.equal(planSpill(big(SPILL_OVER_CHARS), true).spilled, false);
  assert.equal(planSpill("", true).spilled, false);
});

test("output over the threshold spills, keeping head and tail verbatim", () => {
  const text = "HEAD" + big(SPILL_OVER_CHARS * 2) + "TAIL";
  const plan = planSpill(text, true);
  assert.ok(plan.spilled);
  assert.equal(plan.head.length, SPILL_HEAD_CHARS);
  assert.equal(plan.tail.length, SPILL_TAIL_CHARS);
  assert.ok(plan.head.startsWith("HEAD"));
  assert.ok(plan.tail.endsWith("TAIL"));
  assert.equal(plan.omitted, text.length - SPILL_HEAD_CHARS - SPILL_TAIL_CHARS);
});

test("nowhere to write means no spill, whatever the size", () => {
  // The aggressive inline budget is only defensible as a trade against a file
  // holding the rest. Without one it would just be destruction.
  assert.equal(planSpill(big(SPILL_OVER_CHARS * 10), false).spilled, false);
});

test("planSpill counts lines for the marker", () => {
  const text = ("line\n".repeat(SPILL_OVER_CHARS / 2)).slice(0, SPILL_OVER_CHARS * 2);
  const plan = planSpill(text, true);
  assert.ok(plan.spilled);
  assert.equal(plan.lines, text.split("\n").length);
});

// ---------------------------------------------------------------------------
// spill — the write
// ---------------------------------------------------------------------------

test("the full output reaches the file, not the truncated version", () => {
  // The entire point. If the file held the extract too, nothing was gained over
  // plain truncation.
  const f = fakeFs();
  const text = "START" + big(SPILL_OVER_CHARS * 3) + "END";
  const shown = spill(text, { scratch: "/scratch/s1", label: "bash" }, f.deps);
  assert.equal(f.files.size, 1);
  const [path, contents] = [...f.files.entries()][0] as [string, string];
  assert.equal(contents, text, "the file did not receive the complete output");
  // The invariant that matters is a CEILING, not a ratio: the inline cost is the
  // budget plus one marker no matter how vast the command's output was.
  assert.ok(
    shown.length <= SPILL_HEAD_CHARS + SPILL_TAIL_CHARS + 1_000,
    `inline extract was ${shown.length} chars, above the fixed budget`,
  );
  assert.ok(shown.includes(path), "the extract does not name the file it wrote");
});

test("the marker names the size, the path and the follow-up moves", () => {
  // Each clause earns its characters; an agent that cannot compose the follow-up
  // will conclude the file is empty and re-run the command.
  const f = fakeFs();
  const shown = spill("A" + big(SPILL_OVER_CHARS * 2) + "Z", { scratch: "/s", label: "bash" }, f.deps);
  assert.match(shown, /FULL OUTPUT SAVED/);
  assert.match(shown, /chars/);
  assert.match(shown, /lines/);
  assert.match(shown, /\brg\b/);
  assert.match(shown, /bough patterns/);
  assert.match(shown, /view\(/);
  assert.match(shown, /Do not re-run the command/);
  // Head and tail survive around the marker.
  assert.ok(shown.startsWith("A"));
  assert.ok(shown.endsWith("Z"));
});

test("the directory is created before the write", () => {
  const f = fakeFs();
  spill(big(SPILL_OVER_CHARS * 2), { scratch: "/scratch/s9" }, f.deps);
  assert.deepEqual(f.dirs, ["/scratch/s9"]);
});

test("successive spills never overwrite each other", () => {
  // A counter would reset across restarts and clobber a file an earlier turn is
  // still about to read.
  const f = fakeFs();
  const ctx = { scratch: "/s", label: "bash" };
  spill("one" + big(SPILL_OVER_CHARS * 2), ctx, f.deps);
  spill("two" + big(SPILL_OVER_CHARS * 2), ctx, f.deps);
  spill("three" + big(SPILL_OVER_CHARS * 2), ctx, f.deps);
  assert.equal(f.files.size, 3);
  const names = [...f.files.keys()].sort();
  assert.deepEqual(names, ["/s/bash-001.log", "/s/bash-002.log", "/s/bash-003.log"]);
  assert.ok((f.files.get("/s/bash-001.log") as string).startsWith("one"));
  assert.ok((f.files.get("/s/bash-003.log") as string).startsWith("three"));
});

test("the label separates one verb's spills from another's", () => {
  const f = fakeFs();
  spill(big(SPILL_OVER_CHARS * 2), { scratch: "/s", label: "bash" }, f.deps);
  spill(big(SPILL_OVER_CHARS * 2), { scratch: "/s", label: "sh" }, f.deps);
  assert.deepEqual([...f.files.keys()].sort(), ["/s/bash-001.log", "/s/sh-001.log"]);
});

test("a path with a space or a quote survives into the suggested commands", () => {
  const f = fakeFs();
  const shown = spill(big(SPILL_OVER_CHARS * 2), { scratch: "/tmp/my logs", label: "bash" }, f.deps);
  // Unquoted, the rg hint would silently search two different paths.
  assert.match(shown, /rg -n 'error\|fail' '\/tmp\/my logs\/bash-001\.log'/);
});

// ---------------------------------------------------------------------------
// Fallback
// ---------------------------------------------------------------------------

test("without a scratchpad it falls back to the generous head and tail", () => {
  // Nothing is dropped that would not have been dropped before this existed.
  const f = fakeFs();
  const text = big(SPILL_OVER_CHARS * 5);
  const shown = spill(text, {}, f.deps);
  assert.equal(f.files.size, 0);
  assert.equal(shown, text, "a 100k text fits the old budget and must be untouched");
});

test("a failed write degrades to truncation rather than failing the command", () => {
  // A full disk must not turn a successful command into a failed host call.
  const broken: SpillDeps = {
    exists: () => false,
    mkdirp: () => {
      throw new Error("EROFS: read-only file system");
    },
    write: () => {},
    append: () => {},
  };
  const text = big(SPILL_OVER_CHARS * 2);
  assert.doesNotThrow(() => spill(text, { scratch: "/s" }, broken));
  const shown = spill(text, { scratch: "/s" }, broken);
  assert.ok(!shown.includes("FULL OUTPUT SAVED"));
  assert.equal(shown, text, "within the fallback budget, so it should be intact");
});

test("small output is returned completely unchanged", () => {
  const f = fakeFs();
  const text = "ok\n";
  assert.equal(spill(text, { scratch: "/s" }, f.deps), text);
  assert.equal(f.files.size, 0);
});

test("the inline cost is bounded no matter how vast the output is", () => {
  // The whole promise of the feature, stated as one assertion: a command that
  // prints ten megabytes costs the same context as one that prints thirty
  // kilobytes.
  const f = fakeFs();
  const small = spill(big(SPILL_OVER_CHARS * 2), { scratch: "/s", label: "a" }, f.deps);
  const huge = spill(big(10_000_000), { scratch: "/s", label: "b" }, f.deps);
  const ceiling = SPILL_HEAD_CHARS + SPILL_TAIL_CHARS + 1_000;
  assert.ok(small.length <= ceiling);
  assert.ok(huge.length <= ceiling, `10MB of output produced ${huge.length} inline chars`);
  // And the 10MB is genuinely on disk, not discarded.
  assert.equal((f.files.get("/s/b-001.log") as string).length, 10_000_000);
});

// ---------------------------------------------------------------------------
// Streaming sink
// ---------------------------------------------------------------------------

/** Feed `chunks` through the sink the way `append` does, returning the file. */
function stream(chunks: string[], f: ReturnType<typeof fakeFs>) {
  let sink: SpillSink | undefined;
  let seen = "";
  for (const c of chunks) {
    seen += c;
    const snapshot = seen;
    sink = streamSpill(sink, c, {
      scratch: "/s",
      label: "bash",
      totalSoFar: snapshot.length,
      pending: () => snapshot,
    }, f.deps);
  }
  return { sink, contents: sink ? (f.files.get(sink.path) as string) : undefined };
}

test("the streamed file holds every byte, including the chunk that opened it", () => {
  // The regression: opening the sink before writing the triggering chunk dropped
  // exactly one chunk — 262,144 chars of a 1.29MB command — from a file whose
  // banner claimed it held everything.
  const f = fakeFs();
  const chunks = [big(9_000, "a"), big(9_000, "b"), big(9_000, "c"), big(9_000, "d")];
  const { sink, contents } = stream(chunks, f);
  assert.ok(sink);
  assert.equal(contents, chunks.join(""), "the file is not byte-identical to the stream");
  assert.equal(sink.chars, chunks.join("").length);
});

test("the sink stays closed while output is under the threshold", () => {
  // Otherwise every `git status` litters the scratchpad with an empty log.
  const f = fakeFs();
  const { sink } = stream([big(5_000), big(5_000)], f);
  assert.equal(sink, undefined);
  assert.equal(f.files.size, 0);
});

test("the sink survives past the retention cap without losing the middle", () => {
  // The reason it streams at all: the in-memory buffer caps at 400k, so anything
  // written from it afterwards would be missing precisely the part the marker
  // promises is on disk.
  const f = fakeFs();
  const chunks = Array.from({ length: 12 }, (_, i) => big(50_000, String.fromCharCode(97 + i)));
  const { contents } = stream(chunks, f);
  const joined = chunks.join("");
  assert.equal(contents?.length, joined.length);
  assert.equal(contents, joined);
  assert.ok(!(contents as string).includes("omitted from the middle"), "an omission marker got baked into the saved file");
});

test("the marker reports the true total, not the retained size", () => {
  // `spill` is handed text out of the capped buffer; reporting its length would
  // understate a 1.29MB command as 400KB.
  const f = fakeFs();
  const chunks = [big(30_000, "a"), big(30_000, "b")];
  const { sink } = stream(chunks, f);
  assert.ok(sink);
  const retained = big(1_000, "a") + big(1_000, "b");
  const shown = spill(retained, { scratch: "/s", sink, label: "bash" }, f.deps);
  assert.match(shown, /FULL OUTPUT SAVED — 60,000 chars/);
});

test("a write failure mid-stream does not throw at the caller", () => {
  const flaky: SpillDeps = {
    exists: () => false,
    mkdirp: () => {},
    write: () => {},
    append: () => {
      throw new Error("ENOSPC");
    },
  };
  let sink: SpillSink | undefined;
  assert.doesNotThrow(() => {
    for (const c of [big(30_000), big(30_000)]) {
      sink = streamSpill(sink, c, {
        scratch: "/s",
        totalSoFar: 60_000,
        pending: () => c,
      }, flaky);
    }
  });
});
