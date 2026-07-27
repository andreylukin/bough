/**
 * Composer ghost text: the message the user was probably about to type, predicted from
 * the conversation so far — "run the tests", "commit it", "fix the thing you flagged".
 * The second of the cheap tier's three features (spec §12).
 *
 * THE INVARIANT THIS HOLDS: **the ghost is never on a turn's path.** It is fetched by
 * the composer over its own request, resolves to a suggestion or to `null`, and no
 * other surface reads it. That is what makes the whole feature safe to bill on: the
 * only thing a failed ghost costs is a grey line that does not appear, and the route
 * says so by answering `200 {ghost: null}` rather than an error status. A 5xx here
 * would put a red banner on a composer for a feature whose entire value is that you
 * can ignore it.
 *
 * WHY IT READS THE THREAD RATHER THAN THE COMPOSER'S PREFIX. A prefix completion needs
 * the user to have started typing, and the moment the suggestion is worth most is the
 * empty composer right after the agent finished — the "and now what" moment. So the
 * prompt is the conversation TAIL, and the typed prefix (when there is one) is an
 * additional constraint on the answer, not the whole of it.
 *
 * WHY LONG LINES KEEP THEIR TAIL. `renderConvo` truncates from the FRONT, which is
 * backwards from every other truncation in the tree and deliberate: an agent's reply
 * ends with the outcome and what it proposes next, and that ending is the entire
 * signal for predicting the follow-up. Keeping the head would feed the model the
 * preamble and drop the conclusion.
 *
 * Ported from `src/worker/suggest.ts` (the prompt, `renderConvo`'s tail-keeping, the
 * sanitizer and the caps). The local-worker path is gone — spec §17 rules out local
 * inference — and the thread reduction and route are new.
 */
import { z } from "zod";
import { NotFoundError } from "../errors.ts";
import type { Message } from "../schema/parts.ts";
import type { AppCtx } from "../types.ts";
import { json, parseBody } from "../server/http.ts";
import { type CheapCallOpts, cheapText } from "./titles.ts";

// ---------------------------------------------------------------------------
// Prompt shaping (pure)
// ---------------------------------------------------------------------------

export const GHOST_SYSTEM = [
  "You predict the next message a user will type to their coding agent, given",
  "the conversation so far. Reply with that message only: one line, short and",
  "concrete — the natural next step (fix what the agent flagged, run the tests,",
  "commit, extend the change). No quotes, no explanation, no 'user:' label.",
].join(" ");

/** How many trailing turns of context the prediction gets. */
export const MAX_LINES = 8;
/** Per-line budget. Lines longer than this keep their TAIL; see the header. */
export const MAX_LINE_CHARS = 600;
/** The longest suggestion the composer will render as ghost text. */
export const MAX_SUGGESTION = 150;

/** One conversation line, already reduced to its text. */
export interface ConvoLine {
  role: "user" | "agent";
  text: string;
}

/**
 * The thread as conversation lines. Pure, and the reason the route needs nothing from
 * the turn runner: a prediction is a function of what is already persisted.
 *
 * `system` messages are included as `user` lines because that is exactly how they
 * replay to the model (spec §4: harness-injected notes "replay to the model as
 * user-side text") — a detached subagent's report is often the very thing the user's
 * next message is about.
 */
export function convoFrom(messages: readonly Message[]): ConvoLine[] {
  const lines: ConvoLine[] = [];
  for (const m of messages) {
    const text = m.parts
      .filter((p): p is { type: "text"; text: string } => p.type === "text")
      .map((p) => p.text)
      .join("\n")
      .trim();
    if (!text) continue;
    lines.push({ role: m.role === "supervisor" ? "agent" : "user", text });
  }
  return lines;
}

/** The conversation tail as prompt text, oldest first. */
export function renderConvo(lines: readonly ConvoLine[]): string {
  return lines
    .slice(-MAX_LINES)
    .map((l) => {
      const text = l.text.length > MAX_LINE_CHARS ? "…" + l.text.slice(-MAX_LINE_CHARS) : l.text;
      return `${l.role}: ${text}`;
    })
    .join("\n");
}

