/**
 * The turn loop: everything that happens after a user message lands (spec §5).
 *
 * THE INVARIANT THIS HOLDS: **a turn always ends, always ends visibly, and always
 * ends exactly once.** Three separate failures hide behind that one sentence, and
 * every structural decision in this file is one of them:
 *
 *   1. **A turn never ends implicitly.** The model calls `stop` after its final
 *      text, in the same response. A response that just trails off is not an ending
 *      — it is a model that forgot — so it gets nudged, with a bounded count so a
 *      stop-incapable model cannot loop the API forever. The nudges and the `stop`
 *      call itself are loop control, never content: they live only in this turn's
 *      in-memory exchange and are never persisted, so the thread and every future
 *      replay stay clean.
 *   2. **Every turn must produce user-visible text.** A turn of nothing but tool
 *      calls shows the user a stack of collapsed cards and no answer — the agent
 *      looks mute. Worse, narration counts for nothing: a turn that says "let me
 *      implement the changes:" and then ends on a raw `rg` dump has said less than
 *      one that said nothing, because the last thing the user sees is a plan for
 *      work that is already done. So `saidSomething()` asks only about text *after
 *      the last tool call*, and a turn about to end mute is asked once for a closing
 *      report, then forced into a text-only round (`toolChoice: "none"`) — which
 *      reliably yields prose where a second nudge yields another empty stop.
 *   3. **The pending message is closed on every path.** Success, failure,
 *      interrupt, a crash in the loop — `pending` goes false and `message.finished`
 *      fires, because a message left pending is a session the UI shows as busy
 *      forever and a queue that never drains.
 *
 * WHAT IS NOT HERE, DELIBERATELY. There is no acceptance gate (spec §17): the
 * harness does not re-run a committed check, does not grade `done`, and does not
 * block completion. `run_steps`'s `done` flag is the model's own statement that the
 * work is finished, and it is recorded with the call and acted on by nobody. The
 * port had a CHECK gate here; it is gone with the rest of the acceptance machinery.
 *
 * PROVIDER-BLINDNESS. Nothing in this file knows which of the three providers it is
 * talking to. Everything goes through `LlmClient` (plan §8.3) — if a provider name
 * ever appears below, it has leaked, and it will leak everywhere next.
 *
 * REASONING, AND THE ONE PLACE IT IS ECHOED. Across turns, reasoning is dropped
 * (`replay.ts`, plan §6.4). *Within* one turn the block goes back verbatim, `meta`
 * and all, because a provider that signs thinking rejects a tool call whose
 * thinking was altered. The two rules are not in tension: the in-turn echo comes
 * from `LlmResult.content` in memory, the cross-turn drop is about what
 * `replay.ts` reads out of the database, and nothing signed is ever stored.
 *
 * Ported from `src/turn.ts`. Deltas from that port are marked `NOTE:`.
 */
import { z } from "zod";
import { ContextOverflowError } from "../errors.ts";
import { API_KEY_ENV, clientFor, errName, providerFor } from "../llm/client.ts";
import { contextWindowFor } from "../llm/pricing.ts";
import { traceLabel, writeManifest } from "../llm/trace.ts";
import { ensureScratchDir } from "../scratch.ts";
import { createFileHostFns } from "../hostfn/files.ts";
import { createShellHostFns } from "../hostfn/shell.ts";
import { runProgram } from "../harness/vm.ts";
import type { ProgramResult } from "../harness/protocol.ts";
import { HOST_FN_NAMES, type HostFnName } from "../harness/protocol.ts";
import {
  type AssembledPrompt,
  assemblePrompt,
  type PromptInput,
  scratchNote,
  workspaceNote,
} from "../prompt/assemble.ts";
import {
  drainProjectRuleNotes,
  findProjectRules,
  noteProjectRules,
  projectRulesNote,
} from "../prompt/project.ts";
import { createCommandRecorder } from "../history/record.ts";
import { dirTagHints, tagsNoteFor } from "../history/stats.ts";
import { dirname, isAbsolute } from "node:path";
import { boughHome } from "../paths.ts";
import type { Message, Part, Session, Usage } from "../schema/parts.ts";
import type {
  AppCtx,
  Db,
  HostFns,
  LlmBlock,
  LlmClient,
  LlmContentBlock,
  LlmMessage,
  LlmToolDef,
  TurnCtx,
} from "../types.ts";
import { buildThread } from "./replay.ts";
import {
  abortableDelay,
  classifyRoundFailure,
  shortReason,
  shouldDrain,
  TurnRegistry,
  turns as defaultRegistry,
} from "./queue.ts";
import { checkpoint, finishTurn, startTurn } from "./state.ts";

// ---------------------------------------------------------------------------
// The model-facing surface
// ---------------------------------------------------------------------------

/**
 * The entire model-facing API (spec §6). Two tools, and one of them is loop
 * control.
 *
 * A per-session or per-capability tool would split the provider's prompt cache —
 * tool definitions precede the system prompt in the cache order, so one varying byte
 * here costs every session the shared prefix. Capabilities are granted through host
 * functions inside `run_steps` and the prompt sections that document them, never by
 * adding a tool.
 */
export const RUN_STEPS = "run_steps";
export const STOP = "stop";

export const TOOLS: LlmToolDef[] = [
  {
    name: RUN_STEPS,
    description: "Run one JavaScript program in the workspace.",
    inputSchema: {
      type: "object",
      properties: {
        code: {
          type: "string",
          description: "The program. Host functions are pre-injected globals.",
        },
        done: {
          type: "boolean",
          description: "The work is complete after this program.",
        },
      },
      required: ["code"],
      additionalProperties: false,
    },
  },
  {
    name: STOP,
    description: "End the turn. Call after your final text, in the same response.",
    inputSchema: { type: "object", properties: {}, additionalProperties: false },
  },
];

