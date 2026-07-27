/**
 * Topic sections — an LLM partitions a conversation's turns into contiguous stretches
 * labeled by WHAT THE WORK WAS ABOUT ("token refresh race", "theme picker", "gap
 * analysis"), so the client can color history and offer a whole section as one
 * selection for compaction or extraction (spec §14).
 *
 * THE INVARIANT THIS HOLDS: **this is a stateless labeling pass, and the CLIENT decides
 * what a turn is.** The request carries one gist per turn, in thread order, and index i
 * of the reply is turn i of the request. Nothing is read from the database, nothing is
 * written to it, and there is no `sections` table, column or cache anywhere in the tree.
 *
 * Why the gists come from the client rather than being re-derived here from
 * `threadFor`: the returned ranges are only useful if they line up with the rows the
 * user is looking at, and turn grouping is a CLIENT decision — which messages fold
 * together, whether a system note starts a turn, how a subagent rail collapses. A server
 * that re-derived boundaries would answer "turns 3–5" about a sequence the user cannot
 * see, and the selection the label offers would highlight the wrong rows. Sending the
 * gists makes the alignment structural instead of a convention two codebases have to
 * keep agreeing on.
 *
 * That the pass is stateless is also what makes it safe to run repeatedly: the labels
 * are a VIEW, so a client that re-labels after three more turns gets a partition of the
 * new history rather than a stale one stitched onto it, and nothing has to be
 * invalidated.
 *
 * Ported from `src/sections.ts`. Deltas from that port are marked `NOTE:`.
 */
import { z } from "zod";
import { NotFoundError, SectionsError } from "../errors.ts";
import { clientFor, completeText } from "../llm/client.ts";
import { SectionsBody } from "../schema/requests.ts";
import type { AppCtx, LlmClient } from "../types.ts";
import { json, parseBody } from "../server/http.ts";

/** One labeled stretch of history. Inclusive, 0-based, in the request's turn indexes. */
export interface Section {
  start: number;
  end: number;
  /** What this stretch was about, in the model's words. */
  label: string;
}

export interface SectionsCtx {
  /** Injected in tests. Absent = the provider-routed client for the cheap model. */
  llm?: LlmClient;
}

/**
 * Labeling is a cheap classification pass over one line per turn — always the small
 * model, NEVER the session's (possibly frontier) supervisor model, and deliberately not
 * `ctx.model`: a user pinned to Opus for the coding work should not pay Opus rates to
 * put seven-word headings on their history (spec §12's two tiers).
 */
export const SECTIONS_MODEL = "claude-haiku-4-5";
const MAX_TOKENS = 1500;

const SYSTEM = "You label the history of a coding-agent conversation. Given numbered turns " +
  "(each one line: the user's request and a gist of the reply), partition ALL turns into " +
  "contiguous sections BY TOPIC — group consecutive turns that are about the same piece of " +
  "work, and start a new section where the subject genuinely changes. Do NOT categorize by " +
  "activity type (debugging vs editing); the label says WHAT the work was about, in concrete " +
  "terms a reader scanning history would recognize (name the feature, bug, file, or question " +
  "— not 'various requests' or 'misc tasks'). Prefer fewer, broader sections over one per " +
  "turn.\n" +
  'Reply with JSON only, no prose: [{"start":0,"end":2,"label":"auth token refresh race"}] ' +
  "— start/end are inclusive 0-based turn indexes, labels at most 7 words.";

const LlmSections = z.array(z.object({
  start: z.number().int().min(0),
  end: z.number().int().min(0),
  label: z.string(),
}));
type LlmSections = z.infer<typeof LlmSections>;

/** The longest label the UI is asked to render; anything longer is the model ignoring
 * the "7 words" instruction, and a rail is not the place to find that out. */
const LABEL_MAX = 60;

/**
 * Parse the model's reply, tolerating code fences and surrounding prose.
 *
 * Pure and exported so the tolerance is directly testable: "```json\n[…]\n```" and
 * "Here you go: […]" are the two shapes a chat model actually returns when told to emit
 * JSON only, and both are ordinary answers rather than failures.
 */
