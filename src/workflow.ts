/**
 * Workflows — scripted multi-agent orchestration. A workflow is a JavaScript
 * script (written by the supervisor or POSTed over REST) that fans work out to
 * subagents deterministically: the script owns the loop/branching, the host owns
 * the agents. It runs DETACHED from the turn that started it, in a sealed
 * permissions-none Deno Worker (harness/wf_worker.ts) whose only capabilities are
 * agent()/phase()/log() bridged here.
 *
 * Script contract (mirrors Claude Code's Workflow tool):
 *   export const meta = { name, description, phases?: [{title, detail?}] }  // pure literal
 *   ...body: agent(prompt, {label?, phase?, model?}) → report text (throws on failure),
 *   parallel(thunks) barrier, pipeline(items, ...stages), phase(title), log(msg), args.
 *
 * Every agent() call journals into workflow_agents keyed by hash(prompt+opts).
 * A rerun (rerunWorkflow / workflow.rerun) replays journal hits from the source
 * run instantly — edit the script (or its ~/.bough/workflows/<id>.js mirror) and
 * only the changed calls re-run. Stop kills the worker AND interrupts in-flight
 * subagent turns via the run's abort signal; pause gates NEW agent() calls while
 * running ones finish.
 */
import { z } from "zod";
import { HttpError } from "./errors.ts";
import type { Db, WorkflowAgent, WorkflowRun } from "./db/db.ts";
import type { Bus } from "./bus.ts";
import { type HostFns, runProgram } from "./harness/vm.ts";
import { join } from "node:path";
import { boughPath } from "./paths.ts";
import { clip, codeGist } from "./text.ts";

// ---- meta extraction (pure) ------------------------------------------------

const MetaSchema = z.object({
  name: z.string().min(1).max(80),
  description: z.string().min(1).max(500),
  phases: z.array(z.object({
    title: z.string().min(1),
    detail: z.string().optional(),
  })).optional(),
});
export type WorkflowMeta = z.infer<typeof MetaSchema>;

/**
 * Find the `export const meta = {…}` literal: balanced-brace scan that skips
 * string/template contents and comments, so a description containing "{" can't
 * derail it. Returns the literal text, or null when the script has no meta.
 */