/**
 * Validated at the boundary, like every other wire shape (plan §0). A `code` that
 * arrived as a number is a model mistake the next round can fix; it must not reach
 * `runProgram` as one.
 */
export const RunStepsInput = z.object({
  code: z.string(),
  done: z.boolean().optional(),
});

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/** The output reservation every round makes. The context meter measures against it. */
export const MAX_TOKENS = 64_000;

/** Used when neither the ctx nor the session pins one. */
export const DEFAULT_MODEL = "claude-opus-4-8";

/** Re-prompts before the harness stops waiting for an explicit `stop`. */
export const MAX_STOP_NUDGES = 3;

const STOP_NUDGE = "[harness] Your turn is still open — it only ends when you call the stop " +
  "tool. Continue if there is more to do, or call stop now (alone, no other output) if you " +
  "are finished.";

/**
 * Asks for a CLOSING report, not merely "some text".
 *
 * The wording matters and was learned the hard way: "you have written no
 * user-visible text this turn" describes the mute case only, and an agent that
 * narrated on its way through would answer it with nothing, ending on a raw tool
 * dump with its last word being "Let me implement the changes:". What the user needs
 * at the end is the outcome, not the plan.
 */
const REPORT_NUDGE = "[harness] Your turn is about to end and the last thing the user " +
  "can see is tool output — anything you wrote earlier was narration of work in " +
  "progress, not a conclusion. Close the turn now: say what you changed (name the " +
  "files), what you verified and how it came out, and anything you did NOT do or " +
  "left uncertain. A few lines is plenty; do not restate your plan or re-explain " +
  "the code. Then call stop in the same response.";

/**
 * A literal `<stop/>` ending the text, possibly repeated or padded.
 *
 * Models sometimes *emit* the sentinel as text instead of calling the tool. Parsed
 * tolerantly: it is stripped from what gets stored (loop control, not content — the
 * same rule as the `stop` call) and honored as the stop it meant. End-anchored on
 * purpose, so prose that merely mentions the token in a code span is never touched.
 */
const TRAILING_STOP_SENTINEL = /(?:\s*<stop\s*\/>)+\s*$/i;

/** The interrupt note. `⏹` and not `⚠︎`: the user asked for this. */
const STOPPED_NOTE = "⏹ Stopped.";

// ---------------------------------------------------------------------------
// Seams
// ---------------------------------------------------------------------------

/** One `run_steps` execution, as the runner asks for it. */
export interface ProgramRun {
  code: string;
  /** The `tool_use` id, so streamed lines are attributed to the right card. */
  callId: string;
  signal: AbortSignal;
  onLog: (line: string) => void;
}

/**
 * Executes one program. Injected so the whole loop is drivable with a fake and no
 * worker is ever spawned in a unit test — which is the difference between a turn
 * test that runs in milliseconds offline and one that needs a machine.
 */
export type ProgramRunner = (run: ProgramRun) => Promise<ProgramResult>;

/**
 * The host functions the default runner bridges, and therefore the capabilities the
 * prompt grants. Shell and files are always wired (types.ts `HostFns`); everything
 * else arrives with its milestone, and `deps.granted` is how a caller that bridges
 * more says so.
 */
export const BASE_HOST_FNS: HostFnName[] = [
  "bash",
  "sh",
  "bashBg",
  "bashOutput",
  "bashWait",
  "bashKill",
  "view",
  "patch",
  "write",
];

/**
 * Build the always-wired host functions for one turn.
 *
 * `exits` is the seam that lets a round REPORT a command that failed — see
 * `ShellCtx.exits` and `defaultProgramRunner`. Optional so every other caller is
 * unchanged.
 */
export function baseHostFns(ctx: TurnCtx): HostFns {
  // Initialised HERE so every construction path shares one array — see `TurnCtx.exits`.
  const exits = (ctx.exits ??= []);
  // Same rule for the memory seams: one recorder and one read/touch trail per
  // turn, shared by every construction path (`TurnCtx.record` says why). The
  // touch trail must exist BEFORE the recorder closes over the ctx.
  ctx.touched ??= [];
  const record = (ctx.record ??= createCommandRecorder(ctx));
  const reads = (ctx.reads ??= []);
  return {
    ...createShellHostFns({
      sessionId: ctx.sessionId,
      workspace: ctx.workspace,
      signal: ctx.signal,
      scratch: ensureScratchDir(ctx.sessionId),
      exits,
      record,
    }),
    ...createFileHostFns({ sessionId: ctx.sessionId, workspace: ctx.workspace, reads }),
  };
}

/**
 * The production program runner: a fresh worker per round with the turn's host
 * functions bridged and the turn's interrupt wired into the wind-down.
 */
export function defaultProgramRunner(
  ctx: TurnCtx,
  host?: HostFns,
): ProgramRunner {
  const fns = host ?? baseHostFns(ctx);
  // Per TURN on the ctx, read per ROUND by index: the host functions may have been built by
  // the caller (`delegationDeps` does exactly that for every delegating session), so the
  // array cannot live in this closure.
  const exits = (ctx.exits ??= []);
  const reads = (ctx.reads ??= []);
  const touched = (ctx.touched ??= []);
  return async ({ code, signal, onLog }) => {
    const from = exits.length;
    const fromReads = reads.length;
    const fromTouched = touched.length;
    const result = await runProgram({ code, host: fns, signal, onLog });
    return withProjectRuleNotes(
      withDirTagHintNotes(
        withExitNotes(result, exits.slice(from)),
        ctx,
        [...reads.slice(fromReads).map((p) => dirname(p)), ...touched.slice(fromTouched)],
      ),
      ctx,
    );
  };
}

