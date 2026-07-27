/**
 * The workflow engine: the host side of the `permissions: "none"` worker, plus the
 * journal that makes rerun cheap.
 *
 * WHY THIS EXISTS. Subagent fan-out is capped at 8 per turn and 4 concurrent
 * tree-wide (spec §7), which is right for delegation inside a turn and useless for
 * "audit these 300 handlers". A workflow lifts that ceiling by moving the loop into
 * a script that runs DETACHED from the turn that started it: the script owns the
 * control flow, this module owns the agents, and the turn that called
 * `workflow.start` is free to end (spec §8).
 *
 * THE INVARIANT THIS HOLDS: **every `agent()` call is journaled by key before it
 * runs, and a rerun replays a hit instead of paying for it.** `key` is
 * `hash(prompt + label + phase + model + schema)` — everything that decides what the
 * subagent will be asked. So editing one prompt in a 300-agent script and rerunning
 * costs exactly the edited call, and that is the entire iteration loop for a
 * workflow. Two consequences that shape the code below:
 *
 *   - The journal row is written BEFORE the semaphore is acquired, so the run view
 *     can show a queued agent, and `startedAt` is reset when the call actually
 *     starts — otherwise a saturated run shows N agents "working" while only
 *     `concurrency` of them are.
 *   - Only successful calls replay. A failed call re-runs live, because the failure
 *     may well have been the thing the author just fixed.
 *
 * Determinism is the other half of that bargain, and it is enforced in the worker
 * (`harness/wf_worker.ts`): a script that stamps `Date.now()` into a prompt would
 * produce a fresh key every run and silently make replay a no-op (plan §6.15).
 *
 * WHAT IS NOT HERE.
 *   - **Meta extraction.** `meta` is a pure literal the submit boundary extracts and
 *     validates (`workflow/meta.ts`, T5.2) before calling in here; this module takes
 *     the validated shape as a parameter and never parses the script. A rerun with no
 *     explicit meta inherits the source run's.
 *   - **Structured output.** `{schema}` travels through as an opaque part of the call
 *     — it is journaled into the key and handed to the `AgentRunner`, which is where
 *     T5.3 wires `zodOutputFormat`/`messages.parse`.
 *   - **REST and the `workflow.*` verb.** The routes and the program-side dispatcher
 *     are T5.5; everything they need is exported from here.
 *
 * The `AgentRunner` is injected, so the whole engine — worker, journal, semaphore,
 * pause gate, replay — is drivable offline with no LLM, no key and no subagent
 * (plan §7). Production wires `agents/subagent.ts` behind it.
 *
 * Ported from `src/workflow.ts`. Deltas from that port are marked `NOTE:`.
 */

import { NotFoundError, WorkflowError, WorkflowScriptError } from "../errors.ts";
import {
  type FromWorkflowWorker,
  WORKFLOW_HOST_FN_NAMES,
  WORKFLOW_SCRIPT_PARAMS,
  type WorkflowHostCallMessage,
  type WorkflowHostFnName,
} from "../harness/protocol.ts";
import { unterminatedString } from "../harness/vm.ts";
import { workflowScriptPath, workflowsDir } from "../paths.ts";
import type { WorkflowPhase, WorkflowRun } from "../schema/parts.ts";
import type { Bus, Db, WorkflowHostFns } from "../types.ts";

// ---------------------------------------------------------------------------
// Limits
// ---------------------------------------------------------------------------

/**
 * How many agents a run may have in flight. The run's OWN semaphore — the subagent
 * caps deliberately do not apply inside a workflow, so a script queues as many calls
 * as the job needs and this is what meters them (spec §8).
 */
export function workflowConcurrency(): number {
  const n = Number(Deno.env.get("BOUGH_WORKFLOW_CONCURRENCY"));
  return Number.isFinite(n) && n > 0 ? n : 4;
}

/** Wall-clock ceiling on a whole run. A liveness backstop, not a budget. */
export function workflowTimeoutMs(): number {
  const n = Number(Deno.env.get("BOUGH_WORKFLOW_TIMEOUT_MS"));
  return Number.isFinite(n) && n > 0 ? n : 60 * 60_000;
}

