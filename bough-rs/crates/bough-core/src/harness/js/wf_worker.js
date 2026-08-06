// The script side of the workflow worker — adapted from src/harness/wf_worker.ts
// for the Rust port: postMessage → stdout NDJSON, onmessage → readline on stdin.
// Runs under Bun or Node >= 20 as CommonJS; harness/wf.rs ships this file via
// include_str! and materializes it to a cache dir at first use. Do not edit the
// cached copy — edit crates/bough-core/src/harness/js/wf_worker.js.
//
// This module IS the worker, and the orchestration script it executes gets
// exactly five names plus its input — `agent`, `phase`, `log`, `parallel`,
// `pipeline`, `args` (plus `console`, which aliases `log`). A workflow is a
// *plan*, not a program; the work happens in the subagents `agent()` reaches on
// the host (spec §8).
//
// That five-name world is a CONTRACT, not a cage. The sidecar inherits the
// server process's capabilities, so a script that goes looking for `fetch` or a
// dynamic `import()` will find them. Nothing in the engine relies on the denial
// — the traps below are about replay correctness, not confinement — but the
// comments must not claim a boundary that does not exist.
//
// THE INVARIANT THIS HOLDS: **a workflow script is deterministic, and its
// combinators have exactly the concurrency semantics the spec states.** Two
// mechanisms, both load-bearing for something other than tidiness:
//
//   1. **The determinism trap.** `Date.now()`, argless `new Date()`,
//      `Math.random()` — and, for the same reason, `performance.now()` and
//      `crypto.randomUUID()`/`getRandomValues()` — THROW here. Journal rerun
//      keys each `agent()` call by `hash(prompt + opts)` and replays the hits
//      instantly. A script that stamps a clock reading or a random id into a
//      prompt produces a fresh key on every run, so "rerun replays unchanged
//      calls" becomes a lie that fails as *wrong output*, not as an error — the
//      run silently costs full price and nobody is told.
//   2. **The exit trap.** `process.exit()` would terminate the sidecar silently
//      and strand the run until its wall timeout with nothing to report.
//
// COMBINATOR SEMANTICS, which are exact and not negotiable (spec §8):
//
//   - `parallel(thunks)` is a **barrier**: it awaits all of them. A thunk that
//     throws resolves to `null` in its slot, and the call itself NEVER rejects.
//   - `pipeline(items, ...stages)` has **no barrier between stages**: each item
//     flows through every stage independently, so item B can be in stage 3
//     while item A is still in stage 1. A stage that throws drops that item to
//     `null` and skips its remaining stages.
//
// THE THIRD MECHANISM: **every `agent()` call carries a STRUCTURAL COORDINATE,
// computed from the script's shape rather than from the order calls happen to
// reach the host.** The journal is prefix-bounded, so a call's POSITION is part
// of its identity; `pipeline()` is designed NOT to fix arrival order, so
// deriving position from arrival re-billed every call past stage 1 on an
// unchanged relaunch. Frames propagate through `AsyncLocalStorage`, not a
// module-level variable: a stage callback may await before it calls `agent()`,
// and by then a dynamically-scoped global would name whichever other item
// resumed most recently.
"use strict";

const readline = require("node:readline");
const { Console } = require("node:console");
const { AsyncLocalStorage } = require("node:async_hooks");

// Saved before the traps land: the worker itself still needs a real exit when
// the host closes stdin, and its own bookkeeping must not throw on a clock it
// denies the script.
const realExit = process.exit.bind(process);

// stdout IS the protocol channel. The worker's own console — and any stray
// global console use — goes to stderr so it can never corrupt the NDJSON
// stream. The script's `console` is the bound parameter below, which becomes
// `workflow.log` lines.
try {
  globalThis.console = new Console(process.stderr, process.stderr);
} catch { /* frozen globals */ }

// ---------------------------------------------------------------------------
// Mirrors of protocol.rs / wf.rs. The Rust probe test runs a real script
// printing `typeof` of every name — that test is what keeps these lists and the
// Rust lists from drifting (it replaces the TS shared-import invariant).
// ---------------------------------------------------------------------------

const WORKFLOW_SCRIPT_PARAMS = ["agent", "phase", "log", "args"];
const SCRIPT_PARAMS = [...WORKFLOW_SCRIPT_PARAMS, "parallel", "pipeline", "console"];

// ---------------------------------------------------------------------------
// Structural coordinates
// ---------------------------------------------------------------------------

