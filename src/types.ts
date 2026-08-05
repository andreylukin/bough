/**
 * The injection seams. Everything a module needs from the outside world arrives
 * through one of the interfaces here, and nothing reaches for a global.
 *
 * The invariant: **the database, the clock and the LLM are parameters, not
 * imports.** That is what makes the whole tree testable offline — the turn runner
 * drives a scripted fake `LlmClient` and never touches the network; handlers run
 * against an in-memory database with no socket bound; pure functions take `now`
 * rather than calling `Date.now()`. A module that imports a concrete client
 * directly has broken this, and its tests will need a key.
 *
 * Second invariant, and the reason `Db` and `Bus` are declared here rather than in
 * `db/db.ts` and `bus.ts`: `hostfn/` must never import from `server/`. Host
 * functions take a `TurnCtx` and nothing else, so they can be unit-tested with a
 * fabricated context and no server in sight (plan §3, module boundary rule).
 *
 * `Db` and `Bus` below are PORTS. `db/db.ts` (T0.2) and `bus.ts` (T0.3) export
 * concrete implementations that satisfy them and may expose a wider surface; a
 * consumer that only needs the port depends on this file.
 */
import type {
  AskQuestion,
  Message,
  Part,
  Schedule,
  Session,
  Turn,
  TurnStatus,
  Usage,
  WorkflowAgent,
  WorkflowRun,
} from "./schema/parts.ts";
import type { BoughEvent, EventInput } from "./schema/events.ts";
import type { HostFnName } from "./harness/protocol.ts";

// ---- the event bus ----------------------------------------------------------

/**
 * In-process fan-out to the SSE subscribers. Memory-only and
 * persistence-agnostic: the caller persists first, then publishes.
 *
 * A listener that throws must not break fan-out to the others (plan §6.6) — one
 * wedged SSE connection cannot be allowed to silence every other client.
 */
export interface Bus {
  /** Stamps `seq`/`ts`, delivers synchronously, returns the stamped event. */
  publish(event: EventInput): BoughEvent;
  /** Returns an unsubscribe thunk. */
  subscribe(listener: (event: BoughEvent) => void): () => void;
  /** Live subscriber count — the leak check in the SSE tests reads it. */
  readonly size: number;
}

// ---- the database -----------------------------------------------------------

/** A session's non-wire runtime facts, kept off the `Session` shape the UI sees. */
export interface SessionRuntime {
  /** null = fall back to the process default workspace. */
  workspace: string | null;
  /** The git sha the session started from; null for a non-git workspace. */
  base: string | null;
}

/** Aggregated token/cost totals for the status bar. */
export interface UsageTotals {
  inputTokens: number;
  outputTokens: number;
  reasoningTokens: number;
  cacheReadTokens: number;
  cacheWriteTokens: number;
  costUsd: number;
}

/** One keyword-search hit (SQLite FTS over transcripts — no embeddings). */
export interface SearchHit {
  messageId: string;
  sessionId: string;
  /** The matched excerpt, with the FTS snippet markers already resolved. */
  snippet: string;
  createdAt: number;
}

/**
 * One finished shell command entering the tag-history memory (`history/record.ts`
 * builds these; `db/schema.sql`'s command_history group stores them).
 */
export interface CommandRecord {
  sessionId: string;
  ts: number;
  /** Git remote origin URL, else the workspace root path — the scope key. */
  repo: string;
  cmd: string;
  /** Normalized colon-separated tags as one string; "" when the verb carries none. */
  tags: string;
  /** `tags` split and deduped, for the junction table. */
  tagList: string[];
  /** Workspace-relative directories the command was about (`extractDirs`). */
  dirs: string[];
  /** null = unknown (still running when the turn moved on). */
  exitCode: number | null;
  durationMs: number | null;
  /** First ~2k chars of what it printed, as the program saw it. "" = silent. */
  outputHead: string;
  /** The spill file holding the full output, when there was one. */
  spillPath: string | null;
  source: "live" | "backfill";
  /** The supervisor message whose program ran it; absent when there is none. */
  messageId?: string | null;
}