/**
 * Lifetime agent cap per run — a runaway-loop backstop, not a working limit. A
 * script that means to launch 300 agents is doing its job; one that means to launch
 * 300 and has an off-by-one in a `while` is not, and without this it bills until
 * someone notices.
 */
export const MAX_AGENTS_PER_RUN = 200;

// ---------------------------------------------------------------------------
// Pre-flight
// ---------------------------------------------------------------------------

/**
 * The names a script is compiled with: the three bridged verbs and `args` from the
 * frozen `WORKFLOW_SCRIPT_PARAMS`, plus the two pure combinators and `console` that
 * `harness/wf_worker.ts` builds worker-side.
 *
 * NOTE / design gap, surfaced rather than worked around: `protocol.ts` is frozen and
 * declares only `WORKFLOW_SCRIPT_PARAMS`, so this extension is spelled out in two
 * files. It cannot be imported from the worker — that module is a `deno.worker`
 * entry point whose traps and `onmessage` would run in the server process. The drift
 * is pinned behaviorally instead: `run.test.ts` probes a real worker for every name
 * in this list.
 */
export const WORKFLOW_PROGRAM_PARAMS = [
  ...WORKFLOW_SCRIPT_PARAMS,
  "parallel",
  "pipeline",
  "console",
] as const;

// deno-lint-ignore no-explicit-any
const AsyncFunction = Object.getPrototypeOf(async function () {}).constructor as any;

/**
 * The body the worker actually runs: `export const meta = …` demoted to a plain
 * `const`, which leaves a harmless local binding and — unlike removing the statement
 * — preserves every line number, so a syntax error's position matches the script the
 * author wrote.
 */
export function workflowBody(script: string): string {
  return script.replace(/export\s+const\s+meta\s*=/, "const meta =");
}

/**
 * Compile-check a script before a worker is spawned. Returns the message to hand the
 * author, or `null` when it parses. Same contract and the same shadow/newline
 * diagnostics as the program worker's pre-flight (`harness/vm.ts`), against the
 * workflow parameter list.
 */
export function checkWorkflowSyntax(body: string): string | null {
  try {
    new AsyncFunction(...WORKFLOW_PROGRAM_PARAMS, body);
    return null;
  } catch (err) {
    if ((err as Error)?.name !== "SyntaxError") throw err;
    const why = (err as Error).message;
    const shadow = /Identifier '([^']+)' has already been declared/.exec(why);
    if (shadow && (WORKFLOW_PROGRAM_PARAMS as readonly string[]).includes(shadow[1])) {
      return `workflow script does not parse: ${why} — \`${shadow[1]}\` is bound in every ` +
        `workflow's scope, so declaring it shadows the binding. Rename your variable and ` +
        `call \`${shadow[1]}\` as it is.`;
    }
    const hit = unterminatedString(body);
    if (!hit) return `workflow script does not parse: ${why}`;
    return `workflow script does not parse: ${why} — line ${hit.line}: a ${
      hit.quote === '"' ? "double" : "single"
    }-quoted string is closed by a real newline.`;
  }
}

// ---------------------------------------------------------------------------
// The call, and the seam that runs it
// ---------------------------------------------------------------------------

/** What one `agent()` call asks for, parsed from the worker's bridged JSON. */
export interface AgentCall {
  prompt: string;
  /** The journal/display label. Never empty — defaulted from the prompt. */
  label: string;
  phase?: string;
  model?: string;
  /** A JSON Schema (T5.3). Opaque here; part of what `key` hashes. */
  schema?: unknown;
}

/**
 * Runs one agent call to completion. Production adapts `agents/subagent.ts`; tests
 * inject a fake, which is what keeps this whole module offline.
 *
 * Resolves with the report VERBATIM — the string that lands in the journal and comes
 * back on a replay, so a replayed call and a live one are indistinguishable to the
 * script. MUST reject on failure: rejection is what makes `parallel()` map the slot
 * to `null` and `pipeline()` drop the item.
 */
export type AgentRunner = (
  call: AgentCall,
  signal: AbortSignal,
  onSpawned: (subagentSessionId: string) => void,
) => Promise<string>;

