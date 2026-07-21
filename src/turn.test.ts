import { assert, assertEquals, assertExists, assertStringIncludes } from "jsr:@std/assert@1";
import { z } from "zod/v4";
import { Db } from "./db/db.ts";
import { Bus } from "./bus.ts";
import type { AskQuestion, BoughEvent, Message, Part, Session } from "./schema/parts.ts";
import { answerAsk, declineAsk, pendingAsks } from "./asks.ts";
import type { LlmClient, LlmMessage, LlmParams, LlmResult } from "./supervisor/llm.ts";
import type { ToolDef, ToolRunCtx } from "./tools/mod.ts";
import { beginTurn, interruptTurn, isTurnRunning, startUserTurn, type TurnCtx } from "./turn.ts";
import { recoverOrphanedTurns } from "./supervisor/turns.ts";

// ---- harness ---------------------------------------------------------------

function seed(): { db: Db; bus: Bus; events: BoughEvent[]; sessionId: string } {
  const db = new Db(":memory:");
  const bus = new Bus();
  const events: BoughEvent[] = [];
  bus.subscribe((e) => events.push(e));
  const s: Session = { id: "s1", parentId: null, title: "s1", kind: "root", createdAt: 1 };
  db.createSession(s);
  db.createMessage({
    id: "u1",
    sessionId: s.id,
    role: "user",
    parts: [{ type: "text", text: "hi" }],
    pending: false,
    createdAt: 2,
  });
  return { db, bus, events, sessionId: s.id };
}

/** A scripted client: one LlmResult per round, plus the params it was called with.
 * An exhausted script answers the harness's stop-nudge with a stop call — the
 * compliant-model reply — so scripts keep reading as "…and then the turn ends". */
function fakeLlm(script: LlmResult[]): LlmClient & { calls: LlmParams[] } {
  let i = 0;
  const calls: LlmParams[] = [];
  return {
    calls,
    run(params: LlmParams, onText: (d: string) => void): Promise<LlmResult> {
      calls.push(params);
      const result: LlmResult = script[i++] ?? {
        content: [{ type: "tool_use", id: `stop-${i}`, name: "stop", input: {} }],
        stopReason: "tool_use",
      };
      for (const block of result.content) {
        if (block.type === "text") onText(block.text);
      }
      return Promise.resolve(result);
    },
  };
}

function finalMessage(db: Db, id: string): Message {
  const m = db.getMessage(id);
  assertExists(m);
  return m;
}

function eventTypes(events: BoughEvent[], id: string): string[] {
  return events
    .filter((e) =>
      (e.data as { messageId?: string }).messageId === id || (e.data as Message).id === id
    )
    .map((e) => e.type);
}

// ---- tests -----------------------------------------------------------------

Deno.test("text-only turn streams, persists a text part, and finishes", async () => {
  const { db, bus, events, sessionId } = seed();
  const llm = fakeLlm([{
    content: [{ type: "text", text: "hello world" }],
    stopReason: "end_turn",
  }]);
  const ctx: TurnCtx = { db, bus, llm, tools: [] };

  const { message, done } = beginTurn(ctx, sessionId);
  await done;

  const final = finalMessage(db, message.id);
  assertEquals(final.pending, false);
  assertEquals(final.parts, [{ type: "text", text: "hello world" }] as Part[]);

  const deltas = events.filter((e) => e.type === "message.delta").map((e) =>
    (e.data as { delta: string }).delta
  );
  assertEquals(deltas.join(""), "hello world");
  const types = eventTypes(events, message.id);
  assertEquals(types.includes("message.started"), true);
  assertEquals(types.includes("message.finished"), true);
  // turn.finished carries how the turn ended (feeds the UI's status affixes).
  const finished = events.find((e) => e.type === "turn.finished");
  assertEquals((finished?.data as { status: string }).status, "done");
  assertEquals(db.turnsByStatus("done").length, 1);
});

Deno.test("interruptTurn stops an in-flight turn cleanly (marked, not failed)", async () => {
  const { db, bus, sessionId } = seed();
  // A client whose round never resolves on its own — it only rejects when the
  // abort signal fires, standing in for a long streaming request.
  const hanging: LlmClient = {
    run(_p: LlmParams, _t: (d: string) => void, signal?: AbortSignal): Promise<LlmResult> {
      return new Promise((_resolve, reject) => {
        const fail = () => reject(new DOMException("aborted", "AbortError"));
        if (signal?.aborted) return fail();
        signal?.addEventListener("abort", fail);
      });
    },
  };
  const ctx: TurnCtx = { db, bus, llm: hanging, tools: [] };

  const { message, done } = beginTurn(ctx, sessionId);
  // Let drive() enter the LLM call and register the abort listener.
  await new Promise((r) => setTimeout(r, 0));
  assertEquals(isTurnRunning(sessionId), true);

  assertEquals(interruptTurn(sessionId), true);
  await done;

  const final = finalMessage(db, message.id);
  assertEquals(final.pending, false);
  assertStringIncludes((final.parts.at(-1) as { text: string }).text, "Stopped");
  assertEquals(db.turnsByStatus("interrupted").length, 1);
  assertEquals(db.turnsByStatus("error").length, 0);
  assertEquals(isTurnRunning(sessionId), false);
  // Interrupting an idle session is a no-op, not an error.
  assertEquals(interruptTurn(sessionId), false);
});

