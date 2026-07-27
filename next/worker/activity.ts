/**
 * Live activity blurbs: a present-tense one-liner describing what a session is doing
 * right now — "running the test suite", "rewriting the patch parser" — derived from the
 * `run_steps` program as it goes by. The third of the cheap tier's three features
 * (spec §12).
 *
 * THE INVARIANT THIS HOLDS: **one in-flight blurb per session — rounds that land while
 * it is busy are DROPPED, not queued** (plan §6.11). This is the rule the whole module
 * is built around, and the reasoning is worth stating because "queue them" looks like
 * the kinder choice.
 *
 * A blurb describes the round it was generated from. A queue makes the tier's cost
 * scale with the session's round rate rather than with its own latency, so a fast
 * agent — exactly the case where the live map is interesting — buys a blurb for every
 * round and displays them minutes late, each one narrating a program that finished long
 * ago. Dropping bounds the spend at one call per session at a time AND keeps every
 * blurb that does appear true of something recent, because the next round describes
 * itself. Latency and cost fall out of the same rule; there is no version of this where
 * queueing is better.
 *
 * SECOND INVARIANT: **nothing here persists.** Blurbs are ephemeral `session.activity`
 * events and there is no table, no column and no cache — the UI keeps the latest per
 * session and a reconnecting client simply has none until the next round, which is
 * correct: a stale "running the test suite" restored from a database would be a claim
 * about a process that is not running.
 *
 * WHY IT IS A BUS LISTENER, wired only in the server entry. The blurb is a function of
 * an event that is already published, so subscribing costs the turn runner nothing and
 * — crucially — a turn test that builds its own ctx never gets one. Turn tests stay
 * hermetic and offline by construction rather than by remembering to stub a tier.
 *
 * Ported from `src/worker/activity.ts` (the listener shape and the drop rule). New
 * here: the frozen `SessionActivityData` payload, clearing the blurb when the turn
 * ends, and the deferred publish.
 */
import type { Part, ToolCallPart } from "../schema/parts.ts";
import type { AppCtx, Bus } from "../types.ts";
import { type CheapCallOpts, cheapText } from "./titles.ts";

// ---------------------------------------------------------------------------
// Prompt shaping (pure)
// ---------------------------------------------------------------------------

export const ACTIVITY_SYSTEM = [
  "You describe what a coding agent is doing, for a live status line. Given the",
  "JavaScript program it is about to run, reply with one present-participle phrase",
  "of at most six words — 'running the test suite', 'rewriting the patch parser'.",
  "No quotes, no trailing period, no preamble, no code.",
].join(" ");

/** The longest blurb a one-line status is asked to render. */
export const MAX_BLURB = 60;

/** How much of the program the model is shown. The head says what it is going to do. */
export const MAX_CODE_CHARS = 1500;

/**
 * The program as prompt text. Truncated from the HEAD, unlike ghost text's tail-keeping
 * and for the opposite reason: a program's opening lines are its intent, and the last
 * 1,500 characters of a long one are usually output formatting.
 */
export function programGist(code: string): string {
  const head = code.length > MAX_CODE_CHARS ? code.slice(0, MAX_CODE_CHARS) + "\n…" : code;
  return `The program:\n${head}\n\nWhat is it doing?`;
}