/**
 * Append the `AGENTS.md` report queued when this turn's prompt was assembled.
 *
 * Same carrier as the tag hints and for the same reason — the round's RESULT, not
 * the prompt, because a per-turn prompt edit would bust the volatile tier's cache.
 * The queue drains on the first round of the turn, so a multi-round turn says it
 * once. `prompt/project.ts` owns what is worth saying.
 */
function withProjectRuleNotes(result: ProgramResult, ctx: TurnCtx): ProgramResult {
  const lines = drainProjectRuleNotes(ctx.sessionId);
  if (lines.length === 0) return result;
  return { ...result, logs: [...result.logs, ...lines] };
}

/**
 * Append the per-directory tag hints for directories this round newly touched —
 * by `view()` reads or by its shell commands — the mid-turn half of the
 * tag-history memory. Appended to the round's RESULT, never to the prompt: a
 * mid-session prompt edit would bust the volatile-tier cache (`llm/client.ts`).
 * `history/stats.ts` owns per-dir repo resolution, the divergence rule and the
 * caps; the dirs here are absolute.
 */
function withDirTagHintNotes(
  result: ProgramResult,
  ctx: TurnCtx,
  absDirs: readonly string[],
): ProgramResult {
  const dirs = [...new Set(absDirs)].filter((d) => isAbsolute(d));
  if (dirs.length === 0) return result;
  const lines = dirTagHints(ctx.db, ctx.sessionId, ctx.workspace, dirs, (ctx.now ?? Date.now)());
  if (lines.length === 0) return result;
  return { ...result, logs: [...result.logs, ...lines] };
}

/**
 * Append the commands that exited non-zero, when the program did not print them itself.
 *
 * `bash()` returns `[exit code N]` as data rather than throwing, which is the right call —
 * it is a result to read. But the string goes into the program, so a round that never logs
 * it leaves the failure INVISIBLE: a reviewer ran `await bash("exit 3")` without logging and
 * got `◇ run_steps ✓ done` over "(the program ran and printed nothing)", after which the
 * model narrated a confident invented mechanism ("bash() threw on the non-zero exit"). The
 * harness knew the code all along.
 *
 * Only when it is not already there: a program that DID log the output has said it, and
 * saying it twice is its own kind of noise.
 */
export function withExitNotes(
  result: ProgramResult,
  exits: readonly { command: string; code: number }[],
): ProgramResult {
  if (exits.length === 0) return result;
  const said = result.logs.join("\n");
  const unsaid = exits.filter((e) => !said.includes(`[exit code ${e.code}`));
  if (unsaid.length === 0) return result;
  const notes = unsaid.map((e) => `[exit code ${e.code}] ${oneLineCommand(e.command)}`);
  return { ...result, logs: [...result.logs, ...notes] };
}

/** A command on one line, short enough to sit in a result. */
function oneLineCommand(command: string): string {
  const flat = command.replace(/\s+/g, " ").trim();
  return flat.length > 80 ? `${flat.slice(0, 79)}…` : flat;
}

export interface TurnDeps {
  /** Defaults to the process registry. Tests pass their own. */
  registry?: TurnRegistry;
  /**
   * A fixed program runner — what a test passes, since a fake needs nothing from
   * the turn. Wins over `programFor`.
   */
  program?: ProgramRunner;
  /**
   * A runner built from the turn's own ctx (its workspace, its interrupt). This is
   * the shape production needs and the reason the two are separate fields: both are
   * one-argument functions, so a single field could not tell them apart.
   * Defaults to `defaultProgramRunner`.
   */
  programFor?: (ctx: TurnCtx) => ProgramRunner;
  /** Defaults to `assemblePrompt`. */
  assemble?: (input: PromptInput) => AssembledPrompt;
  /** Host functions this turn bridges, for the prompt's capability gating. */
  granted?: HostFnName[];
  /** Extra volatile prompt notes (workspace, running subagents, project rules). */
  notes?: string[];
  /** Injected clock. Absent = `ctx.now`, then `Date.now`. */
  now?: () => number;
  /** Round-retry knobs; tests turn the outage delay down so a test is not a minute. */
  maxRoundRetries?: number;
  outageDelayMs?: number;
  maxTokens?: number;
  /**
   * Background shells that outlive an interrupt, so the stop note can name them
   * (spec §9: they are detached on purpose). Absent = say nothing rather than
   * claim there were none.
   */
  survivingJobs?: (sessionId: string) => string[];
  /** Recursion seam: how a queued drain starts the next turn. Tests observe it. */
  startNext?: (ctx: AppCtx, sessionId: string) => void;
  /**
   * Where a failed turn's raw error is reported. The default logs it, because the
   * UI must never know more than the server log does. A test passes a collector so
   * an intentional failure does not print a stack, and so the reporting can be
   * asserted rather than inferred.
   */
  reportError?: (error: unknown, sessionId: string) => void;
}

/** What a finished turn reports to a caller that awaited it. */
export interface TurnOutcome {
  turnId: string;
  messageId: string;
  status: "done" | "error" | "interrupted";
  error?: string;
  usage: Usage;
}

/** Thrown to unwind the loop on a user interrupt. */
class InterruptedError extends Error {
  constructor() {
    super("interrupted");
    this.name = "InterruptedError";
  }
}

// ---------------------------------------------------------------------------
// Entry points
// ---------------------------------------------------------------------------

