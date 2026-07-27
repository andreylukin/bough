/// <reference no-default-lib="true" />
/// <reference lib="deno.worker" />
/**
 * The script side of the workflow worker. This module IS the worker: it runs with
 * `permissions: "none"`, and the orchestration script it executes gets exactly five
 * names plus its input — `agent`, `phase`, `log`, `parallel`, `pipeline`, `args`.
 * No filesystem, no network, no subprocesses, no imports. A workflow is a *plan*,
 * not a program; the work happens in the subagents `agent()` reaches on the host
 * (spec §8).
 *
 * THE INVARIANT THIS HOLDS: **a workflow script is deterministic, and its
 * combinators have exactly the concurrency semantics the spec states.** Two
 * mechanisms, and both are load-bearing for something other than tidiness:
 *
 *   1. **The determinism trap.** `Date.now()`, `new Date()` with no argument,
 *      `Math.random()` — and, for the same reason, `performance.now()` and
 *      `crypto.randomUUID()`/`getRandomValues()` — THROW here. Journal rerun keys
 *      each `agent()` call by `hash(prompt + opts)` and replays the hits instantly
 *      (spec §8). A script that stamps a clock reading or a random id into a prompt
 *      produces a fresh key on every run, so "rerun replays unchanged calls" becomes
 *      a lie that fails as *wrong output*, not as an error — the run silently costs
 *      full price and nobody is told. This is Temporal's core workflow constraint,
 *      taken as discipline without the dependency (plan §1, §6.15). The messages say
 *      what to do instead: pass timestamps in through `args`, vary prompts by index.
 *   2. **The exit trap.** `process.exit()`/`Deno.exit()` would terminate the worker
 *      silently and strand the run until its wall timeout with nothing to report
 *      (plan §6.2). Same trap the program worker installs, different advice: a
 *      workflow ends by returning a value.
 *
 * COMBINATOR SEMANTICS, which are exact and not negotiable (spec §8):
 *
 *   - `parallel(thunks)` is a **barrier**: it awaits all of them. A thunk that
 *     throws resolves to `null` in its slot, and the call itself NEVER rejects —
 *     one failed branch of a fan-out must not discard the siblings that succeeded.
 *   - `pipeline(items, ...stages)` has **no barrier between stages**: each item
 *     flows through every stage independently, so item B can be in stage 3 while
 *     item A is still in stage 1. A stage that throws drops that item to `null` and
 *     skips its remaining stages, leaving every other item untouched. Stage
 *     callbacks receive `(prev, originalItem, index)`.
 *
 * Both are pure, worker-side, and never cross the wire — which is why
 * `protocol.ts` declares only the three bridged names (see `WORKFLOW_HOST_FN_NAMES`).
 *
 * The script's parameter list is `WORKFLOW_SCRIPT_PARAMS` from `protocol.ts` plus
 * the two combinators and `console`, which are built here rather than bridged.
 * `workflow/run.ts` pre-flights the script against the same extended list; a test
 * there probes a real worker for every one of those names, so the two sides cannot
 * drift into a script that passes validation and then fails to compile in here.
 *
 * Ported from `src/harness/wf_worker.ts`. Deltas are marked `NOTE:`.
 */

import {
  type FromWorkflowWorker,
  type ToWorkflowWorker,
  WORKFLOW_SCRIPT_PARAMS,
  type WorkflowHostFnName,
} from "./protocol.ts";

// ---------------------------------------------------------------------------
// The determinism trap
// ---------------------------------------------------------------------------

/**
 * NOTE: new in the rewrite (plan T5.2). The ported worker allowed a script to read
 * the clock, which is the one thing that makes journal replay unsound.
 */
function nondeterministic(what: string, instead: string): Error {
  return new Error(
    `${what} is not available inside a workflow: scripts must be deterministic, because ` +
      `rerun replays every agent() call whose key — hash(prompt + opts) — is unchanged, ` +
      `and a clock reading or a random value in a prompt changes that key on every run. ` +
      `${instead}`,
  );
}

const PASS_TIMESTAMPS =
  "Pass timestamps in through `args` (workflow.start({script, args}) carries any JSON verbatim).";
const VARY_BY_INDEX =
  "Vary agent prompts by their index instead — pipeline() stage callbacks receive " +
  "(prev, item, index), and parallel() thunks can close over one.";