/** First real line, unquoted, capped, trailing period dropped; `null` if unusable. */
export function sanitizeBlurb(raw: string): string | null {
  const line = raw.trim().split("\n").map((l) => l.trim()).find((l) => l.length > 0) ?? "";
  const clean = line
    .replace(/^["'`]+|["'`.]+$/g, "")
    .slice(0, MAX_BLURB)
    .trim();
  return clean.length > 0 ? clean : null;
}

// ---------------------------------------------------------------------------
// The cheap-tier method
// ---------------------------------------------------------------------------

/**
 * `CheapTier.activity`. Resolves the sanitized blurb, or `null` — never rejects.
 *
 * `maxTokens` is 32, the smallest of the three: six words is the whole answer, and a
 * cap this tight is also the cheapest guard against a model that decides to explain the
 * program instead of naming it.
 */
export async function cheapActivity(
  recent: string,
  opts: Partial<CheapCallOpts> = {},
): Promise<string | null> {
  if (!recent.trim()) return null;
  const raw = await cheapText({
    system: ACTIVITY_SYSTEM,
    prompt: recent,
    maxTokens: 32,
    ...opts,
  });
  return raw === null ? null : sanitizeBlurb(raw);
}

// ---------------------------------------------------------------------------
// The watcher
// ---------------------------------------------------------------------------

/** The `run_steps` code in a part, or `null` for every other part. Pure. */
export function programOf(part: Part | undefined): string | null {
  if (!part || part.type !== "tool_call") return null;
  const call = part as ToolCallPart;
  if (call.name !== "run_steps") return null;
  const code = (call.input as { code?: unknown } | null | undefined)?.code;
  return typeof code === "string" && code.trim() ? code : null;
}

/** What the watcher needs off the app context. `cheap` absent = the feature is off. */
export interface ActivityCtx {
  bus: Bus;
  cheap?: AppCtx["cheap"];
}

/**
 * Start publishing activity blurbs. Returns the unsubscribe.
 *
 * Two triggers and one ledger:
 *
 *   - a `message.part` carrying a `run_steps` call starts a blurb, UNLESS this session
 *     already has one in flight, in which case the round is dropped;
 *   - `turn.finished` clears the session's blurb (`activity: null`), because a status
 *     line that keeps claiming work after the turn ended is worse than an empty one.
 *
 * The listener body is synchronous and does no I/O: the bus fans out synchronously, so
 * anything slow here would be latency charged to whoever published — which for
 * `message.part` is the turn runner, mid-stream. All it does is start a promise nobody
 * holds, which is the literal shape of "fire-and-forget".
 *
 * The clear is deferred to a microtask so the `session.activity` event cannot be
 * stamped and delivered from INSIDE the `turn.finished` fan-out, which would put it
 * ahead of the event that caused it for every subscriber registered after this one.
 */
export function watchActivity(ctx: ActivityCtx): () => void {
  /** Sessions with a call in flight. Membership IS the drop rule. */
  const inflight = new Set<string>();
  /**
   * Bumped whenever a session's turn ends. A blurb carries the value it started at,
   * so an answer that arrives after its turn finished is discarded instead of
   * repainting a status line for work that is over — the same staleness the clear
   * below exists to prevent, arriving from the other direction.
   */
  const epoch = new Map<string, number>();

  return ctx.bus.subscribe((e) => {
    const cheap = ctx.cheap;
    if (!cheap || !e.sessionId) return;
    const sessionId = e.sessionId;

    if (e.type === "turn.finished") {
      epoch.set(sessionId, (epoch.get(sessionId) ?? 0) + 1);
      queueMicrotask(() => {
        ctx.bus.publish({
          type: "session.activity",
          sessionId,
          data: { sessionId, activity: null },
        });
      });
      return;
    }

    if (e.type !== "message.part") return;
    const code = programOf((e.data as { part?: Part } | undefined)?.part);
    if (!code) return;

    // THE DROP RULE. Not a queue, not a debounce, and not a replacement of the pending
    // call: the round is simply not described, and the next one will describe itself.
    // See the header for why this is the better failure and not the lazier one.
    if (inflight.has(sessionId)) return;
    inflight.add(sessionId);
    const started = epoch.get(sessionId) ?? 0;

    // `.catch` on a method the type says cannot reject, because an injected
    // implementation is not bound by the type and an unhandled rejection is a
    // process-level event rather than a missing blurb.
    Promise.resolve(cheap.activity(programGist(code)))
      .then((activity) => {
        if (!activity) return;
        if ((epoch.get(sessionId) ?? 0) !== started) return;
        ctx.bus.publish({ type: "session.activity", sessionId, data: { sessionId, activity } });
      })
      .catch(() => {})
      .finally(() => inflight.delete(sessionId));
  });
}