/**
 * Start a turn. Returns the pending supervisor message immediately and the promise
 * that resolves when the turn is fully finished.
 *
 * The two are separate because the HTTP path is a 202 that discards the promise —
 * the turn outlives the response by minutes — while a test awaits it. The message is
 * created and announced synchronously so a client that reconciles by id sees it even
 * if the turn finishes before the post returns (plan §6.10).
 */
export function beginTurn(
  ctx: AppCtx,
  sessionId: string,
  deps: TurnDeps = {},
): { message: Message; done: Promise<TurnOutcome> } {
  const now = deps.now ?? ctx.now ?? Date.now;
  const registry = deps.registry ?? defaultRegistry;

  // Claim the session FIRST. `begin` throws when one is already running, and it has
  // to throw before the placeholder exists — a message created and announced and
  // then abandoned would sit `pending` in the transcript with no turn to close it,
  // which is the exact hang this whole milestone is about.
  const controller = registry.begin(sessionId);

  const message: Message = {
    id: crypto.randomUUID(),
    sessionId,
    role: "supervisor",
    parts: [],
    pending: true,
    createdAt: now(),
  };
  ctx.db.createMessage(message);
  ctx.bus.publish({ type: "message.started", sessionId, data: message });

  const done = drive(ctx, message, controller, deps)
    .finally(() => {
      registry.end(sessionId, controller);
      // Drain: a message that landed mid-turn becomes a fresh turn now. Started
      // after the release, never before — `begin` would throw on a session this
      // turn had not let go of yet.
      if (shouldDrain(ctx.db, sessionId, registry)) {
        const next = deps.startNext ?? ((c, s) => startDetached(c, s, deps));
        try {
          next(ctx, sessionId);
        } catch (err) {
          console.error(`failed to drain queued message for session ${sessionId}:`, err);
        }
      }
    });

  return { message, done };
}

/**
 * Start a turn nobody will await.
 *
 * The `catch` is not politeness. `drive` handles its own failures, but the few
 * statements before its `try` (opening the turn row, reading the session) can still
 * throw, and an unhandled rejection here would take the process down — losing every
 * other session with it.
 */
function startDetached(ctx: AppCtx, sessionId: string, deps: TurnDeps): void {
  beginTurn(ctx, sessionId, deps).done.catch((err) => {
    (deps.reportError ?? ((e, s) => console.error(`turn failed to start [${s}]:`, e)))(
      err,
      sessionId,
    );
  });
}

/**
 * The `TurnStarter` `server/sessions.ts` reads off the ctx.
 *
 * A post into a session that is already busy never reaches here — the handler
 * checks `busySessionIds()` and 202s — but the guard is repeated anyway: the
 * registry is the authority on "one turn per session", and a second caller
 * (a schedule firing, a system note waking a session) must hit the same wall.
 */
export function createTurnStarter(deps: TurnDeps = {}) {
  const registry = deps.registry ?? defaultRegistry;
  return (ctx: AppCtx, session: Session, _message: Message): void => {
    if (registry.isRunning(session.id)) {
      registry.enqueue(session.id);
      return;
    }
    startDetached(ctx, session.id, deps);
  };
}

/** Stop the session's turn and cascade to its detached children. */
export function interruptTurn(
  sessionId: string,
  registry: TurnRegistry = defaultRegistry,
): boolean {
  return registry.interrupt(sessionId);
}

// ---------------------------------------------------------------------------
// The loop
// ---------------------------------------------------------------------------

