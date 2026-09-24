import { expect, mock, test } from "bun:test";
// No DOM under bun: markdown sanitising is not what these assert.
mock.module("dompurify", () => ({ default: { sanitize: (s: string) => s } }));
const { streamAfter } = await import("../src/app");

// specs/turn_lifecycle.fizz, rec_fresh: an entry was recorded (its runs
// are marked to go when the refetch lands) and the next entry began
// streaming before that refetch. The new fragment extended the marked
// run, so the refetch dropped it with the old text and the reply's
// beginning vanished until its own entry landed.
test("TL: a fragment after a recorded entry starts a run of its own", () => {
  let runs = streamAfter([], { kind: "assistant-delta", text: "first " });
  const sealed = runs.length; // the recorded entry's event marks what it supersedes
  runs = streamAfter(runs, { kind: "assistant-delta", text: "second" }, sealed);
  expect(runs).toEqual([{ kind: "assistant", text: "first " }, { kind: "assistant", text: "second" }]);
  // What the refetch keeps once it drops the sealed runs.
  expect(runs.slice(sealed)).toEqual([{ kind: "assistant", text: "second" }]);
  // Unsealed runs still grow in place.
  expect(streamAfter(runs, { kind: "assistant-delta", text: " more" }, sealed)).toEqual([{ kind: "assistant", text: "first " }, { kind: "assistant", text: "second more" }]);
});