export interface WorkflowCtx {
  db: Db;
  bus: Bus;
  runner: AgentRunner;
  /**
   * Deliver the finished-run note to the owning session (`agents/notes.ts`). Absent =
   * the run still lands in the database and on the bus; nobody is woken.
   */
  notify?: (sessionId: string, text: string) => void;
  /** Injected clock. Absent = `Date.now`. */
  now?: () => number;
}

/** The validated `meta` literal, extracted at the submit boundary. */
export interface WorkflowMetaInput {
  name: string;
  description: string;
  phases?: WorkflowPhase[];
}

export interface StartOpts {
  sessionId: string;
  script: string;
  /** Absent = inherited from `resumeOf`, else a plain default. See the header. */
  meta?: WorkflowMetaInput;
  args?: unknown;
  /** Journal-replay source: matching calls return that run's results instantly. */
  resumeOf?: string;
  /** Overrides for the run's semaphore and wall clock. Absent = the env defaults. */
  concurrency?: number;
  timeoutMs?: number;
  /**
   * The model a call that names none will actually run on (session pin, else the
   * ctx default, else the built-in). Folded into the journal key so a rerun after
   * a model change re-runs instead of replaying the old model's answers.
   */
  effectiveModel?: string;
}

// ---------------------------------------------------------------------------
// Journal keys and labels (pure)
// ---------------------------------------------------------------------------

function clip(s: string, n: number): string {
  return s.length > n ? `${s.slice(0, n - 1)}…` : s;
}

/**
 * FNV-1a over the canonical call shape — the journal replay key. Two passes with
 * different offsets so an accidental 32-bit collision would have to happen twice;
 * a collision here silently returns another agent's report.
 *
 * NOTE: `schema` joins the hashed shape (the port predates structured output).
 * Changing a schema changes what the subagent is asked to produce, so a rerun must
 * treat it as a different call.
 */
export function callKey(call: AgentCall, effectiveModel?: string): string {
  const s = JSON.stringify([
    call.prompt,
    call.label,
    call.phase ?? "",
    // The RESOLVED model, not just one the script named. A script that names no
    // model still runs on *something* — session pin, else ctx default, else the
    // built-in — and hashing only `call.model` made that invisible. Repinning the
    // session and rerunning a byte-identical script then replayed every row from
    // cache and handed back the OLD model's answers as a fresh run on the new one:
    // silent staleness, the exact failure this key exists to prevent.
    call.model ?? effectiveModel ?? "",
    // Canonicalized: JSON.stringify preserves insertion order, so a reordered or
    // prettier-formatted schema literal hashed differently and re-ran every call
    // that used it. Same schema, same key, whatever order it was written in.
    canonicalJson(call.schema ?? null),
  ]);
  let a = 0x811c9dc5, b = 0x01000193;
  for (let i = 0; i < s.length; i++) {
    a = (a ^ s.charCodeAt(i)) >>> 0;
    a = Math.imul(a, 0x01000193) >>> 0;
    b = (b ^ ((s.charCodeAt(i) + 7) & 0xffff)) >>> 0;
    b = Math.imul(b, 0x01000193) >>> 0;
  }
  // Zero-padded: without it the boundary between the two 32-bit halves floats,
  // so (0x1, 0x23) and (0x12, 0x3) both encoded "123". ~12% of keys were short.
  return a.toString(16).padStart(8, "0") + b.toString(16).padStart(8, "0");
}

/**
 * Order-independent JSON for hashing: objects get their keys sorted, recursively.
 * Arrays keep their order — position is meaning there.
 */
function canonicalJson(v: unknown): unknown {
  if (v === null || typeof v !== "object") return v;
  if (Array.isArray(v)) return v.map(canonicalJson);
  const out: Record<string, unknown> = {};
  for (const k of Object.keys(v as Record<string, unknown>).sort()) {
    out[k] = canonicalJson((v as Record<string, unknown>)[k]);
  }
  return out;
}