async function drive(
  ctx: AppCtx,
  message: Message,
  controller: AbortController,
  deps: TurnDeps,
): Promise<TurnOutcome> {
  const { db, bus } = ctx;
  const sessionId = message.sessionId;
  const messageId = message.id;
  const signal = controller.signal;
  const now = deps.now ?? ctx.now ?? Date.now;
  const maxTokens = deps.maxTokens ?? MAX_TOKENS;

  const session = db.getSession(sessionId);
  // Session pin first, then the global default, then the built-in — spec §4:
  // "`model`, `effort`: per-session overrides; absent = global default". The ctx
  // carries the GLOBAL default (`AppCtx.model`), so reading it first would make
  // `setSessionModel` a no-op on any install that sets `BOUGH_MODEL`. Same order as
  // `effort` below, which is not a coincidence: they are one rule.
  const model = session?.model ?? ctx.model ?? DEFAULT_MODEL;
  const effort = (session?.effort ?? ctx.effort ?? undefined) as TurnCtx["effort"];
  const workspace = db.getSessionRuntime(sessionId).workspace ?? process.cwd();
  // The session's scratchpad, made before the prompt names it (`scratch.ts`).
  const scratch = ensureScratchDir(sessionId);

  const turn = startTurn(db, sessionId, messageId, now);

  /** The turn's running usage total. Replaces the row's each checkpoint. */
  const usage: Usage = {
    inputTokens: 0,
    outputTokens: 0,
    reasoningTokens: 0,
    cacheReadTokens: 0,
    cacheWriteTokens: 0,
    costUsd: 0,
  };
  /** Last round's prompt size — a gauge, not a total. Drives the overflow check. */
  let contextTokens = 0;

  const parts: Part[] = [];
  /**
   * Set immediately before the message's final write. A late append (an `ask` that
   * settles as the turn dies) must not flip a finished message back to pending and
   * strand the UI on a turn that already ended.
   */
  let finalized = false;
  const append = (part: Part): void => {
    if (finalized) return;
    parts.push(part);
    db.updateMessage(messageId, parts, true);
    bus.publish({ type: "message.part", sessionId, data: { messageId, part } });
  };

  const turnCtx: TurnCtx = {
    ...ctx,
    sessionId,
    turnId: turn.id,
    messageId,
    workspace,
    model,
    effort,
    signal,
    depth: session?.kind === "subagent" || session?.kind === "workflow_agent" ? 1 : 0,
  };

  const program = deps.program ?? (deps.programFor ?? defaultProgramRunner)(turnCtx);

  // Null unless BOUGH_TRACE_DIR is set. Resolved here because this is the only
  // place that knows both ids, and written to below once the prompt is assembled.
  const trace = traceLabel(sessionId, turn.id);

  const llm: LlmClient = ctx.llm ?? clientFor(model, {
    trace,
    retry: {
      // The provider client's own backoff is invisible otherwise: the UI would show
      // a stalled stream for half a minute with no explanation. A retried round
      // re-streams from the top, so the client also has to drop its partial text.
      onRetry: ({ attempt, maxAttempts, error }) =>
        bus.publish({
          type: "message.retry",
          sessionId,
          data: {
            messageId,
            attempt,
            reason: `${shortReason(error)} — retry ${attempt}/${maxAttempts}`,
          },
        }),
    },
  });

  // The workspace note leads, unconditionally and for every kind. It is not a
  // capability grant, so it has no `granted` gate: a subagent, a workflow agent and
  // a schedule-fired root all edit a real checkout and all need to be told which one
  // (`prompt/assemble.ts`'s `workspaceNote` says why, including the cwd trap). It is
  // built HERE because `workspace` is resolved here, per session, and `deps.notes`
  // is fixed for the life of a starter — boot cannot supply a per-session fact.
  const ruleFiles = findProjectRules(workspace, boughHome());
  const rulesNote = projectRulesNote(ruleFiles, workspace);
  const projectRules = rulesNote === null ? [] : [rulesNote];
  // What went in is reported, from the SAME read the prompt was built from — a
  // line on the first turn, and one on any later turn where a file was added,
  // removed or edited. Drained onto the round's result below, never into the
  // prompt: the model already has the rules themselves, and this is for the human
  // who edited a file mid-session and got no sign it had landed.
  noteProjectRules(sessionId, ruleFiles, workspace);
  // Frozen per session even though this runs per turn — the memo in
  // `history/stats.ts` — because the volatile tier caches per session and a note
  // whose text drifts mid-session would bust it. Null for a project with no
  // command history yet, and then simply omitted.
  const tagsNote = tagsNoteFor(db, sessionId, workspace, ctx.now?.() ?? Date.now());
  const tagNotes = tagsNote === null ? [] : [tagsNote];
  const prompt = (deps.assemble ?? assemblePrompt)({
    kind: session?.kind ?? "root",
    granted: deps.granted ?? BASE_HOST_FNS,
    notes: [
      workspaceNote(workspace),
      scratchNote(scratch),
      ...tagNotes,
      // Read HERE, per turn, for the same reason the workspace note is built here:
      // it is a per-session fact boot cannot supply. Per turn rather than per
      // session so that editing AGENTS.md to correct a misbehaving model takes
      // effect on the next message instead of after a restart (`prompt/project.ts`).
      ...projectRules,
      ...(deps.notes ?? []),
    ],
  });

  // The section identities the raw trace cannot see: `LlmParams` carries the
  // assembled prefix as one opaque string, so which .md files went into it has to
  // be recorded from here (`llm/trace.ts`).
  if (trace) {
    writeManifest(trace, {
      sessionId,
      turnId: turn.id,
      model,
      effort,
      workspace,
      sections: prompt.shas,
      startedAt: Date.now(),
    });
  }

  /**
   * The thread as the provider sees it, minus the message we are writing. Built
   * once: a turn's history does not change under it, and rebuilding per round would
   * re-read every attachment from disk every round.
   */
  const messages: LlmMessage[] = buildThread(db.threadFor(sessionId), {
    exclude: messageId,
    // Reasoning replays only to the model that signed it (turn/replay.ts).
    model,
  });

  /**
   * Has the model written a CLOSING summary — text after the last tool call?
   *
   * Not `parts.some(isText)`. That asked "was there ever any text", which mid-turn
   * narration satisfies, and produced the exact failure described in the module
   * header: the more an agent explained itself as it worked, the more reliably its
   * turn ended on a raw tool result.
   */
  const saidSomething = (): boolean => {
    const lastTool = parts.findLastIndex((p) => p.type === "tool_call");
    return parts.slice(lastTool + 1).some((p) => p.type === "text" && p.text.trim() !== "");
  };

  let nudges = 0;
  let reportNudges = 0;
  /** The last-resort text-only round. Whatever it says is the turn's last word. */
  let forceText = false;
  let currentCallId = "";

  try {
    for (let round = 0;; round++) {
      if (signal.aborted) throw new InterruptedError();

      // Checked before the request, not after the rejection: a round that cannot
      // fit is a turn error naming the limit, and sending it anyway would spend the
      // tokens to be told so in provider dialect. Compaction is the user's move to
      // make (spec §5) — the harness never summarizes a conversation out from
      // under them and never auto-compacts mid-turn.
      const limit = usableContextLimit(model, maxTokens);
      if (limit !== null && contextTokens > limit) {
        throw new ContextOverflowError(
          `this turn no longer fits in ${model}'s context window: the last round's prompt was ` +
            `${contextTokens.toLocaleString()} tokens against a usable limit of ` +
            `${limit.toLocaleString()} (${
              contextWindowFor(model)?.toLocaleString()
            } window minus the ${maxTokens.toLocaleString()}-token output reservation). ` +
            `Compact or fork this session to continue — nothing was summarized automatically.`,
        );
      }

      const result = await runRound(
        llm,
        {
          model,
          system: prompt.system,
          systemVolatile: prompt.systemVolatile,
          maxTokens,
          messages,
          tools: TOOLS,
          ...(forceText ? { toolChoice: "none" as const } : {}),
          ...(effort ? { effort } : {}),
        },
        (delta) => bus.publish({ type: "message.delta", sessionId, data: { messageId, delta } }),
        signal,
        { bus, sessionId, messageId, deps },
      );

      if (result.usage) {
        contextTokens = result.usage.inputTokens +
          (result.usage.cacheReadTokens ?? 0) + (result.usage.cacheWriteTokens ?? 0);
        foldUsage(usage, result.usage);
        db.addSessionUsage(sessionId, result.usage, now());
        const refreshed = db.getSession(sessionId);
        if (refreshed) bus.publish({ type: "session.updated", sessionId, data: refreshed });
      }

      // Persist what the round said, and build the in-memory echo. These diverge in
      // exactly two places, and both are deliberate: `stop` is loop control and is
      // never persisted or replayed, and reasoning goes into the echo WITH its
      // provider meta but is persisted without it (llm/stream.ts).
      let stopRequested = false;
      const assistant: LlmContentBlock[] = [];
      for (const block of result.content) {
        if (block.type === "text") {
          let text = block.text;
          if (TRAILING_STOP_SENTINEL.test(text)) {
            stopRequested = true;
            text = text.replace(TRAILING_STOP_SENTINEL, "");
          }
          if (text) {
            append({ type: "text", text });
            assistant.push({ type: "text", text });
          }
        } else if (block.type === "reasoning") {
          // Persisted WITH its provider payload and the model that signed it, so
          // the next turn can replay it (turn/replay.ts, invariant 1). A block
          // with no displayable text still persists when it is signed — that is a
          // redacted thinking block, and it has to go back whole or not at all.
          if (block.text.trim() || block.meta !== undefined) {
            append({ type: "reasoning", text: block.text, meta: block.meta, model });
          }
          assistant.push(block);
        } else if (block.type === "tool_use") {
          if (block.name === STOP) {
            stopRequested = true;
            continue;
          }
          append({ type: "tool_call", id: block.id, name: block.name, input: block.input });
          assistant.push(block);
        }
      }
      if (assistant.length > 0) messages.push({ role: "assistant", content: assistant });
      checkpoint(db, turn.id, `round:${round + 1}`, usage);

      // The forced round had tools forbidden, so whatever it said is the ending.
      if (forceText) break;

      const toolUses = result.content.filter(
        (b): b is Extract<LlmBlock, { type: "tool_use" }> =>
          b.type === "tool_use" && b.name !== STOP,
      );

      if (toolUses.length > 0) {
        const toolResults: LlmContentBlock[] = [];
        for (const call of toolUses) {
          // Never start a tool once interrupted: stop before the side effect, not
          // after it.
          if (signal.aborted) throw new InterruptedError();
          currentCallId = call.id;
          const executed = await executeTool(call, {
            program,
            signal,
            onLog: (line) =>
              bus.publish({
                type: "tool.log",
                sessionId,
                data: { messageId, callId: currentCallId, line },
              }),
          });
          append({
            type: "tool_result",
            callId: call.id,
            output: executed.output,
            isError: executed.isError,
            ...(executed.interrupted ? { interrupted: true } : {}),
          });
          toolResults.push({
            type: "tool_result",
            toolUseId: call.id,
            content: executed.output,
            isError: executed.isError,
          });
          checkpoint(db, turn.id, `tool:${call.name}`, usage);
        }

        // The report nudge rides INSIDE the tool_result message rather than
        // arriving as a separate user turn: a model answers an inline nudge with
        // text far more reliably than a standalone one, which tends to come back as
        // empty thinking plus another stop.
        if (stopRequested && !saidSomething()) {
          if (reportNudges < 1) {
            reportNudges++;
            toolResults.push({ type: "text", text: REPORT_NUDGE });
            messages.push({ role: "user", content: toolResults });
            continue;
          }
          messages.push({ role: "user", content: toolResults });
          forceText = true;
          continue;
        }
        messages.push({ role: "user", content: toolResults });
        if (stopRequested) break;
        continue;
      }

      if (stopRequested) {
        if (saidSomething()) break;
        if (reportNudges < 1) {
          reportNudges++;
          messages.push({ role: "user", content: [{ type: "text", text: REPORT_NUDGE }] });
          continue;
        }
        // The nudge came back mute — typically empty thinking plus another stop.
        // Ending a prompt on a thinking-only assistant message is itself invalid,
        // so drop that tail before forcing the text round.
        const tail = messages.at(-1);
        if (tail?.role === "assistant" && tail.content.every((b) => b.type === "reasoning")) {
          messages.pop();
        }
        forceText = true;
        continue;
      }

      // Trailed off with no stop and no tools. Nudge — in memory only, never
      // persisted — with a cap, so a model that cannot call `stop` still terminates.
      if (nudges >= MAX_STOP_NUDGES) break;
      nudges++;
      messages.push({ role: "user", content: [{ type: "text", text: STOP_NUDGE }] });
    }

    finalized = true;
    db.updateMessage(messageId, parts, false);
    indexQuietly(db, sessionId, messageId);
    finishTurn(db, turn.id, "done", { usage, step: "done" });
    bus.publish({ type: "message.finished", sessionId, data: { messageId } });
    bus.publish({
      type: "turn.finished",
      sessionId,
      data: { turnId: turn.id, sessionId, status: "done" },
    });
    // Delegation outcome for the tree view. Not an acceptance gate — it records
    // whether the TURN errored, nothing about whether the work was any good.
    if (session?.kind === "subagent" || session?.kind === "workflow_agent") {
      db.setSessionOutcome(sessionId, true);
    }
    return { turnId: turn.id, messageId, status: "done", usage };
  } catch (err) {
    finalized = true;
    const interrupted = err instanceof InterruptedError || signal.aborted ||
      errName(err) === "APIUserAbortError" || errName(err) === "AbortError";

    const note: Part = interrupted
      ? { type: "text", text: stoppedNote(sessionId, deps) }
      : { type: "text", text: `⚠︎ Turn failed: ${friendlyTurnError(err, model)}` };
    parts.push(note);
    db.updateMessage(messageId, parts, false);
    indexQuietly(db, sessionId, messageId);

    const status = interrupted ? "interrupted" : "error";
    const error = interrupted ? undefined : friendlyTurnError(err, model);
    // The UI must never know more than the server log does.
    if (!interrupted) {
      (deps.reportError ?? ((e, s) => console.error(`turn failed [${s}]:`, e)))(err, sessionId);
    }

    finishTurn(db, turn.id, status, { usage, error: error ?? null, step: "ended" });
    bus.publish({ type: "message.part", sessionId, data: { messageId, part: note } });
    bus.publish({ type: "message.finished", sessionId, data: { messageId } });
    bus.publish({
      type: "turn.finished",
      sessionId,
      data: { turnId: turn.id, sessionId, status, ...(error ? { error } : {}) },
    });
    if (session?.kind === "subagent" || session?.kind === "workflow_agent") {
      db.setSessionOutcome(sessionId, false);
    }
    return { turnId: turn.id, messageId, status, ...(error ? { error } : {}), usage };
  }
}