export function metaLiteral(script: string): string | null {
  const m = /export\s+const\s+meta\s*=\s*\{/.exec(script);
  if (!m) return null;
  const start = m.index + m[0].length - 1;
  let depth = 0;
  let quote: string | null = null;
  for (let i = start; i < script.length; i++) {
    const c = script[i];
    if (quote) {
      if (c === "\\") i++;
      else if (c === quote) quote = null;
      continue;
    }
    if (c === '"' || c === "'" || c === "`") quote = c;
    else if (c === "/" && script[i + 1] === "/") i = script.indexOf("\n", i);
    else if (c === "/" && script[i + 1] === "*") i = script.indexOf("*/", i) + 1;
    else if (c === "{") depth++;
    else if (c === "}" && --depth === 0) return script.slice(start, i + 1);
    if (i < 0) break; // unterminated comment
  }
  return null;
}

/**
 * Evaluate the meta literal in a sealed throwaway VM (no host functions) and
 * validate it. Sealed eval — not JSON.parse — because the literal is JS
 * (unquoted keys, trailing commas), but it runs with zero capabilities.
 */
export async function evalMeta(script: string): Promise<WorkflowMeta> {
  const literal = metaLiteral(script);
  if (!literal) {
    throw new HttpError(
      400,
      "workflow script must start with `export const meta = {name, description, phases?}`",
    );
  }
  // No host functions: the literal evaluates with zero capabilities.
  const probe = await runProgram(
    `const meta = ${literal};\nconsole.log(JSON.stringify(meta))`,
    {} as unknown as HostFns,
    5_000,
  );
  if (!probe.ok || !probe.logs[0]) {
    throw new HttpError(400, `workflow meta does not evaluate: ${probe.error ?? "no output"}`);
  }
  const parsed = MetaSchema.safeParse(JSON.parse(probe.logs[0]));
  if (!parsed.success) {
    throw new HttpError(400, "invalid workflow meta: " + parsed.error.message);
  }
  return parsed.data;
}

// ---- body syntax check (pure) ----------------------------------------------

/**
 * Locate a string literal closed by a raw newline. This is THE failure mode for
 * a generated workflow: the supervisor assembles the script inside a template
 * literal, and every `\n` meant for a string in the GENERATED script is consumed
 * by the outer literal, leaving a real newline inside `"..."`. Field case
 * (2026-07-24): a 57-line Rust-rewrite plan died at parse time, 0/0 agents.
 *
 * Written as a scanner rather than a regex because it has to skip comments and
 * template literals, where a raw newline is perfectly legal.
 */
export function unterminatedString(
  src: string,
): { line: number; col: number; text: string; quote: string } | null {
  let line = 1, col = 1, depth = 0;
  for (let i = 0; i < src.length; i++) {
    const c = src[i], next = src[i + 1];
    const bump = () => {
      if (c === "\n") {
        line++;
        col = 1;
      } else col++;
    };
    if (c === "/" && next === "/") {
      while (i < src.length && src[i] !== "\n") i++;
      line++;
      col = 1;
      continue;
    }
    if (c === "/" && next === "*") {
      const end = src.indexOf("*/", i + 2);
      const skipped = src.slice(i, end < 0 ? src.length : end + 2);
      line += (skipped.match(/\n/g) ?? []).length;
      col = 1;
      i = end < 0 ? src.length : end + 1;
      continue;
    }
    // Template literals may span lines legally; walk them (with ${} nesting) so
    // their newlines never look like an unterminated quote.
    if (c === "`") {
      i++;
      for (; i < src.length; i++) {
        if (src[i] === "\\") {
          i++;
          continue;
        }
        if (src[i] === "\n") {
          line++;
          col = 1;
          continue;
        }
        if (src[i] === "$" && src[i + 1] === "{") depth++;
        else if (src[i] === "}" && depth > 0) depth--;
        else if (src[i] === "`" && depth === 0) break;
      }
      col++;
      continue;
    }
    if (c === '"' || c === "'") {
      const startLine = line, startCol = col;
      for (i++; i < src.length; i++) {
        if (src[i] === "\\") {
          i++;
          continue;
        }
        if (src[i] === "\n" || i === src.length - 1 && src[i] !== c) {
          return {
            line: startLine,
            col: startCol,
            text: src.split("\n")[startLine - 1] ?? "",
            quote: c,
          };
        }
        if (src[i] === c) break;
      }
      col++;
      continue;
    }
    bump();
  }
  return null;
}

const AsyncFunctionCtor = Object.getPrototypeOf(async function () {}).constructor as // deno-lint-ignore no-explicit-any
any;

/**
 * Compile-check the script body BEFORE the run is persisted and launched.
 *
 * Without this a syntax error only surfaced from inside the worker, minutes into
 * a detached run, as a bare "SyntaxError: Invalid or unexpected token" over ten
 * frames of Deno internals — no line, no column, no source. The author (usually
 * the supervisor, mid-turn) could do nothing with that but burn another run.
 * Compiling here is side-effect free: the Function constructor parses, it does
 * not execute, and the body never touches the host's scope.
 */
export function assertBodyParses(body: string): void {
  try {
    new AsyncFunctionCtor("agent", "parallel", "pipeline", "phase", "log", "args", "console", body);
  } catch (err) {
    if ((err as Error)?.name !== "SyntaxError") throw err;
    const why = (err as Error).message;
    // V8 gives the Function constructor no position, so say where ourselves when
    // we can recognize the shape.
    const hit = unterminatedString(body);
    if (!hit) throw new HttpError(400, `workflow script does not parse: ${why}`);
    throw new HttpError(
      400,
      `workflow script does not parse: ${why} — line ${hit.line}: a ${
        hit.quote === '"' ? "double" : "single"
      }-quoted string is closed by a real newline.\n  ${hit.line} | ${
        clip(hit.text.trim(), 90)
      }\n` +
        `If you built this script inside a template literal, write \\\\n (escaped) for newlines ` +
        `that belong to the GENERATED script's strings — a bare \\n is consumed by the outer literal.`,
    );
  }
}

// ---- engine ----------------------------------------------------------------

/** What one agent() call asks for, parsed from the worker's bridged JSON. */
export interface AgentCall {
  prompt: string;
  label: string;
  phase?: string;
  model?: string;
}

/**
 * Runs one agent call to completion — the production wiring adapts runSubagent
 * (turn.ts wires it with the spawning session's context); tests inject a fake.
 * Returns the report text; MUST throw on failure so the script can react.
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
  /** Deliver the finished-run note to the owning session (turn.ts postSystemNote). */
  notify?: (sessionId: string, text: string) => void;
}

