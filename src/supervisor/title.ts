/**
 * Title worker: sessions created without a title get the UNTITLED placeholder and,
 * on their first user message, a fire-and-forget worker call that names them.
 * The generated title is persisted and announced via `session.updated` so every
 * connected UI re-renders the sidebar entry. Failures are silent — a session that
 * keeps its placeholder is annoying, not broken.
 *
 * The task runs on bough's LOCAL worker (llama-server + Qwen2.5-Coder-3B — see
 * worker/runtime.ts): a title is exactly
 * the kind of small self-contained unit the worker exists for. Per the worker
 * experiments, local tiers need a frontier backstop — if the local path fails, a
 * small Anthropic model finishes the job.
 */
import type { Db } from "../db/db.ts";
import type { Bus } from "../bus.ts";
import { anthropicClient } from "./llm.ts";
import { ensureWorker } from "../worker/runtime.ts";
import { workerComplete } from "../worker/client.ts";
import { frontierWorkerModel } from "../worker/frontier.ts";

/** Placeholder for sessions created without a title; the trigger for auto-titling. */
export const UNTITLED = "untitled";

/** Frontier backstop model when the local worker is unavailable/unusable. */
export function titleBackstopModel(): string {
  return Deno.env.get("BOUGH_TITLE_MODEL") ?? frontierWorkerModel() ?? "claude-haiku-4-5";
}

const SYSTEM = [
  "You name coding sessions. Given the user's first message, reply with a short",
  "title only: 3-6 words, no quotes, no trailing period, no preamble like 'Title:'.",
].join(" ");

/** Produces a raw title for the given first-message text. Injectable for tests. */
export type Titler = (text: string) => Promise<string>;

export interface TitleCtx {
  db: Db;
  bus: Bus;
  /** Injected for tests; defaults to local worker → frontier backstop. */
  titler?: Titler;
}

/**
 * If the session still carries the UNTITLED placeholder, generate a title from the
 * first user message in the background. Never throws; never blocks the turn.
 */
export function maybeAutoTitle(ctx: TitleCtx, sessionId: string, text: string): void {
  const session = ctx.db.getSession(sessionId);
  if (!session || session.title !== UNTITLED) return;
  generate(ctx, sessionId, text).catch((err) => {
    console.error("title worker failed:", err);
  });
}

async function generate(ctx: TitleCtx, sessionId: string, text: string): Promise<void> {
  // Truncate: a title needs the gist, not a 50KB paste.
  const raw = await (ctx.titler ?? defaultTitler)(text.slice(0, 2000));
  const title = sanitize(raw);
  if (!title) return;
  // The user may have renamed/re-titled meanwhile — don't clobber.
  if (ctx.db.getSession(sessionId)?.title !== UNTITLED) return;
  ctx.db.setSessionTitle(sessionId, title);
  const updated = ctx.db.getSession(sessionId);
  if (updated) ctx.bus.publish({ type: "session.updated", sessionId, data: updated });
}

/** tier1: local worker; on any failure, tier3: frontier backstop. */
async function defaultTitler(text: string): Promise<string> {
  // Frontier mode: no local tier — title straight on the backstop model.
  if (frontierWorkerModel()) return await backstopTitle(text);
  try {
    const url = await ensureWorker();
    // Low temperature: this is a one-shot formatting task, not reasoning.
    return await workerComplete(url, {
      system: SYSTEM,
      user: text,
      maxTokens: 64,
      temperature: 0.2,
    });
  } catch (err) {
    // Privacy tier: local-only mode never backstops to a remote API.
    if (Deno.env.get("BOUGH_WORKER_LOCAL_ONLY") === "1") throw err;
    console.warn("local title worker unavailable, falling back:", (err as Error).message);
    return await backstopTitle(text);
  }
}

/**
 * One-shot titling for non-session callers (compaction branches): sanitized,
 * null on any failure. Same ladder as session titles.
 */
export async function workerTitle(text: string): Promise<string | null> {
  try {
    return sanitize(await defaultTitler(text.slice(0, 2000))) || null;
  } catch {
    return null;
  }
}

async function backstopTitle(text: string): Promise<string> {
  const result = await anthropicClient().run(
    {
      model: titleBackstopModel(),
      maxTokens: 64,
      messages: [{ role: "user", content: [{ type: "text", text: `${SYSTEM}\n\n${text}` }] }],
      tools: [],
    },
    () => {},
  );
  return result.content.find((b) => b.type === "text")?.text ?? "";
}

/** Small models decorate: take the first real line, strip quotes/labels, cap length.
 * The word cap converts a prose echo (the 3B worker sometimes answers the prompt
 * instead of titling it — live finding: a session titled with 13 words of story)
 * into a readable stub; real 3-6-word titles pass untouched. */
function sanitize(raw: string): string {
  const line = raw.trim().split("\n").map((l) => l.trim()).find((l) => l.length > 0) ?? "";
  return line
    .replace(/^(title\s*:)\s*/i, "")
    .replace(/^["'“”`*]+|["'“”`*.]+$/g, "")
    .split(/\s+/).slice(0, 8).join(" ")
    .slice(0, 60)
    .trim();
}
