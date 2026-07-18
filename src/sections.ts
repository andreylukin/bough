/**
 * Section grouping: an LLM partitions a conversation's turns into contiguous
 * sections BY TOPIC — what the work was about ("token refresh race", "theme
 * picker", "gap-analysis research") — so the TUI can color-code history and
 * offer whole sections as range-op selections (compact/extract/delete). The
 * label IS the content; there is no fixed activity taxonomy.
 *
 * The CLIENT sends the turn gists (one line per user-turn, derived from the same
 * tree the selection UI is built on) rather than the server re-deriving turn
 * boundaries — that guarantees the returned indexes align with what the user
 * sees, whatever the client's grouping rules are. Stateless and read-only: a
 * labeling pass over excerpts, nothing is stored.
 */
import { z } from "zod";
import { anthropicClient, type LlmClient } from "./supervisor/llm.ts";

export const SectionsBody = z.object({
  /** One gist per turn, thread order — index i is turn i in the reply. */
  turns: z.array(z.object({ gist: z.string().max(500) })).min(1).max(500),
});
export type SectionsBody = z.infer<typeof SectionsBody>;

export interface Section {
  /** Inclusive 0-based turn range. */
  start: number;
  end: number;
  /** What this stretch of the conversation was about, in the LLM's words. */
  label: string;
}

export class SectionsError extends Error {
  constructor(readonly status: number, message: string) {
    super(message);
    this.name = "SectionsError";
  }
}

export interface SectionsCtx {
  /** Injected for tests; defaults to the real Anthropic client. */
  llm?: LlmClient;
}

// Labeling is a cheap classification pass — always the small model, never the
// session's (possibly frontier) supervisor model.
const MODEL = "claude-haiku-4-5";
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

/** Parse the model's reply (tolerating code fences / surrounding prose). */
export function parseSections(text: string): z.infer<typeof LlmSections> | null {
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
 * Force a possibly-sloppy model answer into a clean partition of [0, n):
 * sorted, clipped to bounds, overlaps trimmed, gaps filled with "…".
 */
export function normalizeSections(raw: z.infer<typeof LlmSections>, n: number): Section[] {
  const sorted = raw
    .filter((s) => s.start < n && s.start <= s.end)
    .map((s) => ({ start: s.start, end: Math.min(s.end, n - 1), label: s.label.slice(0, 60) }))
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

/** Partition `turns` into topic-labeled sections. Throws SectionsError(502) if
 * the model output can't be parsed. */
export async function sectionize(
  ctx: SectionsCtx,
  turns: { gist: string }[],
): Promise<Section[]> {
  const llm = ctx.llm ?? anthropicClient();
  const prompt = turns.map((t, i) => `${i}. ${t.gist.replaceAll("\n", " ")}`).join("\n");
  const result = await llm.run(
    {
      model: MODEL,
      system: SYSTEM,
      maxTokens: MAX_TOKENS,
      messages: [{ role: "user", content: [{ type: "text", text: prompt }] }],
      tools: [],
    },
    () => {},
  );
  const text = result.content
    .filter((b): b is { type: "text"; text: string } => b.type === "text")
    .map((b) => b.text)
    .join("");
  const raw = parseSections(text);
  if (!raw) throw new SectionsError(502, "section labeling failed (unparseable model output)");
  return normalizeSections(raw, turns.length);
}
