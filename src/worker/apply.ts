/**
 * Fast-apply: when an exact-match edit fails because the file drifted (whitespace,
 * a touched neighbor line, slightly stale context), ask the LOCAL worker to point
 * at the text the edit was meant to match — instead of failing the step and paying
 * a frontier round-trip to re-read and re-issue the edit.
 *
 * The worker LOCATES, it never writes: it sees the file with numbered lines and
 * nominates a line range (schema-constrained JSON, so the reply is structurally
 * parseable). The replacement text is then extracted from the file by range —
 * a small model reliably points at code but unreliably quotes it verbatim, so
 * generation is kept out of the loop entirely. Deterministic checks decide
 * whether the nominated region really is the drifted match: sane bounds, length
 * within reason of the search text, boundary lines that appear in the search
 * text, and half the lines in common. Anything else — worker down, timeout,
 * unparseable, checks fail — returns null and the caller surfaces the original
 * mismatch error, so this path can only reduce failures.
 */
import { workerIfRunning } from "./runtime.ts";
import { workerComplete } from "./client.ts";
import { frontierComplete, frontierWorkerModel } from "./frontier.ts";

/** Injectable completion for tests: (system, user, temperature) → raw reply. */
export type Completer = (system: string, user: string, temperature: number) => Promise<string>;

/** Below this search-text size there is too little to anchor on — never reconcile. */
const MIN_OLD_CHARS = 32;
/** Excerpt cap: the worker's slot context is ~8K tokens; leave room for the rest. */
const MAX_EXCERPT_CHARS = 16_000;
/** Whole-reconciliation deadline — an edit must fail fast, not hang on inference. */
const DEADLINE_MS = 15_000;
/** Best-of-2 (proven lever): a cheap fresh sample harvests per-shot variance. */
const ATTEMPT_TEMPERATURES = [0.2, 0.8];

const RANGE_SCHEMA = {
  type: "object",
  properties: {
    start_line: { type: "integer" },
    end_line: { type: "integer" },
  },
  required: ["start_line", "end_line"],
  additionalProperties: false,
};

const SYSTEM = [
  "An automated editor tried to replace an exact block of text in a file and",
  "failed: the search text no longer appears verbatim, usually because whitespace,",
  "quoting, or a nearby line drifted. You are shown the file with numbered lines,",
  "the failed search text, and the intended replacement. Identify the contiguous",
  "line range the search text was meant to match. Reply with JSON",
  '{"start_line": N, "end_line": M} — 1-based, inclusive, using the line numbers',
  'shown. If no region of the file corresponds to the search text, reply',
  '{"start_line": 0, "end_line": 0}.',
].join(" ");

/**
 * Try to recover the edit a failed `old_string` was meant to make. Returns the
 * whole new file text with the replacement applied at the located range, or
 * null when reconciliation isn't safe.
 */
export async function reconcileEdit(
  fileText: string,
  oldString: string,
  newString: string,
  complete: Completer = defaultComplete,
): Promise<string | null> {
  if (Deno.env.get("BOUGH_NO_FAST_APPLY") === "1") return null;
  if (oldString.length < MIN_OLD_CHARS) return null;
  const lines = fileText.split("\n");
  const excerpt = numberedExcerpt(lines, oldString);
  if (excerpt === null) return null;
  const user =
    `FILE (numbered):\n${excerpt}\n\nFAILED SEARCH TEXT:\n${oldString}\n\nINTENDED REPLACEMENT:\n${newString}`;

  let timer: ReturnType<typeof setTimeout> | undefined;
  const deadline = new Promise<null>((r) => {
    timer = setTimeout(() => r(null), DEADLINE_MS);
  });
  try {
    return await Promise.race([attempts(lines, oldString, newString, user, complete), deadline]);
  } catch {
    return null;
  } finally {
    clearTimeout(timer);
  }
}