/**
 * The full prompt. `prefix` is what the user has already typed; when present the model
 * is told to CONTINUE it, because a suggestion that ignores the half-written sentence
 * in front of the cursor is worse than no suggestion.
 */
export function ghostPrompt(lines: readonly ConvoLine[], prefix = ""): string {
  const convo = `Conversation, oldest first:\n${renderConvo(lines)}`;
  const typed = prefix.trim();
  return typed
    ? `${convo}\n\nThe user has started typing: ${typed}\n` +
      `Complete it as the whole next message, starting from what they typed:`
    : `${convo}\n\nThe user's next message:`;
}

/** First real line of the reply, unlabeled, unquoted and capped; `null` if unusable. */
export function sanitizeSuggestion(raw: string): string | null {
  const line = raw.trim().split("\n").map((l) => l.trim()).find((l) => l.length > 0) ?? "";
  const clean = line
    .replace(/^(user|next|suggestion)\s*:\s*/i, "")
    .replace(/^["'`]+|["'`]+$/g, "")
    .slice(0, MAX_SUGGESTION)
    .trim();
  return clean.length > 0 ? clean : null;
}

// ---------------------------------------------------------------------------
// The cheap-tier method
// ---------------------------------------------------------------------------

/**
 * `CheapTier.ghostText`. Resolves the sanitized suggestion, or `null` — never rejects.
 *
 * Takes the assembled prompt rather than the thread, so the tier stays a set of three
 * string-in/string-out methods that a test can replace with three stubs, and so the
 * shaping above stays pure and directly testable.
 */
export async function cheapGhost(
  prompt: string,
  opts: Partial<CheapCallOpts> = {},
): Promise<string | null> {
  if (!prompt.trim()) return null;
  const raw = await cheapText({
    system: GHOST_SYSTEM,
    prompt,
    maxTokens: 64,
    ...opts,
  });
  return raw === null ? null : sanitizeSuggestion(raw);
}

// ---------------------------------------------------------------------------
// The feature
// ---------------------------------------------------------------------------

/**
 * Predict the next message for a session. `null` for an empty conversation, an absent
 * cheap tier, or any failure — the three are the same non-answer to the composer.
 *
 * Never throws for a cheap-model reason. A missing session is the caller's bug and is
 * the route's business, not this function's; it is checked in the handler.
 */
export async function ghostFor(
  ctx: Pick<AppCtx, "db" | "cheap">,
  sessionId: string,
  prefix = "",
): Promise<string | null> {
  if (!ctx.cheap) return null;
  const lines = convoFrom(ctx.db.threadFor(sessionId));
  if (lines.length === 0) return null;
  try {
    return await ctx.cheap.ghostText(ghostPrompt(lines, prefix));
  } catch {
    // The type says this cannot happen; an injected implementation is not bound by
    // the type, and a rejected ghost must not become a 500 on the composer.
    return null;
  }
}

/** `POST /sessions/:id/ghost` — `{prefix?}` in, `{ghost}` out. */
const GhostBody = z.object({ prefix: z.string().optional() }).strict();

/**
 * `POST /sessions/:id/ghost` — always 200 `{ghost: string | null}` for a session that
 * exists.
 *
 * A `function` DECLARATION rather than a `const` arrow, like every other handler this
 * tree's route table reads at module scope: a hoisted declaration exists from module
 * instantiation, a `const` would sit in its temporal dead zone whenever this module
 * evaluates before `server/app.ts`.
 *
 * POST rather than GET despite reading nothing: the typed prefix is user text that has
 * no business in a URL or a server log, and it can be longer than a query string
 * comfortably carries.
 *
 * 404 is the only failure. An unknown session id means the composer is asking about
 * something that is not there — a real client bug worth surfacing — whereas everything
 * downstream of that is a cheap-model outcome and answers `{ghost: null}`.
 */
export async function ghostTextH(
  req: Request,
  ctx: AppCtx,
  params: Record<string, string>,
): Promise<Response> {
  if (!ctx.db.getSession(params.id)) throw new NotFoundError(`session ${params.id} not found`);
  const body = await parseBody(req, GhostBody, {});
  return json({ ghost: await ghostFor(ctx, params.id, body.prefix ?? "") });
}