export interface StartOpts {
  sessionId: string;
  script: string;
  args?: unknown;
  /** Journal-replay source: agent() calls matching that run's journal return its
   * cached results instantly. */
  resumeOf?: string;
}

function concurrency(): number {
  const n = Number(Deno.env.get("BOUGH_WORKFLOW_CONCURRENCY"));
  return Number.isFinite(n) && n > 0 ? n : 4;
}
function wallTimeoutMs(): number {
  const n = Number(Deno.env.get("BOUGH_WORKFLOW_TIMEOUT_MS"));
  return Number.isFinite(n) && n > 0 ? n : 60 * 60_000;
}
/** Lifetime agent cap per run — a runaway-loop backstop, not a working limit. */
const MAX_AGENTS_PER_RUN = 200;

/** FNV-1a over the canonical call shape — the journal replay key. Two passes with
 * different offsets so an accidental 32-bit collision needs to happen twice. */
export function callKey(call: AgentCall): string {
  const s = JSON.stringify([call.prompt, call.label, call.phase ?? "", call.model ?? ""]);
  let a = 0x811c9dc5, b = 0x01000193;
  for (let i = 0; i < s.length; i++) {
    a = (a ^ s.charCodeAt(i)) >>> 0;
    a = Math.imul(a, 0x01000193) >>> 0;
    b = (b ^ ((s.charCodeAt(i) + 7) & 0xffff)) >>> 0;
    b = Math.imul(b, 0x01000193) >>> 0;
  }
  return a.toString(16) + b.toString(16);
}

/** In-memory state of a live run. Like subagent.ts's `detached` map, a server
 * restart orphans these — recoverOrphanedWorkflows marks the stale rows. */
interface LiveRun {
  ctrl: AbortController;
  worker: Worker;
  paused: boolean;
  /** Resolvers parked on the pause gate — resumed in FIFO order. */
  gate: Array<() => void>;
  /** Per-agent handles for the selection-scoped stop/restart keys: aborting one
   * agent's controller leaves the rest of the run alone, and `restart` tells the
   * catch arm to re-issue the same call instead of failing it. */
  agents: Map<string, { ctrl: AbortController; restart: boolean }>;
  timer?: ReturnType<typeof setTimeout>;
}

const live = new Map<string, LiveRun>();

/** Is this run still executing in this process? */
export function isWorkflowLive(id: string): boolean {
  return live.has(id);
}

function publishRun(ctx: Pick<WorkflowCtx, "db" | "bus">, id: string): WorkflowRun | undefined {
  const run = ctx.db.getWorkflow(id);
  if (run) {
    ctx.bus.publish({ type: "workflow.updated", sessionId: run.sessionId, data: run });
  }
  return run;
}

function publishAgent(
  ctx: Pick<WorkflowCtx, "db" | "bus">,
  run: WorkflowRun,
  agentId: string,
): void {
  const rows = ctx.db.listWorkflowAgents(run.id);
  const row = rows.find((a) => a.id === agentId);
  if (row) {
    ctx.bus.publish({ type: "workflow.agent", sessionId: run.sessionId, data: row });
  }
}

/** The mirror dir (env-overridable so tests stay out of the real ~/.bough). */
function workflowsDir(): string {
  return Deno.env.get("BOUGH_WORKFLOW_DIR") ?? boughPath("workflows");
}

/** The script file mirror — edit this and rerun to iterate on a workflow. */
export function scriptPath(id: string): string {
  return join(workflowsDir(), `${id}.js`);
}

/**
 * Start a workflow: validate meta, persist the run + its script mirror, build the
 * journal-replay map when resuming, and launch the sealed worker. Returns the run
 * row immediately — progress flows over workflow.* bus events, and completion
 * posts a system note to the owning session.
 */