async function attempts(
  lines: string[],
  oldString: string,
  newString: string,
  user: string,
  complete: Completer,
): Promise<string | null> {
  for (const temperature of ATTEMPT_TEMPERATURES) {
    let raw: string;
    try {
      raw = await complete(SYSTEM, user, temperature);
    } catch {
      return null; // worker unreachable — a retry would only stall the edit further
    }
    const range = parseRange(raw);
    if (!range) continue;
    const [start, end] = range;
    if (start < 1 || end < start || end > lines.length) continue;
    const extracted = lines.slice(start - 1, end).join("\n");
    if (!accept(oldString, newString, extracted)) continue;
    return [...lines.slice(0, start - 1), newString, ...lines.slice(end)].join("\n");
  }
  return null;
}

function parseRange(raw: string): [number, number] | null {
  try {
    const parsed = JSON.parse(raw) as { start_line?: unknown; end_line?: unknown };
    if (typeof parsed.start_line !== "number" || typeof parsed.end_line !== "number") return null;
    return [parsed.start_line, parsed.end_line];
  } catch {
    return null;
  }
}

/** The deterministic gate: the worker nominates a range, these checks decide. */
function accept(oldString: string, newString: string, extracted: string): boolean {
  if (extracted === newString) return false; // edit already applied — don't reapply
  if (extracted.length < oldString.length / 3) return false;
  if (extracted.length > oldString.length * 3 + 64) return false;
  const el = collapsedLines(extracted);
  const ol = collapsedLines(oldString);
  if (el.length === 0 || ol.length === 0) return false;
  // Boundary lines must appear in the search text: a range that starts or ends on
  // a line the edit never mentioned would splice out unrelated code.
  const os = new Set(ol);
  if (!os.has(el[0]) || !os.has(el[el.length - 1])) return false;
  const common = el.filter((l) => os.has(l)).length;
  return common / Math.max(el.length, ol.length) >= 0.5;
}

/** Non-empty lines with runs of whitespace collapsed — indent/spacing drift erased. */
function collapsedLines(s: string): string[] {
  return s.split("\n").map((l) => l.replace(/\s+/g, " ").trim()).filter((l) => l.length > 0);
}

/**
 * The numbered file when it fits; otherwise a numbered window around the first
 * line of the search text found in the file (drift is local — some line usually
 * still matches). Numbers are true file line numbers, so a range maps straight
 * back. Null when nothing anchors: reconciliation would be a blind guess.
 */
function numberedExcerpt(lines: string[], oldString: string): string | null {
  const number = (from: number, to: number) =>
    lines.slice(from, to).map((l, i) => `${from + i + 1}: ${l}`).join("\n");
  const total = lines.reduce((n, l) => n + l.length + 1, 0);
  if (total <= MAX_EXCERPT_CHARS) return number(0, lines.length);
  const probes = collapsedLines(oldString).filter((l) => l.length >= 8);
  let hit = -1;
  for (const probe of probes) {
    hit = lines.findIndex((l) => l.replace(/\s+/g, " ").trim() === probe);
    if (hit >= 0) break;
  }
  if (hit < 0) return null;
  let start = hit, end = hit + 1, size = lines[hit].length;
  while (size < MAX_EXCERPT_CHARS && (start > 0 || end < lines.length)) {
    if (start > 0) size += lines[--start].length + 1;
    if (end < lines.length && size < MAX_EXCERPT_CHARS) size += lines[end++].length + 1;
  }
  return number(start, end);
}

/** Live path: an already-running worker only — an edit never waits on a cold start. */
async function defaultComplete(system: string, user: string, temperature: number): Promise<string> {
  if (frontierWorkerModel()) {
    // Bigger cap than the local path: a chat model may pad the JSON with prose.
    return await frontierComplete({ system, user, maxTokens: 128, jsonSchema: RANGE_SCHEMA });
  }
  const url = await workerIfRunning();
  if (!url) throw new Error("no local worker running");
  return await workerComplete(url, {
    system,
    user,
    maxTokens: 64,
    temperature,
    jsonSchema: RANGE_SCHEMA,
    cachePrompt: true,
  });
}