/**
 * The display label for a call that passed none. The naive fallback — the prompt's
 * first line — collapses a fan-out into N identical rows whenever the script shares a
 * preamble across its agents, which is the normal way to write one. Field case
 * (2026-07-24): seven module-discovery agents all read "You are contributing evidence
 * to a thoro…" in the run view.
 *
 * So walk the prompt for the first line no sibling has claimed — in a shared-preamble
 * fan-out that is exactly the line carrying this agent's assignment. `taken` is the
 * labels already in the run.
 *
 * Display only: `callKey` hashes the deterministic first-line label, so replay never
 * depends on which siblings happened to exist.
 */
export function distinctLabel(prompt: string, taken: string[]): string {
  const lines = prompt.trim().split("\n").map((l) => l.trim()).filter(Boolean);
  for (const line of lines) {
    const candidate = clip(line, 40);
    if (!taken.includes(candidate)) return candidate;
  }
  // Every line collides (identical prompts): number them so they stay separable.
  const base = clip(lines[0] ?? "agent", 36);
  return `${base} #${taken.filter((t) => t.startsWith(base)).length + 1}`;
}

// ---------------------------------------------------------------------------
// The live registry
// ---------------------------------------------------------------------------

/**
 * In-flight runs, by id. Process-wide on purpose, like `hostfn/jobs.ts` and
 * `agents/caps.ts`: a run outlives the turn, the request and the client that started
 * it, so a per-caller instance would hold nothing. A server restart empties it, which
 * is precisely what `recoverOrphanedWorkflows` reconciles at boot.
 */
interface LiveRun {
  ctrl: AbortController;
  worker: Worker;
  paused: boolean;
  /** Resolvers parked on the pause gate, released FIFO. */
  gate: Array<() => void>;
  timer?: ReturnType<typeof setTimeout>;
}

const live = new Map<string, LiveRun>();

/** Is this run still executing in this process? */
export function isWorkflowLive(id: string): boolean {
  return live.has(id);
}

function publishRun(ctx: Pick<WorkflowCtx, "db" | "bus">, id: string): WorkflowRun | undefined {
  const run = ctx.db.getWorkflow(id);
  if (run) ctx.bus.publish({ type: "workflow.updated", sessionId: run.sessionId, data: run });
  return run;
}

function publishAgent(
  ctx: Pick<WorkflowCtx, "db" | "bus">,
  sessionId: string,
  runId: string,
  agentId: string,
): void {
  const row = ctx.db.listWorkflowAgents(runId).find((a) => a.id === agentId);
  if (row) ctx.bus.publish({ type: "workflow.agent", sessionId, data: row });
}

// ---------------------------------------------------------------------------
// Starting a run
// ---------------------------------------------------------------------------

/**
 * Start a workflow: persist the run and its script mirror, build the journal-replay
 * map when resuming, and launch the sealed worker.
 *
 * Returns the run row IMMEDIATELY — the script is detached from here on. Progress
 * flows over `workflow.*` bus events and completion posts a system note, which is
 * what lets the turn that called `workflow.start` end while the fan-out continues
 * (spec §8).
 */