/** What the memory already knows about one command failing here (`history/echo.ts`). */
export interface PriorFailures {
  /** Failing runs of this exact command in this repo, inside the window. */
  count: number;
  /** How many of those were this session — the loop signal, as opposed to history. */
  inSession: number;
  lastTs: number;
  /** The last failure's exit code, and the first ~2k chars it printed. */
  exitCode: number | null;
  outputHead: string;
}

/** One (tag, outcome) observation, the unit the popularity stats aggregate. */
/** One day of the tag memory, as `bough tags stats` reports it. */
export interface TagDiversityDay {
  /** `YYYY-MM-DD`, local time — the question is about a day someone worked. */
  day: string;
  sessions: number;
  commands: number;
  /** Commands that carried at least one tag. The rest are unfindable by tag. */
  tagged: number;
  /** The vocabulary: distinct COINED tags that day, references excluded. */
  distinctTags: number;
  /**
   * Distinct references that day (`linear.*`, `pr.*`, …), counted apart.
   *
   * Kept out of `distinctTags` because they would inflate it by one per ticket and
   * make "is the model naming more things" unreadable — and reported anyway, because
   * "how many work items did this day touch" is worth its own number.
   */
  distinctRefs: number;
  /** Total tag applications, so `distinctTags / tagUses` reads as repetition. */
  tagUses: number;
  /**
   * Coined tags used EXACTLY ONCE that day — the vocabulary that did not converge.
   *
   * The measurement hygiene is judged by: `singletons / distinctTags` is the share
   * of a day's new words that named something once and were never reached for
   * again. It will never be zero (vocabulary growth is a power law), so the number
   * to watch is the ratio moving on a date, not its absolute value.
   */
  singletons: number;
}

/** One recalled command, as `bough tags show` prints it. */
export interface TaggedCommand {
  ts: number;
  repo: string;
  cmd: string;
  tags: string;
  exitCode: number | null;
  durationMs: number | null;
  sessionId: string;
  /** The message whose program ran it — null for a row written before the link. */
  messageId: string | null;
}

export interface CommandTagRow {
  tag: string;
  ts: number;
  exitCode: number | null;
}

/**
 * Typed persistence. No raw SQL exists outside `db/`, so every read and write in
 * the system goes through a method here.
 *
 * Ordering contract, which several callers depend on and none may re-derive:
 *   - `messagesFor` orders by `(created_at, rowid)`.
 *   - `threadFor` is ancestors root→parent, then the session's own — which is what
 *     makes a fork parented at the target's parent inherit shared ancestors for
 *     free (spec §14).
 *   - `ancestorChain` walks to the lineage root, for root-scoped `session_state`.
 */
export interface Db {
  // sessions
  createSession(session: Session): Session;
  getSession(id: string): Session | undefined;
  /** Runtime facts (workspace, base) that are not on the wire `Session`. */
  getSessionRuntime(id: string): SessionRuntime;
  /**
   * Every session, newest first. Visibility is the CALLER's derivation: listings
   * exclude `subagent` and `workflow_agent`, which surface via `sessionsByOrigin`.
   */
  listSessions(): Session[];
  /** The branches that collapsed under `originId` — the drill-in query. */
  sessionsByOrigin(originId: string): Session[];
  /** Root→self, inclusive. The last element is the session itself. */
  ancestorChain(id: string): Session[];
  setSessionTitle(id: string, title: string): void;
  setSessionWorkspace(id: string, workspace: string): void;
  setSessionBase(id: string, base: string): void;
  setSessionDraft(id: string, draft: string | null): void;
  setSessionModel(id: string, model: string | null): void;
  setSessionEffort(id: string, effort: string | null): void;
  /** Records whether the delegated TURN errored. Not an acceptance gate (spec §17). */
  setSessionOutcome(id: string, ok: boolean): void;
  /** Adds one round's usage to the session totals and updates the context meter. */
  addSessionUsage(id: string, usage: Usage, at: number): void;
  sessionUsage(id: string): UsageTotals;
  /** The session plus every branch collapsed under it — the tree's cost total. */
  treeUsage(id: string): UsageTotals;
  /** Sessions with a `running` turn. One turn per session (spec §5). */
  busySessionIds(): Set<string>;

