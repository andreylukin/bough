/**
 * `ask()` — the mid-task question, and the hold that parks a program on a human.
 *
 * A program calls `await ask("Which environment?", {options: ["dev", "prod"]})` and
 * its turn parks here:
 *
 *   raise ──▶ register the hold, emit `ask.question` (status "pending")
 *         └─▶ block until answer / decline (POST /sessions/:id/questions/:qid) or
 *             the turn's interrupt — then re-emit the SAME id with its final status
 *             and settle the program's promise.
 *
 * THE INVARIANT THIS HOLDS: **a hold is memory-only, and it always settles.**
 *
 * *Memory-only*, deliberately (spec §6: "the hold dies with the turn"). A pending
 * question means nothing once the turn that raised it is gone — there is no program
 * left to hand the answer to — so persisting one would only create a class of stale
 * rows that a restart has to find and heal, and a UI that can render a hold card
 * nobody can answer. Nothing here touches the database except to append the SETTLED
 * question to the transcript, which is the durable record: an `AskPart` that replays
 * as plain text and can never re-block (plan §6.5). A restart therefore leaves
 * nothing pending, with no recovery pass, because there was never anything to
 * recover.
 *
 * *Always settles* is the other half, and it is what makes the first half safe. Four
 * things can end a hold and every one of them re-emits the same id with a final
 * status, so a client that saw the "pending" card always sees it close:
 *
 *   1. the user answers — `ask()` resolves with their text;
 *   2. the user dismisses — `ask()` rejects with a catchable `user declined`, so the
 *      program proceeds on a default it states out loud or stops cleanly (spec §6);
 *   3. the turn's `signal` aborts — the interrupt reaches the parked program like
 *      every other host function, rather than leaving it hanging forever on a
 *      question whose asker has gone;
 *   4. the turn ends while a hold is still parked. This is the one nobody expects: a
 *      program whose worker is torn down by the wall-clock timeout never unwinds its
 *      host promise, so without a sweep the card haunts the session for the life of
 *      the process. The sweep rides `turn.finished` off the bus (see `arm` below).
 *
 * WHY THE SETTLED PART IS BUFFERED UNTIL `message.finished`. The turn runner owns the
 * supervisor message's `parts` array in memory and writes it WHOLESALE on every
 * append (`turn/runner.ts`). A part written to the row from out here — as this module
 * must, since it is not the runner — is therefore erased by the runner's very next
 * append, which during a parked `ask()` is the tool_result of the program that
 * raised it. So settled parts are held and flushed once, after the runner's last
 * write, on `message.finished`; a hold that settles after that (the sweep) is applied
 * straight through. See `appendAskPart`, which preserves the message's `pending` flag
 * so a late append can never flip a finished message back to busy.
 *
 * `hostfn/` imports nothing from `server/` (plan §3): the registry takes a `Bus`, the
 * host function takes a `TurnCtx`, and the HTTP handlers that drive them live in
 * `server/questions.ts`.
 *
 * Ported from `src/asks.ts`. Deltas are marked `NOTE:`.
 */
import { z } from "zod";
import { AskDeclinedError, BadRequestError, ProgramError } from "../errors.ts";
import type { MessageFinishedData } from "../schema/events.ts";
import type { AskPart, AskQuestion, Part } from "../schema/parts.ts";
import type { Bus, Db, HostFns, TurnCtx } from "../types.ts";

// ---------------------------------------------------------------------------
// The registry
// ---------------------------------------------------------------------------

/** How a hold ended. `pending` is the only non-terminal status. */
export type AskSettlement = "answered" | "declined" | "interrupted";

/** What `raise` hands back: the live record, and the promise the program awaits. */
export interface RaisedAsk {
  /**
   * The live record. Its `status` mutates as it settles, so the caller can label
   * the transcript part without re-reading the registry.
   */
  record: AskQuestion;
  /**
   * Resolves with the user's answer; rejects catchably with `user declined` on a
   * dismissal and with an interrupt notice when the turn is stopped.
   */
  answer: Promise<string>;
}

/** One question a program is parked on. */
export interface AskInput {
  sessionId: string;
  /** The supervisor message whose turn raised it — the transcript anchor. */
  messageId: string;
  question: string;
  /** Pick-one choices. Free text stays possible either way. */
  options?: string[];
}

interface PendingAsk {
  record: AskQuestion;
  settle: (status: AskSettlement, answer?: string) => void;
}

/**
 * The live holds.
 *
 * A class rather than a module-level `Map` for the same reason `DetachedSubagents`
 * is one: two tests in one file must not be able to settle each other's questions,
 * and a test that leaks a hold must not poison the next file. Production uses the
 * single process instance below, which is what the HTTP routes and the turn's host
 * function share — the two must see the same map or an answer arrives at nobody.
 */