export async function startWorkflow(ctx: WorkflowCtx, opts: StartOpts): Promise<WorkflowRun> {
  const { db, bus } = ctx;
  const now = ctx.now ?? Date.now;

  if (!db.getSession(opts.sessionId)) {
    throw new NotFoundError(`session ${opts.sessionId} not found`);
  }
  if (typeof opts.script !== "string" || !opts.script.trim()) {
    throw new WorkflowScriptError("workflow: script must be a non-empty string");
  }
  const body = workflowBody(opts.script);
  const bad = checkWorkflowSyntax(body);
  if (bad) throw new WorkflowScriptError(bad);

  // Journal replay. Only successful calls replay — a failed one re-runs live, because
  // the failure may be the very thing this edit fixes. FIFO per key so N identical
  // calls replay their N results in the order they were made.
  const replay = new Map<string, string[]>();
  let args: unknown = opts.args ?? null;
  let meta = opts.meta;
  if (opts.resumeOf) {
    const src = db.getWorkflow(opts.resumeOf);
    if (!src) throw new NotFoundError(`workflow ${opts.resumeOf} not found`);
    if (opts.args === undefined) args = src.args; // a rerun keeps its input by default
    meta ??= { name: src.name, description: src.description, phases: src.phases };
    for (const a of db.listWorkflowAgents(opts.resumeOf)) {
      if ((a.status === "done" || a.status === "cached") && a.result !== null) {
        const q = replay.get(a.key) ?? [];
        q.push(a.result);
        replay.set(a.key, q);
      }
    }
  }

  const id = crypto.randomUUID();
  const run = db.createWorkflow({
    id,
    sessionId: opts.sessionId,
    name: meta?.name ?? "workflow",
    description: meta?.description ?? "",
    script: opts.script,
    phases: meta?.phases ?? [],
    status: "running",
    currentPhase: null,
    result: null,
    error: null,
    args,
    resumeOf: opts.resumeOf ?? null,
    createdAt: now(),
    finishedAt: null,
  });

  // Mirror the script to a real file so "edit it and rerun" is a file edit away
  // (spec §8). A convenience — the canonical script is the row.
  try {
    await Deno.mkdir(workflowsDir(), { recursive: true });
    await Deno.writeTextFile(workflowScriptPath(id), opts.script);
  } catch { /* mirror is best-effort */ }

  bus.publish({ type: "workflow.updated", sessionId: run.sessionId, data: run });

  const ctrl = new AbortController();
  const worker = new Worker(new URL("../harness/wf_worker.ts", import.meta.url).href, {
    type: "module",
    // The script orchestrates; it does not act. Its whole world is agent/phase/log
    // plus `args` (spec §8).
    deno: { permissions: "none" },
  });
  const state: LiveRun = { ctrl, worker, paused: false, gate: [] };
  live.set(id, state);

  const limit = opts.concurrency ?? workflowConcurrency();
  let idx = 0;
  let inFlight = 0;
  const queue: Array<() => void> = [];
  const acquire = () =>
    new Promise<void>((resolve) => {
      if (inFlight < limit) {
        inFlight++;
        resolve();
      } else queue.push(() => (inFlight++, resolve()));
    });
  const release = () => {
    inFlight--;
    queue.shift()?.();
  };
  const awaitGate = () =>
    state.paused ? new Promise<void>((resolve) => state.gate.push(resolve)) : Promise.resolve();

  const finish = (status: "done" | "error" | "stopped", result?: unknown, error?: string) => {
    if (!live.has(id)) return;
    live.delete(id);
    clearTimeout(state.timer);
    worker.terminate();
    // Aborting the run's controller is what interrupts in-flight subagent TURNS —
    // killing the worker only stops the script (spec §8: stop does both).
    ctrl.abort();
    for (const a of db.listWorkflowAgents(id)) {
      if (a.status === "running" || a.status === "queued") {
        db.updateWorkflowAgent(a.id, { status: "stopped", finishedAt: now() });
      }
    }
    db.updateWorkflow(id, {
      status,
      result: result ?? null,
      error: error ?? null,
      finishedAt: now(),
    });
    const updated = publishRun(ctx, id);
    if (ctx.notify && updated) {
      const agents = db.listWorkflowAgents(id);
      const okCount = agents.filter((a) => a.status === "done" || a.status === "cached").length;
      const head = `[workflow ${status}] "${updated.name}" (${id}) — ` +
        `${okCount}/${agents.length} agents succeeded.`;
      const tail = status === "done"
        ? `Result:\n${clip(JSON.stringify(result ?? null, null, 2), 4000)}`
        : status === "error"
        ? `Error: ${clip(error ?? "unknown", 2000)}`
        : "Stopped by the user.";
      ctx.notify(updated.sessionId, `${head}\n${tail}`);
    }
  };

  const timeoutMs = opts.timeoutMs ?? workflowTimeoutMs();
  state.timer = setTimeout(
    () => finish("error", undefined, `workflow timed out after ${timeoutMs}ms`),
    timeoutMs,
  );

  // ---- the three bridged verbs ------------------------------------------------

  const host: WorkflowHostFns = {
    phase(title: string): Promise<string> {
      db.updateWorkflow(id, { currentPhase: String(title) });
      publishRun(ctx, id);
      return Promise.resolve("");
    },

    log(message: string): Promise<string> {
      bus.publish({
        type: "workflow.log",
        sessionId: run.sessionId,
        data: { runId: id, line: String(message) },
      });
      return Promise.resolve("");
    },

    async agent(prompt: string, optsJson: string): Promise<string> {
      const raw = parseAgentOpts(optsJson);
      if (typeof prompt !== "string" || !prompt.trim()) {
        throw new WorkflowError(400, "agent(prompt, opts): prompt must be a non-empty string");
      }
      const call: AgentCall = {
        prompt,
        label: typeof raw.label === "string" && raw.label.trim()
          ? raw.label.trim()
          : clip(prompt.trim().split("\n")[0], 40),
        ...(typeof raw.phase === "string" ? { phase: raw.phase } : {}),
        ...(typeof raw.model === "string" ? { model: raw.model } : {}),
        ...(raw.schema !== undefined ? { schema: raw.schema } : {}),
      };
      const at = idx++;
      if (at >= MAX_AGENTS_PER_RUN) {
        throw new WorkflowError(
          429,
          `workflow agent cap reached (${MAX_AGENTS_PER_RUN} per run) — this is a ` +
            `runaway-loop backstop; split the work across separate runs`,
        );
      }

      // Pause parks the call BEFORE it journals: a parked call has no row, so the UI
      // never shows a "running" agent that has not actually started (field finding —
      // a sequential script's next call surfaced as running and session-less while
      // the run sat paused).
      await awaitGate();

      const key = callKey(call, opts.effectiveModel);
      // Display label: an explicit one wins; otherwise a line this agent does not
      // share with the siblings already in the run.
      const shown = typeof raw.label === "string" && raw.label.trim()
        ? call.label
        : distinctLabel(call.prompt, db.listWorkflowAgents(id).map((a) => a.label));
      const cached = replay.get(key)?.shift();
      const row = db.createWorkflowAgent({
        id: crypto.randomUUID(),
        runId: id,
        idx: at,
        key,
        label: shown,
        phase: call.phase ?? db.getWorkflow(id)?.currentPhase ?? null,
        prompt: call.prompt,
        model: call.model ?? null,
        status: cached !== undefined ? "cached" : "queued",
        result: cached ?? null,
        error: null,
        sessionId: null,
        startedAt: now(),
        finishedAt: cached !== undefined ? now() : null,
      });
      publishAgent(ctx, run.sessionId, id, row.id);
      // A journal hit: no live call, no semaphore slot, no cost.
      if (cached !== undefined) return cached;

      await acquire();
      try {
        if (ctrl.signal.aborted) throw new WorkflowError(409, "workflow stopped");
        // Off the semaphore: the clock starts HERE, not when the call journaled, so
        // elapsed time excludes time parked in the queue.
        db.updateWorkflowAgent(row.id, { status: "running", startedAt: now() });
        publishAgent(ctx, run.sessionId, id, row.id);
        try {
          const report = await ctx.runner(call, ctrl.signal, (sid) => {
            db.updateWorkflowAgent(row.id, { sessionId: sid });
            publishAgent(ctx, run.sessionId, id, row.id);
          });
          db.updateWorkflowAgent(row.id, {
            status: "done",
            result: report,
            finishedAt: now(),
          });
          publishAgent(ctx, run.sessionId, id, row.id);
          return report;
        } catch (err) {
          const message = (err as Error)?.message ?? String(err);
          db.updateWorkflowAgent(row.id, {
            status: ctrl.signal.aborted ? "stopped" : "error",
            error: message,
            finishedAt: now(),
          });
          publishAgent(ctx, run.sessionId, id, row.id);
          // Rethrown, not swallowed: the script's own combinators decide what a
          // failed agent means — `null` in a parallel() slot, a dropped item in a
          // pipeline() — and neither works if this resolves.
          throw err;
        }
      } finally {
        release();
      }
    },
  };

  // ---- the message loop -------------------------------------------------------

  const reply = (callId: number, ok: boolean, value: string) => {
    try {
      worker.postMessage({ type: "host_result", id: callId, ok, value });
    } catch { /* worker already terminated */ }
  };

  const hostCall = async (msg: WorkflowHostCallMessage) => {
    try {
      // Validate against the canonical list before indexing: the worker global is
      // reachable from the script, so `fn` is not guaranteed to be one of ours.
      if (!(WORKFLOW_HOST_FN_NAMES as readonly string[]).includes(msg.fn)) {
        throw new WorkflowError(400, `unknown workflow host function: ${msg.fn}`);
      }
      const fn = host[msg.fn as WorkflowHostFnName];
      // deno-lint-ignore no-explicit-any
      const value = await (fn as any).apply(host, msg.args);
      reply(msg.id, true, String(value));
    } catch (err) {
      reply(msg.id, false, (err as Error)?.message ?? String(err));
    }
  };

  worker.onmessage = async (e: MessageEvent) => {
    const msg = e.data as FromWorkflowWorker;
    if (msg.type === "done") {
      let result: unknown = null;
      try {
        result = JSON.parse(msg.resultJson);
      } catch { /* a script that returned something unserializable finishes as null */ }
      return finish("done", result);
    }
    if (msg.type === "error") return finish("error", undefined, msg.message);
    if (msg.type === "aborted") return; // wind-down ack; `finish` already terminated
    await hostCall(msg);
  };
  worker.onerror = (e) => {
    e.preventDefault();
    finish("error", undefined, `workflow worker error: ${e.message}`);
  };

  worker.postMessage({ type: "run", code: body, argsJson: JSON.stringify(args ?? null) ?? "null" });
  return run;
}