  // messages
  createMessage(message: Message): Message;
  getMessage(id: string): Message | undefined;
  /** The session's OWN messages, ordered `(created_at, rowid)`. */
  messagesFor(sessionId: string): Message[];
  /** Ancestors root→parent, then own. The full replayable thread. */
  threadFor(sessionId: string): Message[];
  updateMessage(id: string, parts: Part[], pending: boolean): void;
  /**
   * Delete a message and every message after it in its session, returning the ids
   * removed. The take-back's backend and the only destructive write on the thread
   * — `history/unsend.ts` owns when it is allowed to be called.
   */
  deleteMessagesFrom(sessionId: string, messageId: string): string[];

  // turns
  createTurn(turn: Turn): Turn;
  getTurn(id: string): Turn | undefined;
  turnForMessage(messageId: string): Turn | undefined;
  turnsForSession(sessionId: string): Turn[];
  /** Boot recovery reads `running` here and orphans every row it finds. */
  turnsByStatus(status: TurnStatus): Turn[];
  latestTurnStatuses(): Map<string, TurnStatus>;
  updateTurn(
    id: string,
    patch: { status?: TurnStatus; step?: string; error?: string | null; usage?: Usage },
  ): void;

  // durable KV, scoped to the lineage root
  getState(rootId: string, key: string): string | undefined;
  setState(rootId: string, key: string, value: string, now: number): void;
  listState(rootId: string): { key: string; bytes: number; updatedAt: number }[];
  deleteState(rootId: string, key: string): boolean;

  // schedules
  createSchedule(schedule: Schedule): Schedule;
  getSchedule(id: string): Schedule | undefined;
  listSchedules(): Schedule[];
  /** Enabled schedules whose `next_run_at` has passed. */
  dueSchedules(now: number): Schedule[];
  updateSchedule(schedule: Schedule): void;
  /** Advances `next_run_at` FROM NOW, never from the stale value (plan §6.8). */
  markScheduleRun(id: string, lastRunAt: number, nextRunAt: number): void;
  deleteSchedule(id: string): void;

  // workflows
  createWorkflow(run: WorkflowRun): WorkflowRun;
  getWorkflow(id: string): WorkflowRun | undefined;
  listWorkflows(sessionId?: string): WorkflowRun[];
  /** Runs still `running`/`paused` at boot — orphaned like turns. */
  unfinishedWorkflows(): WorkflowRun[];
  updateWorkflow(id: string, patch: Partial<WorkflowRun>): void;
  createWorkflowAgent(agent: WorkflowAgent): WorkflowAgent;
  updateWorkflowAgent(id: string, patch: Partial<WorkflowAgent>): void;
  listWorkflowAgents(runId: string): WorkflowAgent[];
  /** Journal lookup on rerun: the source run's row for a call key, if any. */
  findWorkflowAgent(runId: string, key: string): WorkflowAgent | undefined;

  // command-history memory
  /** Append one finished command + its tag/dir junction rows + FTS row. */
  recordCommand(record: CommandRecord): void;
  /**
   * The (tag, ts, exit_code) observations for popularity aggregation, scoped to a
   * repo, optionally to commands attributed to `dir` or its descendants.
   */
  commandTagRows(repo: string, opts?: { dir?: string; sinceTs?: number }): CommandTagRow[];
  /** Distinct repos in the memory, and how many of them use each tag. */
  tagSpread(sinceTs?: number): { repos: number; byTag: Map<string, number> };
  /** Per-day tag coverage and vocabulary size — the `bough tags stats` measurement. */
  tagDiversityByDay(sinceTs: number, repo?: string): TagDiversityDay[];
  /** Commands recorded under one tag, newest first. */
  commandsForTag(tag: string, opts?: { repo?: string; limit?: number }): TaggedCommand[];
  /** This repo's coined tags (references excluded) and their use counts. */
  repoTagCounts(repo: string, sinceTs: number): Map<string, number>;
  /** How this exact command has failed in this repo since `sinceTs`, or null. */
  priorFailures(
    repo: string,
    cmd: string,
    sinceTs: number,
    sessionId: string,
  ): PriorFailures | null;
  /** The newest successful command in this repo starting with a LIKE `prefix`. */
  lastSuccessLike(repo: string, prefix: string, notCmd: string, sinceTs: number): string | null;
  /** The `run_steps` program a supervisor message ran, or null. */
  programForMessage(messageId: string): string | null;

