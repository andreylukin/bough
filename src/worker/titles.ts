/**
 * The cheap tier's shared call, and the first of its three features: **auto session
 * titles.**
 *
 * THE INVARIANT THIS MODULE HOLDS, and the reason all three cheap-tier features are
 * written the way they are: **a cheap-model call can only ever ADD something. It can
 * never take anything away, delay anything, or fail anything.** Spec §12 states it and
 * plan §8.4 names it as a risk: titles, ghost text and activity blurbs bill on every
 * round, and "a synchronous failure there stalls turns for a cosmetic feature". So the
 * contract is enforced at the type level by `CheapTier` (`types.ts`: these methods
 * resolve `null` on failure and NEVER reject) and structurally here by `cheapText`,
 * which is the only path any of the three take to a provider and which has no throwing
 * branch at all.
 *
 * That is stronger than "wrap the call site in try/catch". A missing API key throws
 * from `clientFor` before a request is ever made; a provider 500 throws from `run`; a
 * hung connection never throws at all. All three are the same non-event to a caller,
 * and the third is the one a try/catch alone does not cover — hence the deadline.
 *
 * MODULE POSITION. This file is the BASE of the cheap trio: `worker/ghost.ts` and
 * `worker/activity.ts` import `cheapText` and `cheapModel` from here and nothing
 * imports back, so the three reach DOWN to a shared primitive rather than across to
 * each other. Same rule and same reason as `server/http.ts` — an import cycle between
 * feature modules that are all wired from one place at boot is a `ReferenceError`
 * waiting for whichever module the process happens to enter first.
 *
 * THE TITLE FEATURE ITSELF is a bus listener, not a call site inside the message
 * handler, and that is deliberate. `server/sessions.ts` persists a user message and
 * announces it; nothing about naming a session belongs on that path, and putting it
 * there would mean a cheap-model concern sitting inside the one request the user is
 * actually waiting on. Subscribing to `message.started` instead gets the same trigger
 * with none of the coupling, and it is why this task adds no line to `sessions.ts`.
 *
 * WHICH MODEL. The cheap tier is a single hosted model for the whole install (spec
 * §12: "Cheap — powers auto session titles, composer ghost text, and live activity
 * blurbs"), chosen in the model picker, and read from the environment at CALL time
 * rather than captured at boot. Never `ctx.model`: a user pinned to Opus for the
 * coding work must not pay Opus rates to put five words in a sidebar.
 *
 * Ported from `src/supervisor/title.ts` (the trigger, the placeholder guard, the
 * sanitizer and its word cap). The local-worker ladder that file describes is gone —
 * spec §17 rules out local inference, so the cheap tier is a hosted model with no
 * llama-server tier and no backstop below it.
 */
import { clientFor } from "../llm/client.ts";
import type { Message, Session } from "../schema/parts.ts";
import type { AppCtx, Bus, Db, LlmClient } from "../types.ts";

// ---------------------------------------------------------------------------
// The shared cheap call
// ---------------------------------------------------------------------------

/** Overridden by the model picker, which writes it to the launcher env file. */
export const CHEAP_MODEL_ENV = "BOUGH_CHEAP_MODEL";

/** The floor when the picker has never been used. Small, hosted, and fast. */
export const DEFAULT_CHEAP_MODEL = "claude-haiku-4-5";

/**
 * How long any cheap-model call may take before it is abandoned.
 *
 * A deadline is not politeness here, it is the third failure mode: a provider that
 * neither answers nor errors would otherwise leave a ghost-text request hanging and —
 * worse — hold a session's one activity slot forever, so every later round in that
 * session would be dropped as "already in flight" and the blurb would never come back.
 * Aborting is what makes the drop rule self-healing.
 */
export const CHEAP_TIMEOUT_MS = 12_000;

/** Reads one environment variable. Injected so a test needs no real environment. */
export type Env = (key: string) => string | undefined;

const processEnv: Env = (key) => process.env[key];

/** The cheap model in force. Read per call, so a picker change needs no restart. */
export function cheapModel(env: Env = processEnv): string {
  return env(CHEAP_MODEL_ENV)?.trim() || DEFAULT_CHEAP_MODEL;
}

export interface CheapCallOpts {
  system: string;
  prompt: string;
  maxTokens: number;
  /** Injected in tests. Absent = the provider-routed client for the cheap model. */
  llm?: LlmClient;
  /** Injected in tests. Absent = `cheapModel()`. */
  model?: string;
  timeoutMs?: number;
  env?: Env;
}