Deno.test("tool-call turn runs a tool, appends the result, and loops to completion", async () => {
  const { db, bus, sessionId } = seed();
  let ran = "";
  const fakeBash: ToolDef = {
    name: "bash",
    description: "fake bash",
    schema: z.object({ command: z.string() }),
    run: (input) => {
      ran = (input as { command: string }).command;
      return Promise.resolve("ok: ran");
    },
  };
  const llm = fakeLlm([
    {
      content: [{ type: "tool_use", id: "t1", name: "bash", input: { command: "echo hi" } }],
      stopReason: "tool_use",
    },
    { content: [{ type: "text", text: "done" }], stopReason: "end_turn" },
  ]);
  const ctx: TurnCtx = { db, bus, llm, tools: [fakeBash] };

  const { message, done } = beginTurn(ctx, sessionId);
  await done;

  assertEquals(ran, "echo hi");
  const final = finalMessage(db, message.id);
  assertEquals(final.pending, false);
  assertEquals(final.parts, [
    { type: "tool_call", id: "t1", name: "bash", input: { command: "echo hi" } },
    { type: "tool_result", callId: "t1", output: "ok: ran", isError: false },
    { type: "text", text: "done" },
  ] as Part[]);

  // Round 2 saw the assistant tool_use and the user tool_result we fed back.
  const round2 = llm.calls[1].messages;
  const assistant = round2.find((m: LlmMessage) =>
    m.role === "assistant" && m.content.some((b) => b.type === "tool_use")
  );
  const results = round2.find((m: LlmMessage) => m.content.some((b) => b.type === "tool_result"));
  assertExists(assistant);
  assertExists(results);
});

Deno.test("a tool's onLog streams tool.log events keyed to the running call", async () => {
  const { db, bus, events, sessionId } = seed();
  // A stand-in for run_steps: emits two console lines via ctx.onLog mid-run,
  // exactly as runProgram's onLog callback does per printed line.
  const fakeSteps: ToolDef = {
    name: "run_steps",
    description: "fake run_steps",
    schema: z.object({ code: z.string() }),
    run: (_input, ctx) => {
      ctx.onLog?.("first line");
      ctx.onLog?.("second line");
      return Promise.resolve("first line\nsecond line");
    },
  };
  const llm = fakeLlm([
    {
      content: [{
        type: "tool_use",
        id: "t1",
        name: "run_steps",
        input: { code: "console.log()" },
      }],
      stopReason: "tool_use",
    },
    { content: [{ type: "text", text: "done" }], stopReason: "end_turn" },
  ]);
  const { message, done } = beginTurn({ db, bus, llm, tools: [fakeSteps] }, sessionId);
  await done;

  const logs = events
    .filter((e) => e.type === "tool.log")
    .map((e) => e.data as { messageId: string; callId: string; line: string });
  assertEquals(logs.map((l) => l.line), ["first line", "second line"]);
  // Keyed to the executing tool_call part (t1), not the fake id the tool passed
  // as a placeholder — the turn runner stamps the real call id.
  assert(logs.every((l) => l.callId === "t1"), "log events carry the tool_call id");
  assert(logs.every((l) => l.messageId === message.id), "log events carry the message id");
  // And the turn still completed with the batched output as the tool_result.
  const final = finalMessage(db, message.id);
  assertStringIncludes(
    (final.parts.find((p) => p.type === "tool_result") as { output: string }).output,
    "first line",
  );
});

Deno.test("a session pinned to a model runs its turns on it; unpinned follows the default", async () => {
  const { db, bus, sessionId } = seed();
  db.setSessionModel(sessionId, "claude-haiku-4-5");
  const llm = fakeLlm([{
    content: [
      { type: "text", text: "hi" },
      { type: "tool_use", id: "s", name: "stop", input: {} },
    ],
    stopReason: "tool_use",
  }]);
  const { done } = beginTurn({ db, bus, llm, tools: [] }, sessionId);
  await done;
  assertEquals(llm.calls[0].model, "claude-haiku-4-5");
});

