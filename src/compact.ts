/**
 * Compaction-as-a-branch (design doc "Tree history + branching"). Highlight a span of a
 * session's thread and replace it with an LLM summary on a NEW branch, so the original
 * history is preserved and compacted-vs-full stay comparable — the same mechanism as
 * forking, never a destructive edit.
 *
 * How it fits thread-through-parents WITHOUT touching db.ts/threadFor:
 *   threadFor(s) = (messages of every ancestor, root→parent) ++ (s's own messages).
 *   So to drop a span we branch a SIBLING of the target (parent = target.parentId) and
 *   seed it with: copies of the target's own pre-span messages, then one summary
 *   message, then copies of the post-span messages. The shared ancestors come for free;
 *   the compaction session's own messages reconstruct the thread with the span swapped
 *   for the summary. The original session is never mutated.
 *
 * v1 span semantics (documented constraint, per the "align spans with what the
 * parent-chain expresses cleanly" allowance):
 *   - `fromMessageId` and `toMessageId` must both be the target session's OWN messages
 *     (messagesFor(id)), not messages inherited from an ancestor. A span that reaches
 *     into ancestor history is rejected 400 — compact the ancestor session instead.
 *   - `from` must not come after `to` (a single-message span, from == to, is allowed).
 *   - "turns" in the title = the number of messages in the span (v1 counts messages).
 *
 * Events: emits session.created for the new branch and message.started for each seeded
 * message (summary + copies), so the UI's existing reducers pick it up with no changes.
 */
import { z } from "zod";
import type { Db } from "./db/db.ts";
import type { Bus } from "./bus.ts";
import type { Message, Part, Session } from "./schema/parts.ts";
import { anthropicClient, type LlmClient } from "./supervisor/llm.ts";
import { openBranch } from "./branch.ts";

export const CompactBody = z.object({
  fromMessageId: z.string(),
  toMessageId: z.string(),
  instructions: z.string().optional(),
});
export type CompactBody = z.infer<typeof CompactBody>;

export interface CompactCtx {
  db: Db;
  bus: Bus;
  /** Injected for tests; defaults to the real Anthropic client. */
  llm?: LlmClient;
  model?: string;
}

/** 400 for a bad span, 404 for an unknown session/message. */
export class CompactError extends Error {
  constructor(readonly status: number, message: string) {
    super(message);
    this.name = "CompactError";
  }
}

const SYSTEM =
  "You are compacting a span of a coding-agent conversation. Produce a concise summary " +
  "that preserves the decisions made, files/code changed, the resulting state, and any " +
  "open questions — enough that the conversation can continue as if the original " +
  "messages were still present. Output only the summary text.";

const MAX_TOKENS = 1024;
const PART_CLIP = 2000; // keep the prompt bounded on long tool outputs

function renderPart(role: string, p: Part): string {
  switch (p.type) {
    case "text":
    case "reasoning":
      return `${role}: ${p.text}`;
    case "tool_call":
      return `${role}: [tool ${p.name}] ${clip(JSON.stringify(p.input))}`;
    case "tool_result":
      return `tool_result${p.isError ? " (error)" : ""}: ${clip(stringify(p.output))}`;
  }
}

function clip(s: string): string {
  return s.length > PART_CLIP ? s.slice(0, PART_CLIP) + "…" : s;
}
function stringify(v: unknown): string {
  return typeof v === "string" ? v : JSON.stringify(v);
}

function renderSpan(messages: Message[]): string {
  return messages
    .flatMap((m) => (m.parts.length ? m.parts.map((p) => renderPart(m.role, p)) : [`${m.role}:`]))
    .join("\n");
}

async function summarize(ctx: CompactCtx, span: Message[], instructions?: string): Promise<string> {
  const llm = ctx.llm ?? anthropicClient();
  const model = ctx.model ?? Deno.env.get("BOUGH_MODEL") ?? "claude-opus-4-8";
  const prompt = instructions
    ? `${renderSpan(span)}\n\nAdditional instructions: ${instructions}`
    : renderSpan(span);
  const result = await llm.run(
    { model, system: SYSTEM, maxTokens: MAX_TOKENS, messages: [{ role: "user", content: [{ type: "text", text: prompt }] }], tools: [] },
    () => {},
  );
  return result.content
    .filter((b): b is { type: "text"; text: string } => b.type === "text")
    .map((b) => b.text)
    .join("")
    .trim();
}

/**
 * Compact [fromMessageId..toMessageId] of `sessionId` onto a new compaction branch and
 * return the new session. Throws CompactError (400/404) on invalid input.
 */
export async function compact(ctx: CompactCtx, sessionId: string, args: CompactBody): Promise<Session> {
  const session = ctx.db.getSession(sessionId);
  if (!session) throw new CompactError(404, "session not found");

  const own = ctx.db.messagesFor(sessionId);
  const index = new Map(own.map((m, i) => [m.id, i]));
  const fromIdx = index.get(args.fromMessageId);
  const toIdx = index.get(args.toMessageId);
  if (fromIdx === undefined || toIdx === undefined) {
    throw new CompactError(
      400,
      "span endpoints must be messages of this session (v1 spans can't reach into ancestor history)",
    );
  }
  if (fromIdx > toIdx) throw new CompactError(400, "invalid span: from is after to");

  const span = own.slice(fromIdx, toIdx + 1);
  const summaryText = await summarize(ctx, span, args.instructions);

  // Branch a sibling of the target and seed it: pre-span copies, the summary, post-span
  // copies. The shared ancestors come from thread-through-parents (see branch.ts).
  const seeder = openBranch(ctx, {
    parentId: session.parentId,
    title: `compacted · ${span.length} turns`,
    kind: "compaction",
    originId: session.id, // lineage: the compacted session…
    originMessageId: args.toMessageId, // …and the span-end message
  });
  for (const m of own.slice(0, fromIdx)) seeder.copy(m);
  seeder.add("supervisor", [{ type: "text", text: summaryText }]);
  for (const m of own.slice(toIdx + 1)) seeder.copy(m);

  return seeder.session;
}
