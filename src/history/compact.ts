/**
 * Compaction — replacing a span of a conversation with a summary, WITHOUT rewriting
 * anything.
 *
 * THE INVARIANT THIS HOLDS: **compaction never mutates the session it compacts.** It
 * branches a SIBLING of the target (`parentId: target.parentId`) and seeds it with
 * copies of the pre-span messages, one summary message, then copies of the post-span
 * messages. The original session's rows are untouched — same ids, same parts, same
 * timestamps — so the full and the compacted thread stay side by side in the tree and
 * remain comparable (spec §2.4, §14: "nothing is ever destructively rewritten"). The
 * AC test asserts that literally: the source session and its messages are byte-identical
 * (JSON-identical) after a compaction runs.
 *
 * WHY A SIBLING RATHER THAN A CHILD. `db.threadFor(s)` is *ancestors root→parent, then
 * s's own*. A branch parented at the TARGET'S parent therefore inherits every shared
 * ancestor for free, and its own seeded messages reconstruct the rest of the thread with
 * the span swapped for the summary. Parenting at the target instead would inherit the
 * very messages compaction is removing, and no amount of seeding could take them back
 * out. That is also why a selection may only name the session's OWN messages: a pick
 * reaching into ancestor history is a 400 naming the ancestor, because the operation
 * that removes an ancestor's turns is a compaction OF THE ANCESTOR (spec §14).
 *
 * SELECTION NEED NOT BE CONTIGUOUS. Each maximal run of adjacent selected messages
 * collapses to ONE summary in place; unselected messages are copied verbatim around the
 * summaries, preserving thread order. A user who compacts three separate debugging
 * detours and keeps the design discussion between them gets exactly that — three
 * summaries with the design discussion intact between them — rather than one summary of
 * everything from the first pick to the last, which would silently swallow the messages
 * they deliberately did not select.
 *
 * A pick may carry `parts` indexes to narrow what the SUMMARIZER SEES (a turn's prose
 * without its tool output). The message is still wholly replaced: compaction shrinks, so
 * unpicked parts drop rather than surviving verbatim beside the summary.
 *
 * ORDER OF OPERATIONS: summarize FIRST, branch second. Every LLM call for the whole
 * selection completes before a single row is written, so a summarizer that fails leaves
 * no half-seeded branch behind for the user to find and clean up. The cost of that is a
 * failed compaction is a total one; the alternative is a session whose transcript is
 * part copy, part missing, and nothing that can finish it.
 *
 * Ported from `src/compact.ts`. Deltas from that port are marked `NOTE:`.
 */
import { CompactError } from "../errors.ts";
import { clientFor, completeText } from "../llm/client.ts";
import type { Message, Part, Session } from "../schema/parts.ts";
import { CompactBody } from "../schema/requests.ts";
import { DEFAULT_MODEL } from "../turn/runner.ts";
import type { AppCtx, Bus, Db, LlmClient } from "../types.ts";
import { json, parseBody } from "../server/http.ts";
import { openBranch, type PartPick, resolvePicks } from "./branch.ts";
import { exploreSpan } from "./explore.ts";

/**
 * What compaction needs from the world. Structurally satisfied by `AppCtx`, so a
 * handler passes the ctx it already has; declared narrowly so a test hands over a
 * database, a bus and a scripted `LlmClient` and nothing else (plan §0, DI over
 * globals).
 */
export interface CompactCtx {
  db: Db;
  bus: Bus;
  /** Injected in tests. Absent = the provider-routed client for the resolved model. */
  llm?: LlmClient;
  /** The global model default; a session's own pin wins over it. */
  model?: string;
  /** Injected clock, forwarded to the seeder. Absent = `Date.now`. */
  now?: () => number;
  /**
   * The cheap tier (T10.1). Used ONLY to rename the branch from its first summary,
   * fire-and-forget: absent, or failing, leaves the deterministic title in place and
   * nothing else degrades (spec §12: every cheap-tier call fails silently).
   */
  cheap?: AppCtx["cheap"];
  /**
   * The scout (`history/explore.ts`): a bash-capable subagent that reads the current
   * state of the directories a span touched, so the summary can describe the checkout
   * rather than the conversation's memory of it. A seam so a test drives compaction
   * with no shell and no second provider key.
   *
   * Returning `null` — which it does for every failure, by design — is the pre-scout
   * behaviour: summarize from the transcript alone.
   */
  explore?: (span: readonly Message[], workspace: string) => Promise<string | null>;
}

