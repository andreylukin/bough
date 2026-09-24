import { expect, test } from "bun:test";
import { requeue } from "../src/app";

// specs/queue_vs_refusal.fizz RetryWaitsItsTurn: a retried message goes
// back in the queue ahead of every younger message, never straight to
// serve (posted at once, it steered a turn a younger message started).
test("a retried message rejoins the queue in its place", () => {
  const m = (at: number) => ({ id: `${at}-0.5`, text: `m${at}` });
  expect(requeue([m(20), m(30)], m(10)).map((x) => x.text)).toEqual(["m10", "m20", "m30"]);
  expect(requeue([m(10), m(30)], m(20)).map((x) => x.text)).toEqual(["m10", "m20", "m30"]);
  expect(requeue([m(10)], m(20)).map((x) => x.text)).toEqual(["m10", "m20"]);
  expect(requeue([], m(20)).map((x) => x.text)).toEqual(["m20"]);
});

test("a retried direct send (no enqueue time in its id) goes first", () => {
  const q = [{ id: "20-0.5", text: "queued" }];
  expect(requeue(q, { id: "3f2a9c1e-uuid", text: "sent" }).map((x) => x.text)).toEqual(["sent", "queued"]);
});