  // keyword search
  /** Idempotent: re-indexing a message replaces its rows. */
  indexMessage(message: Message): void;
  searchMessages(query: string, opts?: { sessionId?: string; limit?: number }): SearchHit[];
  /** Must produce results identical to incremental indexing (plan T8.9). */
  rebuildSearchIndex(): void;

  close(): void;
}

// ---- the LLM boundary -------------------------------------------------------

/** Thinking depth. Not every model accepts one; an unsupported value is a turn error. */
export type Effort = "low" | "medium" | "high" | "xhigh" | "max";

/** A content block as the model produces it. */
export type LlmBlock =
  | { type: "text"; text: string }
  /**
   * `meta` is an opaque provider payload replayed VERBATIM — within a turn,
   * because some providers require a tool round's signed thinking to precede its
   * tool_use on the next round, and ACROSS turns for as long as the same model is
   * being asked. It is never inspected outside the provider's own mapper in
   * `llm/client.ts`: that is what keeps the replay rule provider-agnostic, since
   * each mapper already knows what its own payload is worth.
   */
  | { type: "reasoning"; text: string; meta?: unknown }
  | { type: "tool_use"; id: string; name: string; input: unknown };

/** A block as it appears in a request message. */
export type LlmContentBlock =
  | LlmBlock
  | { type: "tool_result"; toolUseId: string; content: string; isError: boolean }
  /** Base64-encoded at assembly time; each provider maps it to its native shape. */
  | { type: "image"; data: string; mediaType: string; name: string };

export interface LlmMessage {
  role: "user" | "assistant";
  content: LlmContentBlock[];
}

/** The model sees exactly two of these: `run_steps` and `stop` (spec §6). */
export interface LlmToolDef {
  name: string;
  description: string;
  inputSchema: Record<string, unknown>;
}

export interface LlmParams {
  model: string;
  /**
   * The STABLE system prefix. Prompt-cache contract: byte-identical across
   * sessions and turns per delegation tier, so the provider cache can share it.
   * Anything carrying per-session facts belongs in `systemVolatile` — one volatile
   * byte early in the prefix defeats cross-session sharing.
   */
  system?: string;
  /**
   * The per-session suffix (workspace paths, MCP catalog, skills, running-subagent
   * notes). Sent after `system` with its own cache breakpoint, so it still caches
   * across turns within a session without poisoning the shared prefix.
   */
  systemVolatile?: string;
  maxTokens: number;
  messages: LlmMessage[];
  tools: LlmToolDef[];
  /**
   * "none" forbids tool calls for this round, forcing plain text. The runner's
   * last resort against a turn that would otherwise end with nothing user-visible
   * (spec §5: every turn must produce user-visible text).
   */
  toolChoice?: "none";
  effort?: Effort;
}

export interface LlmResult {
  content: LlmBlock[];
  stopReason: string;
  usage?: Usage;
}

/**
 * The whole provider surface. Anthropic, OpenAI and OpenRouter each satisfy this
 * and the turn runner must not know which it is talking to — if provider-specific
 * handling leaks past this interface, it leaks everywhere (plan §8.3).
 */
export interface LlmClient {
  /**
   * One round. `onText` receives streamed text deltas as they arrive. Aborting
   * `signal` cancels the in-flight request; the caller treats the resulting abort
   * as an interrupt.
   */
  run(
    params: LlmParams,
    onText: (delta: string) => void,
    signal?: AbortSignal,
  ): Promise<LlmResult>;
}

/**
 * The cheap tier: auto titles, composer ghost text, live activity blurbs. Every
 * one of these bills on every round, so **each must fail silently** — these
 * methods resolve `null` on failure and NEVER reject, because a synchronous
 * failure here would stall a turn for a cosmetic feature (spec §12, plan §8.4).
 * One in-flight blurb per session: drop, don't queue (plan §6.11).
 */
export interface CheapTier {
  title(firstMessage: string): Promise<string | null>;
  ghostText(prefix: string): Promise<string | null>;
  activity(recent: string): Promise<string | null>;
}

// ---- application contexts ---------------------------------------------------

/**
 * What every HTTP handler receives. Handlers are `(req, ctx, params)`; the router
 * builds this once at boot and hands the same object to all of them.
 */
