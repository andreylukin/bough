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
 * Selection semantics (documented constraint, per the "align spans with what the
 * parent-chain expresses cleanly" allowance):
 *   - `picks` selects any subset of the target session's OWN messages (messagesFor(id)),
 *     not messages inherited from an ancestor. A selection that reaches into ancestor
 *     history is rejected 400 — compact the ancestor session instead.
 *   - The selection need not be contiguous: each maximal run of adjacent selected
 *     messages collapses to ONE summary in place; unselected messages are copied
 *     verbatim around the summaries, preserving thread order.
 *   - A pick may carry `parts` (indexes into the message's parts) to narrow what the
 *     summarizer SEES — e.g. a turn's prose without its tool calls. The message is
 *     still wholly replaced by the summary: compaction shrinks, so unpicked parts
 *     drop rather than being kept verbatim.
 *   - "turns" in the title = the number of picked messages (v1 counts messages).
 *
 * Events: emits session.created for the new branch and message.started for each seeded
 * message (summary + copies), so the UI's existing reducers pick it up with no changes.
 */
import { HttpError } from "./errors.ts";
import { z } from "zod";
import type { Db } from "./db/db.ts";
import type { Bus } from "./bus.ts";
import type { Message, Part, Session } from "./schema/parts.ts";
import { anthropicClient, completeText, type LlmClient } from "./supervisor/llm.ts";
import { mergePicks, openBranch, PartPick, pickParts } from "./branch.ts";

export const CompactBody = z.object({
  /** The session's OWN messages to compact; each contiguous run becomes one summary. */
  picks: z.array(PartPick).min(1),
  instructions: z.string().optional(),
});
export type CompactBody = z.infer<typeof CompactBody>;

export interface CompactCtx {
  db: Db;
  bus: Bus;
  /** Injected for tests; defaults to the real Anthropic client. */
  llm?: LlmClient;
  model?: string;
  /**
   * Optional retitler for the compaction branch (production: the local title
   * worker, wired in server main). Absent = keep the deterministic title.
   */
  retitler?: (text: string) => Promise<string | null>;
}

/** 400 for a bad span, 404 for an unknown session/message. */
export class CompactError extends HttpError {}

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
    case "ask":
      // A settled ask() Q/A: what was asked and how the human resolved it.
      return `ask: ${p.question} → ${
        p.status === "answered" ? `user answered: ${p.answer}` : p.status
      }`;
  }
}

function clip(s: string): string {
  return s.length > PART_CLIP ? s.slice(0, PART_CLIP) + "…" : s;
}
function stringify(v: unknown): string {
  return typeof v === "string" ? v : JSON.stringify(v);
}

/** Messages rendered as a plain transcript for an LLM prompt (shared with handoff.ts). */
export function renderSpan(messages: Message[]): string {
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
  return (await completeText(llm, { model, system: SYSTEM, maxTokens: MAX_TOKENS, prompt })).trim();
}

/**
 * Compact the selected messages of `sessionId` onto a new compaction branch and return
 * the new session. Each contiguous run of selected messages is replaced in place by one
 * summary; everything unselected is copied verbatim. Throws CompactError (400/404).
 */
export async function compact(
  ctx: CompactCtx,
  sessionId: string,
  args: CompactBody,
): Promise<Session> {
  const session = ctx.db.getSession(sessionId);
  if (!session) throw new CompactError(404, "session not found");

  const own = ctx.db.messagesFor(sessionId);
  const index = new Map(own.map((m, i) => [m.id, i]));
  // The picked messages in thread order, each narrowed to its picked parts — the
  // narrowed view is what the summarizer sees; the whole message is still replaced.
  const picked = [...mergePicks(args.picks)]
    .map(([id, sel]) => {
      const i = index.get(id);
      if (i === undefined) {
        throw new CompactError(
          400,
          "picks must be messages of this session (v1 selections can't reach into ancestor history)",
        );
      }
      const parts = pickParts(own[i], sel);
      if (parts === undefined) {
        throw new CompactError(400, `part index out of range for message ${id}`);
      }
      return { idx: i, view: { ...own[i], parts } };
    })
    .sort((a, b) => a.idx - b.idx);

  // Maximal runs of adjacent selected indices; each run collapses to one summary.
  const runs: { start: number; end: number; span: Message[] }[] = [];
  for (const p of picked) {
    const last = runs.at(-1);
    if (last && p.idx === last.end + 1) {
      last.end = p.idx;
      last.span.push(p.view);
    } else runs.push({ start: p.idx, end: p.idx, span: [p.view] });
  }
  const summaries = await Promise.all(
    runs.map((r) => summarize(ctx, r.span, args.instructions)),
  );

  // Branch a sibling of the target and seed it: copies of unselected messages with each
  // run swapped for its summary, in thread order. The shared ancestors come from
  // thread-through-parents (see branch.ts).
  const seeder = openBranch(ctx, {
    parentId: session.parentId,
    title: `compacted · ${picked.length} turns`,
    kind: "compaction",
    originId: session.id, // lineage: the compacted session…
    originMessageId: own[picked[picked.length - 1].idx].id, // …and the last picked message
  });
  let run = 0;
  for (let i = 0; i < own.length; i++) {
    if (run < runs.length && i === runs[run].start) {
      seeder.add("supervisor", [{ type: "text", text: summaries[run] }]);
      i = runs[run].end; // skip the rest of the run (loop's i++ lands on end+1)
      run++;
    } else {
      seeder.copy(own[i]);
    }
  }

  // Fire-and-forget: name the branch from its first summary (local title worker).
  // The deterministic placeholder stays if the worker is cold or the user renamed.
  if (ctx.retitler) {
    const branchId = seeder.session.id;
    const placeholder = seeder.session.title;
    ctx.retitler(summaries[0]).then((t) => {
      if (!t || ctx.db.getSession(branchId)?.title !== placeholder) return;
      ctx.db.setSessionTitle(branchId, `${t} · compacted ${picked.length}`);
      const updated = ctx.db.getSession(branchId);
      if (updated) {
        ctx.bus.publish({ type: "session.updated", sessionId: branchId, data: updated });
      }
    }).catch(() => {});
  }

  return seeder.session;
}