// One nesting level of the script's shape. `path` is where this frame sits;
// `next` is the slot the next child created inside it will take.
//
// Mutable `next` on a shared object is the point: every call made in one frame
// draws from the same counter, so siblings are numbered in the order the script
// CREATES them — a fact about the source text and its sequential control flow,
// not about scheduling.
const ROOT = { path: [], next: 0 };
const frames = new AsyncLocalStorage();

function currentFrame() {
  return frames.getStore() ?? ROOT;
}

/** Claim the enclosing frame's next slot. Synchronous, and must stay that way. */
function claimSlot() {
  const frame = currentFrame();
  return [...frame.path, frame.next++];
}

function childFrame(path) {
  return { path, next: 0 };
}

/** The wire form: dot-joined slot indexes, e.g. `0.1.1.0`. */
function posOf(path) {
  return path.join(".");
}

// ---------------------------------------------------------------------------
// The determinism trap
// ---------------------------------------------------------------------------

function nondeterministic(what, instead) {
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
  // working (a script may format a timestamp it was handed through `args`), and
  // only the ARGLESS construction — the one that reads the wall clock — is denied.
  const GuardedDate = new Proxy(RealDate, {
    construct(target, argArray, newTarget) {
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
  globalThis.Date = GuardedDate;

  Math.random = () => {
    throw nondeterministic("Math.random()", VARY_BY_INDEX);
  };

  // The same hazard wearing different names. `performance.now()` is a clock, and
  // a uuid or a random buffer in a prompt guarantees zero journal hits forever.
  const perf = globalThis.performance;
  if (perf) {
    perf.now = () => {
      throw nondeterministic("performance.now()", PASS_TIMESTAMPS);
    };
  }
  const c = globalThis.crypto;
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
  // nondeterministic script degrades to "rerun re-runs everything", not to
  // silence.
}

// ---------------------------------------------------------------------------
// The exit trap
// ---------------------------------------------------------------------------

const exitTrap = (code) => {
  throw new Error(
    `exit(${code ?? 0}) is not available in a workflow — a script ends by returning its ` +
      `result, and signals failure by throwing an Error. Calling exit() would terminate ` +
      `the run mid-flight with nothing to report.`,
  );
};
try {
  if (process) process.exit = exitTrap;
} catch { /* frozen globals — nothing to guard */ }

// ---------------------------------------------------------------------------
// The bridge
// ---------------------------------------------------------------------------

const send = (msg) => {
  process.stdout.write(JSON.stringify(msg) + "\n");
};

const pending = new Map();
let seq = 0;

function hostCall(fn, args, pos) {
  const id = ++seq;
  const p = new Promise((resolve, reject) => pending.set(id, { resolve, reject }));
  send({ type: "host", id, fn, args, ...(pos === undefined ? {} : { pos }) });
  return p;
}

function show(v) {
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
 * `agent(prompt, opts?)` — run one subagent, resolve to its report.
 *
 * MUST throw when the subagent fails: that is what makes `parallel()` map the
 * slot to `null` and `pipeline()` drop the item. The report crosses the wire
 * verbatim; only a `{schema}` call parses it, because only then is it JSON by
 * contract.
 *
 * The slot is claimed SYNCHRONOUSLY, on the way in, before the first `await`.
 * That is what makes the coordinate a property of the script's structure.
 */
const agent = async (prompt, opts) => {
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
 * Fire-and-forget progress. A workflow NEVER blocks on display, so these return
 * nothing and swallow their own transport failures: a wedged UI must not be able
 * to stall a fan-out, and a script that `await`s a phase marker by mistake gets
 * an already-resolved promise rather than a hang.
 */
const phase = (title) => void hostCall("phase", [String(title)]).catch(() => {});
const log = (message) => void hostCall("log", [show(message)]).catch(() => {});

/**
 * `parallel(thunks)` — a barrier that never rejects. Every thunk runs; a thunk
 * that throws (or that isn't callable) resolves to `null` in its slot.
 *
 * `Promise.resolve().then(t)` rather than `t()` so a thunk that throws
 * SYNCHRONOUSLY lands in the same `.catch` as one that rejects — otherwise the
 * whole call would throw before a single sibling started.
 *
 * Each thunk runs in its own frame, numbered by its SLOT INDEX. Slot 3's agents
 * are `…3.0`, `…3.1`, … whether slot 3 started first or last, which is what
 * keeps a relaunch's positions matching under varying agent latency.
 */
const parallel = (thunks) => {
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
          frames.run(childFrame([...base, slot]), () => (typeof t === "function" ? t() : t))
        )
        .catch(() => null)
    ),
  );
};

/**
 * `pipeline(items, ...stages)` — each item flows through every stage
 * independently.
 *
 * NO BARRIER between stages, and that is the whole point of the primitive: with
 * N items and a slow one among them, a barrier would idle every other item at
 * each boundary. A throwing stage drops THAT item to `null` and skips its
 * remaining stages; the other items are untouched. The returned promise resolves
 * once every item has settled, in input order.
 */
const pipeline = (items, ...stages) => {
  if (!Array.isArray(items)) {
    return Promise.reject(new TypeError("pipeline(items, ...stages): `items` must be an array"));
  }
  const base = claimSlot();
  return Promise.all(items.map(async (item, index) => {
    let prev = item;
    try {
      for (let s = 0; s < stages.length; s++) {
        const stage = stages[s];
        if (typeof stage !== "function") {
          throw new TypeError("pipeline(items, ...stages): every stage must be a function");
        }
        // `prev` is read before the frame is entered, so the value threaded
        // between stages is unaffected by the coordinate machinery.
        const carried = prev;
        // STAGE-MAJOR, not item-major. The coordinate is the frontier the replay
        // prefix is compared against, so its order has to be the CAUSAL order or
        // the frontier stops meaning anything.
        //
        // Item-major ([...base, index, s]) put every cell of item 0 before any
        // cell of item 1. But pipeline has no barrier — that is the point of it
        // — so item 1's stage 1 routinely dispatches before item 0's stage 2. A
        // cell sorting BEFORE the divergence could therefore be dispatched
        // AFTER it, sail past the `blocked` test, and replay: a verdict computed
        // against a tree a live agent had already begun rewriting.
        //
        // Stage-major recovers both properties. Every stage-s cell
        // happens-after its own item's stage-(s−1) cell, so structural order
        // implies causal order; stage-s cells across items are mutually
        // concurrent and sort adjacently, so nothing imposes an order the script
        // did not declare.
        prev = await frames.run(childFrame([...base, s, index]), () => stage(carried, item, index));
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

const AsyncFunction = Object.getPrototypeOf(async function () {}).constructor;

async function run(code, args) {
  const scope = { agent, phase, log, args, parallel, pipeline, console: scriptConsole };
  // Built from the array, in order, so the list is spelled out exactly once.
  const script = new AsyncFunction(...SCRIPT_PARAMS, code);
  return await script(...SCRIPT_PARAMS.map((name) => scope[name]));
}

// ---------------------------------------------------------------------------
// stdin — the message loop
// ---------------------------------------------------------------------------

const rl = readline.createInterface({ input: process.stdin, terminal: false });

rl.on("line", (raw) => {
  let msg;
  try {
    msg = JSON.parse(raw);
  } catch {
    return; // not protocol — dropped
  }

  // Stop requested. A script binds nothing that spawns, so there are no children
  // to sweep and the ack is immediate — but it is still an ack rather than a
  // bare terminate(), so the host's wind-down is one handshake for both workers.
  if (msg.type === "abort") {
    for (const p of pending.values()) p.reject(new Error("workflow stopped"));
    pending.clear();
    send({ type: "aborted" });
    return;
  }

  if (msg.type === "host_result") {
    const p = pending.get(msg.id);
    pending.delete(msg.id);
    if (!p) return; // unknown pending id — dropped silently
    if (msg.ok) p.resolve(msg.value);
    else p.reject(new Error(msg.value));
    return;
  }

  // Pre-flight parse, delegated here for engine parity: constructing the
  // AsyncFunction parses, it does not execute, and the code never touches this
  // scope. The host shapes the model-facing message from the raw engine words.
  if (msg.type === "check") {
    try {
      new AsyncFunction(...SCRIPT_PARAMS, msg.code);
      send({ type: "check_result" });
    } catch (err) {
      send({
        type: "check_result",
        name: (err && err.name) || "Error",
        message: String((err && err.message) ?? err),
      });
    }
    return;
  }

  if (msg.type === "run") {
    let args = null;
    try {
      args = JSON.parse(msg.argsJson);
    } catch {
      // A run with unparseable input still runs, with `args` null — the script's
      // own guards are a better error than a dead worker.
    }
    run(msg.code, args)
      .then((result) => {
        let resultJson = "null";
        try {
          resultJson = JSON.stringify(result ?? null) ?? "null";
        } catch {
          // Unserializable return value: the run finishes as null rather than
          // dying with nothing to report.
        }
        send({ type: "done", resultJson });
      })
      .catch((err) => send({ type: "error", message: String((err && err.stack) ?? err), logs: [] }));
  }
});

// The host closed stdin: the run is over either way. Use the saved real exit —
// `process.exit` is trapped above.
rl.on("close", () => realExit(0));