Deno.test("a turn that trails off without stop is nudged; nudge + stop never persist", async () => {
  const { db, bus, sessionId } = seed();
  const llm = fakeLlm([
    // Trails off — the harness must re-prompt instead of ending the turn.
    { content: [{ type: "text", text: "half done." }], stopReason: "end_turn" },
    // The re-prompted round finishes compliantly.
    {
      content: [
        { type: "text", text: "all done." },
        { type: "tool_use", id: "s", name: "stop", input: {} },
      ],
      stopReason: "tool_use",
    },
  ]);
  const { message, done } = beginTurn({ db, bus, llm, tools: [] }, sessionId);
  await done;

  assertEquals(llm.calls.length, 2); // exactly one nudge round
  // The nudge reached the model as an in-memory user message (the fake aliases
  // the live messages array, so check content, not final position).
  assertStringIncludes(
    JSON.stringify(llm.calls[1].messages.filter((m) => m.role === "user")),
    "[harness]",
  );
  // …but neither it nor the stop call landed in the persisted thread.
  const final = finalMessage(db, message.id);
  assertEquals(final.parts, [
    { type: "text", text: "half done." },
    { type: "text", text: "all done." },
  ] as Part[]);
  assertEquals(
    db.threadFor(sessionId).some((m) => JSON.stringify(m.parts).includes("[harness]")),
    false,
  );
  assertEquals(db.turnsByStatus("done").length, 1);
});

Deno.test("a stop with no text this turn is nudged for a report first", async () => {
  const { db, bus, sessionId } = seed();
  const llm = fakeLlm([
    // Mute ending: stop alone, nothing user-visible said — must not end the turn.
    {
      content: [{ type: "tool_use", id: "s1", name: "stop", input: {} }],
      stopReason: "tool_use",
    },
    {
      content: [
        { type: "text", text: "the answer." },
        { type: "tool_use", id: "s2", name: "stop", input: {} },
      ],
      stopReason: "tool_use",
    },
  ]);
  const { message, done } = beginTurn({ db, bus, llm, tools: [] }, sessionId);
  await done;

  assertEquals(llm.calls.length, 2); // exactly one report nudge round
  assertStringIncludes(
    JSON.stringify(llm.calls[1].messages.filter((m) => m.role === "user")),
    "user-visible text",
  );
  const final = finalMessage(db, message.id);
  assertEquals(final.parts, [{ type: "text", text: "the answer." }] as Part[]);
  assertEquals(db.turnsByStatus("done").length, 1);
});

Deno.test("an accepted done-check with no text is nudged for a report before ending", async () => {
  const { db, bus, sessionId } = seed();
  const fakeSteps: ToolDef = {
    name: "run_steps",
    description: "fake run_steps",
    schema: z.object({ program: z.string(), done: z.boolean().optional() }),
    run: () => Promise.resolve("ok\n[done] accepted — check passed"),
  };
  const llm = fakeLlm([
    // Work round: check-gated done accepted, but the model said nothing.
    {
      content: [{
        type: "tool_use",
        id: "t1",
        name: "run_steps",
        input: { program: "…", done: true },
      }],
      stopReason: "tool_use",
    },
    {
      content: [
        { type: "text", text: "shipped; check passed." },
        { type: "tool_use", id: "s", name: "stop", input: {} },
      ],
      stopReason: "tool_use",
    },
  ]);
  const { message, done } = beginTurn({ db, bus, llm, tools: [fakeSteps] }, sessionId);
  await done;

  assertEquals(llm.calls.length, 2);
  const final = finalMessage(db, message.id);
  assertEquals(final.parts.at(-1), { type: "text", text: "shipped; check passed." } as Part);
  // A repeat-mute model still ends after the single report nudge (no loop): covered
  // by llm.calls length above — one extra round only.
  assertEquals(db.turnsByStatus("done").length, 1);
});

Deno.test("a mute nudge round escalates to a forced text-only round", async () => {
  const { db, bus, sessionId } = seed();
  const stopAlone = {
    content: [{ type: "tool_use", id: "s", name: "stop", input: {} }],
    stopReason: "tool_use",
  } as LlmResult;
  const llm = fakeLlm([
    stopAlone, // mute end attempt → report nudge
    stopAlone, // ignores the nudge → forced round (tools off)
    { content: [{ type: "text", text: "forced report." }], stopReason: "end_turn" },
  ]);
  const { message, done } = beginTurn({ db, bus, llm, tools: [] }, sessionId);
  await done;

  assertEquals(llm.calls.length, 3);
  assertEquals(llm.calls[2].toolChoice, "none"); // the last round forbids tools
  const final = finalMessage(db, message.id);
  assertEquals(final.parts, [{ type: "text", text: "forced report." }] as Part[]);
  assertEquals(db.turnsByStatus("done").length, 1);
});

Deno.test("a model that never calls stop hits the nudge cap instead of looping forever", async () => {
  const { db, bus, sessionId } = seed();
  // Index-based script: every round trails off; exhaustion would return the
  // auto-stop, so script enough trailing rounds to outlast the cap (1 + 3 nudges).
  const trail = { content: [{ type: "text", text: "…" }], stopReason: "end_turn" } as LlmResult;
  const llm = fakeLlm([trail, trail, trail, trail, trail, trail]);
  const { done } = beginTurn({ db, bus, llm, tools: [] }, sessionId);
  await done;

  assertEquals(llm.calls.length, 4); // initial round + MAX_STOP_NUDGES retries
  assertEquals(db.turnsByStatus("done").length, 1); // still ends cleanly
});

