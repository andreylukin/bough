import { assertEquals, assertExists, assertStringIncludes } from "jsr:@std/assert@1";
import { z } from "zod/v4";
import { Db } from "./db/db.ts";
import { Bus } from "./bus.ts";
import type { BoughEvent, Message, Part, Session } from "./schema/parts.ts";
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

    // the system prompt carried the catalog, and the program's call round-tripped
    const system = llm.calls[0].system ?? "";
    assertStringIncludes(system, "# MCP tools");
    assertStringIncludes(system, 'server "echo" (3 tools):');
    assertStringIncludes(system, "Active skill: /browse");
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
