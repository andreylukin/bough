/**
 * The stored-parts → provider-messages mapping, unit-tested with no filesystem and
 * no database.
 *
 * The two invariants asserted here cannot be recovered from a spec by reading the
 * code: reasoning is dropped (plan §6.4) and a settled `ask` replays as plain text
 * that can never re-block (plan §6.5). Both are asserted positively *and*
 * negatively — that the part disappears, and that everything around it survives —
 * because a mapping that dropped the whole message would pass a test that only
 * checked the first half.
 */
import assert from "node:assert/strict";
import type { ImagePart, Message, Part } from "../schema/parts.ts";
import type { LlmContentBlock } from "../types.ts";
import {
  buildThread,
  lostAttachmentText,
  messageToLlm,
  stringifyOutput,
  stripReasoning,
} from "./replay.ts";

// ---- fixtures ---------------------------------------------------------------

let seq = 0;
function message(role: Message["role"], parts: Part[]): Message {
  return {
    id: `m${++seq}`,
    sessionId: "s1",
    role,
    parts,
    pending: false,
    createdAt: 1_000 + seq,
  };
}

const image: ImagePart = {
  type: "image",
  path: "abc.png",
  mediaType: "image/png",
  name: "screenshot.png",
  size: 4_096,
};

/** A loader that always answers, so the "found" path needs no file. */
const found = () => ({ data: "AAAA", mediaType: "image/png" });
/** A loader that never answers — the moved/deleted attachment. */
const lost = () => null;

function types(blocks: LlmContentBlock[]): string[] {
  return blocks.map((b) => b.type);
}

// ---- user and system messages ----------------------------------------------

Deno.test("a user message becomes one user message of text and image blocks", () => {
  const out = messageToLlm(
    message("user", [{ type: "text", text: "look at this" }, image]),
    { loadImage: found },
  );
  assert.equal(out.length, 1);
  assert.equal(out[0].role, "user");
  assert.deepEqual(types(out[0].content), ["text", "image"]);
  assert.deepEqual(out[0].content[1], {
    type: "image",
    data: "AAAA",
    mediaType: "image/png",
    name: "screenshot.png",
  });
});

Deno.test("a lost attachment replays as placeholder text, never as a failure", () => {
  const out = messageToLlm(message("user", [image]), { loadImage: lost });
  assert.equal(out.length, 1);
  assert.deepEqual(types(out[0].content), ["text"]);
  const placeholder = (out[0].content[0] as { text: string }).text;
  assert.equal(placeholder, lostAttachmentText(image));
  // It names the file and says the BYTES are what is missing — a model told only
  // "[image]" describes a picture it cannot see.
  assert.match(placeholder, /screenshot\.png/);
  assert.match(placeholder, /no longer on disk/);
});

Deno.test("a system note replays user-side: it is input to the model, not words it said", () => {
  const out = messageToLlm(
    message("system", [{ type: "text", text: "[subagent finished] audit-handlers" }]),
  );
  assert.equal(out.length, 1);
  assert.equal(out[0].role, "user");
});

Deno.test("a message with nothing to say produces no message at all", () => {
  assert.deepEqual(messageToLlm(message("user", [])), []);
  assert.deepEqual(messageToLlm(message("user", [{ type: "text", text: "" }])), []);
  assert.deepEqual(messageToLlm(message("supervisor", [])), []);
});

// ---- supervisor messages ----------------------------------------------------

Deno.test("a supervisor round becomes an assistant message and then its tool results", () => {
  const out = messageToLlm(message("supervisor", [
    { type: "text", text: "Running it." },
    { type: "tool_call", id: "c1", name: "run_steps", input: { code: "1" } },
    { type: "tool_result", callId: "c1", output: "ok", isError: false },
  ]));

  assert.deepEqual(out.map((m) => m.role), ["assistant", "user"]);
  assert.deepEqual(types(out[0].content), ["text", "tool_use"]);
  assert.deepEqual(out[1].content, [{
    type: "tool_result",
    toolUseId: "c1",
    content: "ok",
    isError: false,
  }]);
});

Deno.test("reasoning is dropped on replay and takes nothing else with it", () => {
  const out = messageToLlm(message("supervisor", [
    { type: "reasoning", text: "SECRET-THINKING" },
    { type: "text", text: "Here is the answer." },
    { type: "tool_call", id: "c1", name: "run_steps", input: { code: "1" } },
    { type: "tool_result", callId: "c1", output: "ok", isError: false },
  ]));

  assert.ok(!JSON.stringify(out).includes("SECRET-THINKING"));
  assert.deepEqual(types(out[0].content), ["text", "tool_use"], "everything else survives");
});

