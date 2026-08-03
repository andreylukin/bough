/**
 * Tests for the job view's command line(s).
 *
 * The rule under test: **the full command is visible.** The sub line used to be one
 * truncated row, so anything past the width — the part of a `for` loop that says
 * what it loops over — was simply gone. Now it wraps, the body budget shrinks to
 * match (`jobBodyRows`), and only a genuinely huge command (half the view) is
 * capped, visibly, with an ellipsis.
 */
import { test } from "bun:test";
import assert from "node:assert/strict";
import stripAnsi from "strip-ansi";
import type { BackgroundJob } from "../../schema/parts.ts";
import { width } from "../format.ts";
import { jobBodyRows, jobSubLines } from "./JobOutput.tsx";

function job(command: string): BackgroundJob {
  return {
    id: "job-1",
    name: "dev server",
    sessionId: "s1",
    pid: 4242,
    command,
    status: "running",
    startedAt: 1_700_000_000_000,
  };
}

test("a short command stays on one row", () => {
  const lines = jobSubLines(job("bun test"), "job-1", 80, 20);
  assert.equal(lines.length, 1);
  assert.match(stripAnsi(lines[0]), /job-1 · pid 4242 · bun test/);
});

test("a long command wraps instead of being cut off", () => {
  const cmd = "bun run build --target=node --minify --outdir dist && rsync -av dist/ deploy@host:/srv/app/";
  const lines = jobSubLines(job(cmd), "job-1", 40, 20);
  assert.ok(lines.length > 1);
  for (const l of lines) assert.ok(width(l) <= 40, `row overflows: ${stripAnsi(l)}`);
  // Nothing was lost: the rows joined back together still contain the whole command.
  const joined = lines.map(stripAnsi).join("");
  assert.ok(joined.replace(/\s+/g, "").includes("deploy@host:/srv/app/"));
});

test("a huge command is capped at half the view, with an ellipsis", () => {
  const lines = jobSubLines(job("x".repeat(4000)), "job-1", 40, 20);
  // height 20 → cap = floor((20 - 3) / 2) = 8.
  assert.equal(lines.length, 8);
  assert.ok(stripAnsi(lines[7]).endsWith("…"));
  for (const l of lines) assert.ok(width(l) <= 40);
});

test("body budget shrinks by the rows the command takes", () => {
  assert.equal(jobBodyRows(20), 16); // one sub row — the old height - 4
  assert.equal(jobBodyRows(20, 3), 14);
  assert.equal(jobBodyRows(4, 8), 1); // never less than one row
});
