import { expect, mock, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
// No DOM under bun: markdown sanitising is not what these assert.
mock.module("dompurify", () => ({ default: { sanitize: (s: string) => s } }));
const { NativeCall, ToolRun, TurnView, liveNative, streamAfter, withRunningCalls, wakeLabel } = await import("../src/app");
const { groupTools, groupTurns, splitWork, isNativeCall, isQuiet } = await import("../src/render");
const { nativeEdits } = await import("../src/changes");
import type { Line } from "../src/types";

// An engine session records a native call as one "call" entry with the
// provider's call id (a string) and no code block around it (§10.1).
const t0 = Date.parse("2026-01-02T10:00:00Z");
const at = (s: number) => new Date(t0 + s * 1000).toISOString();
const call = (seq: number, s: number, tool: string, text: string, data: Record<string, unknown> = {}): Line =>
  ({ seq, at: at(s), kind: "call", text, data: { id: "toolu_" + seq, tool, ms: 1200, ...data } });

const input: Line = { seq: 1, at: at(0), kind: "input", text: "Fix the test." };
const view = call(2, 1, "view", "a.go", { output: "package a" });
const patch = call(3, 2, "patch", "a.go", { add: 2, del: 1, output: "patched a.go" });
const test1 = call(4, 9, "bash", "go test ./...", { exit: 1, cmd: "go test ./...", error: "exit status 1", output: "--- FAIL: TestA\nFAIL\tpkg" });

test("ER: a native call is told from a loop block's call by its string id", () => {
  expect(isNativeCall(view)).toBe(true);
  expect(isNativeCall({ ...view, data: { ...view.data, id: 1 } })).toBe(false);
  expect(isQuiet("engine")).toBe(true);
});

test("ER: native calls with no code parent group into one tools item", () => {
  const items = groupTools([view, patch, test1].map((l) => ({ kind: "line" as const, seq: l.seq, line: l })), []);
  expect(items.length).toBe(1);
  expect(items[0].kind).toBe("tools");
});

test("ER: splitWork counts each native call as an action and its failure", () => {
  const items = groupTools([view, patch, test1].map((l) => ({ kind: "line" as const, seq: l.seq, line: l })), []);
  const [seg] = splitWork(items, [], false);
  if (seg.kind !== "work") throw new Error("want a work segment");
  expect(seg.actions).toBe(3);
  expect(seg.failed).toBe(1);
  // Two or more calls are one header row (the walker wraps them), as blocks are.
  expect(seg.rows).toBe(1);
  expect(seg.step).toBe("Ran go test ./...");
});

test("ER: the walker renders a run of native calls as rows under one header, named from their records", () => {
  const html = renderToStaticMarkup(<ToolRun lines={[view, patch, test1]} codes={[]} />);
  expect((html.match(/class="block thin toolcall call-native/g) ?? []).length).toBe(3);
  expect(html).toContain('<span class="block-label">Edited a.go</span>');
  expect(html).toContain('<span class="num rt-add">+2</span>');
  expect(html).toContain("read 1 file · ran 1 command");
  expect(html).toContain("3 calls · 4s");
  expect(html).toContain("1 failed");
  // Two commands of one kind read as that verb and the first target.
  const two = renderToStaticMarkup(<ToolRun lines={[call(5, 1, "bash", "ls"), call(6, 2, "bash", "pwd")]} codes={[]} />);
  expect(two).toContain('<span class="block-label">Ran</span>');
  expect(two).toContain('title="ls">ls</span>');
});

test("ER: a finished call opens onto its output; its evidence and what happened to it are on the line", () => {
  const html = renderToStaticMarkup(<NativeCall line={test1} />);
  expect(html).toContain("block-failed");
  expect(html).toContain('<span class="block-label">Ran</span>');
  expect(html).toContain('class="tool-thrown"');
  expect(html).toContain('<span class="tool-meta-failed">exit 1</span>');
  expect(html).toContain("--- FAIL: TestA");
  expect(html).toContain(`data-seq="${test1.seq}"`);
  const bg = renderToStaticMarkup(<NativeCall line={call(7, 1, "bash", "make serve", { job: 3, adopted: true, late: true, output: "up", truncated: true })} />);
  expect(bg).toContain(">Job 3</span>");
  expect(bg).toContain(">Late</span>");
  expect(bg).toContain("Output shortened here");
  const cancelled = renderToStaticMarkup(<NativeCall line={call(8, 1, "bash", "sleep 100", { canceled: true, error: "cancelled by the user", output: "Cancelled: cancelled by the user" })} />);
  expect(cancelled).toContain("Cancelled</span>");
  expect(cancelled).not.toContain("block-failed");
});

test("ER: a running call spins with its live tail, and gives way to its record by id", () => {
  let m = liveNative(new Map(), { session: "s", seq: 1, at: at(3), kind: "call", text: "go test ./...", extra: { id: "toolu_9", tool: "bash", phase: "start" } });
  m = liveNative(m, { session: "s", seq: 0, at: at(4), kind: "call-delta", text: "ok a\nok b\nok c\nrunning d\n", extra: { id: "toolu_9" } });
  // A delta for a call it never saw start changes nothing.
  expect(liveNative(m, { session: "s", seq: 0, at: at(4), kind: "call-delta", text: "x", extra: { id: "nope" } })).toBe(m);
  const shown = withRunningCalls([input], m);
  expect(shown.length).toBe(2);
  expect(shown[1].data?.phase).toBe("start");
  const html = renderToStaticMarkup(<TurnView turn={groupTurns(shown)[0]} />);
  expect(html).toContain('<span class="block-label">Running</span>');
  expect(html).toContain("spin-mark");
  expect(html).toContain('<pre class="mono call-tail" aria-live="off">ok b\nok c\nrunning d</pre>');
  expect(html).not.toContain("ok a");
  // Recorded: the running row is gone, whatever the map still holds.
  const done = withRunningCalls([input, call(10, 9, "bash", "go test ./...", { id: "toolu_9", exit: 0 })], m);
  expect(done.length).toBe(2);
  expect(done[1].data?.phase).toBeUndefined();
  // The turn's done clears the map: a call that ran on is a job, in Work.
  expect(liveNative(m, { session: "s", seq: 2, at: at(9), kind: "done", text: "" }).size).toBe(0);
});

test("ER: delta-reset clears the streamed text a retry replaced", () => {
  let runs = streamAfter([], { kind: "thinking-delta", text: "hm" });
  runs = streamAfter(runs, { kind: "assistant-delta", text: "The fix is" });
  expect(runs.length).toBe(2);
  expect(streamAfter(runs, { kind: "delta-reset", text: "" })).toEqual([]);
  expect(streamAfter(streamAfter(runs, { kind: "delta-reset", text: "" }), { kind: "assistant-delta", text: "Again" })).toEqual([{ kind: "assistant", text: "Again" }]);
});

test("ER: a wake turn opens on a quiet line, holding the call that finished, not a user bubble", () => {
  const bg = call(20, 30, "bash", "make build", { exit: 0, job: 1, adopted: true, output: "built" });
  const wake: Line = { seq: 21, at: at(31), kind: "input", text: "[call toolu_20 finished]", data: { wake: true, reason: "call", calls: ["toolu_20"] } };
  const lines: Line[] = [
    input, { seq: 2, at: at(1), kind: "engine", text: "", data: { engine: "unreal" } },
    { seq: 3, at: at(2), kind: "done", text: "", data: { running: 1 } },
    bg, wake,
    { seq: 22, at: at(32), kind: "assistant", text: "The build finished." },
    { seq: 23, at: at(33), kind: "done", text: "", data: { wake: true } },
  ];
  const turns = groupTurns(lines);
  expect(turns.length).toBe(2);
  expect(turns[0].body.some((l) => l.kind === "engine")).toBe(false);
  expect(turns[1].prompt?.seq).toBe(21);
  expect(turns[1].body[0].seq).toBe(20);
  const html = renderToStaticMarkup(<TurnView turn={turns[1]} />);
  expect(html).toContain("A background call finished");
  expect(html).not.toContain("prompt-bubble");
  expect(html).toContain("make build");
  expect(wakeLabel({ ...wake, data: { wake: true, reason: "heartbeat" } })).toBe("The agent checked on its running calls");
  // The call has since reported, so the turn that left it running no
  // longer says it still runs.
  expect(renderToStaticMarkup(<TurnView turn={turns[0]} />)).not.toContain("still running");
  // Until it reports, the footer says so; its job line names the call.
  const job: Line = { seq: 2, at: at(1), kind: "job", text: "job 1: make build", data: { id: 1, event: "started", cmd: "make build", call: "toolu_20" } };
  const early = groupTurns([input, job, { seq: 3, at: at(2), kind: "done", text: "", data: { running: 1 } }]);
  expect(renderToStaticMarkup(<TurnView turn={early[0]} />)).toContain("1 call still running");
  const later = groupTurns([input, job, { seq: 3, at: at(2), kind: "done", text: "", data: { running: 1 } }, bg]);
  expect(later[0].settled).toBe(1);
  expect(renderToStaticMarkup(<TurnView turn={later[0]} />)).not.toContain("still running");
});

test("ER: a turn that ended on a failed native call opens it and names it in the footer", () => {
  const lines: Line[] = [input, view, patch, test1, { seq: 5, at: at(10), kind: "assistant", text: "The test still fails." },
    { seq: 6, at: at(11), kind: "done", text: "", data: { files: ["a.go"] } }];
  const html = renderToStaticMarkup(<TurnView turn={groupTurns(lines)[0]} />);
  expect(html).toMatch(new RegExp(`<details class="block thin toolcall call-native block-failed" data-seq="${test1.seq}" open`));
  expect(html).toContain("TestA failed · exit 1");
  // The footer names the model from the reply's provenance when the done does not.
  const said = [input, { seq: 2, at: at(1), kind: "assistant", text: "Done.", data: { model: "anthropic/claude-opus-5-5", provider: "llm-anthropic" } }, { seq: 3, at: at(2), kind: "done", text: "" }];
  expect(renderToStaticMarkup(<TurnView turn={groupTurns(said)[0]} />)).toContain('title="anthropic/claude-opus-5-5">claude-opus-5-5</span>');
  // The files chip counts the native patch's lines.
  expect(html).toMatch(/1 file<span class="num rt-add">\+2<\/span><span class="num rt-del">−1<\/span>/);
  // A later success is not a failure to point at.
  const fixed = [input, test1, call(7, 12, "bash", "go test ./...", { exit: 0, output: "ok" }), { seq: 8, at: at(13), kind: "done", text: "" }];
  expect(renderToStaticMarkup(<TurnView turn={groupTurns(fixed)[0]} />)).not.toContain("failed · exit");
});

test("ER: native edits come from the records, skipping failed and running calls", () => {
  const failed = call(9, 3, "write", "b.go", { error: "denied" });
  const running = { ...call(10, 3, "write", "c.go"), data: { id: "x", tool: "write", phase: "start" } };
  expect(nativeEdits([view, patch, failed, running, call(11, 4, "write", "new.go", { add: 5, path: "/w/new.go" })]))
    .toEqual([{ path: "a.go", add: 2, del: 1, hunks: [] }, { path: "/w/new.go", add: 5, del: 0, hunks: [] }]);
});