/**
 * One cheap-model completion. **Never rejects, never hangs, never logs.**
 *
 * Returns the concatenated text blocks, or `null` for every failure there is: no key,
 * an unroutable model id, a provider error, a refusal, an empty answer, or the
 * deadline. The caller cannot tell them apart and must not try — every one of them
 * means the same thing, which is that this round has no title/ghost/blurb and the next
 * one will describe itself.
 *
 * Silent by design, including the absence of a `console.warn`. A cosmetic call that
 * fires on every round would turn a lapsed API key into thousands of lines of server
 * log, burying the failures that matter.
 */
export async function cheapText(opts: CheapCallOpts): Promise<string | null> {
  const timeoutMs = opts.timeoutMs ?? CHEAP_TIMEOUT_MS;
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), timeoutMs);
  try {
    const model = opts.model ?? cheapModel(opts.env);
    // Inside the try: `clientFor` throws for a missing key or an unroutable id, and
    // that is a failed cheap call like any other, not an exception the caller sees.
    const llm = opts.llm ?? clientFor(model);
    const result = await llm.run(
      {
        model,
        system: opts.system,
        maxTokens: opts.maxTokens,
        messages: [{ role: "user", content: [{ type: "text", text: opts.prompt }] }],
        tools: [],
      },
      () => {},
      controller.signal,
    );
    const text = result.content
      .filter((b): b is { type: "text"; text: string } => b.type === "text")
      .map((b) => b.text)
      .join("")
      .trim();
    return text.length > 0 ? text : null;
  } catch {
    return null;
  } finally {
    // Always, including the success path: a live timer would keep the process (and a
    // `deno test` run) awake for the length of the deadline after the work is done.
    clearTimeout(timer);
  }
}

// ---------------------------------------------------------------------------
// Titles
// ---------------------------------------------------------------------------

/**
 * CONSOLIDATED, never appended to. A small model asked to satisfy a list of clauses
 * satisfies the first few; the measured failure mode on haiku (see the harness-bench
 * memory) is that a fourth sentence dilutes the first three rather than adding to them.
 *
 * The grounding sentence came from a measurement, not from taste. Asked to title "What
 * does parse.py do? One sentence.", the cheap tier returned
 *
 *   Parse.py parses code into syntax trees
 *
 * — a claim about a file nothing had read, shown as the conversation's name in the tree,
 * in the switcher, and in the title of every handoff branched from it. Sampled three
 * prompts three times each: with the clause, nothing invented a subject.
 */
export const TITLE_SYSTEM = [
  "You name coding sessions. Given the user's first message, reply with a short title only:",
  "3-6 words, sentence case, no quotes, no trailing period, no preamble like 'Title:'.",
  "Name only what the message names — never invent a subject, file, or domain it does not",
  "mention.",
].join(" ");

/** A title needs the gist, not a 50KB paste — and the paste is what is being billed. */
export const TITLE_MAX_INPUT = 2000;

/** The longest title the sidebar is asked to render. */
export const TITLE_MAX_CHARS = 60;

/**
 * Small models decorate. Take the first real line, strip the label and the quoting,
 * then cap.
 *
 * The WORD cap is the one that came from a live finding rather than from taste: a
 * small model asked to title a message sometimes answers it instead, and the old tree
 * shipped a session titled with thirteen words of story. Eight words turns that into a
 * readable stub; a genuine 3-6 word title passes through untouched.
 */
