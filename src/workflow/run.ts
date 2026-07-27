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
 * runs, and a relaunch replays the longest UNCHANGED PREFIX of those calls instead of
 * paying for it.** `key` is `hash(prompt + label + phase + model + schema)` —
 * everything that decides what the subagent will be asked. So editing one prompt in a
 * 300-agent script and relaunching costs the edited call and everything after it, and
 * that is the entire iteration loop for a workflow. Three consequences that shape the
 * code below:
 *
 *   - **Replay stops at the first changed call and never resumes** (T5.7, spec §8's
 *     "Replay is prefix-bounded"). A key covers a call's PROMPT, not the filesystem
 *     that prompt runs against, and workflow agents all share one checkout: two calls
 *     can say "run the test suite" byte-identically and mean different questions
 *     because an upstream agent rewrote the code in between. Replaying the later one
 *     answers about a tree that no longer exists. A miss costs money; a stale hit is a
 *     wrong answer presented as a fresh one, so the engine buys the cheap failure.
 *     `replayPlan` below is therefore indexed by call POSITION, not a key→result map:
 *     position is part of the identity of a call.
 *
 *   - **Position comes from the script's STRUCTURE, never from arrival order** — the
 *     defect that made the point. A call's position used to be a monotonic counter
 *     incremented as calls reached this bridge, which is reproducible only for a
 *     sequential script. `pipeline()` has no barrier by design (spec §8), so its
 *     stage-2 calls are issued in stage-1 COMPLETION order: `pipeline(['A','B'], s1,
 *     s2)` where `s1 A` takes 60ms and `s1 B` takes 1ms journaled `[s1 A, s1 B, s2 B,
 *     s2 A]`, a relaunch of the byte-identical script resolved its replayed prefix in
 *     dispatch order, the last two positions transposed, and an UNCHANGED script
 *     re-billed every call past stage 1. That is spec §8's own canonical example.
 *     The combinators know the shape, so they supply it: `harness/wf_worker.ts` sends a
 *     structural coordinate (`"0.1.1.0"` — dot-joined slot indexes) with every
 *     `agent()` call, and a bare call falls back to the enclosing frame's counter,
 *     which for a sequential script is the old numbering exactly. The journal key is
 *     `<pos>|<contentHash>`, so a call that MOVED and a call that was EDITED are
 *     different facts and the divergence report says which one happened — the previous
 *     message claimed "its key changed" for a transposition, which is the one surface
 *     that exists to make a key defect visible saying the opposite of what occurred.
 *
 *   - The journal row is written BEFORE the semaphore is acquired, so the run view
 *     can show a queued agent, and `startedAt` is reset when the call actually
 *     starts — otherwise a saturated run shows N agents "working" while only
 *     `concurrency` of them are.
 *
 *   - **Pause gates ADMISSION, not issuance, and a stopped run leaves nothing
 *     non-terminal.** These two are one mechanism and they are checked in `admit()`.
 *     A `parallel()` fan-out issues every call at dispatch, so a single gate check
 *     before the semaphore is a no-op for it: pause released nothing, gated nothing,
 *     and the run billed to completion — for precisely the shape workflows exist for.
 *     The gate is therefore consulted again after a slot is taken, with the call's row
 *     still `queued`. The same edge owns the other half: stop opens the gate to unpark
 *     what is on it, and a call woken that way must not journal a row after the
 *     wind-down already swept them, nor step over the handler that settles the row it
 *     does hold. Spec §8 recommends pausing before stopping; that sequence is what
 *     found both.
 *   - Only successful calls replay. A failed call re-runs live, because the failure
 *     may well have been the thing the author just fixed — and, under the prefix rule,
 *     so does everything after it: the re-run agent works in the same checkout the
 *     later answers were computed against.
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
 *   - **Relaunching from a journal.** Choosing the source run, resolving the edited
 *     script, and reporting what the prefix cost are `workflow/relaunch.ts` (T5.7).
 *     This module holds only the mechanism: `replayPlan` reads a source journal and
 *     `StartOpts.resumeOf` hands it to a new run.
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
import { workflowScriptPath } from "../paths.ts";
import type { WorkflowAgent, WorkflowPhase, WorkflowRun } from "../schema/parts.ts";
import type { Bus, Db, WorkflowHostFns } from "../types.ts";
import { mirrorScript, resolveRerunScript } from "./journal.ts";

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
  if (Number.isFinite(n) && n > 0) return n;
  return defaultWorkflowConcurrency();
}