export async function startWorkflow(ctx: WorkflowCtx, opts: StartOpts): Promise<WorkflowRun> {
  const { db, bus } = ctx;
  if (!db.getSession(opts.sessionId)) throw new HttpError(404, "session not found");
  if (typeof opts.script !== "string" || !opts.script.trim()) {
    throw new HttpError(400, "workflow: script must be a non-empty string");
  }
  const meta = await evalMeta(opts.script);
  // Same demotion the worker gets (line numbers are preserved, so positions in
  // the error match the script the author wrote).
  assertBodyParses(opts.script.replace(/export\s+const\s+meta\s*=/, "const meta ="));
  const replay = new Map<string, string[]>();
  let args: unknown = opts.args ?? null;
  if (opts.resumeOf) {
    const src = db.getWorkflow(opts.resumeOf);
    if (!src) throw new HttpError(404, `workflow ${opts.resumeOf} not found`);
    if (opts.args === undefined) args = src.args; // a rerun keeps its input by default
    // Only successful calls replay; failures re-run live. FIFO per key so N
    // identical calls replay their N results in order.
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
    name: meta.name,
    description: meta.description,
    script: opts.script,
    phases: meta.phases ?? [],
    status: "running",
    currentPhase: null,
    result: null,
    error: null,
    args,
    resumeOf: opts.resumeOf ?? null,
    createdAt: Date.now(),
    finishedAt: null,
  });
  // Mirror the script to a real file so "modify and rerun" is a file edit away.
  try {
    await Deno.mkdir(workflowsDir(), { recursive: true });
    await Deno.writeTextFile(scriptPath(id), opts.script);
  } catch {
    // mirror is a convenience; the canonical script is the DB row
  }
  bus.publish({ type: "workflow.updated", sessionId: run.sessionId, data: run });

  const ctrl = new AbortController();
  const worker = new Worker(new URL("./harness/wf_worker.ts", import.meta.url).href, {
    type: "module",
    deno: { permissions: "none" },
  });
  const state: LiveRun = { ctrl, worker, paused: false, gate: [], agents: new Map() };
  live.set(id, state);

  // The body keeps the meta statement (demoted to a plain const) — removal would
  // need exact statement bounds; a harmless local binding needs none.
  const body = opts.script.replace(/export\s+const\s+meta\s*=/, "const meta =");

  let idx = 0;
  let inFlight = 0;
  const queue: Array<() => void> = [];
  const acquire = () =>
    new Promise<void>((resolve) => {
      if (inFlight < concurrency()) {
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
    ctrl.abort();
    // In-flight journal rows die with the run.
    for (const a of db.listWorkflowAgents(id)) {
      if (a.status === "running") {
        db.updateWorkflowAgent(a.id, { status: "stopped", finishedAt: Date.now() });
      }
    }
    db.updateWorkflow(id, {
      status,
      result: result ?? null,
      error: error ?? null,
      finishedAt: Date.now(),
    });
    const updated = publishRun(ctx, id);
    if (ctx.notify && updated) {
      const agents = db.listWorkflowAgents(id);
      const okCount = agents.filter((a) => a.status === "done" || a.status === "cached").length;
      const head =
        `[workflow ${status}] "${updated.name}" (${id}) — ${okCount}/${agents.length} agents succeeded.`;
      const tail = status === "done"
        ? `Result:\n${clip(JSON.stringify(result ?? null, null, 2), 4000)}`
        : status === "error"
        ? `Error: ${clip(error ?? "unknown", 2000)}`
        : "Stopped by the user.";
      ctx.notify(updated.sessionId, `${head}\n${tail}`);
    }
  };

  state.timer = setTimeout(
    () => finish("error", undefined, `workflow timed out after ${wallTimeoutMs()}ms`),
    wallTimeoutMs(),
  );

  worker.onmessage = async (e: MessageEvent) => {
    const msg = e.data as
      | { type: "host"; id: number; fn: "agent" | "phase" | "log"; args: unknown[] }
      | { type: "done"; result: string }
      | { type: "error"; message: string };
    if (msg.type === "done") return finish("done", JSON.parse(msg.result));
    if (msg.type === "error") return finish("error", undefined, msg.message);
    const reply = (ok: boolean, value: string) => {
      try {
        worker.postMessage({ type: "host_result", id: msg.id, ok, value });
      } catch {
        // worker already terminated
      }
    };
    try {
      if (msg.fn === "phase") {
        const title = String(msg.args[0] ?? "");
        db.updateWorkflow(id, { currentPhase: title });
        publishRun(ctx, id);
        return reply(true, "");
      }
      if (msg.fn === "log") {
        bus.publish({
          type: "workflow.log",
          sessionId: run.sessionId,
          data: { runId: id, line: String(msg.args[0] ?? "") },
        });
        return reply(true, "");
      }
      // agent(...) — journal, replay or run, journal again.
      const raw = JSON.parse(String(msg.args[0])) as Partial<AgentCall>;
      if (typeof raw.prompt !== "string" || !raw.prompt.trim()) {
        throw new Error("agent(prompt, opts): prompt must be a non-empty string");
      }
      const call: AgentCall = {
        prompt: raw.prompt,
        label: typeof raw.label === "string" && raw.label.trim()
          ? raw.label.trim()
          : clip(raw.prompt.trim().split("\n")[0], 40),
        ...(typeof raw.phase === "string" ? { phase: raw.phase } : {}),
        ...(typeof raw.model === "string" ? { model: raw.model } : {}),
      };
      const at = idx++;
      if (at >= MAX_AGENTS_PER_RUN) {
        throw new Error(`workflow agent cap reached (${MAX_AGENTS_PER_RUN})`);
      }
      // Pause parks the call BEFORE it journals: a parked call has no row, so
      // the UI never shows a "running" agent that hasn't actually started
      // (live-test finding — a sequential script's next call surfaced as
      // running, session-less, while the run sat paused).
      await awaitGate();
      const key = callKey(call);
      const cached = replay.get(key)?.shift();
      const row = db.createWorkflowAgent({
        id: crypto.randomUUID(),
        runId: id,
        idx: at,
        key,
        label: call.label,
        phase: call.phase ?? db.getWorkflow(id)?.currentPhase ?? null,
        prompt: call.prompt,
        model: call.model ?? null,
        status: cached !== undefined ? "cached" : "running",
        result: cached ?? null,
        sessionId: null,
        startedAt: Date.now(),
        finishedAt: cached !== undefined ? Date.now() : null,
      });
      publishAgent(ctx, run, row.id);
      if (cached !== undefined) return reply(true, JSON.stringify(cached));
      await acquire();
      try {
        // Loop so `r` on a running agent can re-issue the SAME call on a fresh
        // subagent session: restartWorkflowAgent aborts this attempt's own
        // controller with the restart flag set, and we come back around instead
        // of failing the call (the script is still parked on this one promise).
        for (;;) {
          if (ctrl.signal.aborted) throw new Error("workflow stopped");
          const own = new AbortController();
          const relay = () => own.abort();
          ctrl.signal.addEventListener("abort", relay, { once: true });
          const handle = { ctrl: own, restart: false };
          state.agents.set(row.id, handle);
          try {
            const report = await ctx.runner(call, own.signal, (sid) => {
              db.updateWorkflowAgent(row.id, { sessionId: sid });
              publishAgent(ctx, run, row.id);
            });
            db.updateWorkflowAgent(row.id, {
              status: "done",
              result: report,
              finishedAt: Date.now(),
            });
            publishAgent(ctx, run, row.id);
            reply(true, JSON.stringify(report));
            break;
          } catch (err) {
            if (handle.restart && !ctrl.signal.aborted) {
              db.updateWorkflowAgent(row.id, {
                status: "running",
                result: null,
                sessionId: null,
                finishedAt: null,
              });
              publishAgent(ctx, run, row.id);
              continue;
            }
            db.updateWorkflowAgent(row.id, {
              status: own.signal.aborted ? "stopped" : "error",
              result: (err as Error).message ?? String(err),
              finishedAt: Date.now(),
            });
            publishAgent(ctx, run, row.id);
            reply(false, (err as Error).message ?? String(err));
            break;
          } finally {
            ctrl.signal.removeEventListener("abort", relay);
            state.agents.delete(row.id);
          }
        }
      } finally {
        release();
      }
    } catch (err) {
      reply(false, (err as Error).message ?? String(err));
    }
  };
  worker.onerror = (e) => {
    e.preventDefault();
    finish("error", undefined, `worker error: ${e.message}`);
  };
  worker.postMessage({ type: "run", code: body, args });
  return run;
}

/** Stop a run: kill the worker, interrupt in-flight subagent turns, mark rows. */
export function stopWorkflow(ctx: Pick<WorkflowCtx, "db" | "bus">, id: string): WorkflowRun {
  const run = ctx.db.getWorkflow(id);
  if (!run) throw new HttpError(404, "workflow not found");
  const state = live.get(id);
  if (!state) {
    if (run.status === "running" || run.status === "paused") {
      ctx.db.updateWorkflow(id, { status: "orphaned", finishedAt: Date.now() });
      return publishRun(ctx, id)!;
    }
    return run;
  }
  // Release anything parked on the pause gate so nothing leaks, then finish.
  state.paused = false;
  state.gate.splice(0).forEach((open) => open());
  // finish() aborts ctrl → the runner's signal interrupts live subagent turns.
  const finishViaState = () => {
    live.delete(id);
    clearTimeout(state.timer);
    state.worker.terminate();
    state.ctrl.abort();
    for (const a of ctx.db.listWorkflowAgents(id)) {
      if (a.status === "running") {
        ctx.db.updateWorkflowAgent(a.id, { status: "stopped", finishedAt: Date.now() });
      }
    }
    ctx.db.updateWorkflow(id, { status: "stopped", finishedAt: Date.now() });
  };
  finishViaState();
  return publishRun(ctx, id)!;
}

/**
 * Selection-scoped agent control (the run view's x/r on a single agent). Stop
 * fails just that agent() call — the script sees the rejection and its
 * parallel()/pipeline() slot goes null, the rest of the run continues. Restart
 * re-issues the same call on a fresh subagent session, leaving the script parked
 * on the promise it is already awaiting.
 */
export function controlWorkflowAgent(
  ctx: Pick<WorkflowCtx, "db" | "bus">,
  runId: string,
  agentId: string,
  action: "stop" | "restart",
): WorkflowAgent {
  const row = ctx.db.listWorkflowAgents(runId).find((a) => a.id === agentId);
  if (!row) throw new HttpError(404, "workflow agent not found");
  if (row.status !== "running") throw new HttpError(409, `agent is ${row.status}, not running`);
  const handle = live.get(runId)?.agents.get(agentId);
  if (!handle) throw new HttpError(409, "agent is not live in this process");
  handle.restart = action === "restart";
  handle.ctrl.abort();
  return row;
}

/** Pause: new agent() calls park on the gate; running agents finish normally. */
export function pauseWorkflow(ctx: Pick<WorkflowCtx, "db" | "bus">, id: string): WorkflowRun {
  const state = live.get(id);
  if (!state) throw new HttpError(409, "workflow is not running");
  state.paused = true;
  ctx.db.updateWorkflow(id, { status: "paused" });
  return publishRun(ctx, id)!;
}

/** Resume a paused run: open the gate and release parked agent() calls. */
export function resumeWorkflow(ctx: Pick<WorkflowCtx, "db" | "bus">, id: string): WorkflowRun {
  const state = live.get(id);
  if (!state) throw new HttpError(409, "workflow is not running");
  state.paused = false;
  state.gate.splice(0).forEach((open) => open());
  ctx.db.updateWorkflow(id, { status: "running" });
  return publishRun(ctx, id)!;
}

export interface RerunOpts {
  /** Script override; absent = the ~/.bough/workflows/<id>.js mirror (which the
   * user may have edited), falling back to the stored script. */
  script?: string;
  args?: unknown;
}

/**
 * Rerun a finished run with journal replay: unchanged agent() calls return the
 * old run's results instantly, edited/new calls run live. The script defaults to
 * the run's file mirror so "edit the file, press r" is the whole iteration loop.
 */
export async function rerunWorkflow(
  ctx: WorkflowCtx,
  id: string,
  opts: RerunOpts = {},
): Promise<WorkflowRun> {
  const src = ctx.db.getWorkflow(id);
  if (!src) throw new HttpError(404, "workflow not found");
  if (live.has(id)) throw new HttpError(409, "workflow is still running — stop it first");
  let script = opts.script;
  if (script === undefined) {
    script = await Deno.readTextFile(scriptPath(id)).catch(() => src.script);
  }
  return await startWorkflow(ctx, {
    sessionId: src.sessionId,
    script,
    args: opts.args,
    resumeOf: id,
  });
}

/** Server-boot recovery: runs left "running"/"paused" by a dead process. */
export function recoverOrphanedWorkflows(db: Db): number {
  let n = 0;
  for (const run of db.unfinishedWorkflows()) {
    if (live.has(run.id)) continue;
    db.updateWorkflow(run.id, {
      status: "orphaned",
      error: "the server restarted before this workflow finished",
      finishedAt: Date.now(),
    });
    n++;
  }
  return n;
}

// ---- request bodies (validated at the app.ts edge) -------------------------

export const WorkflowCreateBody = z.object({
  sessionId: z.string().min(1),
  script: z.string().min(1),
  args: z.unknown().optional(),
});
export type WorkflowCreateBody = z.infer<typeof WorkflowCreateBody>;

export const WorkflowRerunBody = z.object({
  script: z.string().min(1).optional(),
  args: z.unknown().optional(),
});
export type WorkflowRerunBody = z.infer<typeof WorkflowRerunBody>;

// ---- the workflow.* host-fn verbs (run_steps bridge) -----------------------

const StartArgs = z.object({
  script: z.string().min(1),
  args: z.unknown().optional(),
});
const RerunArgs = z.object({
  id: z.string().min(1),
  script: z.string().min(1).optional(),
  args: z.unknown().optional(),
});
const IdArgs = z.object({ id: z.string().min(1) });

/**
 * One agent row plus what the run view needs to show WITHOUT opening its
 * session: cumulative tokens, how many tool calls it made, and the last few
 * call gists. The activity trail is the difference between "running" and
 * "running, and here is what it is doing" — a stuck agent used to be
 * indistinguishable from a slow one.
 */
export interface WorkflowAgentView extends WorkflowAgent {
  tokens: number;
  toolCalls: number;
  activity: string[];
}

const ACTIVITY_LINES = 4;

export function workflowAgentViews(db: Db, runId: string): WorkflowAgentView[] {
  return db.listWorkflowAgents(runId).map((a) => {
    if (!a.sessionId) return { ...a, tokens: 0, toolCalls: 0, activity: [] };
    const usage = db.sessionUsage(a.sessionId);
    const calls = db.messagesFor(a.sessionId)
      .flatMap((m) => m.parts)
      .filter((p) => p.type === "tool_call");
    return {
      ...a,
      tokens: usage.inputTokens + usage.outputTokens,
      toolCalls: calls.length,
      activity: calls.slice(-ACTIVITY_LINES).map((c) => `${c.name}(${codeGist(c.input, 48)})`),
    };
  });
}

/** Trim a run for program/route consumption (script omitted from lists). */
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
      running: agents.filter((a) => a.status === "running").length,
      failed: agents.filter((a) => a.status === "error").length,
    },
    result: run.result,
    error: run.error,
    resumeOf: run.resumeOf,
    createdAt: run.createdAt,
    finishedAt: run.finishedAt,
    scriptFile: scriptPath(run.id),
  };
}

