import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { Thread } from "../src/app";
import type { Row } from "../src/types";

const row = { id: "s1", cwd: "/tmp/x", status: "running" } as unknown as Row;
const at = "2026-01-02T10:00:00Z";
const render = (sending: any, lines: any) => renderToStaticMarkup(<Thread row={row} lines={lines} sending={sending} onSend={async () => null} onAnswer={async () => null}
  onInterrupt={() => {}} onArchive={() => {}} onRename={async () => {}} onModel={() => {}} onEffort={() => {}} onAssign={() => {}}
  onBack={() => {}} onContext={() => {}} onAck={() => {}} projects={[]} busy={false} jump={null} />);

// specs/first_prompt_delivery.fizz, RecordSecond: a first "/" line whose
// child died before reading it never records its command, so cmdLanded
// never lands it. The next message's input claims it by the count rule,
// as it claims a lost prompt; the "/" preview stayed "Sending…" beside a
// running turn until a reload.
test("a lost / first line is claimed by the next message's input", () => {
  const sending = [{ id: "p1", text: "/think medium", after: 0 }, { id: "p2", text: "second", after: 1, steer: true }];
  const lines = [{ seq: 1, at, kind: "meta", text: "" }, { seq: 2, at, kind: "input", text: "second" }];
  const html = render(sending, lines);
  expect(html).not.toContain("/think medium");
  expect(html).not.toContain("turn-sending-state");
});

// A prompt sent before the "/" line claims the first input: the "/" line
// waits for its own command, or an input beyond that one.
test("a / line is not claimed by the input of a prompt sent before it", () => {
  const sending = [{ id: "p1", text: "first", after: 0 }, { id: "p2", text: "/think medium", after: 0 }];
  const lines = [{ seq: 1, at, kind: "meta", text: "" }, { seq: 2, at, kind: "input", text: "first" }];
  expect(render(sending, lines)).toContain("/think medium");
});