/**
 * Up to 16 at once, fewer on a small machine (spec §8: "up to 16 agents at once, fewer
 * on machines with few cores").
 *
 * Two cores are left for everything that is NOT a workflow agent: the server's own turn
 * runner, the program worker a supervisor is running, the subagent turns those spawn.
 * A run that saturates every core makes the session that started it unusable, which is
 * the failure this backs off from — 16 is the ceiling because the meter that matters
 * beyond that is the provider's, not the machine's.
 *
 * `navigator.hardwareConcurrency` is the count Deno exposes; a runtime that reports
 * nothing usable falls back to the old default of 4 rather than to 1, because a
 * conservative guess here costs wall-clock on every fan-out.
 */
export function defaultWorkflowConcurrency(): number {
  const cores = Number(navigator?.hardwareConcurrency);
  if (!Number.isFinite(cores) || cores <= 0) return 4;
  return Math.max(1, Math.min(16, Math.floor(cores) - 2));
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
 *
 * 1,000, per spec §8. It was 200, which is inside the range a real audit legitimately
 * asks for ("review every handler" over a large tree), so it was a working limit
 * wearing a backstop's name: it fired on jobs that were doing exactly what they meant
 * to. The advisory surface — the size guideline and the large-run flag
 * (`workflow/report.ts`) — is what tells a user a run is big. This one only stops a
 * loop that has lost its exit condition.
 */
export const MAX_AGENTS_PER_RUN = 1000;

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

// ---------------------------------------------------------------------------
// Structural positions (pure)
// ---------------------------------------------------------------------------

/**
 * A call's structural coordinate: dot-joined slot indexes from the script's shape,
 * e.g. `"0.1.1.0"` for pipeline 0, item 1, stage 1, first agent. `harness/wf_worker.ts`
 * computes it; this module only orders and compares.
 */
export type CallPos = string;

/**
 * Component-wise NUMERIC comparison. `"0.10"` sorts after `"0.9"`, which string
 * comparison gets backwards — and a fan-out of ten items is not an exotic case.
 * A prefix sorts before what extends it, so a bare call at `"2"` precedes the
 * combinator subtree at `"2.0.0"` that would only exist if they were the same slot.
 */
export function comparePos(a: CallPos, b: CallPos): number {
  const x = a.split(".");
  const y = b.split(".");
  for (let i = 0; i < Math.max(x.length, y.length); i++) {
    const dx = i < x.length ? Number(x[i]) : -1;
    const dy = i < y.length ? Number(y[i]) : -1;
    if (dx !== dy) return dx < dy ? -1 : 1;
  }
  return 0;
}

/**
 * The stored journal key: the call's position and the hash of what it asks for, joined
 * by a character neither half can contain (positions are digits and dots, the hash is
 * hex).
 *
 * Both halves, in one column, because both are part of a call's identity and the DB
 * schema is frozen with exactly one key field (plan §4). Keeping them RECOVERABLE
 * rather than hashing them together is the whole point: it is what lets the divergence
 * report distinguish a call that was edited (same position, different hash) from one
 * that moved (same hash, different position). Hashing the pair would have made both
 * read as "its key changed".
 */
export function journalKey(pos: CallPos, contentKey: string): string {
  return `${pos}|${contentKey}`;
}

/** The inverse. A key with no separator is pre-coordinate; `pos` reads as `null`. */
export function splitJournalKey(key: string): { pos: CallPos | null; content: string } {
  const at = key.indexOf("|");
  if (at < 0) return { pos: null, content: key };
  return { pos: key.slice(0, at), content: key.slice(at + 1) };
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
// Prefix-bounded replay (pure)
// ---------------------------------------------------------------------------

/** One call of a source run's journal, as the replay decision sees it. */
export interface ReplayStep {
  /** The call's STRUCTURAL coordinate — what a relaunch matches on. */
  pos: CallPos;
  /** Hash of what the call asks for, position excluded. `callKey`'s output. */
  content: string;
  /** The stored key exactly as journaled — `<pos>|<content>`. */
  key: string;
  /** The call's dispatch index in the source run. Display and ordering of rows only. */
  idx: number;
  /** The stored report, or `null` when that call has no answer to hand back. */
  result: string | null;
  /** Carried for reporting — which call the prefix broke on is the useful line. */
  prompt: string;
}

/**
 * A source run's calls, addressable by structural coordinate and ordered by it.
 *
 * An object rather than a bare array because the coordinate is a PATH, not a dense
 * index — `"0.1.1.0"` cannot be an array subscript, and the ordering the prefix is
 * defined over is the component-wise numeric one (`comparePos`), not integer order.
 * `byContent` exists for one job: telling a call that MOVED apart from a call that was
 * EDITED when replay stops.
 */
export interface ReplayPlan {
  /** Every journaled call, sorted by `comparePos`. */
  steps: ReplayStep[];
  byPos: Map<CallPos, ReplayStep>;
  /** Content hash → the coordinates the source ran that exact call at. */
  byContent: Map<string, CallPos[]>;
}

/** The plan a first run replays from: nothing. */
export function emptyReplayPlan(): ReplayPlan {
  return { steps: [], byPos: new Map(), byContent: new Map() };
}

/**
 * Read a source run's journal into a replay plan.
 *
 * NOTE: the "is this an answer" test is spelled out here rather than imported from
 * `workflow/journal.ts`, whose `isReplayable` says the same thing on the read side.
 * The import would close a cycle (`journal.ts` re-exports `callKey` from this file),
 * and this module is the one that WRITES those statuses. Both must stay in step: only
 * `done` and `cached` rows with a non-null result are answers; `error`, `stopped`,
 * `queued` and `running` are not.
 *
 * A row journaled before coordinates existed has no `pos` half in its key. It is given
 * its dispatch index as a coordinate, which is what a sequential script produces
 * anyway — so an old sequential journal still replays, and an old concurrent one
 * misses and re-runs, which is the safe direction.
 */
export function replayPlan(db: Db, sourceRunId: string): ReplayPlan {
  const plan = emptyReplayPlan();
  for (const a of db.listWorkflowAgents(sourceRunId)) {
    const answered = (a.status === "done" || a.status === "cached") && a.result !== null;
    const { pos, content } = splitJournalKey(a.key);
    const step: ReplayStep = {
      pos: pos ?? String(a.idx),
      content,
      key: a.key,
      idx: a.idx,
      result: answered ? a.result : null,
      prompt: a.prompt,
    };
    plan.steps.push(step);
    plan.byPos.set(step.pos, step);
    const at = plan.byContent.get(step.content);
    if (at) at.push(step.pos);
    else plan.byContent.set(step.content, [step.pos]);
  }
  plan.steps.sort((x, y) => comparePos(x.pos, y.pos));
  return plan;
}

/**
 * How many leading calls of a plan could replay AT BEST — the ceiling a relaunch can
 * claim before its own keys are known. Zero is not a defect on its own: a source that
 * failed its first call has nothing to offer a relaunch, which is a full live run.
 *
 * "Leading" is in STRUCTURAL order, which is the order the prefix rule is defined over.
 */
export function replayablePrefix(plan: ReplayPlan): number {
  let n = 0;
  while (n < plan.steps.length && plan.steps[n].result !== null) n++;
  return n;
}

// ---------------------------------------------------------------------------
// Why replay stopped (pure)
// ---------------------------------------------------------------------------

/**
 * The four ways a call can fail to replay. They are separated because they call for
 * four different next moves, and one of them used to be reported as another.
 *
 *   - `changed` — same coordinate, different content hash. The call was EDITED. This is
 *     the ordinary, intended reason a relaunch costs money.
 *   - `moved` — the content hash is unchanged and the source ran this exact call, at a
 *     DIFFERENT coordinate. The script's shape changed (an item added, a stage
 *     reordered), or something upstream renumbered it. Saying "its key changed" here is
 *     a lie, and it was the lie that hid the pipeline transposition defect: the one
 *     surface that exists to make a key problem visible reported the opposite of what
 *     happened.
 *   - `added` — no call at that coordinate and no call anywhere in the source asks the
 *     same thing. It is new work.
 *   - `unanswered` — the source made this exact call and has nothing to hand back: it
 *     failed, was stopped, or never finished. It re-runs because the failure may be the
 *     thing the author just fixed (spec §8).
 */
export type DivergenceKind = "changed" | "moved" | "added" | "unanswered";

export interface Divergence {
  /** This run's coordinate for the call replay stopped at. */
  pos: CallPos;
  kind: DivergenceKind;
  /** Where the source ran this same call, when `kind` is `moved`. */
  sourcePos?: CallPos;
  /** One sentence, in the words every surface prints. Names the distinction. */
  reason: string;
}

/** Why the call at `pos` asking for `content` cannot replay from `plan`. */
export function classifyDivergence(
  plan: ReplayPlan,
  pos: CallPos,
  content: string,
): Divergence {
  const step = plan.byPos.get(pos);
  if (step && step.content === content) {
    return {
      pos,
      kind: "unanswered",
      reason: `the source run made this call at ${pos} and has no answer for it — it ` +
        `failed, was stopped, or never finished, so it runs live`,
    };
  }
  if (step) {
    return {
      pos,
      kind: "changed",
      reason: `the call at ${pos} was edited: same position in the script, different key`,
    };
  }
  const elsewhere = plan.byContent.get(content);
  if (elsewhere && elsewhere.length > 0) {
    return {
      pos,
      kind: "moved",
      sourcePos: elsewhere[0],
      reason: `the call MOVED: its key did not change — the source run made this exact ` +
        `call at ${elsewhere[0]}, and this run makes it at ${pos}. The script's shape ` +
        `changed, not its prompts`,
    };
  }
  return {
    pos,
    kind: "added",
    reason: `the source run never made a call at ${pos}, and none of its calls ask for ` +
      `the same thing — this call is new`,
  };
}

/**
 * What a run did with its journal, folded from the rows it wrote. One implementation,
 * so the completion note, `GET /workflows/:id/replay` and the run view cannot disagree
 * about where replay stopped.
 *
 * The divergence reported is the STRUCTURALLY FIRST live call the plan could not serve,
 * not the first by dispatch index — dispatch index is the thing that was never
 * reproducible, and quoting it would put the original defect back into the report.
 */
export interface ReplayAudit {
  /** The structurally first call replay could not serve, or `null` if none. */
  diverged: Divergence | null;
  /** Its dispatch index in this run — "call N of this run", for a human line. */
  divergedAt: number | null;
  /**
   * Calls that ran live although their coordinate AND key still matched a stored
   * answer — the price of the prefix rule, stated rather than hidden.
   */
  forced: number;
}

export function replayAudit(plan: ReplayPlan, rows: readonly WorkflowAgent[]): ReplayAudit {
  // Nothing to diverge FROM. A first run has no source, and a relaunch of a run that
  // journaled nothing has an empty one; in both cases every call is live because there
  // was never an alternative, not because something changed. Reporting a divergence
  // here would put "the source run never made a call at 0.0.0.0" on a run with no
  // source — an accusation with no defendant, on the most ordinary path there is.
  // The engine makes the same check before it decides anything (`plan.steps.length`).
  if (plan.steps.length === 0) return { diverged: null, divergedAt: null, forced: 0 };
  let diverged: Divergence | null = null;
  let divergedAt: number | null = null;
  let divergedPos: CallPos | null = null;
  let forced = 0;
  for (const row of rows) {
    if (row.status === "cached") continue;
    const { pos, content } = splitJournalKey(row.key);
    const at = pos ?? String(row.idx);
    const step = plan.byPos.get(at);
    if (step && step.content === content && step.result !== null) {
      forced++;
      continue;
    }
    if (divergedPos === null || comparePos(at, divergedPos) < 0) {
      divergedPos = at;
      divergedAt = row.idx;
      diverged = classifyDivergence(plan, at, content);
    }
  }
  return { diverged, divergedAt, forced };
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

  // Journal replay, PREFIX-BOUNDED. The source run's calls in call order; the engine
  // below replays the longest leading run of them whose keys still match and stops
  // for good at the first that does not (spec §8, and the header). Only calls that
  // ANSWERED are replayable — a failed one re-runs live, because the failure may be
  // the very thing this edit fixes, and everything after it re-runs too, because a
  // live call may have changed the checkout the later ones were answered against.
  let plan: ReplayPlan = emptyReplayPlan();
  let args: unknown = opts.args ?? null;
  let meta = opts.meta;
  if (opts.resumeOf) {
    const src = db.getWorkflow(opts.resumeOf);
    if (!src) throw new NotFoundError(`workflow ${opts.resumeOf} not found`);
    if (opts.args === undefined) args = src.args; // a relaunch keeps its input by default
    meta ??= { name: src.name, description: src.description, phases: src.phases };
    plan = replayPlan(db, opts.resumeOf);
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

  // Mirror the script to a real file so "edit it and relaunch" is a file edit away
  // (spec §8). A convenience — the canonical script is the row — and best-effort: a
  // read-only `~/.bough` must not stop a run from starting. `workflow/journal.ts` owns
  // every mirror read and write in the tree, so the confinement guard on the path is
  // applied here too rather than only where a relaunch reads it back (T5.8).
  await mirrorScript(id, opts.script);

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
  /**
   * Prefix-bounded replay state: the STRUCTURALLY SMALLEST coordinate at which this
   * run's calls stopped matching the source, or `null` while the prefix still holds. A
   * call may replay only when its own coordinate sorts strictly before it — so the
   * divergent call and everything structurally after it runs live, including calls
   * whose own key never changed (spec §8).
   *
   * A coordinate rather than a boolean, because dispatch order is not structural order:
   * `pipeline()` issues stage 2 in stage-1 completion order, so a call can arrive after
   * a divergence and still sit BEFORE it in the script. Under a boolean flag that call
   * would be forced live for no reason — the flag would be recording latency, which is
   * the whole class of bug this change removes.
   *
   * What it cannot do is retract: a call already replayed before a structurally-earlier
   * divergence was discovered stays replayed. That only happens between calls the
   * script itself declared CONCURRENT (a barrier-free pipeline's stage-N cells), which
   * have no order relative to each other in the source run either — the run they
   * replayed from interleaved them arbitrarily too. A sequentially-later call is
   * dispatched after its predecessor by construction, so it always sees the divergence.
   */
  let divergedPos: CallPos | null = null;
  /** The divergent call's dispatch index, for the human line. `null` = it never broke. */
  let divergedAt: number | null = null;
  /** WHY it broke — edited, moved, added, or unanswered. Carried into the note. */
  let divergence: Divergence | null = null;
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

  /**
   * Take a semaphore slot, but only once the gate is ALSO open — and re-check both as
   * a loop, because either can change while this call awaits the other.
   *
   * WHY THIS IS NOT JUST `acquire()`. Pause used to be consulted once, before the
   * journal row and before the semaphore, which made it a no-op for the one shape
   * workflows exist for. `parallel()` issues every thunk at dispatch, so a fan-out of
   * six at concurrency two has all six calls past the gate within the first tick;
   * four of them are merely parked on the semaphore. Pausing then released nothing
   * and gated nothing — the two in flight finished, the queue drained, and all four
   * remaining agents launched anyway. Spec §8 sells pause as the way to preserve the
   * most work before a stop, and `workflow/relaunch.ts` repeats that advice in a
   * user-facing 409; for a fan-out it changed nothing and the run billed to
   * completion. The existing coverage was a strictly sequential script, the one shape
   * where a single pre-dispatch check happens to be enough.
   *
   * A call parked here keeps its journal row at `queued`, never `running`: the row is
   * already written (that is what makes a saturated run show queued agents rather
   * than pretending all of them work), and the `running` write happens only after
   * this resolves true.
   *
   * The slot is RELEASED while parked rather than held. A paused run admits nothing,
   * so holding it would only mean that on resume the semaphore's own FIFO no longer
   * matches the order calls arrived in.
   *
   * Returns false when the run was stopped while this call was parked — the caller
   * settles its row rather than starting an agent for a run that no longer exists.
   */
  const admit = async (): Promise<boolean> => {
    for (;;) {
      if (ctrl.signal.aborted) return false;
      if (state.paused) {
        await awaitGate();
        continue; // re-check abort before touching the semaphore: stop opens the gate
      }
      await acquire();
      if (ctrl.signal.aborted || state.paused) {
        release();
        if (ctrl.signal.aborted) return false;
        continue;
      }
      return true;
    }
  };

  const finish = (status: "done" | "error" | "stopped", result?: unknown, error?: string) => {
    if (!live.has(id)) return;
    live.delete(id);
    clearTimeout(state.timer);
    worker.terminate();
    // Aborting the run's controller is what interrupts in-flight subagent TURNS —
    // killing the worker only stops the script (spec §8: stop does both).
    ctrl.abort();
    // Then open the gate, in that order, so everything unparked here wakes to an
    // ALREADY-aborted signal and takes the wind-down path by construction rather than
    // by microtask timing. Leaving calls parked on a run that no longer exists would
    // strand their promises for the lifetime of the process.
    state.paused = false;
    state.gate.splice(0).forEach((open) => open());
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
      // Replay is REPORTED, always (spec §8). A relaunch that replayed nothing and one
      // that replayed everything produce the same row, the same events and the same
      // result — the counts are the only thing that makes a broken key visible, so
      // they ride the note the model actually reads rather than a view someone may
      // open. `divergedAt` names the call the prefix broke on, which is the line that
      // turns "why did 38 agents run again" into an answer.
      const replayed = agents.filter((a) => a.status === "cached").length;
      const head = `[workflow ${status}] "${updated.name}" (${id}) — ` +
        `${okCount}/${agents.length} agents succeeded.\n` +
        (plan.steps.length > 0
          ? `Replay: ${replayed} replayed from run ${updated.resumeOf}, ` +
            `${agents.length - replayed} ran live` +
            (divergence === null
              ? " (the whole prefix matched)."
              : `, from ${divergedPos} (call ${divergedAt}) on — ${divergence.reason}.`)
          : `Replay: none — this run had no journal to replay.`);
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

  // `WorkflowHostFns` (types.ts, frozen at T-1) declares `agent(prompt, optsJson)`. The
  // structural coordinate rides the message ENVELOPE, not the argument list, so the
  // frozen shape still describes what the script calls — the coordinate is the bridge's
  // business, never the script's. Widened here, and only here, so the dispatcher can
  // hand it over.
  const host: WorkflowHostFns & {
    agent(prompt: string, optsJson: string, pos?: string): Promise<string>;
  } = {
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

    async agent(prompt: string, optsJson: string, pos?: string): Promise<string> {
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

      // The call's coordinate. The worker computes it from the script's SHAPE — a
      // pipeline cell, a parallel slot, the enclosing frame's counter for a bare call
      // (`harness/wf_worker.ts`). Absent only when something other than that worker is
      // driving this host (a unit test, a future driver), in which case the old
      // monotonic counter is the right answer and is exactly what a sequential script's
      // coordinates already are.
      const callPos: CallPos = typeof pos === "string" && pos.length > 0 ? pos : String(at);
      const content = callKey(call, opts.effectiveModel);
      const key = journalKey(callPos, content);

      // THE PREFIX DECISION, made SYNCHRONOUSLY — before the gate, before the
      // semaphore, in the same uninterrupted block that assigned `at`. Deciding here
      // makes the answer a pure function of (coordinate, key) and never a function of
      // which concurrent call happened to resume first. Deciding after an `await` would
      // let a later call's hit be recorded before an earlier call's miss had moved the
      // frontier, and a run would replay past its own divergence.
      let cached: string | undefined;
      if (plan.steps.length > 0) {
        const blocked = divergedPos !== null && comparePos(callPos, divergedPos) >= 0;
        const step = plan.byPos.get(callPos);
        if (blocked) {
          // Behind a divergence that has already been announced. It runs live and says
          // nothing more — one line per divergence, not one per consequence.
        } else if (step && step.content === content && step.result !== null) {
          cached = step.result;
        } else {
          const why = classifyDivergence(plan, callPos, content);
          divergedPos = callPos;
          divergedAt = at;
          divergence = why;
          // Said out loud, because this is the moment a relaunch stops being free and
          // the whole rest of the run becomes live work. A run that quietly replayed
          // nothing looks exactly like one that replayed everything (spec §8:
          // "replay is always reported"). The message names WHICH of the four things
          // happened — an edited call and a moved one are different problems with
          // different fixes, and reporting a move as "its key changed" is how a
          // position defect stayed invisible.
          bus.publish({
            type: "workflow.log",
            sessionId: run.sessionId,
            data: {
              runId: id,
              line: `replay ends at ${callPos} (call ${at}, ${clip(call.label, 60)}): ` +
                `${why.reason} — it and everything after it in the script run live, ` +
                `including calls whose own key is unchanged (agents share one checkout)`,
            },
          });
        }
      }

      // Pause parks the call BEFORE it journals: a call that has not been admitted yet
      // has no row, so the UI never shows a "running" agent that has not actually
      // started (field finding — a sequential script's next call surfaced as running
      // and session-less while the run sat paused). This is the FIRST of two gate
      // checks; `admit()` below is the one that holds for a fan-out, whose calls are
      // all past this line before anybody can press pause.
      await awaitGate();

      // Stop opens the gate on the way DOWN, not only resume — so nothing is left
      // parked on a run that no longer exists. A call woken that way must not journal:
      // the wind-down has already swept every non-terminal row, so a row written after
      // it would sit at `queued` with nothing left in this process that could settle
      // it. That was the leak, and the pause→stop sequence spec §8 recommends is
      // exactly how you hit it.
      if (ctrl.signal.aborted) {
        throw new WorkflowError(409, "workflow stopped — this call was never journaled");
      }

      // Display label: an explicit one wins; otherwise a line this agent does not
      // share with the siblings already in the run.
      const shown = typeof raw.label === "string" && raw.label.trim()
        ? call.label
        : distinctLabel(call.prompt, db.listWorkflowAgents(id).map((a) => a.label));
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

      // The gate and the semaphore, together. A paused run holds the call HERE, with
      // its row still `queued`, however many calls the script already dispatched.
      const admitted = await admit();
      // ONE try/catch, not two nested ones. The abort check used to sit inside an
      // outer try whose only `finally` released the semaphore, so throwing on it
      // stepped straight over the handler that settles the row — a call stopped
      // between journaling and starting left `queued` behind forever. Every exit from
      // here now runs the same settle, which is what makes "a stopped run leaves no
      // row in a non-terminal state" true of the code rather than of one path.
      try {
        if (!admitted || ctrl.signal.aborted) {
          throw new WorkflowError(
            409,
            `workflow stopped — "${clip(call.label, 60)}" was queued and never started`,
          );
        }
        // Off the semaphore and past the gate: the clock starts HERE, not when the
        // call journaled, so elapsed time excludes time parked or paused.
        db.updateWorkflowAgent(row.id, { status: "running", startedAt: now() });
        publishAgent(ctx, run.sessionId, id, row.id);
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
      } finally {
        // Only if a slot was actually taken. `admit()` returning false took none.
        if (admitted) release();
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
      // The coordinate is appended for `agent` only — `phase` and `log` are not
      // journaled, so they have no position and the worker sends none.
      const args = msg.fn === "agent" ? [...msg.args, msg.pos] : msg.args;
      const fn = host[msg.fn as WorkflowHostFnName];
      // deno-lint-ignore no-explicit-any
      const value = await (fn as any).apply(host, args);
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
  state.worker.terminate();
  state.ctrl.abort();
  // Then release anything parked on the pause gate, so no promise leaks with the
  // worker gone. ABORT FIRST, deliberately: everything unparked here wakes to an
  // already-aborted signal and takes the wind-down path in `agent()` — journaling
  // nothing if it had not journaled yet, settling its own row if it had. The reverse
  // order made that a microtask race, and losing it left a `queued` row on a stopped
  // run with nothing left in the process that could ever settle it.
  state.paused = false;
  state.gate.splice(0).forEach((open) => open());
  // The sweep covers every row that exists at this instant. Rows can still settle
  // after it — a call unparked above rejects and writes its own terminal status —
  // but none can be CREATED after it, which is what closes the hole.
  for (const a of ctx.db.listWorkflowAgents(id)) {
    if (a.status === "running" || a.status === "queued") {
      ctx.db.updateWorkflowAgent(a.id, { status: "stopped", finishedAt: now() });
    }
  }
  ctx.db.updateWorkflow(id, { status: "stopped", finishedAt: now() });
  return publishRun(ctx, id)!;
}

/**
 * Pause: no further agent STARTS; the ones already running finish normally.
 *
 * "Starts", not "is issued". The distinction is the whole of spec §8's promise —
 * pause is what preserves the most work before a stop, and it only does that if it
 * bites on a fan-out, whose calls are all issued at dispatch and then sit on the
 * semaphore. `admit()` is where that holds: a call already past the pre-journal gate
 * is stopped at the semaphore instead, and its journal row stays `queued`.
 */
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
 * Rerun a finished run with journal replay: the unchanged PREFIX of its `agent()`
 * calls returns the old run's results instantly, and the first changed call plus
 * everything after it runs live. The script defaults to the run's file mirror, so
 * "edit the file, press r" is the whole iteration loop.
 *
 * A rerun is a NEW run pointing back via `resumeOf`, never an edit of the old one —
 * nothing in bough is destructively rewritten (spec §2.4). It is the same operation as
 * a relaunch (`workflow/relaunch.ts`); a rerun is just the case where the script did
 * not change (spec §8).
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
  // Explicit script, else the mirror the user may have edited, else the stored row —
  // one resolution, in `workflow/journal.ts`, which the control layer calls too (T5.8).
  const { script } = await resolveRerunScript(src, opts.script);
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