export interface AppCtx {
  db: Db;
  bus: Bus;
  /** Injected in tests; production wires the provider-routed client. */
  llm?: LlmClient;
  /** Absent = the global default (env, then the built-in). */
  model?: string;
  effort?: Effort;
  /** Injected clock. Absent = `Date.now`. Pure core takes this, never the global. */
  now?: () => number;
  /** Absent in tests — every cheap-tier feature degrades to nothing. */
  cheap?: CheapTier;
}

/**
 * What a running turn — and therefore every host function — receives. Host
 * functions take this and nothing else, which is what keeps `hostfn/` free of any
 * import from `server/`.
 */
export interface TurnCtx extends AppCtx {
  sessionId: string;
  turnId: string;
  /** The pending supervisor message the turn is producing. */
  messageId: string;
  /** Resolved at turn start; subagents share it — one checkout, no worktrees. */
  workspace: string;
  model: string;
  /**
   * The turn's interrupt. A program's children are killed and the worker wound
   * down when this fires, and the partial tool result is persisted with
   * `interrupted: true` (spec §5).
   */
  signal: AbortSignal;
  /**
   * Commands that exited non-zero during this turn, recorded by `bash()` so a round can
   * REPORT one the program never printed (`turn/runner.ts`: `withExitNotes`).
   *
   * On the ctx rather than in a closure because the host functions are built from it in more
   * than one place — `baseHostFns` for an ordinary turn, and again inside `delegationDeps`
   * for every session that can delegate, which is all of them. A closure-local array was
   * silently bypassed by the second path, which is exactly how the first version of this
   * shipped green tests and did nothing live.
   */
  exits?: { command: string; code: number }[];
  /**
   * Where finished shell commands enter the tag-history memory (`history/record.ts`).
   *
   * On the ctx for the same reason `exits` is: the host functions are built from it in
   * more than one place (`baseHostFns`, and again inside `delegationDeps`), and a
   * closure-local recorder would be silently bypassed by the second path.
   */
  record?: (e: {
    command: string;
    tags: string;
    exitCode: number | null;
    durationMs: number | null;
    outputHead: string;
    spillPath: string | null;
  }) => void;
  /**
   * Absolute paths the turn's programs viewed, appended by `view()` — what the
   * directory-triggered tag hints key on (`history/stats.ts`). Shared across
   * construction paths exactly like `exits`.
   */
  reads?: string[];
  /**
   * Absolute directories the turn's shell commands were about, appended by the
   * command recorder — the other hint trigger, so shell exploration surfaces
   * directory memory without a single `view()`. Shared like `exits`.
   */
  touched?: string[];
  /**
   * MCP servers inherited from the spawning turn. The human's grant to a spawner
   * extends to the subagents doing parts of that same granted work. Captured at
   * spawn time, so a later manual continuation does not inherit (spec §7).
   */
  mcpGrant?: string[];
  /**
   * Delegation depth. 0 = top level (may `spawn` and start workflows); 1 = a
   * subagent, which may delegate one level further, blocking only (spec §7).
   */
  depth: number;
}

// ---- host functions ---------------------------------------------------------

/**
 * The host side of the program bridge. Every signature is string-in/string-out:
 * the postMessage wire is string-only, so an object argument is serialized by the
 * worker and a structured result comes back as JSON, which the worker re-inflates
 * before the program sees it. `view`/`patch` are the exception — their text IS the
 * format, so nothing is JSON-wrapped.
 *
 * Optionality is the capability grant. A function the turn does not bridge is
 * simply absent, and calling it rejects with "unknown host function" — which is
 * correct, because the system prompt also omits its section, so a well-behaved
 * model never reaches for it (spec §6: a host function exists only when the prompt
 * grants it). Shell and file verbs are always wired and therefore required here.
 */