Deno.test("multi-round loop executes tools across several rounds", async () => {
  const { db, bus, sessionId } = seed();
  let calls = 0;
  const counter: ToolDef = {
    name: "count",
    description: "counts",
    schema: z.object({}),
    run: () => {
      calls++;
      return Promise.resolve(`call ${calls}`);
    },
  };
  const llm = fakeLlm([
    { content: [{ type: "tool_use", id: "a", name: "count", input: {} }], stopReason: "tool_use" },
    { content: [{ type: "tool_use", id: "b", name: "count", input: {} }], stopReason: "tool_use" },
    // The compliant ending: final text + stop in ONE response — no extra round.
    {
      content: [
        { type: "text", text: "fin" },
        { type: "tool_use", id: "s", name: "stop", input: {} },
      ],
      stopReason: "tool_use",
    },
  ]);
  const { message, done } = beginTurn({ db, bus, llm, tools: [counter] }, sessionId);
  await done;

  assertEquals(calls, 2);
  assertEquals(llm.calls.length, 3);
  const final = finalMessage(db, message.id);
  assertEquals(final.parts.filter((p) => p.type === "tool_result").length, 2);
  // The stop call is loop control — it must never persist into the thread.
  assertEquals(final.parts.some((p) => p.type === "tool_call" && p.name === "stop"), false);
  assertEquals(final.pending, false);
});

Deno.test("a failing API call surfaces an error part and marks the turn error", async () => {
  const { db, bus, events, sessionId } = seed();
  const llm: LlmClient = { run: () => Promise.reject(new Error("boom")) };
  const { message, done } = beginTurn({ db, bus, llm, tools: [] }, sessionId);
  await done;

  const final = finalMessage(db, message.id);
  assertEquals(final.pending, false);
  assertEquals(final.parts.length, 1);
  assertStringIncludes((final.parts[0] as { text: string }).text, "boom");
  assertEquals(db.turnsByStatus("error").length, 1);
  assertEquals(eventTypes(events, message.id).includes("message.finished"), true);
});

Deno.test("a tool that throws yields an error tool_result but the turn continues", async () => {
  const { db, bus, sessionId } = seed();
  const boom: ToolDef = {
    name: "boom",
    description: "always throws",
    schema: z.object({}),
    run: () => Promise.reject(new Error("nope")),
  };
  const llm = fakeLlm([
    { content: [{ type: "tool_use", id: "x", name: "boom", input: {} }], stopReason: "tool_use" },
    { content: [{ type: "text", text: "recovered" }], stopReason: "end_turn" },
  ]);
  const { message, done } = beginTurn({ db, bus, llm, tools: [boom] }, sessionId);
  await done;

  const final = finalMessage(db, message.id);
  const result = final.parts.find((p) => p.type === "tool_result") as {
    output: string;
    isError: boolean;
  };
  assertEquals(result.isError, true);
  assertStringIncludes(result.output, "nope");
  assertEquals(db.turnsByStatus("done").length, 1);
});

Deno.test("a mid-turn user message steers: the turn yields at the round boundary and a follow-up answers it", async () => {
  const { db, bus, sessionId } = seed();
  const llm = fakeLlm([
    // Round 1 asks for a tool and would keep looping (stopReason tool_use)…
    { content: [{ type: "tool_use", id: "t1", name: "poke", input: {} }], stopReason: "tool_use" },
    // …but the steered message ends the turn at the boundary; this round is the
    // follow-up's, ending compliantly with text + stop in one response.
    {
      content: [
        { type: "text", text: "doing Y" },
        { type: "tool_use", id: "s1", name: "stop", input: {} },
      ],
      stopReason: "tool_use",
    },
  ]);
  const ctx: TurnCtx = { db, bus, llm, tools: [] };
  const poke: ToolDef = {
    name: "poke",
    description: "posts a user message while the turn runs",
    schema: z.object({}),
    run: () => {
      // Simulate POST /messages landing mid-turn.
      startUserTurn(ctx, sessionId, "actually, do Y instead");
      return Promise.resolve("ok");
    },
  };
  ctx.tools = [poke];

  const { message, done } = beginTurn(ctx, sessionId);
  await done;
  // The follow-up turn starts from the first turn's drain — wait for it to finish.
  while (isTurnRunning(sessionId)) await new Promise((r) => setTimeout(r, 1));

  // First turn stopped after its tool round — no second LLM call of its own.
  const first = finalMessage(db, message.id);
  assertEquals(first.pending, false);
  assertEquals(first.parts, [
    { type: "tool_call", id: "t1", name: "poke", input: {} },
    { type: "tool_result", callId: "t1", output: "ok", isError: false },
  ] as Part[]);
  assertEquals(db.turnsByStatus("done").length, 2);

  // Exactly one call per turn; the follow-up saw the steered message and the
  // first turn's replayed tool work.
  assertEquals(llm.calls.length, 2);
  const followUp = JSON.stringify(llm.calls[1].messages);
  assertStringIncludes(followUp, "actually, do Y instead");
  assertStringIncludes(followUp, "tool_use");

  // The follow-up's reply landed as the thread's last message.
  const last = db.threadFor(sessionId).at(-1);
  assertExists(last);
  assertEquals(last.role, "supervisor");
  assertEquals(last.parts, [{ type: "text", text: "doing Y" }] as Part[]);
});

