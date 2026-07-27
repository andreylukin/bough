/**
 * The `ask()` REST surface: read the live holds, settle one.
 *
 * The invariant this module holds is the router's own — **HTTP lives in `server/`
 * and nowhere else** (plan §3, `hostfn/` never imports from `server/`). Both
 * handlers are thin translations over `hostfn/ask.ts`'s registry: no hold logic
 * lives here, and no HTTP lives there.
 *
 * `GET /questions` is a RECONNECT path, not a feed. Events are display transport and
 * never replay (plan §6.16), so a client that attaches mid-hold has no way to learn
 * about a parked question except by asking for the live set — which is exactly what
 * this returns, oldest first. It answers from memory because that is where holds
 * live: a pending question means nothing once its turn is gone, so a restart leaves
 * this list empty and there is nothing stale to heal (spec §6).
 *
 * `POST /sessions/:id/questions/:qid` is the only way to settle one. It is scoped by
 * session id as well as question id so a client cannot answer another session's hold
 * by guessing a uuid, and a question that settled between the read and the write is a
 * 409 rather than a silent success — the caller's click did nothing, and telling them
 * so is the difference between "the user's answer was used" and "the user thinks it
 * was".
 */
import { BadRequestError, ConflictError, NotFoundError } from "../errors.ts";
import { answerAsk, declineAsk, getAsk, pendingAsks } from "../hostfn/ask.ts";
import { AnswerQuestionBody } from "../schema/requests.ts";
import { type Handler, json, parseBody } from "./http.ts";

/**
 * `GET /questions[?sessionId=]` — every question awaiting an answer, oldest first.
 *
 * A bare array, like `GET /sessions`: the list IS the resource.
 */
export const listQuestions: Handler = (req) => {
  const sessionId = new URL(req.url).searchParams.get("sessionId") ?? undefined;
  return json(pendingAsks(sessionId));
};

/**
 * `POST /sessions/:id/questions/:qid` — `{answer}` resolves the program's `ask()`;
 * `{decline: true}` rejects it with a catchable "user declined".
 *
 * The empty-answer check is not pedantry. An empty string would resolve `ask()` with
 * nothing, and the program would branch on "" as though the user had chosen it —
 * a dismissal is what "I am not answering this" means, and it has its own flag.
 */
export const answerQuestion: Handler = async (req, _ctx, params) => {
  const question = getAsk(params.qid);
  if (!question || question.sessionId !== params.id) {
    throw new NotFoundError(
      `no question awaiting an answer for ${params.qid} in session ${params.id} — ` +
        `holds are memory-only, so one that was already settled, interrupted, or ` +
        `raised before a restart is gone. GET /questions lists the live ones.`,
    );
  }

  const body = await parseBody(req, AnswerQuestionBody, {});

  if (body.decline === true) {
    if (!declineAsk(params.qid)) throw settledMeanwhile(params.qid);
    return json({ ok: true, id: params.qid, status: "declined" });
  }

  if (typeof body.answer !== "string" || body.answer.trim() === "") {
    throw new BadRequestError(
      `body must be {answer: "…"} with non-empty text, or {decline: true} to dismiss ` +
        `the question`,
    );
  }
  if (!answerAsk(params.qid, body.answer)) throw settledMeanwhile(params.qid);
  return json({ ok: true, id: params.qid, status: "answered" });
};

/** The read-then-write race: someone else settled it in between. */
function settledMeanwhile(qid: string): ConflictError {
  return new ConflictError(
    `question ${qid} settled before this answer arrived — it was answered, declined, ` +
      `or its turn ended. Nothing was applied.`,
  );
}