export class AskHolds {
  readonly #pending = new Map<string, PendingAsk>();
  /** Injected clock, so `ts` (which orders the list) is assertable. */
  readonly #now: () => number;

  constructor(now: () => number = Date.now) {
    this.#now = now;
  }

  /**
   * Raise one question and park until it settles.
   *
   * No timeout: a question is user-paced by design, and a deadline would turn "the
   * user stepped away" into a spurious failure of work that was going fine. The turn
   * is what bounds it — its interrupt, and the sweep when it ends.
   */
  raise(bus: Bus, q: AskInput, signal?: AbortSignal): RaisedAsk {
    const options = q.options?.filter((o) => o !== "");
    const record: AskQuestion = {
      id: crypto.randomUUID(),
      sessionId: q.sessionId,
      messageId: q.messageId,
      question: q.question,
      ...(options?.length ? { options } : {}),
      status: "pending",
      ts: this.#now(),
    };

    const answer = new Promise<string>((resolve, reject) => {
      const onAbort = () => settle("interrupted");
      const settle = (status: AskSettlement, ans?: string): void => {
        // `delete` returning false means someone already settled this one. Answering
        // twice is an ordinary race — two clients, or a decline that crossed an
        // answer — not an error, and the first one wins.
        if (!this.#pending.delete(record.id)) return;
        signal?.removeEventListener("abort", onAbort);
        record.status = status;
        if (ans !== undefined) record.answer = ans;
        // Re-emitted on the SAME id, so the hold card updates in place instead of a
        // second card appearing next to a stale one.
        bus.publish({ type: "ask.question", sessionId: record.sessionId, data: { ...record } });
        if (status === "answered") resolve(ans ?? "");
        else if (status === "declined") reject(declined(record.question));
        else reject(interrupted(record.question));
      };

      // Registered BEFORE the announcement, so a listener that answers synchronously
      // — a test, or a same-process client — finds the hold rather than racing it.
      this.#pending.set(record.id, { record, settle });
      bus.publish({ type: "ask.question", sessionId: record.sessionId, data: { ...record } });
      // Already stopped: settle immediately rather than registering a listener on a
      // signal that will never fire again.
      if (signal?.aborted) return settle("interrupted");
      signal?.addEventListener("abort", onAbort, { once: true });
    });

    return { record, answer };
  }

  /** Settle with the user's answer. False when the id is not (or no longer) waiting. */
  answer(id: string, answer: string): boolean {
    const hold = this.#pending.get(id);
    if (!hold) return false;
    hold.settle("answered", answer);
    return true;
  }

  /** Dismiss: `ask()` rejects with a catchable `user declined`. */
  decline(id: string): boolean {
    const hold = this.#pending.get(id);
    if (!hold) return false;
    hold.settle("declined");
    return true;
  }

  /** A pending question by id — the route's lookup. Settled ones are gone. */
  get(id: string): AskQuestion | undefined {
    return this.#pending.get(id)?.record;
  }

  /**
   * Questions currently awaiting an answer, oldest first, optionally for one
   * session. This is how a freshly-attached client rebuilds its hold cards: events
   * are display transport and never replay (plan §6.16), so the live registry is the
   * only place a card can come from.
   */
  list(sessionId?: string): AskQuestion[] {
    return [...this.#pending.values()]
      .map((h) => h.record)
      .filter((r) => sessionId === undefined || r.sessionId === sessionId)
      .sort((a, b) => a.ts - b.ts);
  }

  /**
   * Settle every still-parked hold as `interrupted`, for one session or all of them.
   * Returns how many were swept.
   *
   * This is failure-mode 4 from the header: a program torn down without unwinding
   * (the wall-clock timeout terminates the worker, not the host promise) leaves a
   * hold nobody will ever answer. Sweeping it is what keeps "the hold dies with the
   * turn" true in fact and not just in intent.
   */
  expire(sessionId?: string): number {
    let n = 0;
    for (const hold of [...this.#pending.values()]) {
      if (sessionId !== undefined && hold.record.sessionId !== sessionId) continue;
      hold.settle("interrupted");
      n++;
    }
    return n;
  }

  /** Live hold count. The leak checks read it. */
  get size(): number {
    return this.#pending.size;
  }
}

/**
 * The process-wide registry: the HTTP routes and every turn's `ask()` share it.
 *
 * Module-level state, like `turn/queue.ts`'s running-turn registry and
 * `hostfn/delegate.ts`'s detached register, and for the same reason — an answer
 * arrives on a different request than the one that parked the program, so the two
 * must reach the same map without threading an instance through `AppCtx`.
 */
export const askHolds: AskHolds = new AskHolds();

// ---- the process registry, as free functions (what the routes call) ----------

/** Raise a question on the process registry. */
export function raiseAsk(bus: Bus, q: AskInput, signal?: AbortSignal): RaisedAsk {
  return askHolds.raise(bus, q, signal);
}

/** Settle a pending question with the user's answer. */
export function answerAsk(id: string, answer: string): boolean {
  return askHolds.answer(id, answer);
}

/** Dismiss a pending question — `ask()` rejects catchably. */
export function declineAsk(id: string): boolean {
  return askHolds.decline(id);
}

/** A pending question by id, or undefined. */
export function getAsk(id: string): AskQuestion | undefined {
  return askHolds.get(id);
}

/** Pending questions, oldest first, optionally for one session. */
export function pendingAsks(sessionId?: string): AskQuestion[] {
  return askHolds.list(sessionId);
}

/** Interrupt-and-clear parked questions whose turn is gone. Returns the count. */
export function expireAsks(sessionId?: string): number {
  return askHolds.expire(sessionId);
}

// ---------------------------------------------------------------------------
// Errors the program catches
// ---------------------------------------------------------------------------

/**
 * The dismissal. The phrase `user declined` is load-bearing: spec §6 names it as
 * what a declined `ask()` must convey, the prompt's ask section tells the model to
 * catch exactly this, and the question is repeated so a program holding several
 * knows which one was dismissed.
 */
function declined(question: string): Error {
  return new AskDeclinedError(
    `user declined to answer: ${question} — the question was dismissed, not missed. ` +
      `Proceed on a default you state out loud, or stop cleanly; do not ask again.`,
  );
}

/** The interrupt. Distinct from a decline: nobody said no, the turn was stopped. */
function interrupted(question: string): Error {
  return new ProgramError(
    `ask() interrupted — the turn was stopped before the question was answered: ` +
      `${question}. Nothing was decided; work already done still stands.`,
  );
}

// ---------------------------------------------------------------------------
// The settled part
// ---------------------------------------------------------------------------

/**
 * Append one settled question to its supervisor message.
 *
 * Two details carry the weight:
 *
 *   - **`message.pending` is preserved, never set.** A hold that settles as the turn
 *     dies would otherwise flip a finished message back to pending, and the UI would
 *     show a session busy forever on a turn that already ended (`turn/runner.ts`
 *     guards its own appends for exactly this).
 *   - **It is idempotent on the part's id.** The flush and the sweep can both reach
 *     the same part, and a transcript with the same question in it twice is worse
 *     than one missing it.
 *
 * Returns whether it wrote. A message that no longer exists is not an error worth
 * raising into a program that has already been told its answer.
 */
export function appendAskPart(
  db: Db,
  bus: Bus,
  sessionId: string,
  messageId: string,
  part: AskPart,
): boolean {
  const message = db.getMessage(messageId);
  if (!message) return false;
  if (message.parts.some((p: Part) => p.type === "ask" && p.id === part.id)) return false;
  db.updateMessage(messageId, [...message.parts, part], message.pending);
  bus.publish({ type: "message.part", sessionId, data: { messageId, part } });
  return true;
}

/** The transcript record for one settled hold. */
function askPartOf(record: AskQuestion, status: AskSettlement, answer?: string): AskPart {
  return {
    type: "ask",
    id: record.id,
    question: record.question,
    ...(record.options?.length ? { options: record.options } : {}),
    status,
    ...(answer !== undefined ? { answer } : {}),
  };
}

// ---------------------------------------------------------------------------
// The bridged host function
// ---------------------------------------------------------------------------

/**
 * `ask()`'s options bag, validated at the bridge because the bridge IS a boundary
 * (plan §0). Deliberately lenient about the *contents*: a model that passes
 * `{options: [1, 2]}` meant two choices, and refusing the question over it costs a
 * round to learn nothing. A non-object bag is refused, because that is a call shaped
 * wrongly rather than a value typed loosely.
 */
const AskOptions = z.object({
  options: z.array(z.unknown()).optional(),
}).passthrough();

function parseAskOptions(optsJson: string): string[] | undefined {
  const text = (optsJson ?? "").trim();
  if (text === "" || text === "null" || text === "undefined") return undefined;
  let raw: unknown;
  try {
    raw = JSON.parse(text);
  } catch (err) {
    throw new BadRequestError(
      `ask(question, opts): the options could not be read as JSON ` +
        `(${err instanceof Error ? err.message : String(err)}). Pass a plain object, ` +
        `e.g. ask("Which environment?", {options: ["dev", "prod"]}).`,
    );
  }
  if (raw === null || raw === undefined) return undefined;
  const parsed = AskOptions.safeParse(raw);
  if (!parsed.success) {
    throw new BadRequestError(
      `ask(question, opts): the second argument must be an object like ` +
        `{options: ["dev", "prod"]} — free text is always possible, so options are ` +
        `a convenience, never a requirement.`,
    );
  }
  const options = parsed.data.options
    ?.map((o) => String(o).trim())
    .filter((o) => o !== "");
  return options?.length ? options : undefined;
}

/** Seams, so `ask()` is drivable with no worker, no client and no server. */
export interface AskDeps {
  /** Absent = the process registry, which is what the HTTP routes also reach. */
  holds?: AskHolds;
  /**
   * Where a settled question is recorded. Absent = buffered and appended to the
   * turn's supervisor message (see the module header for why it is buffered). A test
   * passes a collector to assert on the parts without a database.
   */
  append?: (part: AskPart) => void;
}

/**
 * Build `ask(question, optsJson)` for one turn.
 *
 * The answer comes back as a PLAIN string, not JSON — `ask` is the one bridged
 * function besides `view`/`patch` whose payload is already text
 * (`harness/vm_worker.ts`), so a program gets the user's words with no unwrapping.
 */
export function createAskHostFn(ctx: TurnCtx, deps: AskDeps = {}): Pick<HostFns, "ask"> {
  const holds = deps.holds ?? askHolds;

  /** Settled parts waiting for the runner's last write. See the module header. */
  const buffered: AskPart[] = [];
  /** True once the supervisor message is closed and safe to append to directly. */
  let closed = false;
  let off: (() => void) | null = null;

  const sink: (part: AskPart) => void = deps.append ??
    ((part) => {
      if (closed) appendAskPart(ctx.db, ctx.bus, ctx.sessionId, ctx.messageId, part);
      else buffered.push(part);
    });

  const flush = (): void => {
    closed = true;
    for (const part of buffered.splice(0)) sink(part);
  };

  const disarm = (): void => {
    const stop = off;
    off = null;
    stop?.();
  };

  /**
   * Watch this turn's own lifecycle, from the first question onwards.
   *
   * Armed lazily, so a turn that never asks anything never subscribes, and removed
   * on `turn.finished` — which the runner emits on every path it can end by, success,
   * failure and interrupt alike. That is also where the sweep lives: whatever is
   * still parked when the turn ends can never be answered, so it is settled as
   * `interrupted` and its part written straight through.
   */
  const arm = (): void => {
    if (off) return;
    off = ctx.bus.subscribe((event) => {
      if (
        event.type === "message.finished" &&
        (event.data as MessageFinishedData)?.messageId === ctx.messageId
      ) {
        flush();
        return;
      }
      if (event.type === "turn.finished" && event.sessionId === ctx.sessionId) {
        // Unsubscribe FIRST: the sweep below settles holds, which appends parts,
        // and re-entering this listener from inside itself would be a needless
        // second pass over an empty buffer.
        disarm();
        // A turn that ended without `message.finished` should not exist, but if one
        // does, the buffered parts belong on the message rather than in memory.
        flush();
        holds.expire(ctx.sessionId);
      }
    });
  };

  return {
    ask: async (question: string, optsJson = "{}"): Promise<string> => {
      const options = parseAskOptions(optsJson);
      const text = String(question ?? "").trim();
      if (text === "") {
        throw new BadRequestError(
          `ask(): the question is empty. Ask something a human can answer in one ` +
            `line, e.g. ask("Deploy to prod or staging?", {options: ["prod", "staging"]}).`,
        );
      }
      // Refuse before announcing a card nobody can answer: the turn is already over.
      if (ctx.signal.aborted) throw interrupted(text);

      arm();
      const { record, answer } = holds.raise(
        ctx.bus,
        {
          sessionId: ctx.sessionId,
          messageId: ctx.messageId,
          question: text,
          ...(options ? { options } : {}),
        },
        ctx.signal,
      );

      try {
        const given = await answer;
        sink(askPartOf(record, "answered", given));
        return given;
      } catch (err) {
        // `record.status` is set by the settle that rejected us, so the transcript
        // says which of the two happened — "you dismissed it" and "the turn was
        // stopped" are different facts for both the user and the next round.
        sink(askPartOf(record, record.status === "declined" ? "declined" : "interrupted"));
        throw err;
      }
    },
  };
}