Deno.test("history from a prior turn is replayed as assistant + tool_result messages", async () => {
  const { db, bus, sessionId } = seed();
  // A completed supervisor turn with a tool call and its result.
  db.createMessage({
    id: "sup0",
    sessionId,
    role: "supervisor",
    parts: [
      { type: "reasoning", text: "thinking..." },
      { type: "tool_call", id: "p1", name: "bash", input: { command: "ls" } },
      { type: "tool_result", callId: "p1", output: "file.txt", isError: false },
      { type: "text", text: "there is one file" },
    ],
    pending: false,
    createdAt: 3,
  });
  db.createMessage({
    id: "u2",
    sessionId,
    role: "user",
    parts: [{ type: "text", text: "and now?" }],
    pending: false,
    createdAt: 4,
  });

  const llm = fakeLlm([{ content: [{ type: "text", text: "ok" }], stopReason: "end_turn" }]);
  const { done } = beginTurn({ db, bus, llm, tools: [] }, sessionId);
  await done;

  const history = llm.calls[0].messages;
  // reasoning dropped; assistant carries text + tool_use; a following user msg carries the tool_result.
  const assistant = history.find((m) => m.role === "assistant");
  assertExists(assistant);
  assertEquals(assistant.content.some((b) => b.type === "reasoning"), false);
  assertEquals(assistant.content.some((b) => b.type === "tool_use"), true);
  const toolResultMsg = history.find((m) =>
    m.role === "user" && m.content.some((b) => b.type === "tool_result")
  );
  assertExists(toolResultMsg);
});

Deno.test("an explicit session workspace makes the turn sandboxed and hands tools a sandbox dir", async () => {
  const { db, bus, sessionId } = seed();
  const ws = await Deno.makeTempDir(); // non-repo → snapshot prep is skipped
  const snap = await Deno.makeTempDir();
  db.setSessionWorkspace(sessionId, ws);
  Deno.env.set("BOUGH_SNAPSHOT_BASE", snap);

  let seen: ToolRunCtx | null = null;
  const probe: ToolDef = {
    name: "probe",
    description: "records its ctx",
    schema: z.object({}),
    run: (_input, ctx) => {
      seen = ctx;
      return Promise.resolve("ok");
    },
  };
  const llm = fakeLlm([
    { content: [{ type: "tool_use", id: "1", name: "probe", input: {} }], stopReason: "tool_use" },
    { content: [{ type: "text", text: "done" }], stopReason: "end_turn" },
  ]);
  try {
    const { done } = beginTurn({ db, bus, llm, tools: [probe] }, sessionId);
    await done;
  } finally {
    Deno.env.delete("BOUGH_SNAPSHOT_BASE");
    await Deno.remove(ws, { recursive: true });
    await Deno.remove(snap, { recursive: true });
  }

  assertExists(seen);
  const ctx = seen as ToolRunCtx;
  assertEquals(ctx.workspace, ws);
  assertExists(ctx.sandbox);
  assertEquals(ctx.sandbox.sessionDir, `${snap}/${sessionId}`);
});

Deno.test("recoverOrphanedTurns orphans a stranded turn and finishes its message", () => {
  const { db, bus, events, sessionId } = seed();
  db.createMessage({
    id: "sup1",
    sessionId,
    role: "supervisor",
    parts: [{ type: "text", text: "partial" }],
    pending: true,
    createdAt: 5,
  });
  db.createTurn({
    id: "turn1",
    sessionId,
    messageId: "sup1",
    status: "running",
    step: "round:1",
    updatedAt: 6,
    firstOutputAt: null,
  });

  const recovered = recoverOrphanedTurns(db, bus);

  assertEquals(recovered, 1);
  assertEquals(db.getTurn("turn1")?.status, "orphaned");
  const msg = finalMessage(db, "sup1");
  assertEquals(msg.pending, false);
  assertStringIncludes((msg.parts.at(-1) as { text: string }).text, "Interrupted");
  assertEquals(eventTypes(events, "sup1").includes("message.finished"), true);
});