try {
  const RealDate = Date;
  // A Proxy rather than a subclass: `new Date(ms)` and `new Date(iso)` must keep
  // working (a script may format a timestamp it was handed through `args`), and only
  // the ARGLESS construction — the one that reads the wall clock — is denied.
  const GuardedDate = new Proxy(RealDate, {
    construct(target, argArray: unknown[], newTarget) {
      if (argArray.length === 0) {
        throw nondeterministic("new Date() with no argument", PASS_TIMESTAMPS);
      }
      return Reflect.construct(target, argArray, newTarget);
    },
    get(target, prop, receiver) {
      if (prop === "now") {
        return () => {
          throw nondeterministic("Date.now()", PASS_TIMESTAMPS);
        };
      }
      return Reflect.get(target, prop, receiver);
    },
  });
  (globalThis as { Date: DateConstructor }).Date = GuardedDate;

  Math.random = () => {
    throw nondeterministic("Math.random()", VARY_BY_INDEX);
  };

  // The same hazard wearing different names. `performance.now()` is a clock, and a
  // uuid or a random buffer in a prompt guarantees zero journal hits forever.
  const perf = (globalThis as { performance?: { now?: unknown } }).performance;
  if (perf) {
    perf.now = () => {
      throw nondeterministic("performance.now()", PASS_TIMESTAMPS);
    };
  }
  const c = (globalThis as { crypto?: { randomUUID?: unknown; getRandomValues?: unknown } }).crypto;
  if (c) {
    c.randomUUID = () => {
      throw nondeterministic("crypto.randomUUID()", VARY_BY_INDEX);
    };
    c.getRandomValues = () => {
      throw nondeterministic("crypto.getRandomValues()", VARY_BY_INDEX);
    };
  }
} catch {
  // Frozen globals — nothing to guard. The host still journals by key, so a
  // nondeterministic script degrades to "rerun re-runs everything", not to silence.
}

// ---------------------------------------------------------------------------
// The exit trap
// ---------------------------------------------------------------------------

const exitTrap = (code?: unknown): never => {
  throw new Error(
    `exit(${code ?? 0}) is not available in a workflow — a script ends by returning its ` +
      `result, and signals failure by throwing an Error. Calling exit() would terminate ` +
      `the run mid-flight with nothing to report.`,
  );
};
try {
  const g = globalThis as { process?: { exit?: unknown }; Deno?: { exit?: unknown } };
  if (g.process) g.process.exit = exitTrap;
  if (g.Deno) g.Deno.exit = exitTrap;
} catch { /* frozen globals — nothing to guard */ }

// ---------------------------------------------------------------------------
// The bridge
// ---------------------------------------------------------------------------

const pending = new Map<number, { resolve: (v: string) => void; reject: (e: Error) => void }>();
let seq = 0;

const send = (msg: FromWorkflowWorker) => self.postMessage(msg);

function hostCall(fn: WorkflowHostFnName, args: unknown[]): Promise<string> {
  const id = ++seq;
  const p = new Promise<string>((resolve, reject) => pending.set(id, { resolve, reject }));
  send({ type: "host", id, fn, args });
  return p;
}

function show(v: unknown): string {
  if (typeof v === "string") return v;
  try {
    return JSON.stringify(v) ?? String(v);
  } catch {
    return String(v);
  }
}

// ---------------------------------------------------------------------------
// The script's scope
// ---------------------------------------------------------------------------

/**
 * The two names this file adds to `WORKFLOW_SCRIPT_PARAMS`, plus `console`. They are
 * built here because they are pure combinators over `agent` — nothing about them
 * needs the host, and bridging them would put a postMessage round trip in the middle
 * of every fan-out.
 *
 * `workflow/run.ts` repeats this extension for its pre-flight parse and pins it with
 * a probe against a real worker. It is not imported from there because importing a
 * `deno.worker` module into the host would evaluate this file's traps and message
 * handler in the server process.
 */
const SCRIPT_PARAMS = [...WORKFLOW_SCRIPT_PARAMS, "parallel", "pipeline", "console"] as const;

interface AgentOpts {
  label?: string;
  phase?: string;
  model?: string;
  /** A JSON Schema. Present = the report comes back as validated JSON (T5.3). */
  schema?: unknown;
}

/**
 * `agent(prompt, opts?)` — run one subagent, resolve to its report.
 *
 * MUST throw when the subagent fails: that is what makes `parallel()` map the slot
 * to `null` and `pipeline()` drop the item. A resolved-with-an-error-string design
 * would put failure detection back in the script's hands, and every script would get
 * it slightly differently.
 *
 * The report crosses the wire verbatim. Only a `{schema}` call parses it, because
 * only then is it JSON by contract — spec §8: the script branches on typed data
 * rather than parsing prose.
 */
const agent = async (prompt: string, opts?: AgentOpts): Promise<unknown> => {
  const report = await hostCall("agent", [String(prompt), JSON.stringify(opts ?? {})]);
  if (!opts || opts.schema === undefined) return report;
  try {
    return JSON.parse(report);
  } catch {
    throw new Error(
      `agent(prompt, {schema}) did not return valid JSON — the report began: ` +
        `${report.slice(0, 200)}`,
    );
  }
};