/**
 * One verb-dispatched entry point for the program-side `workflow.*` methods,
 * mirroring schedule.call. Verbs: start {script, args?} · rerun {id, script?,
 * args?} · stop {id} · pause {id} · resume {id} · status {id} · list {}.
 */
export async function workflowVerb(
  ctx: WorkflowCtx,
  sessionId: string,
  verb: string,
  args: unknown,
): Promise<unknown> {
  switch (verb) {
    case "start": {
      const a = StartArgs.safeParse(args);
      if (!a.success) {
        throw new HttpError(400, "workflow.start({script, args?}): " + a.error.message);
      }
      const run = await startWorkflow(ctx, { sessionId, script: a.data.script, args: a.data.args });
      return workflowSummary(ctx.db, run);
    }
    case "rerun": {
      const a = RerunArgs.safeParse(args);
      if (!a.success) {
        throw new HttpError(400, "workflow.rerun({id, script?, args?}): " + a.error.message);
      }
      const run = await rerunWorkflow(ctx, a.data.id, { script: a.data.script, args: a.data.args });
      return workflowSummary(ctx.db, run);
    }
    case "stop":
    case "pause":
    case "resume":
    case "status": {
      const a = IdArgs.safeParse(args);
      if (!a.success) throw new HttpError(400, `workflow.${verb}({id}): ` + a.error.message);
      const run = verb === "stop"
        ? stopWorkflow(ctx, a.data.id)
        : verb === "pause"
        ? pauseWorkflow(ctx, a.data.id)
        : verb === "resume"
        ? resumeWorkflow(ctx, a.data.id)
        : ctx.db.getWorkflow(a.data.id);
      if (!run) throw new HttpError(404, "workflow not found");
      return verb === "status"
        ? { ...workflowSummary(ctx.db, run), agentRows: ctx.db.listWorkflowAgents(run.id) }
        : workflowSummary(ctx.db, run);
    }
    case "list":
      return ctx.db.listWorkflows().map((r) => workflowSummary(ctx.db, r));
    default:
      throw new HttpError(
        400,
        `unknown workflow verb: ${verb} (start|rerun|stop|pause|resume|status|list)`,
      );
  }
}