Deno.test("interrupt reaches INTO a running tool via ctx.signal (stop means stop)", async () => {
  const { db, bus, sessionId } = seed();
  // A tool that only finishes when the turn's signal aborts — a stand-in for a
  // long-running run_steps program / bash child observing ctx.signal.
  const longTool: ToolDef = {
    name: "long",
    description: "runs until interrupted",
    schema: z.object({}),
    run: (_input, ctx?: ToolRunCtx) =>
      new Promise<string>((_resolve, reject) => {
        ctx?.signal?.addEventListener(
          "abort",
          () => reject(new Error("killed: turn interrupted")),
          { once: true },
        );
      }),
  };
  const llm = fakeLlm([
    { content: [{ type: "tool_use", id: "t1", name: "long", input: {} }], stopReason: "tool_use" },
  ]);
  const { message, done } = beginTurn({ db, bus, llm, tools: [longTool] }, sessionId);
  await new Promise((r) => setTimeout(r, 10)); // let the tool start
  assertEquals(interruptTurn(sessionId), true);
  await done; // resolves because the tool observed the signal — no hang
  const final = finalMessage(db, message.id);
  assertEquals(final.pending, false);
  assertStringIncludes((final.parts.at(-1) as { text: string }).text, "Stopped");
  assertEquals(db.turnsByStatus("interrupted").length, 1);
});

Deno.test("delegation fit note: injected for a decomposable request, absent otherwise", async () => {
  const { db, bus, sessionId } = seed();
  // A fresh user message shaped like independent fan-out work (the seed's "hi"
  // is not); lastUserText picks the latest, so this drives the gate.
  db.createMessage({
    id: "u2",
    sessionId,
    role: "user",
    parts: [{ type: "text", text: "Audit each of the three services for PII logging." }],
    pending: false,
    createdAt: 3,
  });
  const llm = fakeLlm([{ content: [{ type: "text", text: "ok" }], stopReason: "end_turn" }]);
  const { done } = beginTurn({ db, bus, llm, tools: [] }, sessionId);
  await done;
  // Per-turn text lives in the VOLATILE tier — the stable prefix must stay
  // byte-identical across requests, hint or no hint.
  const volatile = llm.calls[0].systemVolatile ?? "";
  assertStringIncludes(volatile, "# Delegation fit (this request)");
  assertStringIncludes(volatile, "spawn(t)");
  assertEquals((llm.calls[0].system ?? "").includes("# Delegation fit"), false);
});

Deno.test("delegation fit note: cohesive request gets no note", async () => {
  const { db, bus, sessionId } = seed(); // "hi" — nothing decomposable
  const llm = fakeLlm([{ content: [{ type: "text", text: "ok" }], stopReason: "end_turn" }]);
  const { done } = beginTurn({ db, bus, llm, tools: [] }, sessionId);
  await done;
  const all = (llm.calls[0].system ?? "") + (llm.calls[0].systemVolatile ?? "");
  assertEquals(all.includes("# Delegation fit"), false);
});

Deno.test("delegation fit note: never for subagent turns (they have no spawn())", async () => {
  const { db, bus, sessionId } = seed();
  const sub: Session = {
    id: "sub1",
    parentId: null,
    title: "sub",
    kind: "subagent",
    originId: sessionId,
    createdAt: 4,
  };
  db.createSession(sub);
  db.createMessage({
    id: "su1",
    sessionId: sub.id,
    role: "user",
    parts: [{ type: "text", text: "Audit each of the three services for PII logging." }],
    pending: false,
    createdAt: 5,
  });
  const llm = fakeLlm([{ content: [{ type: "text", text: "ok" }], stopReason: "end_turn" }]);
  const { done } = beginTurn({ db, bus, llm, tools: [] }, sub.id);
  await done;
  const system = llm.calls[0].system ?? "";
  // Depth 1 still delegates (blocking agent()), but the spawn-shaped note must not render.
  assertStringIncludes(system, "await agent(task)");
  const all = system + (llm.calls[0].systemVolatile ?? "");
  assertEquals(all.includes("# Delegation fit"), false);
});