// ---------------------------------------------------------------------------
// One round
// ---------------------------------------------------------------------------

/**
 * One provider round, with the turn-level retry ring around it.
 *
 * The ring is above whatever the client already does internally (`withRetries`),
 * and it exists for two failures that layer cannot fix. A **truncated tool call**
 * is the important one: `llm/stream.ts` refuses to invent `{}` for a call whose
 * arguments were cut off, because executing it would run the wrong program against
 * the user's checkout. Re-streaming is the only correct answer, and it is
 * immediate — nothing is broken, a frame was lost. The other is a **provider
 * outage** long enough to outlive the client's ~30s of backoff; a turn with all its
 * work intact should not die because the network flapped for a minute.
 *
 * Every re-attempt emits `message.retry`, because a retried round re-streams from
 * the top and a client holding partial text must drop it (spec §5 Retry).
 */
async function runRound(
  llm: LlmClient,
  params: Parameters<LlmClient["run"]>[0],
  onText: (delta: string) => void,
  signal: AbortSignal,
  wiring: {
    bus: AppCtx["bus"];
    sessionId: string;
    messageId: string;
    deps: TurnDeps;
  },
): Promise<Awaited<ReturnType<LlmClient["run"]>>> {
  const { bus, sessionId, messageId, deps } = wiring;
  for (let attempt = 1;; attempt++) {
    try {
      return await llm.run(params, onText, signal);
    } catch (err) {
      const decision = classifyRoundFailure(err, attempt, {
        maxRetries: deps.maxRoundRetries,
        outageDelayMs: deps.outageDelayMs,
      });
      if (!decision.retry || signal.aborted) throw err;
      bus.publish({
        type: "message.retry",
        sessionId,
        data: { messageId, attempt, reason: decision.reason },
      });
      await abortableDelay(decision.delayMs, signal);
    }
  }
}

