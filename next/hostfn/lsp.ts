/**
 * The bridged `lsp(verb, argsJson)` host function — how the curated symbol verbs
 * reach a running program.
 *
 * THE INVARIANT THIS HOLDS: **one bridge per turn, constructed lazily, and its two
 * failure classes survive the trip to the model intact.** The worker rebuilds
 * `lsp.find(...)`/`lsp.refs(...)` from the verb list in `harness/protocol.ts` and
 * sends each as `lsp(verb, JSON.stringify(args))`; this module is the host end of
 * that call and nothing else. All of the judgement — empty versus broken, latch,
 * argv — lives in `lsp/lsp.ts`, which is pure and has no turn in it.
 *
 * WHY THE BRIDGE IS BUILT HERE AND NOT AT BOOT. The latch ("the backend already
 * failed, stop calling it") and the workspace registration memo are both per-turn
 * state, and a turn is exactly the right lifetime for both: within a turn a dead
 * backend stays dead so the model is not tempted to re-litigate it, and across turns
 * it is retried, because the user may have installed the thing in between. A
 * process-wide bridge would make a transient failure permanent until restart; a
 * per-call bridge would re-register the workspace on every verb and report the same
 * outage over and over.
 *
 * WHY CONSTRUCTION IS FREE. `createLspHostFn` is called for every turn, including
 * the overwhelming majority that never ask about a symbol. It stats nothing, spawns
 * nothing and reads no environment — the first `lsp.*` call does all of it (spec §10:
 * lazy, nothing spawns until the first call).
 *
 * WHAT CROSSES THE WIRE. Strings, both ways (`harness/protocol.ts`). The worker
 * JSON-parses every result of a verb-dispatched host function, so the plain text the
 * backend produced is returned JSON-encoded and the program receives it as an
 * ordinary string. An EMPTY result comes back through this path like any other
 * answer — resolved, not rejected — which is the whole behaviour spec §10 is about.
 *
 * `hostfn/` imports nothing from `server/` (plan §3): this takes a `TurnCtx` and its
 * own deps, so it is drivable with a fabricated context, a fake backend and no
 * server, worker or binary in sight.
 */
import { LspError } from "../errors.ts";
import { createLspBridge, lspAvailable, type LspBridge, type LspRun } from "../lsp/lsp.ts";
import type { HostFns, TurnCtx } from "../types.ts";

export interface LspHostDeps {
  /** Fake backend for tests; absent = spawn the located binary. */
  run?: LspRun;
  /** Per-invocation ceiling. Absent = the module default. */
  timeoutMs?: number;
  /**
   * Where a backend outage is reported, at most once per turn. The default writes
   * one server-log line: the program already receives the full diagnosis as its
   * caught exception, so this exists for the operator, not the model.
   */
  onBackendDown?: (detail: string, ctx: TurnCtx) => void;
}

/** Is the backend installed? The prompt gate — re-exported so callers need one import. */
export { lspAvailable };

/**
 * Build `lsp(verb, argsJson)` for one turn.
 *
 * Always bridged, whether or not the backend is installed: an install that appears
 * mid-session is picked up by the next call, and a call with nothing installed
 * rejects with a message that says so. The PROMPT is what is gated on availability
 * (`prompt/assemble.ts` takes `lsp: lspAvailable()`), so a model is never told about
 * a backend that is not there — which is the half of spec §6 that matters.
 */
export function createLspHostFn(
  ctx: TurnCtx,
  deps: LspHostDeps = {},
): Pick<HostFns, "lsp"> {
  /**
   * Built on first use, not at construction. Nothing in `createLspBridge` spawns
   * either, but the deferral is the invariant written down where it is easy to
   * break — an eager `findBackend()` here would put a filesystem walk on every turn.
   */
  let bridge: LspBridge | undefined;
  const bridgeFor = (): LspBridge => (bridge ??= createLspBridge({
    workspace: ctx.workspace,
    signal: ctx.signal,
    ...(deps.run ? { run: deps.run } : {}),
    ...(deps.timeoutMs !== undefined ? { timeoutMs: deps.timeoutMs } : {}),
    onBackendDown: (detail) => (deps.onBackendDown ?? defaultReport)(detail, ctx),
  }));

  return {
    lsp: async (verb: string, argsJson: string): Promise<string> => {
      const args = parseArgs(verb, argsJson);
      const text = await bridgeFor().call(verb, args);
      // The worker re-inflates with JSON.parse, so even plain text ships encoded.
      return JSON.stringify(text);
    },
  };
}

/**
 * The JSON envelope, parsed at the boundary (plan §0).
 *
 * The worker sends `JSON.stringify(args ?? null)`, so `"null"` and `""` are both
 * "called with no arguments" and are handed on as `null` — `lsp/lsp.ts` decides
 * whether that verb can be called without them, because that is a verb question and
 * this is a transport.
 */
function parseArgs(verb: string, argsJson: string): unknown {
  const text = (argsJson ?? "").trim();
  if (text === "") return null;
  try {
    return JSON.parse(text);
  } catch (err) {
    throw new LspError(
      400,
      `lsp.${verb}: the arguments could not be read as JSON ` +
        `(${err instanceof Error ? err.message : String(err)}). Pass a plain object, ` +
        `e.g. lsp.${verb}({symbol: "Gate.decide"}).`,
    );
  }
}

/**
 * One line, once per turn. Deliberately not an event or a system note: the model is
 * already told, in full, by the rejection it caught, and a second announcement in the
 * transcript would invite exactly the "the task is blocked" reading spec §10 rules
 * out.
 */
function defaultReport(detail: string, ctx: TurnCtx): void {
  console.log(`lsp backend unavailable for session ${ctx.sessionId}: ${detail}`);
}