export function parseSections(text: string): LlmSections | null {
  const lo = text.indexOf("[");
  const hi = text.lastIndexOf("]");
  if (lo < 0 || hi <= lo) return null;
  try {
    const parsed = LlmSections.safeParse(JSON.parse(text.slice(lo, hi + 1)));
    return parsed.success ? parsed.data : null;
  } catch {
    return null;
  }
}

/**
 * Force a possibly-sloppy answer into a clean PARTITION of `[0, n)`: sorted, clipped to
 * bounds, overlaps trimmed, gaps filled with "…".
 *
 * The client renders these ranges directly and offers them as selections, so a gap would
 * be turns the user can see and cannot select, and an overlap would be one turn claiming
 * two labels. Normalizing here rather than re-prompting is the right trade for a
 * cosmetic pass: a slightly mislabeled boundary is a usable answer, a second round-trip
 * is a stall.
 */
export function normalizeSections(raw: LlmSections, n: number): Section[] {
  const sorted = raw
    .filter((s) => s.start < n && s.start <= s.end)
    .map((s) => ({
      start: s.start,
      end: Math.min(s.end, n - 1),
      label: s.label.slice(0, LABEL_MAX),
    }))
    .sort((a, b) => a.start - b.start || a.end - b.end);
  const out: Section[] = [];
  let next = 0;
  for (const s of sorted) {
    if (s.end < next) continue; // fully covered by an earlier section
    const start = Math.max(s.start, next);
    if (start > next) out.push({ start: next, end: start - 1, label: "…" });
    out.push({ start, end: s.end, label: s.label });
    next = s.end + 1;
  }
  if (next < n) out.push({ start: next, end: n - 1, label: "…" });
  return out;
}

/**
 * Partition `turns` into topic-labeled sections. Stateless: no database, no session, no
 * storage. Throws `SectionsError(502)` when the model's output cannot be parsed at all.
 */
export async function sectionize(
  ctx: SectionsCtx,
  turns: readonly { gist: string }[],
): Promise<Section[]> {
  const llm = ctx.llm ?? clientFor(SECTIONS_MODEL);
  // One line per turn, numbered — the numbers ARE the contract with the reply, so a
  // gist containing a newline is flattened rather than shifting every index after it.
  const prompt = turns.map((t, i) => `${i}. ${t.gist.replaceAll("\n", " ")}`).join("\n");
  const text = await completeText(llm, {
    model: SECTIONS_MODEL,
    system: SYSTEM,
    maxTokens: MAX_TOKENS,
    prompt,
  });
  const raw = parseSections(text);
  if (!raw) {
    throw new SectionsError(
      502,
      `section labeling failed: ${SECTIONS_MODEL} returned no parseable JSON array ` +
        `(nothing was stored — history is unchanged; retry, or scroll without labels)`,
    );
  }
  return normalizeSections(raw, turns.length);
}

// ---------------------------------------------------------------------------
// REST
// ---------------------------------------------------------------------------

/**
 * `POST /sessions/:id/sections` — `{sections}` for the turns in the body.
 *
 * A `function` DECLARATION for the same reason as every other handler outside
 * `server/`: this module and `server/app.ts` form an import cycle (app.ts imports this
 * for its route table, this imports app.ts's `json`/`parseBody`), and only a hoisted
 * declaration is guaranteed initialized whichever module evaluates first.
 *
 * NOTE: the session id is VALIDATED and then unused — the labeling itself never touches
 * the database. It is checked because the URL claims a session: a client sending a stale
 * or mistyped id gets a 404 instead of a paid LLM round-trip whose ranges point at a
 * thread nobody is looking at.
 */
export async function sectionsH(
  req: Request,
  ctx: AppCtx,
  params: Record<string, string>,
): Promise<Response> {
  if (!ctx.db.getSession(params.id)) {
    throw new NotFoundError(`session ${params.id} not found`);
  }
  const body = await parseBody(req, SectionsBody);
  return json({ sections: await sectionize(ctx, body.turns) });
}
