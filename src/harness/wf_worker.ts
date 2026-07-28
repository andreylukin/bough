/**
 * The script side of the workflow worker. This module IS the worker, and the
 * orchestration script it executes gets exactly five names plus its input —
 * `agent`, `phase`, `log`, `parallel`, `pipeline`, `args`. A workflow is a *plan*,
 * not a program; the work happens in the subagents `agent()` reaches on the host
 * (spec §8).
 *
 * That five-name world is a CONTRACT, not a cage. It used to be enforced — the
 * worker ran under Deno's `permissions: "none"`, so a script that reached for the
 * filesystem was denied by the runtime. A Bun worker inherits the server process's
 * capabilities, so the narrow scope is now upheld only by what this file binds: a
 * script that goes looking for `Bun`, `fetch` or a dynamic `import()` will find
 * them. Nothing in the engine relies on the denial (the traps below are about
 * replay correctness, not confinement), but the comments must not claim a boundary
 * that no longer exists.
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
 *   2. **The exit trap.** `process.exit()` would terminate the worker
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
 * THE THIRD MECHANISM, and the reason this file changed: **every `agent()` call
 * carries a STRUCTURAL COORDINATE, computed from the script's shape rather than from
 * the order calls happen to reach the host.**
 *
 * The journal is prefix-bounded — a relaunch replays the longest unchanged leading run
 * of `agent()` calls, so a call's POSITION is part of its identity (spec §8). The host
 * used to derive that position from arrival order, and arrival order is exactly what
 * `pipeline()` is designed not to fix: with no barrier, stage 2 is issued in stage-1
 * COMPLETION order. Give `pipeline(['A','B'], s1, s2)` a stage-1 call that takes 60ms
 * for A and 1ms for B and it journals `[s1 A, s1 B, s2 B, s2 A]`; relaunching the
 * byte-identical script resolves the replayed prefix in DISPATCH order, the last two
 * positions transpose, and an UNCHANGED script re-bills every call past stage 1. That
 * is spec §8's own canonical example, so it was not an exotic case.
 *
 * Only this file can fix it, because only this file knows the shape. Each `agent()`
 * call takes the next slot of the frame that encloses it; `parallel()` opens a frame
 * per thunk (its slot index); `pipeline()` opens one per (item, stage) cell. The
 * coordinate is the path of those slots — `"0.1.1.0"` — and it is stable under any
 * interleaving, because nothing in it is a function of when a call returned.
 *
 * Frames propagate through `AsyncLocalStorage`, not a module-level variable, and that
 * choice is load-bearing: a stage callback is free to `await` before it calls
 * `agent()`, and by then a dynamically-scoped global would be describing whichever
 * other item resumed most recently. The store follows the await chain, so it does not.
 * A call made outside any combinator — a bare `agent()` in the script body — falls back
 * to the root frame's counter, which for a sequential script is exactly the monotonic
 * numbering the host used to assign.
 *
 * The script's parameter list is `WORKFLOW_SCRIPT_PARAMS` from `protocol.ts` plus
 * the two combinators and `console`, which are built here rather than bridged.
 * `workflow/run.ts` pre-flights the script against the same extended list; a test
 * there probes a real worker for every one of those names, so the two sides cannot
 * drift into a script that passes validation and then fails to compile in here.
 *
 * Ported from `src/harness/wf_worker.ts`. Deltas are marked `NOTE:`.
 */

import { AsyncLocalStorage } from "node:async_hooks";

import {
  type FromWorkflowWorker,
  type ToWorkflowWorker,
  WORKFLOW_SCRIPT_PARAMS,
  type WorkflowHostFnName,
} from "./protocol.ts";

// ---------------------------------------------------------------------------
// Structural coordinates
// ---------------------------------------------------------------------------

/**
 * One nesting level of the script's shape. `path` is where this frame sits; `next` is
 * the slot the next child created inside it will take.
 *
 * Mutable `next` on a shared object is the point: every call made in one frame draws
 * from the same counter, so siblings are numbered in the order the script CREATES them
 * — which is a fact about the source text and its sequential control flow, not about
 * scheduling. Two `agent()` calls in one stage callback are always 0 and 1 in that
 * frame however long either of them takes.
 */
interface Frame {
  readonly path: readonly number[];
  next: number;
}

/**
 * The script body's own frame. Its counter is what a bare `agent()` draws from, so a
 * purely sequential script numbers its calls `0, 1, 2, …` — identical to the monotonic
 * counter the host assigned before coordinates existed.
 */
const ROOT: Frame = { path: [], next: 0 };

/**
 * The enclosing frame, followed across `await`s.
 *
 * NOT a module-level "current frame" variable. A stage callback may await anything it
 * likes before reaching `agent()`, and by then a plain variable would name whichever
 * other item's callback resumed last — which is the very latency-order bug this whole
 * mechanism exists to remove, reintroduced one layer down. `AsyncLocalStorage` binds
 * the frame to the async chain instead, so it survives an await and cannot leak
 * sideways into a concurrent branch.
 */