Deno.test("mcp end-to-end: /skill grant connects the server, prompts tools, bridges mcp()", async () => {
  if ((await Deno.permissions.query({ name: "run" })).state !== "granted") return;
  const skillsDir = Deno.makeTempDirSync({ prefix: "bough-skills-" });
  const mcpDir = Deno.makeTempDirSync({ prefix: "bough-mcp-" });
  Deno.env.set("BOUGH_SKILLS_DIR", skillsDir);
  Deno.env.set("BOUGH_BUNDLED_SKILLS_DIR", "/nonexistent-bough-bundled");
  Deno.env.set("BOUGH_MCP_DIR", mcpDir);
  try {
    // a skill that grants the fixture server, and a registry that defines it
    Deno.mkdirSync(`${skillsDir}/browse`, { recursive: true });
    Deno.writeTextFileSync(
      `${skillsDir}/browse/SKILL.md`,
      "---\nname: browse\ndescription: echo things\nmcp: echo\n---\n\nUse the echo tool.\n",
    );
    const fixture = new URL("./mcp/testdata/echo_server.ts", import.meta.url).pathname;
    const { saveRegistry } = await import("./mcp/config.ts");
    saveRegistry({
      servers: {
        echo: { command: Deno.execPath(), args: ["run", "--quiet", "--no-config", fixture] },
      },
    });

    const { db, bus, sessionId } = seed();
    db.createMessage({
      id: "u2",
      sessionId,
      role: "user",
      parts: [{ type: "text", text: "/browse round-trip the fixture" }],
      pending: false,
      createdAt: 3,
    });
    const llm = fakeLlm([
      {
        content: [{
          type: "tool_use",
          id: "t1",
          name: "run_steps",
          input: {
            code: 'console.log("mcp says:", (await mcp("echo", "echo", {text: "e2e"})).echoed);',
          },
        }],
        stopReason: "tool_use",
      },
      { content: [{ type: "text", text: "done" }], stopReason: "end_turn" },
    ]);
    const ctx: TurnCtx = { db, bus, llm, workspace: Deno.makeTempDirSync() };

    const { message, done } = beginTurn(ctx, sessionId);
    await done;

    // the system prompt carried the catalog, and the program's call round-tripped.
    // MCP catalog + skills are per-turn facts, so they ride the VOLATILE tier —
    // never the stable prefix (cache contract, see turn.ts).
    const volatile = llm.calls[0].systemVolatile ?? "";
    assertStringIncludes(volatile, "# MCP tools");
    assertStringIncludes(volatile, 'server "echo" (3 tools):');
    assertStringIncludes(volatile, "Active skill: /browse");
    assertEquals((llm.calls[0].system ?? "").includes('server "echo"'), false);
    const final = finalMessage(db, message.id);
    const result = final.parts.find((p) => p.type === "tool_result") as { output: string };
    assertStringIncludes(result.output, "mcp says: e2e");
  } finally {
    const { mcpManager } = await import("./mcp/manager.ts");
    await mcpManager().dropAll();
    Deno.env.delete("BOUGH_SKILLS_DIR");
    Deno.env.delete("BOUGH_BUNDLED_SKILLS_DIR");
    Deno.env.delete("BOUGH_MCP_DIR");
  }
});

Deno.test("an @image ref composes an image part and replays as a base64 image block", async () => {
  // 1×1 PNG; HOME is swapped to a temp dir so the attachment copy lands there.
  const PNG_B64 =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";
  const home = await Deno.makeTempDir({ prefix: "imghome-" });
  const dir = await Deno.makeTempDir({ prefix: "imgsrc-" });
  const origHome = Deno.env.get("HOME");
  try {
    Deno.env.set("HOME", home);
    await Deno.writeFile(`${dir}/shot.png`, Uint8Array.fromBase64(PNG_B64));
    const { db, bus, sessionId } = seed();
    const llm = fakeLlm([{
      content: [{ type: "text", text: "looks fine" }],
      stopReason: "end_turn",
    }]);
    const ctx: TurnCtx = { db, bus, llm, tools: [] };

    const { userMessage, done } = startUserTurn(
      ctx,
      sessionId,
      `what is this? @${dir}/shot.png`,
    );
    await done;

    // Composed message: the text part plus an attachment-backed image part.
    assertEquals(userMessage.parts.length, 2);
    const img = userMessage.parts[1] as Extract<Part, { type: "image" }>;
    assertEquals(img.type, "image");
    assertEquals(img.mediaType, "image/png");
    assertEquals(img.path.startsWith(`${home}/.bough/attachments/`), true);

    // Replay: the LLM round saw the image as a base64 block on the user message.
    // calls[0].messages is a live reference the loop appends to across rounds,
    // so index the composed message (after seed's "hi") rather than taking last.
    const users = llm.calls[0].messages.filter((m) => m.role === "user");
    const blocks = users[1].content;
    assertEquals(blocks[1], {
      type: "image",
      data: PNG_B64,
      mediaType: "image/png",
      name: `${dir}/shot.png`,
    });
  } finally {
    if (origHome) Deno.env.set("HOME", origHome);
    await Deno.remove(home, { recursive: true });
    await Deno.remove(dir, { recursive: true });
  }
});

// ---- ask(): the mid-task question hold --------------------------------------

