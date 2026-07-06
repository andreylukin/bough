/**
 * Tool-output digestion: a program that prints a 100KB build log would push all of
 * it into the supervisor's context verbatim. Over a threshold, keep the head and
 * tail verbatim (context sits at the top, errors at the bottom) and have the LOCAL
 * worker digest the omitted middle into a few lines. The digest is advisory
 * context, clearly labeled; nothing gates on it.
 *
 * Privacy tier: this path is local-only BY DESIGN — command output is exactly the
 * kind of text (env dumps, tokens in logs) that shouldn't ride to a remote API as
 * a side effect. When no local worker is reachable the middle is simply dropped
 * with a deterministic omission marker; there is no frontier fallback.
 */
import { workerIfRunning } from "./runtime.ts";
import { workerComplete } from "./client.ts";

/** Injectable completion for tests: (system, user) → digest text. */
export type DigestCompleter = (system: string, user: string) => Promise<string>;

/** Output at or below this passes through untouched. */
const THRESHOLD_CHARS = 16_000;
/** Verbatim head and tail kept around the digested middle. */
const HEAD_CHARS = 3_000;
const TAIL_CHARS = 3_000;
/** Cap on what the worker sees of the middle (its slot context is ~8K tokens). */
const SAMPLE_CHARS = 6_000;
/** Digestion must never stall a turn — past this, fall back to plain omission. */
const DEADLINE_MS = 10_000;

const SYSTEM = [
  "You compress noisy command output for a coding agent. You get excerpts of a long",
  "output: its first lines, its last lines, and error-looking lines from the omitted",
  "middle. Reply with 3-6 short lines: what ran, whether it succeeded, and the exact",
  "errors, failures, or warnings — keep file:line references verbatim. No preamble.",
].join(" ");

const SALIENT = /error|fail(ed|ure)?|warn(ing)?|exception|traceback|panic|fatal|denied|✗|✘/i;

/**
 * Pass small output through; compress big output to head + worker digest + tail.
 * Never throws and never blocks past its deadline.
 */
export async function digestOutput(
  text: string,
  complete: DigestCompleter = defaultComplete,
): Promise<string> {
  if (text.length <= THRESHOLD_CHARS || Deno.env.get("BOUGH_NO_DIGEST") === "1") return text;
  const head = text.slice(0, HEAD_CHARS);
  const tail = text.slice(-TAIL_CHARS);
  const middle = text.slice(HEAD_CHARS, -TAIL_CHARS);
  const omitted = middle.split("\n").length;
  const marker = (summary: string | null) =>
    `${head}\n[…${omitted} middle lines omitted${
      summary ? `; local-worker digest:\n${summary}` : ""
    }]\n${tail}`;

  let timer: ReturnType<typeof setTimeout> | undefined;
  const deadline = new Promise<null>((r) => {
    timer = setTimeout(() => r(null), DEADLINE_MS);
  });
  try {
    const summary = await Promise.race([summarize(head, middle, tail, complete), deadline]);
    return marker(summary ? summary.trim() : null);
  } catch {
    return marker(null);
  } finally {
    clearTimeout(timer);
  }
}

async function summarize(
  head: string,
  middle: string,
  tail: string,
  complete: DigestCompleter,
): Promise<string | null> {
  // The worker can't read the whole middle — forward the lines that matter: a
  // bounded sample of error-looking lines, else a slice from each end.
  const salient = middle.split("\n").filter((l) => SALIENT.test(l));
  const sample = salient.length
    ? salient.join("\n").slice(0, SAMPLE_CHARS)
    : `${middle.slice(0, SAMPLE_CHARS / 2)}\n…\n${middle.slice(-SAMPLE_CHARS / 2)}`;
  const user = `OUTPUT HEAD:\n${head}\n\nFROM THE OMITTED MIDDLE (${
    salient.length ? "error-looking lines" : "excerpts"
  }):\n${sample}\n\nOUTPUT TAIL:\n${tail}`;
  const digest = await complete(SYSTEM, user);
  return digest.trim() ? digest : null;
}

/** Live path: an already-running worker only, and strictly local — no fallback. */
async function defaultComplete(system: string, user: string): Promise<string> {
  const url = await workerIfRunning();
  if (!url) throw new Error("no local worker running");
  return await workerComplete(url, {
    system,
    user,
    maxTokens: 256,
    temperature: 0.2,
    cachePrompt: true,
  });
}