const SYSTEM =
  "You are compacting a span of a coding-agent conversation. Produce a concise summary " +
  "that preserves the decisions made, files/code changed, the resulting state, and any " +
  "open questions — enough that the conversation can continue as if the original " +
  "messages were still present. Output only the summary text.";

/**
 * Appended to the system prompt only when a scout actually returned notes.
 *
 * Separate from `SYSTEM`, and both halves matter. A summary that quietly averaged the
 * transcript against the tree would be the worst of both — so the notes are named as
 * the authority on present state, and the transcript stays the authority on what was
 * decided and why, which no amount of reading the checkout can recover.
 */
const SCOUT_SYSTEM =
  " You are also given SCOUT NOTES: what a subagent found in the files this span " +
  "touched, read from the checkout as it stands now. Where the notes and the " +
  "conversation disagree about the state of the code, the notes are right and the " +
  "summary must say what is actually there — the conversation records intentions, some " +
  "of which were undone later. The conversation remains the only source for decisions, " +
  "reasons and open questions.";

const MAX_TOKENS = 1024;
/** Keeps the prompt bounded when a span contains a 200KB tool result. */
const PART_CLIP = 2000;

function clip(s: string, n: number): string {
  return s.length > n ? `${s.slice(0, n)}…` : s;
}

function stringify(v: unknown): string {
  if (typeof v === "string") return v;
  // `JSON.stringify(undefined)` is `undefined`, not a string — a tool result that
  // carried nothing must render as a word, not as the literal "undefined" crashing
  // `clip`.
  const s = JSON.stringify(v);
  return s === undefined ? String(v) : s;
}

/**
 * One part as a line of transcript.
 *
 * Exhaustive over the part union on purpose (no `default`): a new part kind is a
 * compile error here rather than a span that silently summarizes without it.
 */
function renderPart(role: string, p: Part): string {
  switch (p.type) {
    case "text":
    case "reasoning":
      return `${role}: ${p.text}`;
    case "tool_call":
      return `${role}: [tool ${p.name}] ${clip(stringify(p.input), PART_CLIP)}`;
    case "tool_result":
      return `tool_result${p.isError ? " (error)" : ""}${p.interrupted ? " (interrupted)" : ""}: ${
        clip(stringify(p.output), PART_CLIP)
      }`;
    case "image":
      return `${role}: [image ${p.name}]`;
    case "ask":
      // A settled ask() Q/A: what was asked and how the human resolved it. The answer
      // is often the decision the rest of the span rests on.
      return `ask: ${p.question} → ${
        p.status === "answered" ? `user answered: ${p.answer ?? ""}` : p.status
      }`;
    case "workflow":
      // Kept in the summary even though replay drops it: a compacted span is the
      // only place left that remembers a fan-out was launched here once the
      // `[workflow done]` note has itself been compacted away.
      return `${role}: [workflow ${p.name}] ${clip(p.description, PART_CLIP)}`;
  }
}

/**
 * Messages rendered as a plain transcript for an LLM prompt.
 *
 * Exported because handoff (T8.7) renders the same thing for a different prompt, and a
 * second renderer would drift the moment a part kind is added.
 */
export function renderSpan(messages: readonly Message[]): string {
  return messages
    .flatMap((m) => (m.parts.length ? m.parts.map((p) => renderPart(m.role, p)) : [`${m.role}:`]))
    .join("\n");
}

/** One maximal run of adjacent selected messages — what becomes a single summary. */
interface Run {
  start: number;
  end: number;
  span: Message[];
}

/**
 * Group picked thread indexes into maximal runs of ADJACENT messages. Pure, and the
 * whole of the non-contiguous rule: two picks separated by an unselected message are
 * two runs, and therefore two summaries with that message copied between them.
 */