// ---------------------------------------------------------------------------
// Tools
// ---------------------------------------------------------------------------

interface ExecutedTool {
  output: string;
  isError: boolean;
  interrupted?: boolean;
}

/**
 * What to say when the model calls a tool that does not exist.
 *
 * The common case is not a hallucination, and answering it as one wasted rounds.
 * `view`, `bash`, `patch` and the rest are REAL — they are host functions, called
 * from inside the program — and a model under pressure reaches for them at the
 * tool layer, which is exactly where the names look like they should live. A
 * haiku run did this twice in one turn, got "unknown tool: view", concluded the
 * capability was missing, and rewrote the whole approach around `bash`.
 *
 * So when the name IS a host function, say where it lives and show the call. The
 * plain unknown-name case keeps the old message, which is right for a name that
 * really is invented.
 */
function unknownToolMessage(name: string): string {
  const tools = `The only tools are \`${RUN_STEPS}\` and \`${STOP}\`.`;
  if (!(HOST_FN_NAMES as readonly string[]).includes(name)) {
    return `unknown tool: ${name}. ${tools}`;
  }
  return `unknown tool: ${name} — but \`${name}\` IS available: it is a host ` +
    `function, already in scope inside the program you pass to \`${RUN_STEPS}\`, ` +
    `not a tool of its own. ${tools} Call it as code, e.g. ` +
    `\`const text = await ${name}(...)\`.`;
}

/**
 * Run one tool call. **This never throws.** Every failure — an unknown name, a
 * malformed input, a program that threw, a program the user stopped — is an
 * ordinary result the next round can act on, and a thrown one would end the turn
 * instead of letting the model recover.
 */
async function executeTool(
  call: Extract<LlmBlock, { type: "tool_use" }>,
  wiring: { program: ProgramRunner; signal: AbortSignal; onLog: (line: string) => void },
): Promise<ExecutedTool> {
  if (call.name !== RUN_STEPS) {
    return { output: unknownToolMessage(call.name), isError: true };
  }

  const parsed = RunStepsInput.safeParse(call.input);
  if (!parsed.success) {
    return {
      output: `invalid input for ${RUN_STEPS}: ${
        parsed.error.issues.map((i) => `${i.path.join(".") || "(root)"}: ${i.message}`).join("; ")
      }. It takes {code: string, done?: boolean}.`,
      isError: true,
    };
  }

  const result = await wiring.program({
    code: parsed.data.code,
    callId: call.id,
    signal: wiring.signal,
    onLog: wiring.onLog,
  });

  return programOutput(result);
}

