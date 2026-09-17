import { expect, test } from "bun:test";
import { jobWakeNotes } from "../src/work";

const wake = "[background job] A command you started in the background has finished while you were idle. Deal with it if it needs anything, then reply to the user with what happened.\n\n";

test("a typed prompt is not a job wake-up", () => {
  expect(jobWakeNotes("run the tests")).toBeNull();
  expect(jobWakeNotes("job 1 [exited 0] ls (0s)")).toBeNull();
});

test("a wake-up yields its job notes, output kept with each", () => {
  const text = wake + 'job 8 [failed] # bash "$BOUGH_SCRATCH/orb-setup.sh" … (1m37s): exit status 137\n\n\njob 9 [exited 0] go test ./... (4s)\nok  pkg\t0.1s\n';
  expect(jobWakeNotes(text)).toEqual([
    'job 8 [failed] # bash "$BOUGH_SCRATCH/orb-setup.sh" … (1m37s): exit status 137',
    "job 9 [exited 0] go test ./... (4s)\nok  pkg\t0.1s",
  ]);
});

// R2-D: an agent's finish note wakes the parent with the same prefix, but carries no job notes.
test("an agent wake-up yields no job notes and its agent notes", async () => {
  const { agentWakeNotes } = await import("../src/work");
  const text = wake + "[agent List files · 01a0 finished] Wrote COUNTS.md\n\n[agent Lint · 02b1 failed] boom";
  expect(jobWakeNotes(text)).toEqual([]);
  expect(agentWakeNotes(text)).toEqual(["[agent List files · 01a0 finished] Wrote COUNTS.md", "[agent Lint · 02b1 failed] boom"]);
  expect(agentWakeNotes("run the tests")).toBeNull();
});

test("a mixed wake-up keeps agent notes out of the job's output", async () => {
  const { agentWakeNotes } = await import("../src/work");
  const text = wake + "job 3 [exited 0] make (1s)\nok\n[agent Lint · 02b1 finished] clean";
  expect(jobWakeNotes(text)).toEqual(["job 3 [exited 0] make (1s)\nok"]);
  expect(agentWakeNotes(text)).toEqual(["[agent Lint · 02b1 finished] clean"]);
});
