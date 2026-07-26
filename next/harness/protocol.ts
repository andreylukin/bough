/**
 * The worker bridge contract: the postMessage message types, and the single
 * canonical list of host-function names.
 *
 * The invariant: **host names are declared here exactly once, and both sides
 * import them.** The host side dispatches on them, the worker side binds them as
 * program parameters, and the pre-flight syntax check uses the same list to reject
 * a program that shadows one (`let bash = 1`). Three copies of that list is three
 * chances for the pre-flight check and the worker to disagree about what is legal
 * — which shows up as a program that passes validation and then fails to compile
 * inside the worker, with the model left to guess why. A test pins the two sides
 * equal (plan T3.1).
 *
 * The list is CLOSED. Every name is declared now, including ones whose
 * implementations land in M6 (`ask`, `state`, `schedule`, `image`, `fetch`,
 * `artifact`, `mcp`, `mcpStatus`, `lsp`, `workflow`). Later tasks implement against
 * an existing name; none adds one.
 *
 * The wire is string-only, both directions. A host function that logically takes
 * or returns an object serializes it (`agent(task, JSON.stringify(opts))` →
 * `JSON.parse(result)`), and the worker side re-inflates it before the program
 * ever sees it. Two reasons: it keeps the protocol trivially inspectable, and it
 * makes structured-clone failures on exotic values impossible. The one exception
 * to "everything is JSON" is `view`/`patch`, where the text IS the payload.
 *
 * Declaring a name here does not grant it. A host function exists in a program
 * only when the turn bridges it AND the system prompt documents it — the model
 * never guesses at capabilities (spec §6).
 */

// ---- host function names ----------------------------------------------------

/**
 * Every bridged host function, in the order they are bound as program parameters.
 *
 * Absent by design (spec §17): `read` and `edit` (one editing idiom — `view`,
 * `patch`, `write`), `extract` (no output digestion; oversized output is truncated
 * deterministically), and `recall` (no semantic search; cross-session search is
 * keyword FTS behind an HTTP route, not a program verb).
 */
export const HOST_FN_NAMES = [
  // shell
  "bash",
  "sh",
  "bashBg",
  "bashOutput",
  "bashWait",
  "bashKill",
  // files — the one editing idiom
  "view",
  "patch",
  "write",
  // delegation
  "agent",
  "spawn",
  "join",
  "adopt",
  // orchestration
  "workflow",
  // session verbs
  "ask",
  "state",
  "schedule",
  "image",
  "fetch",
  "artifact",
  "mcp",
  "mcpStatus",
  "lsp",
] as const;

export type HostFnName = (typeof HOST_FN_NAMES)[number];

/**
 * The program's parameter names: every host function plus `console`.
 *
 * `console` is bound as a parameter rather than left as the worker's global
 * because it is not the worker's console — every line both streams to the UI as a
 * `tool.log` event AND batches into the tool result the model receives (spec §5).
 * It is in this list, and not in `HOST_FN_NAMES`, because it is a plain object
 * built worker-side, not a bridged call.
 */
export const PROGRAM_PARAMS = [...HOST_FN_NAMES, "console"] as const;

export type ProgramParam = (typeof PROGRAM_PARAMS)[number];

/**
 * The verbs each method-object host function fans out to. One bridged function
 * carries all of them (`state("get", argsJson)`), and the worker side rebuilds the
 * `state.get(...)` shape the program calls. Declared here so the host dispatcher
 * and the worker's method-object construction cannot drift.
 */
export const HOST_FN_VERBS = {
  state: ["get", "set", "list", "delete"],
  schedule: ["list", "add", "enable", "disable", "remove"],
  workflow: ["start", "rerun", "stop", "pause", "resume", "status", "list"],
  lsp: ["find", "show", "def", "refs", "impls", "calls", "overview", "rename"],
} as const satisfies Record<string, readonly string[]>;

// ---- program worker: main → worker ------------------------------------------

/** Start the program. Sent once, after the worker is constructed. */
export interface RunMessage {
  type: "run";
  code: string;
}

/**
 * The result of one bridged call. `ok: false` rejects the program's promise with
 * `value` as the message — host-function failures are ordinary catchable
 * exceptions inside the program, never a killed worker.
 */
export interface HostResultMessage {
  type: "host_result";
  id: number;
  ok: boolean;
  value: string;
}

/**
 * Stop. The worker kills the processes it spawned and acks with `aborted`; only
 * then does the host call `terminate()`. Reverse order orphans processes — a
 * program spawns children of the SERVER process, and `terminate()` does not touch
 * them (plan §6.3).
 */
export interface AbortMessage {
  type: "abort";
}

export type ToProgramWorker = RunMessage | HostResultMessage | AbortMessage;

// ---- program worker: worker → main ------------------------------------------

/** One bridged call. `args` are strings by convention — see the module header. */
export interface HostCallMessage {
  type: "host";
  id: number;
  fn: HostFnName;
  args: unknown[];
}

/** One `console.*` line, as printed. Streamed live AND kept in the batch. */
export interface LogMessage {
  type: "log";
  line: string;
}

/** Children swept; safe to terminate. The host waits briefly for this. */
export interface AbortedMessage {
  type: "aborted";
}

export interface DoneMessage {
  type: "done";
  logs: string[];
}

export interface ProgramErrorMessage {
  type: "error";
  message: string;
  logs: string[];
}

export type FromProgramWorker =
  | HostCallMessage
  | LogMessage
  | AbortedMessage
  | DoneMessage
  | ProgramErrorMessage;

/** What `runProgram` resolves to. `logs` is what the model receives as the tool result. */
export interface ProgramResult {
  ok: boolean;
  /** `console.*` output, in order. Partial output survives an interrupt. */
  logs: string[];
  /**
   * Present when `ok` is false: the thrown error with its stack, the timeout
   * notice, or the interrupt notice. Timeout and interrupt must be
   * distinguishable, and must say what partial work survived (spec §6).
   */
  error?: string;
  /** True when the program was stopped by a user interrupt rather than failing. */
  interrupted?: boolean;
}

// ---- workflow worker --------------------------------------------------------

/**
 * The workflow worker runs with `permissions: "none"` and bridges only these
 * three. `parallel` and `pipeline` are NOT here — they are pure combinators over
 * `agent`, implemented worker-side, so they never cross the wire (spec §8).
 */
export const WORKFLOW_HOST_FN_NAMES = ["agent", "phase", "log"] as const;

export type WorkflowHostFnName = (typeof WORKFLOW_HOST_FN_NAMES)[number];

/** The script's parameter names: the three verbs plus its input value. */
export const WORKFLOW_SCRIPT_PARAMS = [...WORKFLOW_HOST_FN_NAMES, "args"] as const;

/** Start the script. `argsJson` is the run's input, handed over verbatim as `args`. */
export interface WorkflowRunMessage {
  type: "run";
  code: string;
  argsJson: string;
}

export type ToWorkflowWorker = WorkflowRunMessage | HostResultMessage | AbortMessage;

export interface WorkflowHostCallMessage {
  type: "host";
  id: number;
  fn: WorkflowHostFnName;
  args: unknown[];
}

/** The script returned. `resultJson` is its return value. */
export interface WorkflowDoneMessage {
  type: "done";
  resultJson: string;
}

export type FromWorkflowWorker =
  | WorkflowHostCallMessage
  | AbortedMessage
  | WorkflowDoneMessage
  | ProgramErrorMessage;