Deno.test("ask(): the program parks, the answer resolves it, and the Q/A persists + replays", async () => {
  const { db, bus, events, sessionId } = seed();
  const llm = fakeLlm([
    {
      content: [{
        type: "tool_use",
        id: "t1",
        name: "run_steps",
        input: {
          code: 'console.log("got:", await ask("Which env?", { options: ["dev", "prod"] }));',
        },
      }],
      stopReason: "tool_use",
    },
    { content: [{ type: "text", text: "deployed to prod" }], stopReason: "end_turn" },
  ]);
  // Answer the hold the moment it's raised (synchronous bus listener = the same
  // path a TUI answer takes through POST /sessions/:id/questions/:qid).
  bus.subscribe((e) => {
    if (e.type !== "ask.question") return;
    const q = e.data as AskQuestion;
    if (q.status === "pending") answerAsk(q.id, "prod");
  });
  const ctx: TurnCtx = { db, bus, llm, workspace: Deno.makeTempDirSync() };

  const { message, done } = beginTurn(ctx, sessionId);
  await done;

  // The program saw the answer, and nothing is left holding.
  const final = finalMessage(db, message.id);
  const result = final.parts.find((p) => p.type === "tool_result") as { output: string };
  assertStringIncludes(result.output, "got: prod");
  assertEquals(pendingAsks().length, 0);

  // The settled Q/A persisted as an ask part on the supervisor message…
  const askPart = final.parts.find((p) => p.type === "ask") as Extract<Part, { type: "ask" }>;
  assertExists(askPart);
  assertEquals(askPart.question, "Which env?");
  assertEquals(askPart.options, ["dev", "prod"]);
  assertEquals(askPart.status, "answered");
  assertEquals(askPart.answer, "prod");
  // …and the hold's lifecycle was announced pending → answered on one id.
  const askEvents = events.filter((e) => e.type === "ask.question")
    .map((e) => e.data as AskQuestion);
  assertEquals(askEvents.map((q) => q.status), ["pending", "answered"]);
  assertEquals(new Set(askEvents.map((q) => q.id)).size, 1);

  // Replay: a later turn sees the Q/A as plain user-side text — it can never
  // re-raise the hold (no pending ask exists while the new turn builds history).
  db.createMessage({
    id: "u2",
    sessionId,
    role: "user",
    parts: [{ type: "text", text: "and staging?" }],
    pending: false,
    createdAt: 99,
  });
  const llm2 = fakeLlm([{ content: [{ type: "text", text: "ok" }], stopReason: "end_turn" }]);
  await beginTurn({ db, bus, llm: llm2, tools: [] }, sessionId).done;
  const replayed = llm2.calls[0].messages
    .flatMap((m) => m.content)
    .find((b) => b.type === "text" && b.text.startsWith("[ask]")) as { text: string };
  assertExists(replayed);
  assertStringIncludes(replayed.text, "Which env?");
  assertStringIncludes(replayed.text, "the user answered: prod");
});

Deno.test("ask(): a decline rejects catchably in the program and persists as declined", async () => {
  const { db, bus, sessionId } = seed();
  const llm = fakeLlm([
    {
      content: [{
        type: "tool_use",
        id: "t1",
        name: "run_steps",
        input: {
          code:
            `try { await ask("Push to main?"); } catch (e) { console.log("caught:", e.message); }`,
        },
      }],
      stopReason: "tool_use",
    },
    { content: [{ type: "text", text: "stopped short of pushing" }], stopReason: "end_turn" },
  ]);
  bus.subscribe((e) => {
    if (e.type !== "ask.question") return;
    const q = e.data as AskQuestion;
    if (q.status === "pending") declineAsk(q.id);
  });
  const ctx: TurnCtx = { db, bus, llm, workspace: Deno.makeTempDirSync() };

  const { message, done } = beginTurn(ctx, sessionId);
  await done;

  const final = finalMessage(db, message.id);
  const result = final.parts.find((p) => p.type === "tool_result") as { output: string };
  assertStringIncludes(result.output, "caught: user declined to answer: Push to main?");
  const askPart = final.parts.find((p) => p.type === "ask") as Extract<Part, { type: "ask" }>;
  assertEquals(askPart.status, "declined");
  assertEquals(db.turnsByStatus("done").length, 1);
});

Deno.test("ask(): turn interrupt rejects the hold and the turn ends interrupted", async () => {
  const { db, bus, sessionId } = seed();
  const llm = fakeLlm([
    {
      content: [{
        type: "tool_use",
        id: "t1",
        name: "run_steps",
        // Parks forever — only the interrupt can release it.
        input: { code: `await ask("Which?", { options: ["a", "b"] });` },
      }],
      stopReason: "tool_use",
    },
  ]);
  bus.subscribe((e) => {
    if (e.type !== "ask.question") return;
    const q = e.data as AskQuestion;
    if (q.status === "pending") interruptTurn(sessionId);
  });
  const ctx: TurnCtx = { db, bus, llm, workspace: Deno.makeTempDirSync() };

  const { message, done } = beginTurn(ctx, sessionId);
  await done;

  const final = finalMessage(db, message.id);
  assertEquals(final.pending, false);
  assertStringIncludes((final.parts.at(-1) as { text: string }).text, "Stopped");
  const askPart = final.parts.find((p) => p.type === "ask") as Extract<Part, { type: "ask" }>;
  assertEquals(askPart.status, "interrupted");
  assertEquals(db.turnsByStatus("interrupted").length, 1);
  // Nothing haunts the next session: the hold is gone.
  assertEquals(pendingAsks().length, 0);
});
