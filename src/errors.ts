/**
 * The error hierarchy. Every domain error subclasses `HttpError` and carries the
 * status it should become, so the router has exactly ONE try/catch that turns a
 * thrown error into a response and no handler contains a per-error catch block.
 *
 * The invariant this holds is that a domain module never constructs a `Response`.
 * `history/fork.ts` throwing `new ForkError(400, …)` is the whole of its HTTP
 * contract; it needs no import from `server/`, which is what keeps it unit-testable
 * without a server.
 *
 * **Error text is a product surface** (spec §6). Two audiences read these:
 *
 *   - The user, via an HTTP response.
 *   - The MODEL, when a host function rejects inside a running program — the
 *     message becomes the exception the program catches and the text the next
 *     round reasons over.
 *
 * For the second audience the bar is specified, not incidental: each message
 * names *what failed*, *the state that caused it*, and *the move that resolves
 * it*. A patch conflict says which file and line range and that someone else
 * changed those lines, so the program re-views instead of retrying blind. A spawn
 * cap says WHICH cap, so the program batches instead of hammering. A message that
 * says only "failed" is a defect.
 */

/** A domain error that maps directly to an HTTP response: `status` + message. */
export class HttpError extends Error {
  constructor(readonly status: number, message: string) {
    super(message);
    // Subclasses get their own name for free — used in logs and in the JSON body.
    this.name = new.target.name;
  }
}

// ---- generic shapes ---------------------------------------------------------

/** 400 — the request itself is wrong (shape, or a value this layer can judge). */
export class BadRequestError extends HttpError {
  constructor(message: string) {
    super(400, message);
  }
}

/** 404 — the named session, message, job, workflow or artifact does not exist. */
export class NotFoundError extends HttpError {
  constructor(message: string) {
    super(404, message);
  }
}

/**
 * 409 — the request is well-formed but the current state forbids it. The canonical
 * case is spec §5's "one turn per session": posting into a session that is already
 * running a turn queues instead, so a 409 here means something stronger, like
 * adopting a subagent that already finished.
 */
export class ConflictError extends HttpError {
  constructor(message: string) {
    super(409, message);
  }
}

// ---- path and filesystem ----------------------------------------------------

/**
 * A path escaped the root it was confined to (`paths.confine`). Thrown for
 * artifact names, artifact session ids, skill dirs and attachment paths. Not a
 * security boundary — programs run with the user's full authority (spec §2) — but
 * the *server's* own path handling must not be steerable by a name in a URL.
 */
export class PathError extends HttpError {
  constructor(message: string) {
    super(400, message);
  }
}

// ---- turn and program -------------------------------------------------------

/** A turn could not start, or ended in a way the caller must hear about. */
export class TurnError extends HttpError {}

/**
 * The turn would exceed the model's context window. The message NAMES the limit —
 * compaction is a deliberate, user-initiated branch, and the harness never
 * silently summarizes a conversation out from under the user (spec §5).
 */
export class ContextOverflowError extends HttpError {
  constructor(message: string) {
    super(413, message);
  }
}

/**
 * A program failed inside the worker: a syntax error caught pre-flight, a
 * timeout, or an interrupt. The message must distinguish timeout from interrupt
 * and say what partial work survived (spec §6).
 */
export class ProgramError extends HttpError {
  constructor(message: string) {
    super(400, message);
  }
}

/**
 * A patch could not be applied. The message carries the file, the line range, and
 * that someone else changed those lines — so the program re-views rather than
 * retrying blind. This is the primary safeguard against silent clobbering under
 * shared-checkout delegation, so its text is load-bearing (spec §6, plan §8.1).
 */
export class PatchError extends HttpError {
  constructor(message: string) {
    super(400, message);
  }
}

/**
 * The user dismissed an `ask()`. Rejects the program catchably so it can proceed
 * on a stated default or stop cleanly — never a bare "failed" (spec §6).
 */
export class AskDeclinedError extends HttpError {
  constructor(message: string) {
    super(400, message);
  }
}

// ---- delegation -------------------------------------------------------------

/** A subagent could not be launched, joined or adopted. */
export class AgentError extends HttpError {}

/**
 * A spawn cap was hit. The message says WHICH cap — per-turn (8) or concurrent
 * tree-wide (4) — so the program batches rather than retrying immediately. Only
 * the refused launch fails; siblings that already started are unaffected, which is
 * why spawners use `Promise.allSettled` (spec §7, plan §6.9).
 */
export class SpawnCapError extends AgentError {
  constructor(message: string) {
    super(429, message);
  }
}

/** A workflow could not be started, controlled, or rerun. */
export class WorkflowError extends HttpError {}

/**
 * A workflow script is not admissible: no pure-literal `meta`, or an `agent()`
 * schema the provider cannot honor (no recursion, no numeric/length constraints,
 * `additionalProperties: false` required). Raised at SUBMIT time, not mid-run
 * (plan T5.3).
 */
export class WorkflowScriptError extends WorkflowError {
  constructor(message: string) {
    super(400, message);
  }
}

// ---- history operations -----------------------------------------------------

/**
 * All history operations branch; none mutates in place. A selection reaching into
 * ancestor history is a 400 telling the user to operate on the ancestor (spec §14).
 */
export class BranchError extends HttpError {}
export class ForkError extends BranchError {}
export class CompactError extends BranchError {}
export class SectionsError extends BranchError {}
export class ExtractError extends BranchError {}
export class MoveError extends BranchError {}
export class HandoffError extends BranchError {}

// ---- other domains ----------------------------------------------------------

/**
 * A Changes-rail operation failed. A non-git workspace has no base and therefore
 * no change set: it says so plainly rather than showing an empty diff (spec §13).
 */
export class ChangesError extends HttpError {}

/** A schedule spec did not parse, or the schedule does not exist (spec §9). */
export class ScheduleError extends HttpError {}

/** Durable KV: unknown verb, or a value over the 16KB-per-key limit (spec §6). */
export class StateError extends HttpError {}

/** Artifact publish/serve, including confinement to the session's directory. */
export class ArtifactError extends HttpError {}

/**
 * An MCP server is unreachable, unauthorized, or not registered. A 401 surfaces
 * as "not authorized — open the mcp panel (^p) and press a" and NEVER as a hang; a server that fails
 * to start surfaces as catalog status (plan T7.1, T7.2).
 */
export class McpError extends HttpError {}

/**
 * The LSP backend itself failed — distinct from an empty result, which is an
 * ordinary answer and not an error at all (spec §10, plan §6.14). The message says
 * the backend failed, not that the symbol is missing, so the program drops to `rg`
 * for the rest of the task instead of concluding the symbol does not exist.
 */
export class LspError extends HttpError {}

/** Host `fetch()`: transport failure, deadline, or a non-http(s) URL. A non-2xx
 * response is DATA, not an exception (spec §6). */
export class NetError extends HttpError {}

/** A skill could not be discovered or parsed (missing/invalid SKILL.md frontmatter). */
export class SkillError extends HttpError {}

/** Provider/transport failure from an LLM call. `status`, when known, drives retry
 * classification; no status = a transport fault, always retryable. */
export class LlmError extends HttpError {
  constructor(
    message: string,
    status = 502,
    /** The provider's Retry-After hint, in ms. */
    readonly retryAfterMs?: number,
  ) {
    super(status, message);
  }
}