/** The `agent()` options blob, defensively parsed — it crossed a string-only wire. */
function parseAgentOpts(optsJson: string): {
  label?: unknown;
  phase?: unknown;
  model?: unknown;
  schema?: unknown;
} {
  try {
    const parsed = JSON.parse(optsJson ?? "{}");
    return parsed && typeof parsed === "object" ? parsed : {};
  } catch {
    return {};
  }
}

// ---------------------------------------------------------------------------
// Control
// ---------------------------------------------------------------------------

/**
 * Stop a run: kill the worker AND interrupt the subagent turns it started. Both,
 * because the worker holds the script and the run's abort signal holds the agents —
 * terminating only the worker would leave a fan-out billing with nobody reading it
 * (spec §8).
 */
export function stopWorkflow(
  ctx: Pick<WorkflowCtx, "db" | "bus" | "now">,
  id: string,
): WorkflowRun {
  const now = ctx.now ?? Date.now;
  const run = ctx.db.getWorkflow(id);
  if (!run) throw new NotFoundError(`workflow ${id} not found`);
  const state = live.get(id);
  if (!state) {
    // Not live here: either it already finished, or the process that owned it died.
    if (run.status === "running" || run.status === "paused") {
      ctx.db.updateWorkflow(id, { status: "orphaned", finishedAt: now() });
      return publishRun(ctx, id)!;
    }
    return run;
  }
  live.delete(id);
  clearTimeout(state.timer);
  // Release anything parked on the pause gate first, so no promise leaks with the
  // worker gone, then wind down.
  state.paused = false;
  state.gate.splice(0).forEach((open) => open());
  state.worker.terminate();
  state.ctrl.abort();
  for (const a of ctx.db.listWorkflowAgents(id)) {
    if (a.status === "running" || a.status === "queued") {
      ctx.db.updateWorkflowAgent(a.id, { status: "stopped", finishedAt: now() });
    }
  }
  ctx.db.updateWorkflow(id, { status: "stopped", finishedAt: now() });
  return publishRun(ctx, id)!;
}