export interface HostFns {
  /**
   * Combined output. Carries the turn's interrupt, and **auto-backgrounds past
   * 60s** — it returns "…moved to background as bg_N" and the command keeps
   * running, so a program must never write a sleep/poll loop (plan §6.7).
   *
   * `tags` is REQUIRED: 1–3 colon-separated intent tags (`"git:push"`,
   * `"psql:migrate"`) that index the command in the cross-session history. The
   * wire cannot enforce arity, so the host ALSO enforces it at runtime — a call
   * without tags rejects with a catchable ProgramError naming the format.
   */
  bash(cmd: string, tags: string): Promise<string>;
  /**
   * Concurrent shells. The commands travel in as a JSON array and `[{code, out},
   * …]` comes back as JSON, in order. **Never throws on a non-zero exit** — the
   * code is data. An element is either a bare command string (untagged) or
   * `{cmd, tag}` — the tag stamps that leg in the command history.
   */
  sh(cmdsJson: string): Promise<string>;
  /**
   * Explicit background shell outliving the turn. Returns `{id, name, pid}` as
   * JSON. The NAME comes first and is required — it is what the user sees in the
   * rail and in the job view, and it is refused when blank (`hostfn/jobs.ts`).
   */
  bashBg(name: string, cmd: string): Promise<string>;
  /** Output since the last call plus a `[running]`/`[exited]` line. Safe while running. */
  bashOutput(id: string): Promise<string>;
  bashWait(id: string): Promise<string>;
  bashKill(id: string): Promise<string>;
  /** `[path#TAG]` header plus numbered `N:text` lines. The TAG pins the version read. */
  view(path: string): Promise<string>;
  /**
   * Hash-anchored line edits; returns the file's new TAG so a second patch can
   * chain without viewing again. An empty tag means "the version I just viewed".
   * Multi-file patches apply all-or-none. A conflict names the file and the line
   * range and says someone else changed those lines (spec §6).
   */
  patch(input: string): Promise<string>;
  /** New files and wholesale rewrites. There is no `read()` and no `edit()`. */
  write(path: string, content: string): Promise<string>;

  /** Blocking subagent. Returns `{sessionId, ok, report, changedFiles}` as JSON. */
  agent?(task: string, optsJson: string): Promise<string>;
  /** Detached subagent. Returns `{sessionId, title}` as JSON, immediately. */
  spawn?(task: string, optsJson: string): Promise<string>;
  /** Claim a detached subagent's result in-band. */
  join?(sessionId: string): Promise<string>;
  /** Take over a subagent's session. */
  adopt?(sessionId: string): Promise<string>;

  /** Verb-dispatched: start/rerun/stop/pause/resume/status/list. */
  workflow?(verb: string, argsJson: string): Promise<string>;

  /**
   * Park the program and ask the human. Returns their answer as a plain string;
   * rejects catchably with "user declined" on dismissal, so the program proceeds
   * on a stated default or stops cleanly. Memory-only — the hold dies with the turn.
   */
  ask?(question: string, optsJson: string): Promise<string>;
  /** Verb-dispatched: get/set/list/delete. Scoped to the LINEAGE ROOT, 16KB per key. */
  state?(verb: string, argsJson: string): Promise<string>;
  /** Verb-dispatched: list/add/enable/disable/remove. */
  schedule?(verb: string, argsJson: string): Promise<string>;
  /** Publish a file for browser viewing under the session's artifact dir; returns `{url, href}`. */
  artifact?(name: string, content: string): Promise<string>;
}

/** Names in the protocol list that `HostFns` does not declare. Must be `never`. */
export type UnboundHostFn = Exclude<HostFnName, keyof HostFns>;
/** Names `HostFns` declares that the protocol list does not. Must be `never`. */
export type UnknownHostFn = Exclude<keyof HostFns, HostFnName>;

type AssertNever<T extends never> = T;

/**
 * Compile-time proof that `HostFns` and `HOST_FN_NAMES` agree. If either alias
 * above stops being `never`, this line fails to typecheck — which is the point:
 * the two lists cannot drift without `deno task check` saying so.
 */
export type HostFnsMatchProtocol = AssertNever<UnboundHostFn> | AssertNever<UnknownHostFn>;

/**
 * The workflow worker's bridge. Three verbs, `permissions: "none"`, nothing else.
 * `parallel` and `pipeline` are pure combinators over `agent` and live worker-side.
 */
export interface WorkflowHostFns {
  /** Runs a subagent and returns its report. Throws on failure (spec §8). */
  agent(prompt: string, optsJson: string): Promise<string>;
  /** Fire-and-forget progress. Never blocks. */
  phase(title: string): Promise<string>;
  log(message: string): Promise<string>;
}

// ---- re-exports -------------------------------------------------------------
// One import for consumers that need the ctx and the shapes it carries.

export type { AskQuestion, Message, Part, Session, Turn, Usage };