export function sanitizeTitle(raw: string): string {
  const line = raw.trim().split("\n").map((l) => l.trim()).find((l) => l.length > 0) ?? "";
  const cleaned = line
    .replace(/^(title\s*:)\s*/i, "")
    // MARKDOWN LEADERS. A live row read `# Big Python File Creation`: asked for a short
    // title, a small model answers with a heading or a bullet, and the marker rode all the
    // way into the tree — it also masked the casing fix below, since `#` is not prose. The
    // quote-stripping right after this never saw them.
    .replace(/^\s*(#{1,6}|[-*•])\s+/, "")
    .replace(/^["'“”`*]+|["'“”`*.]+$/g, "")
    .trim();
  if (cleaned === "") return "";

  // A TITLE HAS TO CARRY INFORMATION. Asked to "List the numbers 1 to 60", the cheap tier
  // titled a session `1` — a row in the switcher that names nothing, and the tree is the
  // one surface where every row has to be recognisable. Refusing falls back to the
  // workspace name, which is at least true (see the reply case below for the same logic).
  // Three LETTERS is the floor: "Bug" and "CI fix" pass, `1`, `42`, `-` and `ok` do not.
  if ((cleaned.match(/[a-z]/gi)?.length ?? 0) < 3) return "";

  const words = cleaned.split(/\s+/);
  // The model ANSWERED instead of titling. Capping that at eight words does not
  // rescue it, it manufactures a lie: a live session was headed
  //
  //   I don't have access to your codebase, so
  //
  // — a truncated sentence, false on its face (bough had just read the file), shown
  // above every screen and in both the sessions and tree tabs. No title at all is
  // strictly better; the session then falls back to its workspace name, which is
  // true. A reply is easy to spot because a title is a noun phrase and these never
  // begin one.
  if (
    /^(i|i'm|i'll|i've|sorry|sure|certainly|okay|ok|here|let|as|based|the user|you)\b/i.test(
      cleaned,
    )
  ) {
    return "";
  }
  // Trailing connectives are trimmed rather than refused: a cap that lands on one
  // leaves "… and the", which reads as a half-finished thought, but the words
  // before it are usually a perfectly good name.
  const capped = words.slice(0, 8);
  while (capped.length > 1) {
    const last = capped[capped.length - 1].replace(/[,;:]$/, "").toLowerCase();
    if (!/^(and|or|but|so|because|that|which|with|for|to|of|in|on|a|an|the)$/.test(last)) break;
    capped.pop();
  }
  return sentenceCase(capped.join(" ")).slice(0, TITLE_MAX_CHARS).replace(/[,;:]$/, "").trim();
}

/**
 * Undo the model's Title Case. The prompt now asks for sentence case, but a prompt is a
 * request and the tree is where it shows: two sibling rows read
 *
 *   ● ✓ C Function Implementation
 *   ● ✓ Add b() function to mod.py
 *
 * — same install, same model, minutes apart. Mixed casing down a column looks like two
 * different programs wrote it, which is exactly the "not quite finished" feel the TUI
 * work is chasing out.
 *
 * ONLY fires when EVERY word is capitalized, which is the decorating tic and nothing
 * else: a title already in sentence case has a lowercase second word and is returned
 * untouched. Words that carry their own capitalization are never lowered — `C`, `CI` and
 * `API` pass through a title that IS rewritten, while a lowercase-initial word such as
 * `getUser` or `mod.py` stops the rewrite entirely. Both directions protect the same
 * thing: an identifier is not prose, and recasing one makes the title wrong rather than
 * merely inconsistent.
 */
function sentenceCase(title: string): string {
  const words = title.split(" ");
  // An ordinary capitalized word — the only kind that gets lowered.
  const prose = (w: string) => /^[A-Z][a-z]+$/.test(w);
  // Anything else a Title-Cased title may legitimately contain: `C`, `API`, `b()`, `3`.
  const opaque = (w: string) => !/^[a-z]/.test(w);
  // A single lowercase-initial word ends it: the title is already sentence case (or is
  // Title Case with a lowercased "of", which is worth leaving alone to never mangle one).
  if (!words.every((w) => prose(w) || opaque(w))) return title;
  if (!words.some(prose)) return title;
  return words.map((w, i) => (i === 0 || !prose(w) ? w : w.toLowerCase())).join(" ");
}

/**
 * `CheapTier.title`. Resolves the sanitized title, or `null` — never rejects.
 *
 * Also used by `history/compact.ts` to name a compaction branch from its first
 * summary, which is why it takes free text rather than a session id.
 */
export async function cheapTitle(
  firstMessage: string,
  opts: Partial<CheapCallOpts> = {},
): Promise<string | null> {
  const text = firstMessage.slice(0, TITLE_MAX_INPUT).trim();
  if (!text) return null;
  const raw = await cheapText({
    system: TITLE_SYSTEM,
    prompt: text,
    maxTokens: 64,
    ...opts,
  });
  if (raw === null) return null;
  return sanitizeTitle(raw) || null;
}

// ---------------------------------------------------------------------------
// The auto-title feature
// ---------------------------------------------------------------------------

/** What titling needs off the app context. `cheap` absent = the feature is off. */
export interface TitleCtx {
  db: Db;
  bus: Bus;
  cheap?: AppCtx["cheap"];
}

export interface AutoTitleOpts {
  /**
   * The title a session must still be carrying for a generated one to replace it.
   * Defaults to `""`, which is what `POST /sessions` stores (`server/sessions.ts`).
   * A subagent passes its spawn-time task stub so a content-derived name still
   * supersedes it.
   */
  placeholder?: string;
  /**
   * The in-flight ledger. One title per session at a time: two messages posted in
   * quick succession must not buy two titles for the same placeholder, since the
   * second would be discarded by the re-check below after it had already been paid
   * for. `watchTitles` owns one; a direct caller may share it.
   */
  inflight?: Set<string>;
}

/**
 * Name a session from its first user message, in the background.
 *
 * **Returns `void` and never throws.** Not `Promise<void>`: a caller that could await
 * it would eventually await it, and the whole point is that nothing waits. The session
 * is renamed — and `session.updated` published, which is what re-renders every
 * connected sidebar — if and only if the cheap tier answers.
 *
 * Two guards, both about not overwriting a fact someone else established. Before the
 * call: the session must still be carrying the placeholder, so a titled or renamed
 * session is never re-titled and never re-billed. After it: the SAME check again,
 * because a user can rename during the round-trip and the answer that comes back is
 * about a name they already replaced.
 */
export function maybeAutoTitle(
  ctx: TitleCtx,
  sessionId: string,
  text: string,
  opts: AutoTitleOpts = {},
): void {
  const placeholder = opts.placeholder ?? "";
  const cheap = ctx.cheap;
  if (!cheap) return;
  if (!text.trim()) return;

  const session: Session | undefined = ctx.db.getSession(sessionId);
  // A `! command` title is PROVISIONAL. The TUI names a conversation after the shell command that
  // created it (`store.runShell`, so a shell-only conversation is not "(untitled)" forever) — and
  // this guard, which only ever replaced an EMPTY title, then left a conversation that went on to
  // do real work permanently called `! ls -1 src`. Seen on a fresh-install walk: the first thing
  // typed was `!ls -1 src`, the second was a real task, and the header still said the former.
  const provisional = session?.title.startsWith("! ") === true;
  if (!session || (session.title !== placeholder && !provisional)) return;

  const inflight = opts.inflight;
  if (inflight?.has(sessionId)) return;
  inflight?.add(sessionId);

  // `.catch` on a method the type says cannot reject, because the type is a contract
  // this module cannot enforce on an injected implementation — and an unhandled
  // rejection here is a process-level event, not a missing title.
  Promise.resolve(cheap.title(text.slice(0, TITLE_MAX_INPUT)))
    .then((title) => {
      if (!title) return;
      // The SAME provisional rule as the guard above — re-checked here because the title may
      // have been set while the cheap call was in flight, and missing it here is why the first
      // version of this fix did nothing: a `! command` title passed the entry guard and was then
      // rejected on the way out.
      const now = ctx.db.getSession(sessionId)?.title;
      if (now !== placeholder && !(now?.startsWith("! ") === true)) return;
      ctx.db.setSessionTitle(sessionId, title);
      const updated = ctx.db.getSession(sessionId);
      if (updated) ctx.bus.publish({ type: "session.updated", sessionId, data: updated });
    })
    .catch(() => {})
    .finally(() => inflight?.delete(sessionId));
}

/** The user's words in a message, joined. Pure; empty when the message is images only. */
export function userText(message: Message): string {
  return message.parts
    .filter((p): p is { type: "text"; text: string } => p.type === "text")
    .map((p) => p.text)
    .join("\n")
    .trim();
}

/**
 * Start auto-titling. Returns the unsubscribe.
 *
 * Listens for `message.started` with a `user` role — the event `server/sessions.ts`
 * publishes the moment a posted message is persisted, and the same one the turn runner
 * publishes for its own supervisor messages, which the role check excludes.
 *
 * The listener does no work of its own beyond the two cheap guards inside
 * `maybeAutoTitle`, and it is synchronous: the bus fans out synchronously to every
 * subscriber, so anything slow here would be latency on the caller's publish. All this
 * does is start a promise nobody holds.
 */
export function watchTitles(ctx: TitleCtx): () => void {
  const inflight = new Set<string>();
  return ctx.bus.subscribe((e) => {
    if (e.type !== "message.started" || !e.sessionId) return;
    const message = e.data as Message | undefined;
    if (!message || message.role !== "user") return;
    maybeAutoTitle(ctx, e.sessionId, userText(message), { inflight });
  });
}