/** Pause: NEW `agent()` calls park on the gate; running ones finish normally. */
export function pauseWorkflow(ctx: Pick<WorkflowCtx, "db" | "bus">, id: string): WorkflowRun {
  const state = live.get(id);
  if (!state) throw new WorkflowError(409, `workflow ${id} is not running in this process`);
  state.paused = true;
  ctx.db.updateWorkflow(id, { status: "paused" });
  return publishRun(ctx, id)!;
}

/** Resume: open the gate and release the parked calls, FIFO. */
export function resumeWorkflow(ctx: Pick<WorkflowCtx, "db" | "bus">, id: string): WorkflowRun {
  const state = live.get(id);
  if (!state) throw new WorkflowError(409, `workflow ${id} is not running in this process`);
  state.paused = false;
  state.gate.splice(0).forEach((open) => open());
  ctx.db.updateWorkflow(id, { status: "running" });
  return publishRun(ctx, id)!;
}

export interface RerunOpts {
  /**
   * Script override. Absent = the `~/.bough/workflows/<id>.js` mirror, which the user
   * may have edited, falling back to the stored script.
   */
  script?: string;
  args?: unknown;
  /** Absent = the source run's meta. Pass it when an edited script changed `meta`. */
  meta?: WorkflowMetaInput;
  /** See `StartOpts.effectiveModel`. Must resolve the same way the source run did. */
  effectiveModel?: string;
}