export function runsOf(picked: readonly { idx: number; view: Message }[]): Run[] {
  const runs: Run[] = [];
  for (const p of picked) {
    const last = runs.at(-1);
    if (last && p.idx === last.end + 1) {
      last.end = p.idx;
      last.span.push(p.view);
    } else runs.push({ start: p.idx, end: p.idx, span: [p.view] });
  }
  return runs;
}

/**
 * The model that summarizes.
 *
 * NOTE: the old tree read `ctx.model ?? BOUGH_MODEL ?? <default>`, ignoring the
 * session's own pin. Here it is resolved exactly as the turn runner resolves it —
 * session pin, then the global default, then `DEFAULT_MODEL` — because a model id IS a
 * provider routing decision (`llm/client.ts`): a session pinned to an OpenAI or
 * OpenRouter model belongs to a user who may hold only that provider's key, and
 * summarizing it on the Anthropic default would fail the compaction with an auth error
 * on a conversation that runs fine.
 */
function modelFor(ctx: CompactCtx, session: Session): string {
  return session.model ?? ctx.model ?? DEFAULT_MODEL;
}

/**
 * Summarize a span of messages. Exported because a BRANCH SWITCH needs the same
 * thing compaction does: pi's `/tree` carries "the essence of abandoned work
 * without all the token-heavy details" onto the new path, and that is this
 * function with a different span (`history/fork.ts`).
 */
export async function summarizeSpan(
  ctx: CompactCtx,
  model: string,
  span: readonly Message[],
  instructions?: string,
): Promise<string> {
  return await summarize(ctx, model, span, instructions);
}

async function summarize(
  ctx: CompactCtx,
  model: string,
  span: readonly Message[],
  instructions?: string,
  notes?: string | null,
): Promise<string> {
  const llm = ctx.llm ?? clientFor(model);
  const rendered = renderSpan(span);
  const parts = [rendered];
  if (notes) parts.push(`Scout notes — the files this span touched, as they are now:\n${notes}`);
  if (instructions) parts.push(`Additional instructions: ${instructions}`);
  const prompt = parts.join("\n\n");
  const text = (await completeText(llm, {
    model,
    system: notes ? SYSTEM + SCOUT_SYSTEM : SYSTEM,
    maxTokens: MAX_TOKENS,
    prompt,
  }))
    .trim();
  // An empty summary is not a summary. Seeding it would put an empty message where a
  // span of work used to be — a branch that silently lost the span rather than
  // compacting it. Raised before anything is written, so the branch never exists.
  if (!text) {
    throw new CompactError(
      502,
      `the summarizer (${model}) returned no text for a span of ${span.length} message(s) — ` +
        `nothing was written; retry, or narrow the selection`,
    );
  }
  return text;
}

/**
 * Reject a pick that is not one of the session's own messages, with the error that says
 * what to do about it.
 *
 * NOTE: the old tree folded this into its resolve step and answered "picks must be
 * messages of this session" for every case. Naming the ancestor is the difference
 * between an error the user can act on and one they cannot: the operation they want
 * exists, it just runs on a different session (spec §14).
 */
function assertOwnMessages(
  db: Db,
  session: Session,
  picks: readonly PartPick[],
  own: readonly Message[],
): void {
  const ownIds = new Set(own.map((m) => m.id));
  let ancestors: Set<string> | null = null;
  for (const p of picks) {
    if (ownIds.has(p.messageId)) continue;
    ancestors ??= new Set(db.ancestorChain(session.id).slice(0, -1).map((s) => s.id));
    const foreign = db.getMessage(p.messageId);
    if (foreign && ancestors.has(foreign.sessionId)) {
      throw new CompactError(
        400,
        `message ${p.messageId} belongs to ancestor session ${foreign.sessionId}, not to ` +
          `${session.id} — a compaction branches a sibling of the session it compacts, so it ` +
          `can only replace that session's own turns. Compact ${foreign.sessionId} instead.`,
      );
    }
    throw new CompactError(
      400,
      `message ${p.messageId} is not a message of session ${session.id}` +
        (foreign ? ` (it belongs to ${foreign.sessionId})` : ""),
    );
  }
}

/**
 * Compact the selected messages of `sessionId` onto a new compaction branch and return
 * the new session.
 *
 * Each maximal contiguous run of selected messages is replaced in place by one summary;
 * everything unselected is copied verbatim. The source session is never touched.
 * Throws `CompactError` — 404 for an unknown session, 400 for a selection this
 * operation cannot express, 502 for a summarizer that produced nothing.
 */