/**
 * Fire-and-forget progress. A workflow NEVER blocks on display (spec §8), so these
 * return nothing and swallow their own transport failures: a wedged UI must not be
 * able to stall a fan-out, and a script that `await`s a phase marker by mistake gets
 * an already-resolved promise rather than a hang.
 */
const phase = (title: string): void => void hostCall("phase", [String(title)]).catch(() => {});
const log = (message: unknown): void => void hostCall("log", [show(message)]).catch(() => {});

/**
 * `parallel(thunks)` — a barrier that never rejects. Every thunk runs; a thunk that
 * throws (or that isn't callable) resolves to `null` in its slot.
 *
 * `Promise.resolve().then(t)` rather than `t()` so a thunk that throws SYNCHRONOUSLY
 * lands in the same `.catch` as one that rejects — otherwise the whole call would
 * throw before a single sibling started.
 */
const parallel = (thunks: unknown): Promise<unknown[]> => {
  if (!Array.isArray(thunks)) {
    return Promise.reject(
      new TypeError(
        "parallel(thunks): expected an array of zero-argument functions, e.g. " +
          "parallel(items.map(x => () => agent(`…${x}`))) — note the () => , without it " +
          "the calls have already started and there is nothing to schedule",
      ),
    );
  }
  return Promise.all(
    thunks.map((t) =>
      Promise.resolve()
        .then(() => (typeof t === "function" ? (t as () => unknown)() : t))
        .catch(() => null)
    ),
  );
};

/**
 * `pipeline(items, ...stages)` — each item flows through every stage independently.
 *
 * NO BARRIER between stages, and that is the whole point of the primitive: with N
 * items and a slow one among them, a barrier would idle every other item at each
 * boundary. Here item B can reach stage 3 while item A is still in stage 1, which is
 * what keeps a fan-out saturating the run's agent semaphore.
 *
 * A throwing stage drops THAT item to `null` and skips its remaining stages; the
 * other items are untouched. The returned promise resolves once every item has
 * settled, in input order.
 */
const pipeline = (
  items: unknown,
  ...stages: Array<(prev: unknown, item: unknown, index: number) => unknown>
): Promise<unknown[]> => {
  if (!Array.isArray(items)) {
    return Promise.reject(
      new TypeError("pipeline(items, ...stages): `items` must be an array"),
    );
  }
  return Promise.all(items.map(async (item, index) => {
    let prev: unknown = item;
    try {
      for (const stage of stages) {
        if (typeof stage !== "function") {
          throw new TypeError("pipeline(items, ...stages): every stage must be a function");
        }
        prev = await stage(prev, item, index);
      }
      return prev;
    } catch {
      // The item is dropped, not the run. Its remaining stages never run.
      return null;
    }
  }));
};

/** A script's `console.*` becomes a `workflow.log` line — there is no stdout here. */
const scriptConsole = { log, error: log, warn: log, info: log, debug: log };

async function run(code: string, args: unknown): Promise<unknown> {
  // deno-lint-ignore no-explicit-any
  const AsyncFunction = Object.getPrototypeOf(async function () {}).constructor as any;
  const scope: Record<(typeof SCRIPT_PARAMS)[number], unknown> = {
    agent,
    phase,
    log,
    args,
    parallel,
    pipeline,
    console: scriptConsole,
  };
  // Built from the array, in order, so the list is spelled out exactly once.
  const script = new AsyncFunction(...SCRIPT_PARAMS, code);
  return await script(...SCRIPT_PARAMS.map((name) => scope[name]));
}

self.onmessage = (e: MessageEvent) => {
  const msg = e.data as ToWorkflowWorker;

  // Stop requested. A permissions-none worker has no children to sweep, so the ack
  // is immediate — but it is still an ack rather than a bare terminate(), so the
  // host's wind-down is one handshake for both workers (plan §6.3).
  if (msg.type === "abort") {
    for (const p of pending.values()) p.reject(new Error("workflow stopped"));
    pending.clear();
    send({ type: "aborted" });
    return;
  }

  if (msg.type === "host_result") {
    const p = pending.get(msg.id);
    pending.delete(msg.id);
    if (!p) return;
    if (msg.ok) p.resolve(msg.value);
    else p.reject(new Error(msg.value));
    return;
  }

  let args: unknown = null;
  try {
    args = JSON.parse(msg.argsJson);
  } catch {
    // A run with unparseable input still runs, with `args` null — the script's own
    // guards are a better error than a dead worker.
  }
  run(msg.code, args)
    .then((result) => send({ type: "done", resultJson: JSON.stringify(result ?? null) ?? "null" }))
    .catch((err) =>
      send({ type: "error", message: String((err as Error)?.stack ?? err), logs: [] })
    );
};