/**
 * Rerun a finished run with journal replay: unchanged `agent()` calls return the old
 * run's results instantly, edited and new ones run live. The script defaults to the
 * run's file mirror, so "edit the file, press r" is the whole iteration loop.
 *
 * A rerun is a NEW run pointing back via `resumeOf`, never an edit of the old one —
 * nothing in bough is destructively rewritten (spec §2.4).
 */
export async function rerunWorkflow(
  ctx: WorkflowCtx,
  id: string,
  opts: RerunOpts = {},
): Promise<WorkflowRun> {
  const src = ctx.db.getWorkflow(id);
  if (!src) throw new NotFoundError(`workflow ${id} not found`);
  if (live.has(id)) {
    throw new WorkflowError(409, `workflow ${id} is still running — stop it first`);
  }
  const script = opts.script ??
    await Deno.readTextFile(workflowScriptPath(id)).catch(() => src.script);
  return await startWorkflow(ctx, {
    sessionId: src.sessionId,
    script,
    meta: opts.meta,
    args: opts.args,
    resumeOf: id,
    ...(opts.effectiveModel !== undefined ? { effectiveModel: opts.effectiveModel } : {}),
  });
}

/**
 * Boot recovery: runs left `running`/`paused` by a process that died. Same rule as
 * orphaned turns (`turn/state.ts`) — a restart is SURFACED, not resumed. The worker
 * and every subagent turn it was driving went with the old process; re-running them
 * would spend the user's money on work they did not ask for twice.
 */
export function recoverOrphanedWorkflows(
  db: Db,
  bus?: Bus,
  now: () => number = Date.now,
): string[] {
  const recovered: string[] = [];
  for (const run of db.unfinishedWorkflows()) {
    if (live.has(run.id)) continue;
    for (const a of db.listWorkflowAgents(run.id)) {
      if (a.status === "running" || a.status === "queued") {
        db.updateWorkflowAgent(a.id, { status: "stopped", finishedAt: now() });
      }
    }
    db.updateWorkflow(run.id, {
      status: "orphaned",
      error: "the server restarted before this workflow finished",
      finishedAt: now(),
    });
    recovered.push(run.id);
    const updated = db.getWorkflow(run.id);
    if (bus && updated) {
      bus.publish({ type: "workflow.updated", sessionId: updated.sessionId, data: updated });
    }
  }
  return recovered;
}

// ---------------------------------------------------------------------------
// Views
// ---------------------------------------------------------------------------

/**
 * A run trimmed for program and route consumption. The script text is omitted — it
 * is the largest field by far and a `workflow.list()` that carried N copies of it
 * would flood the model's context for no purpose.
 */
export function workflowSummary(db: Db, run: WorkflowRun): Record<string, unknown> {
  const agents = db.listWorkflowAgents(run.id);
  return {
    id: run.id,
    name: run.name,
    description: run.description,
    status: run.status,
    currentPhase: run.currentPhase,
    phases: run.phases,
    agents: {
      total: agents.length,
      done: agents.filter((a) => a.status === "done" || a.status === "cached").length,
      cached: agents.filter((a) => a.status === "cached").length,
      running: agents.filter((a) => a.status === "running").length,
      queued: agents.filter((a) => a.status === "queued").length,
      failed: agents.filter((a) => a.status === "error").length,
    },
    result: run.result,
    error: run.error,
    resumeOf: run.resumeOf,
    createdAt: run.createdAt,
    finishedAt: run.finishedAt,
    scriptFile: workflowScriptPath(run.id),
  };
}