/**
 * A program's result as the model sees it: the console output it printed, plus the
 * error that ended it when one did.
 *
 * Partial output leads even on a failure. A program that printed twenty lines and
 * then threw has told the model most of what it needs; leading with the error and
 * dropping the lines would throw the round away.
 */
export function programOutput(result: ProgramResult): ExecutedTool {
  const body = result.logs.join("\n");
  if (result.ok) {
    return {
      output: body ||
        "(the program ran and printed nothing — console.log what you need to see)",
      isError: false,
    };
  }
  const error = result.error ?? "the program failed with no message";
  return {
    output: body ? `${body}\n\n${error}` : error,
    isError: true,
    ...(result.interrupted ? { interrupted: true } : {}),
  };
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/**
 * The usable prompt budget: the catalog window minus the reservation every round
 * makes for output. `null` when the model is not in the catalog — an unknown window
 * must not become a fabricated limit that fails turns that would have worked.
 */
export function usableContextLimit(model: string, maxTokens = MAX_TOKENS): number | null {
  const window = contextWindowFor(model);
  return window === null ? null : window - maxTokens;
}

/** Sum a round into the turn's running total. */
function foldUsage(total: Usage, round: Usage): void {
  total.inputTokens += round.inputTokens;
  total.outputTokens += round.outputTokens;
  total.reasoningTokens = (total.reasoningTokens ?? 0) + (round.reasoningTokens ?? 0);
  total.cacheReadTokens = (total.cacheReadTokens ?? 0) + (round.cacheReadTokens ?? 0);
  total.cacheWriteTokens = (total.cacheWriteTokens ?? 0) + (round.cacheWriteTokens ?? 0);
  total.costUsd = (total.costUsd ?? 0) + (round.costUsd ?? 0);
}

/**
 * The interrupt note, naming the background shells that survive it.
 *
 * They are detached on purpose (spec §9) — a `bashBg` or an auto-backgrounded build
 * outlives the turn — so a stop that silently leaves them running looks like a stop
 * that did not work. Absent seam = say nothing, rather than claim there were none.
 */
function stoppedNote(sessionId: string, deps: TurnDeps): string {
  const survivors = deps.survivingJobs?.(sessionId) ?? [];
  if (survivors.length === 0) return STOPPED_NOTE;
  return `${STOPPED_NOTE}\n${survivors.join(", ")} still running — ${
    survivors.length === 1 ? "it survives" : "they survive"
  } the interrupt.`;
}

/**
 * Keyword search is maintained on insert (plan T8.9). Failing to index is a
 * degraded search, not a failed turn, so it never propagates.
 */
function indexQuietly(db: Db, _sessionId: string, messageId: string): void {
  try {
    const stored = db.getMessage(messageId);
    if (stored) db.indexMessage(stored);
  } catch (err) {
    console.error(`failed to index message ${messageId}:`, err);
  }
}

/**
 * Provider failures in plain language, with the fix at hand.
 *
 * Error text is a product surface (spec §6): what a failure says determines whether
 * the user fixes it or files a bug. A missing key must not read as a model outage,
 * and a provider's multi-line escaped-JSON 400 body must not be pasted into a
 * transcript card — it is folded to one line, because these also travel upward as a
 * subagent's report.
 */
export function friendlyTurnError(err: unknown, model: string): string {
  const msg = (err as Error)?.message ?? String(err);
  const key = providerFor(model);
  const provider = {
    openai: "OpenAI",
    openrouter: "OpenRouter",
    cloudflare: "Cloudflare",
    anthropic: "Anthropic",
  }[key];
  // THE ENV VAR, BECAUSE THERE IS NO KEYS PANEL. Both messages below used to send the
  // reader to one; `ModelPicker.tsx` says in its header that the API-keys section was
  // never ported, since keys are environment variables in this tree and there is no
  // `/config/keys` route to write to. So the very first screen a new user with a bad key
  // saw named a surface that does not exist — the same defect as a legend naming a key
  // that is not bound, on the one screen where the reader has nothing else to go on.
  const envVar = API_KEY_ENV[key];

  // Cloudflare is account-scoped: a valid key with no account id still cannot reach
  // an endpoint, and the generic "no key" line would send the reader to the wrong var.
  if (/CLOUDFLARE_ACCOUNT_ID is not set/.test(msg)) {
    return `No Cloudflare account id set — export CLOUDFLARE_ACCOUNT_ID and restart the bough server.`;
  }

  if (/Could not resolve authentication method|apiKey or authToken|API_KEY is not set/i.test(msg)) {
    return `No ${provider} API key set — export ${envVar} and restart the bough server.`;
  }
  if (/invalid x-api-key|authentication_error|Incorrect API key/i.test(msg)) {
    return `${provider} rejected the key in ${envVar} — fix it and restart the bough server.`;
  }
  const http = /:\s*(\d{3})\s+([\s\S]+)$/.exec(msg);
  if (http) {
    const status = Number(http[1]);
    const body = http[2];
    if (/tool_calls|tool_call_id|must be followed by tool/i.test(body)) {
      return `${provider} rejected the tool-call formatting (${status}); a repaired retry usually clears it.`;
    }
    if (status >= 400) {
      return `${provider} error ${status}: ${shortReason(new Error(body))}`;
    }
  }
  return msg;
}
