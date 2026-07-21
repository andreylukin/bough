/**
 * Composer ghost text: predict the user's NEXT message from the conversation so
 * far — the follow-up they were about to type (run the tests, commit it, fix the
 * thing the agent flagged). Runs on the worker (local llama-server, or the
 * frontier worker when BOUGH_WORKER_FRONTIER is set). No remote backstop when
 * the local worker is down — a ghost suggestion is optional sugar and must never
 * start billing a frontier API as a silent fallback. Any failure = no suggestion.
 */
import { z } from "zod";
import { ensureWorker } from "./runtime.ts";
import { workerComplete } from "./client.ts";
import { frontierComplete, frontierWorkerModel } from "./frontier.ts";

export const SuggestBody = z.object({ sessionId: z.string() });

/** One conversation line, already reduced to its text parts. */
export interface ConvoLine {
  role: "user" | "agent";
  text: string;
}

const SYSTEM = [
  "You predict the next message a user will type to their coding agent, given",
  "the conversation so far. Reply with that message only: one line, short and",
  "concrete — the natural next step (fix what the agent flagged, run the tests,",
  "commit, extend the change). No quotes, no explanation, no 'user:' label.",
].join(" ");

const MAX_LINES = 8;
const MAX_LINE_CHARS = 600;
const MAX_SUGGESTION = 150;

/** The conversation tail as prompt text. Long lines keep their TAIL — an agent
 * reply ends with the outcome and proposed next steps, which is the signal. */
export function renderConvo(lines: ConvoLine[]): string {
  return lines
    .slice(-MAX_LINES)
    .map((l) => {
      const text = l.text.length > MAX_LINE_CHARS ? "…" + l.text.slice(-MAX_LINE_CHARS) : l.text;
      return `${l.role}: ${text}`;
    })
    .join("\n");
}

/** First real line of the reply, unwrapped and capped; null when nothing usable. */
export function sanitizeSuggestion(raw: string): string | null {
  const line = raw.trim().split("\n").map((l) => l.trim()).find((l) => l.length > 0) ?? "";
  const clean = line
    .replace(/^(user|next|suggestion)\s*:\s*/i, "")
    .replace(/^["'`]+|["'`]+$/g, "")
    .slice(0, MAX_SUGGESTION)
    .trim();
  return clean.length > 0 ? clean : null;
}

/** Predict the user's next message, or null (no worker, empty convo, any error). */
export async function suggestNextStep(lines: ConvoLine[]): Promise<string | null> {
  if (lines.length === 0) return null;
  const user = `Conversation, oldest first:\n${renderConvo(lines)}\n\nThe user's next message:`;
  try {
    const reply = frontierWorkerModel()
      ? await frontierComplete({ system: SYSTEM, user, maxTokens: 64 })
      : await workerComplete(await ensureWorker(), {
        system: SYSTEM,
        user,
        maxTokens: 64,
        temperature: 0.2,
        cachePrompt: true,
      });
    return sanitizeSuggestion(reply);
  } catch {
    return null;
  }
}
