/**
 * `bough patterns`, driven end to end with nothing on disk and nothing on a pipe.
 * Every effect is injected, so these are ordinary function calls.
 */
import { test } from "bun:test";
import assert from "node:assert/strict";
import { isUsageError, parseArgs, type PatternDeps, runPatterns } from "./patterns.ts";

/** A deps object over a fixed set of lines, capturing what was written. */
function fixture(lines: string[], isTty = false) {
  const out: string[] = [];
  const err: string[] = [];
  const deps: PatternDeps = {
    readLines: (file) => {
      if (file === "missing.log") throw new Error("ENOENT");
      return lines;
    },
    out: (l) => out.push(l),
    err: (l) => err.push(l),
    isTty,
  };
  return { deps, out: () => out.join("\n"), err: () => err.join("\n") };
}

/** A small log with two statements, one of them failing. */
function sampleLog(): string[] {
  const lines: string[] = [];
  const base = Date.UTC(2024, 0, 15, 14, 0, 0);
  for (let i = 0; i < 60; i++) {
    const t = new Date(base + i * 1000).toISOString();
    lines.push(`${t} INFO Request from 10.0.1.${i % 4} completed in ${20 + (i % 30)}ms status=200`);
  }
  for (let i = 0; i < 5; i++) {
    const t = new Date(base + i * 1000).toISOString();
    lines.push(`${t} ERROR Timeout connecting to 10.0.9.${i} after ${5000 + i}ms`);
  }
  return lines;
}

// ---------------------------------------------------------------------------
// Parsing — pure and total
// ---------------------------------------------------------------------------

test("parseArgs defaults to twenty patterns and stdin", () => {
  const a = parseArgs([]);
  assert.ok(!isUsageError(a));
  assert.equal(a.top, 20);
  assert.equal(a.file, undefined);
  assert.equal(a.format, undefined);
});

test("parseArgs takes flags before or after the file", () => {
  const before = parseArgs(["--json", "--top", "5", "app.log"]);
  const after = parseArgs(["app.log", "--top", "5", "--json"]);
  assert.deepEqual(before, after);
  assert.ok(!isUsageError(before));
  assert.equal(before.file, "app.log");
  assert.equal(before.format, "json");
  assert.equal(before.top, 5);
});

test("parseArgs treats `-` as stdin rather than as a file", () => {
  const a = parseArgs(["-"]);
  assert.ok(!isUsageError(a));
  assert.equal(a.file, undefined);
});

test("parseArgs rejects two contradicting formats", () => {
  // Silently taking the last one produces output the caller did not ask for and
  // will parse with the wrong reader.
  const a = parseArgs(["--json", "--llm"]);
  assert.ok(isUsageError(a));
  assert.match(a.usageError, /cannot both be given/);
});

test("parseArgs accepts a format repeated", () => {
  assert.ok(!isUsageError(parseArgs(["--json", "--json"])));
});

test("parseArgs validates every numeric option", () => {
  for (const argv of [
    ["--top", "0"],
    ["--top", "x"],
    ["--threshold", "0"],
    ["--threshold", "1.5"],
    ["--year", "24"],
  ]) {
    assert.ok(isUsageError(parseArgs(argv)), `${argv.join(" ")} was accepted`);
  }
  assert.ok(!isUsageError(parseArgs(["--threshold", "1"])), "1.0 is a valid threshold");
});

test("parseArgs rejects unknown options and a second file", () => {
  assert.ok(isUsageError(parseArgs(["--nope"])));
  assert.ok(isUsageError(parseArgs(["a.log", "b.log"])));
});

test("parseArgs never throws", () => {
  // Total by contract: a missing option value must become a usage error rather
  // than a crash on `Number(undefined)`.
  for (const argv of [["--top"], ["--threshold"], ["--year"], ["--"]]) {
    assert.doesNotThrow(() => parseArgs(argv));
  }
});

// ---------------------------------------------------------------------------
// Running
// ---------------------------------------------------------------------------

test("--help exits zero and prints usage", async () => {
  const f = fixture([]);
  assert.equal(await runPatterns(["--help"], f.deps), 0);
  assert.match(f.out(), /usage: bough patterns/);
});

test("a usage error exits 2 and explains itself", async () => {
  const f = fixture([]);
  assert.equal(await runPatterns(["--top", "0"], f.deps), 2);
  assert.match(f.err(), /--top needs a positive integer/);
});

test("an unreadable input exits 1", async () => {
  const f = fixture([]);
  assert.equal(await runPatterns(["missing.log"], f.deps), 1);
  assert.match(f.err(), /cannot read missing\.log/);
});

test("an empty log is not an error", async () => {
  // Pointing this at an empty log file is ordinary, and a non-zero exit would
  // break the pipelines that do it.
  const f = fixture([]);
  assert.equal(await runPatterns([], f.deps), 0);
  assert.match(f.err(), /no log lines found/);
});