const frames = new AsyncLocalStorage<Frame>();

function currentFrame(): Frame {
  return frames.getStore() ?? ROOT;
}

/** Claim the enclosing frame's next slot. Synchronous, and must stay that way. */
function claimSlot(): number[] {
  const frame = currentFrame();
  return [...frame.path, frame.next++];
}

function childFrame(path: readonly number[]): Frame {
  return { path, next: 0 };
}

/** The wire form: dot-joined slot indexes, e.g. `0.1.1.0` (see `protocol.ts`). */
function posOf(path: readonly number[]): string {
  return path.join(".");
}

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
  const g = globalThis as { process?: { exit?: unknown } };
  if (g.process) g.process.exit = exitTrap;
} catch { /* frozen globals — nothing to guard */ }

// ---------------------------------------------------------------------------
// The bridge
// ---------------------------------------------------------------------------

const pending = new Map<number, { resolve: (v: string) => void; reject: (e: Error) => void }>();
let seq = 0;

const send = (msg: FromWorkflowWorker) => self.postMessage(msg);

function hostCall(fn: WorkflowHostFnName, args: unknown[], pos?: string): Promise<string> {
  const id = ++seq;
  const p = new Promise<string>((resolve, reject) => pending.set(id, { resolve, reject }));
  send({ type: "host", id, fn, args, ...(pos === undefined ? {} : { pos }) });
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
 * worker module into the host would evaluate this file's traps and message handler
 * in the server process — which would break `Date.now()` for the whole server.
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
 *
 * The slot is claimed SYNCHRONOUSLY, on the way in, before the first `await`. That is
 * what makes the coordinate a property of the script's structure: an `agent()` issued
 * from a stage callback is numbered by the frame that called it, whatever else is in
 * flight at that instant.
 */
const agent = async (prompt: string, opts?: AgentOpts): Promise<unknown> => {
  const pos = posOf(claimSlot());
  const report = await hostCall("agent", [String(prompt), JSON.stringify(opts ?? {})], pos);
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
 *
 * Each thunk runs in its own frame, numbered by its SLOT INDEX. Slot 3's agents are
 * `…3.0`, `…3.1`, … whether slot 3 started first or last, which is what keeps a
 * relaunch's positions matching under varying agent latency.
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
  const base = claimSlot();
  return Promise.all(
    thunks.map((t, slot) =>
      Promise.resolve()
        .then(() =>
          frames.run(
            childFrame([...base, slot]),
            () => (typeof t === "function" ? (t as () => unknown)() : t),
          )
        )
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
 *
 * Every stage callback runs in a frame named by its (item, stage) CELL, so the agents
 * it issues are `…item.stage.n`. That is the fix for the defect this combinator caused:
 * without it, stage 2's journal positions came out in stage-1 completion order, and an
 * unchanged relaunch of the spec's own example re-ran every call past stage 1.
 * Coordinates are item-major — item 0's whole run of stages precedes item 1's — which
 * gives a total order over the cells that no interleaving can disturb.
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
  const base = claimSlot();
  return Promise.all(items.map(async (item, index) => {
    let prev: unknown = item;
    try {
      for (let s = 0; s < stages.length; s++) {
        const stage = stages[s];
        if (typeof stage !== "function") {
          throw new TypeError("pipeline(items, ...stages): every stage must be a function");
        }
        // `prev` is read before the frame is entered, so the value threaded between
        // stages is unaffected by the coordinate machinery.
        const carried = prev;
        // STAGE-MAJOR, not item-major. The coordinate is the frontier the replay
        // prefix is compared against, so its order has to be the CAUSAL order or the
        // frontier stops meaning anything.
        //
        // Item-major ([...base, index, s]) put every cell of item 0 before any cell of
        // item 1. But pipeline has no barrier — that is the point of it — so item 1's
        // stage 1 routinely dispatches before item 0's stage 2. A cell sorting BEFORE
        // the divergence could therefore be dispatched AFTER it, sail past the
        // `blocked` test, and replay: a verdict computed against a tree a live agent
        // had already begun rewriting. That is the stale hit the whole prefix rule
        // exists to prevent, and it was a regression against the old dispatch-index
        // scheme, which had the temporal property and lacked only reproducibility.
        //
        // Stage-major recovers both. Every stage-s cell happens-after its own item's
        // stage-(s-1) cell, so structural order implies causal order; stage-s cells
        // across items are mutually concurrent and sort adjacently, so nothing imposes
        // an order the script did not declare. Coordinates stay latency-independent,
        // so the transposition defect this numbering was introduced to fix stays fixed.
        prev = await frames.run(
          childFrame([...base, s, index]),
          () => stage(carried, item, index),
        );
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

  // Stop requested. A script binds nothing that spawns, so there are no children to
  // sweep and the ack is immediate — but it is still an ack rather than a bare
  // terminate(), so the host's wind-down is one handshake for both workers
  // (plan §6.3).
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
