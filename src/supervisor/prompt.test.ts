import { assert, assertEquals, assertStringIncludes } from "jsr:@std/assert@1";
import { decomposableRequest, delegationHintNote } from "./prompt.ts";

// ---- decomposableRequest: synthetic shapes ---------------------------------

Deno.test("decomposableRequest: fires on the decomposable shapes", () => {
  const positives = [
    // survey verb + distributive marker in one sentence
    "Audit each of the three services for PII logging",
    "Research error handling patterns across our repos and summarize",
    "Review every module in src/ for unchecked casts",
    // count + independence adjective in one sentence
    "We have four independent modules that all fail their tests",
    "There are three separate config loaders that need the same fix",
    // "each … its own"
    "Each package has its own lint failures right now",
    // explicit parallel intent
    "Run the lint fixes and the doc update in parallel",
    "Fix mod_a and mod_b concurrently, they don't overlap",
    // a bundle of independent questions
    "What sets the port? How does auth refresh work? Where is retry configured?",
  ];
  for (const text of positives) {
    assert(decomposableRequest(text), `should fire: ${text}`);
  }
});

Deno.test("decomposableRequest: stays quiet on cohesive requests", () => {
  const negatives = [
    "hi",
    "Fix the bug in the implementation so all tests pass",
    // cohesive rename — "every"/"across" without a survey verb must not fire
    "Rename calc to compute_total, updating every caller across the repo",
    // incidental independence adjective without a count (rename-precise decoy)
    "Everything else stays as it is: the unrelated Report class must not change",
    "Find the root cause and fix it so all tests pass",
    "Refactor the parser module and make the smallest change that fixes it",
    // survey verb without a distributive marker
    "Review this diff before I ship it",
    // one or two questions are a conversation, not a fan-out
    "What does this function do? Is it dead code?",
  ];
  for (const text of negatives) {
    assertEquals(decomposableRequest(text), false, `should NOT fire: ${text}`);
  }
});

// ---- calibration against the full bench task bank --------------------------
// The detector's contract: on the bench it fires on the fanout-* tasks and on
// NOTHING else. This test pins that against the real prompt.md files, so a new
// task or a detector edit that breaks selectivity fails here, not in a sweep.

Deno.test("decomposableRequest: bench bank calibration — fanout-* only", async () => {
  const tasksDir = new URL("../../bench/tasks/", import.meta.url);
  const seen: string[] = [];
  for await (const entry of Deno.readDir(tasksDir)) {
    if (!entry.isDirectory) continue;
    let prompt: string;
    try {
      prompt = await Deno.readTextFile(new URL(`${entry.name}/prompt.md`, tasksDir));
    } catch {
      continue; // task without a prompt.md — nothing to calibrate against
    }
    seen.push(entry.name);
    const expected = entry.name.startsWith("fanout-");
    assertEquals(
      decomposableRequest(prompt),
      expected,
      `${entry.name}: expected decomposableRequest=${expected}`,
    );
  }
  // Sanity: the bank was actually scanned, including both target tasks.
  assert(seen.includes("fanout-bugs") && seen.includes("fanout-heavy"), `scanned: ${seen}`);
  assert(seen.length >= 10, `bank unexpectedly small: ${seen.length}`);
});

// ---- delegationHintNote ----------------------------------------------------

Deno.test("delegationHintNote: empty for cohesive requests", () => {
  assertEquals(delegationHintNote("Fix the failing test in parser.py"), "");
  assertEquals(delegationHintNote(""), "");
});

Deno.test("delegationHintNote: decision rule + literal spawn shape when it fires", () => {
  const note = delegationHintNote("Audit each of the three services for PII logging");
  assertStringIncludes(note, "# Delegation fit");
  // The literal code shape the model can copy: parallel spawn, not serial agent().
  assertStringIncludes(note, "Promise.allSettled(tasks.map((t) => spawn(t)))");
  // The escape hatch that keeps false positives harmless.
  assertStringIncludes(note, "ignore this note and do it yourself");
});
