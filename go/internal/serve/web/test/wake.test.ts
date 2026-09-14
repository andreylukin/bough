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
