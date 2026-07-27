/**
 * `POST /sessions/:id/interrupt` — the user stopping a turn that is running.
 *
 * THE INVARIANT THIS HOLDS: **the interrupt spec §5 requires is reachable from a
 * client.** `turn/runner.ts` has had `interruptTurn` since M2 — it aborts the turn's
 * controller and fires the cascade hooks that kill the program's children and
 * interrupt every detached subagent underneath it — and nothing exposed it over
 * HTTP. Both clients said so in their own headers rather than papering over it
 * (`tui/api.ts`, `cli/exec.ts` on its timeout path), which is the honest thing to do
 * with a gap and no substitute for closing it: a harness whose only stop button is
 * ^C on the server is a harness that spends the user's money until it finishes.
 *
 * SECOND — **it is an ANSWER, not an error, that nothing was running.** A stop
 * pressed a beat after the turn ended, a double-tap, a retry after a dropped
 * connection: all of them are 200 with `interrupted: false`. The alternative — a 409
 * for "no turn here" — makes every client write a race-condition branch for a button
 * whose whole job is to be safe to press. `interrupted` is what the caller reads to
 * decide whether to say anything.
 *
 * THIRD — **it does not wait.** The abort travels to a worker that has children to
 * kill and a partial tool result to persist (`turn/runner.ts` writes it with
 * `interrupted: true`), and that unwinding is what publishes `turn.finished`. This
 * handler signals and returns; the client learns the turn actually stopped from the
 * event stream, which is the same way it learns everything else about a turn.
 *
 * The registry is injectable for the same reason everything else here is: a test
 * drives the real route with its own `TurnRegistry` and no turn machinery at all.
 */
import type { Session } from "../schema/parts.ts";
import { NotFoundError } from "../errors.ts";
import { type TurnRegistry, turns } from "../turn/queue.ts";
import { interruptTurn } from "../turn/runner.ts";
import type { AppCtx } from "../types.ts";
import { type Handler, json } from "./http.ts";

/** What a client gets back. `interrupted` is the only field worth branching on. */
export interface InterruptResult {
  sessionId: string;
  /** True when a turn (or a cascade hook) was there to signal. */
  interrupted: boolean;
  /** Human-readable, so a CLI can print it verbatim. */
  message: string;
}

/**
 * The seam a test fills. Absent = the process-wide registry the runner uses, which
 * is the only correct answer in production: the turn was started by a different
 * request than the one stopping it, so a per-request registry would find nothing.
 */
export interface WithTurnRegistry {
  turnRegistry?: TurnRegistry;
}

function registryOf(ctx: AppCtx): TurnRegistry {
  return (ctx as AppCtx & WithTurnRegistry).turnRegistry ?? turns;
}

function requireSession(ctx: AppCtx, id: string): Session {
  const session = ctx.db.getSession(id);
  if (!session) throw new NotFoundError(`no session ${id}`);
  return session;
}

export const interruptSession: Handler = (_req, ctx, params) => {
  const session = requireSession(ctx, params.id);
  const interrupted = interruptTurn(session.id, registryOf(ctx));
  const result: InterruptResult = {
    sessionId: session.id,
    interrupted,
    message: interrupted
      ? "interrupting — the program's children are killed and the partial result is kept"
      : "nothing was running in this session",
  };
  return json(result);
};