Deno.test("a reasoning-only message vanishes rather than replaying as an empty turn", () => {
  assert.deepEqual(messageToLlm(message("supervisor", [{ type: "reasoning", text: "hm" }])), []);
});

Deno.test("a settled ask replays as plain text, after the tool results", () => {
  const out = messageToLlm(message("supervisor", [
    { type: "tool_call", id: "c1", name: "run_steps", input: { code: "1" } },
    {
      type: "ask",
      id: "q1",
      question: "Which branch?",
      options: ["main", "next"],
      status: "answered",
      answer: "next",
    },
    { type: "tool_result", callId: "c1", output: "ok", isError: false },
  ]));

  // A tool_use's result must LEAD the user message that follows it; text in front
  // of it is a provider 400. The ask therefore lands after, whatever the part order.
  assert.deepEqual(types(out[1].content), ["tool_result", "text"]);
  const replayed = (out[1].content[1] as { text: string }).text;
  assert.equal(replayed, "[ask] Which branch?\n→ the user answered: next");
  // Nothing carries the hold forward: the ask arrives as a `text` block and there
  // is no block type left that the harness could re-raise from.
  assert.deepEqual(types(out[1].content), ["tool_result", "text"]);
  assert.ok(!JSON.stringify(out).includes('"ask"'));
});

Deno.test("a declined or interrupted ask says which, in the past tense", () => {
  const declined = messageToLlm(message("supervisor", [
    { type: "ask", id: "q1", question: "Proceed?", status: "declined" },
  ]));
  assert.match((declined[0].content[0] as { text: string }).text, /the user declined to answer/);

  const cut = messageToLlm(message("supervisor", [
    { type: "ask", id: "q2", question: "Proceed?", status: "interrupted" },
  ]));
  assert.match(
    (cut[0].content[0] as { text: string }).text,
    /the turn was interrupted before an answer/,
  );
});

Deno.test("a tool_use with no result gets a synthetic one so the thread stays valid", () => {
  // The shape a crash, an orphaned turn or an interrupt between call and result
  // leaves behind. Every provider rejects the open pair.
  const out = messageToLlm(message("supervisor", [
    { type: "tool_call", id: "c1", name: "run_steps", input: { code: "1" } },
    { type: "tool_call", id: "c2", name: "run_steps", input: { code: "2" } },
    { type: "tool_result", callId: "c1", output: "ok", isError: false },
  ]));

  const results = out[1].content as { toolUseId: string; isError: boolean; content: string }[];
  assert.deepEqual(results.map((r) => r.toolUseId), ["c1", "c2"], "in call order");
  assert.equal(results[1].isError, true);
  assert.match(results[1].content, /interrupted/);
});

Deno.test("non-string tool output is stringified rather than dropped", () => {
  assert.equal(stringifyOutput("plain"), "plain");
  assert.equal(stringifyOutput({ a: 1 }), '{"a":1}');
  assert.equal(stringifyOutput(undefined), "");
  const cyclic: Record<string, unknown> = {};
  cyclic.self = cyclic;
  assert.equal(typeof stringifyOutput(cyclic), "string", "a cyclic value must not throw");
});

// ---- whole threads ----------------------------------------------------------

Deno.test("a thread replays in order, minus the message being written", () => {
  const user = message("user", [{ type: "text", text: "do it" }]);
  const supervisor = message("supervisor", [
    { type: "reasoning", text: "thinking" },
    { type: "text", text: "done" },
  ]);
  const pending = message("supervisor", []);

  const out = buildThread([user, supervisor, pending], { exclude: pending.id });
  assert.deepEqual(out.map((m) => m.role), ["user", "assistant"]);
  assert.ok(!JSON.stringify(out).includes("thinking"));

  // Without the exclusion the pending message would still contribute nothing —
  // but the caller must not rely on that, since a partially-written one would.
  assert.equal(buildThread([user, supervisor]).length, 2);
});

Deno.test("stripReasoning drops in-turn thinking and the messages left empty by it", () => {
  const messages = [
    { role: "user" as const, content: [{ type: "text", text: "go" } as LlmContentBlock] },
    {
      role: "assistant" as const,
      content: [
        { type: "reasoning", text: "hm", meta: { signature: "sig" } } as LlmContentBlock,
        { type: "text", text: "ok" } as LlmContentBlock,
      ],
    },
    {
      role: "assistant" as const,
      content: [{ type: "reasoning", text: "only thinking" } as LlmContentBlock],
    },
  ];
  stripReasoning(messages);

  assert.equal(messages.length, 2, "the thinking-only message went with it");
  assert.deepEqual(types(messages[1].content), ["text"]);
});