export async function compact(
  ctx: CompactCtx,
  sessionId: string,
  args: CompactBody,
): Promise<Session> {
  const session = ctx.db.getSession(sessionId);
  if (!session) throw new CompactError(404, `session ${sessionId} not found`);

  // The schema already rejects an empty selection at the router edge (`CompactBody`),
  // but this function is also called directly — by a test, and by whatever calls it
  // next — and an empty selection would otherwise reach the seeding loop and index
  // `picked[-1]`. A 400 is the same answer, stated where the assumption lives.
  if (args.picks.length === 0) {
    throw new CompactError(400, "compaction needs at least one picked message");
  }

  const own = ctx.db.messagesFor(sessionId);
  if (own.length === 0) {
    throw new CompactError(400, `session ${sessionId} has no messages of its own to compact`);
  }
  assertOwnMessages(ctx.db, session, args.picks, own);

  // Resolved against the session's OWN messages, so a thread index here is an index into
  // exactly the sequence the branch re-seeds. `resolvePicks` merges duplicate picks,
  // validates part ranges, and restores thread order — a user shift-clicking upward
  // sends a selection, not a sequence.
  const picked = resolvePicks(own, args.picks, (m) => new CompactError(400, m));
  const runs = runsOf(picked);

  // Every summary before the first write (see the header). `Promise.all` rather than
  // `allSettled`: one failed summary means this compaction cannot be expressed, and
  // there is nothing partial worth keeping — unlike a fan-out of independent launches
  // (plan §6.9), where the siblings that started are real work.
  const model = modelFor(ctx, session);
  // The runtime is read HERE rather than at the seeder, because the scout needs the
  // workspace and the scout runs before the first summary — see the ordering note in
  // the header: everything that can fail happens before anything is written.
  const runtime = ctx.db.getSessionRuntime(sessionId);
  // One scout PER RUN, concurrently with nothing: each run is a separate summary about
  // a separate stretch of work, and pointing one scout at the union of their files
  // would scope every summary by every other run's subject. Failures are already `null`
  // inside `exploreSpan`, so this cannot reject.
  // No workspace, no scout: there is no checkout to read, and the transcript is then
  // all there ever was to summarize from.
  const workspace = runtime.workspace;
  const explore = ctx.explore ?? ((span: readonly Message[], dir: string) =>
    exploreSpan({ sessionId, workspace: dir }, span));
  const notes = workspace
    ? await Promise.all(runs.map((r) => explore(r.span, workspace)))
    : runs.map(() => null);
  const summaries = await Promise.all(
    runs.map((r, i) => summarize(ctx, model, r.span, args.instructions, notes[i])),
  );

  const seeder = openBranch(ctx, {
    // The TARGET'S parent — a sibling, not a child. This is the whole mechanism.
    parentId: session.parentId,
    title: compactionTitle(picked.length),
    kind: "compaction",
    // NOTE: not in the old tree, which snapshotted workspaces. A compaction continues
    // the same work in the same checkout, so it inherits both the workspace and the
    // `base` sha its change set is measured from — otherwise the branch shows no
    // changes for work that is plainly in the tree (spec §13, `branch.ts`).
    workspace: runtime.workspace,
    base: runtime.base,
    originDir: session.originDir ?? null,
    originId: session.id, // lineage: the compacted session…
    originMessageId: own[picked[picked.length - 1].idx].id, // …and the last picked message
  });

  // Seed in thread order: copies of the unselected messages, each run swapped for its
  // one summary. The shared ancestors come from thread-through-parents.
  let run = 0;
  for (let i = 0; i < own.length; i++) {
    if (run < runs.length && i === runs[run].start) {
      // `supervisor`, not `system`: the summary stands in for a stretch of the
      // conversation and replays as an assistant message, which is what makes the
      // compacted thread read — and replay — as a continuation rather than as a
      // harness note about one (spec §4).
      seeder.add("supervisor", [{ type: "text", text: summaries[run] } satisfies Part]);
      i = runs[run].end; // skip the rest of the run (the loop's i++ lands on end+1)
      run++;
    } else {
      seeder.copy(own[i]);
    }
  }

  const branch = inheritPins(ctx, session, seeder.session);
  retitle(ctx, branch, summaries[0], picked.length);
  return branch;
}