test("finding errors does not change the exit code", async () => {
  // Whether an ERROR line is a failure is a question about the caller's intent.
  const f = fixture(sampleLog());
  assert.equal(await runPatterns(["--llm"], f.deps), 0);
  assert.match(f.out(), /ERROR/);
});

// ---------------------------------------------------------------------------
// Format selection
// ---------------------------------------------------------------------------

test("the default format follows the consumer", async () => {
  // Off a terminal something else is reading, and that something is far more often
  // a model than a person running `less`.
  const piped = fixture(sampleLog(), false);
  await runPatterns([], piped.deps);
  assert.match(piped.out(), /^# \d+ lines/, "piped output was not the llm format");

  const tty = fixture(sampleLog(), true);
  await runPatterns([], tty.deps);
  assert.match(tty.out(), /lines → \d+ patterns/);
});

test("--no-color suppresses ANSI on a terminal", async () => {
  const plain = fixture(sampleLog(), true);
  await runPatterns(["--no-color"], plain.deps);
  assert.ok(!plain.out().includes("["), "ANSI survived --no-color");

  const coloured = fixture(sampleLog(), true);
  await runPatterns([], coloured.deps);
  assert.ok(coloured.out().includes("["), "a terminal got no colour");
});

test("--json emits parseable output matching the Analysis shape", async () => {
  const f = fixture(sampleLog());
  assert.equal(await runPatterns(["--json"], f.deps), 0);
  const parsed = JSON.parse(f.out());
  assert.equal(parsed.lines, 65);
  assert.ok(parsed.patternCount >= 2);
  assert.ok(Array.isArray(parsed.patterns));
  assert.equal(typeof parsed.truncated, "boolean");
  const p = parsed.patterns[0];
  assert.ok(typeof p.template === "string" && p.template.length > 0);
  assert.ok(Array.isArray(p.vars));
});

test("--json on an empty log still emits an object", async () => {
  // A consumer parsing stdout must not have to special-case the empty file.
  const f = fixture([]);
  await runPatterns(["--json"], f.deps);
  assert.equal(JSON.parse(f.out()).lines, 0);
});

// ---------------------------------------------------------------------------
// What the output says
// ---------------------------------------------------------------------------

test("the llm view leads with problems", async () => {
  // A model weights early content more heavily, so spending the first position on
  // the 92%-of-traffic INFO pattern wastes the most valuable slot in the context.
  const f = fixture(sampleLog());
  await runPatterns(["--llm"], f.deps);
  const text = f.out();
  const problems = text.indexOf("## Problems");
  const rest = text.indexOf("## Everything else");
  assert.ok(problems >= 0, "no problems section");
  assert.ok(rest > problems, "the INFO pattern was rendered above the errors");
});

test("no output format advertises anything", async () => {
  // The llm view lands in a context window on every invocation; a line that is not
  // about the log displaces one that is.
  for (const flag of ["--llm", "--human", "--json"]) {
    const f = fixture(sampleLog(), flag === "--human");
    await runPatterns([flag, "--no-color"], f.deps);
    assert.doesNotMatch(f.out(), /powered by|\.ai\b|learn more|https?:\/\//i, `${flag} carried a footer`);
  }
});

test("--top truncates the rendering but not the count", async () => {
  const f = fixture(sampleLog());
  await runPatterns(["--json", "--top", "1"], f.deps);
  const parsed = JSON.parse(f.out());
  assert.equal(parsed.patterns.length, 1);
  assert.ok(parsed.patternCount > 1, "patternCount was truncated along with the rendering");
});

test("the analysis compresses and reports its own reduction honestly", async () => {
  const f = fixture(sampleLog());
  await runPatterns(["--json"], f.deps);
  const parsed = JSON.parse(f.out());
  // 65 lines of two statements must not become 65 patterns.
  assert.ok(parsed.patternCount <= 4, `65 lines produced ${parsed.patternCount} patterns`);
  const totals = parsed.patterns.reduce((s: number, p: { count: number }) => s + p.count, 0);
  assert.equal(totals, 65, "counts do not add up to the lines read");
});

test("a log with no timestamps analyzes without a span", async () => {
  // Build output and stack traces have none, and they still cluster.
  const f = fixture(["make: entering dir /a", "make: entering dir /b", "cc -o x x.c"]);
  assert.equal(await runPatterns(["--json"], f.deps), 0);
  const parsed = JSON.parse(f.out());
  assert.equal(parsed.timeSpan, undefined);
  assert.equal(parsed.lines, 3);
});

test("blank lines are skipped rather than clustered", async () => {
  const f = fixture(["INFO a", "", "   ", "INFO b"]);
  await runPatterns(["--json"], f.deps);
  assert.equal(JSON.parse(f.out()).lines, 2);
});