/**
 * Carry the source's per-session model/effort pins onto the branch.
 *
 * NOTE: not in the old tree, which had no per-session pins to carry. A compaction is a
 * CONTINUATION of the same conversation — the user compacts precisely so they can keep
 * going — so a branch that dropped the pin would silently move the next turn onto the
 * global default. On a session pinned to another provider that is not a preference
 * change but a different vendor, a different price and possibly a missing key (spec
 * §12: switching the model leaves other existing sessions alone).
 */
function inheritPins(ctx: CompactCtx, source: Session, branch: Session): Session {
  if (!source.model && !source.effort) return branch;
  if (source.model) ctx.db.setSessionModel(branch.id, source.model);
  if (source.effort) ctx.db.setSessionEffort(branch.id, source.effort);
  const stored = ctx.db.getSession(branch.id);
  if (!stored) return branch;
  ctx.bus.publish({ type: "session.updated", sessionId: stored.id, data: stored });
  return stored;
}

/**
 * The deterministic branch title.
 *
 * NOTE: deliberately NOT built on `baseTitle(session.title)` the way fork's is. Its
 * strip list does not include this prefix, so composing "compacted · <base>" would
 * accumulate — compact a compaction and the picker shows "compacted · compacted · X".
 * A standalone label cannot accumulate, and the cheap tier replaces it with something
 * about the content as soon as the first summary exists.
 */
function compactionTitle(picks: number): string {
  return `compacted · ${picks} turn${picks === 1 ? "" : "s"}`;
}

/**
 * Name the branch from its first summary. Fire-and-forget by design: the response
 * carries the branch immediately, and the rename lands as a `session.updated` when (and
 * only if) the cheap tier answers.
 *
 * Two guards, both about not overwriting a fact the user established: the rename is
 * skipped if the branch's title is no longer the placeholder — the user renamed it
 * first, or a previous rename already landed — and every failure is swallowed, because
 * a cosmetic title must never turn a completed compaction into an error (spec §12).
 */
function retitle(ctx: CompactCtx, branch: Session, summary: string, picks: number): void {
  if (!ctx.cheap) return;
  const placeholder = branch.title;
  ctx.cheap.title(summary)
    .then((title) => {
      if (!title) return;
      if (ctx.db.getSession(branch.id)?.title !== placeholder) return;
      ctx.db.setSessionTitle(branch.id, `${title} · compacted ${picks}`);
      const updated = ctx.db.getSession(branch.id);
      if (updated) {
        ctx.bus.publish({ type: "session.updated", sessionId: branch.id, data: updated });
      }
    })
    .catch(() => {});
}

// ---------------------------------------------------------------------------
// REST
// ---------------------------------------------------------------------------

/**
 * `POST /sessions/:id/compact` — 201 with the new compaction branch.
 *
 * A `function` DECLARATION, not a `const` arrow, and that is load-bearing: this module
 * and `server/app.ts` form an import cycle (app.ts imports this handler for its route
 * table; this file imports app.ts's `json`/`parseBody`). Hoisted declarations exist from
 * module instantiation, so the route table can always read this binding; a `const` would
 * be in its temporal dead zone whenever this module is evaluated first — which is
 * exactly what `compact.test.ts` does.
 *
 * 201 because a compaction CREATES a session, the same as `POST /sessions`. The thread
 * rides along for the same reason `GET /sessions/:id` carries it: the client is about to
 * switch to this branch, and a create that answered with a bare session would force an
 * immediate second fetch to render anything at all. It is `threadFor`, not the seeded
 * messages — the inherited ancestors are half of what the user will be looking at, and
 * they were never seeded (see the header).
 */
export async function compactH(
  req: Request,
  ctx: AppCtx,
  params: Record<string, string>,
): Promise<Response> {
  const body = await parseBody(req, CompactBody);
  const session = await compact(ctx, params.id, body);
  return json({ session, thread: ctx.db.threadFor(session.id) }, 201);
}
