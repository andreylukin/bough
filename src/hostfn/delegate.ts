/**
 * The delegation host functions: `agent`, `spawn`, `join`, `adopt`.
 *
 * THE INVARIANT THIS HOLDS: **a blocking child is part of its spawner's turn; a
 * detached one is not.** Every difference between the four verbs falls out of that
 * one sentence, and each half of it is a failure mode if it slips:
 *
 *   - `agent()` and `join()` are work the current turn is doing. The spawner's
 *     program is parked on the child, so a stop that did not reach the child would
 *     leave the user watching a turn they cancelled keep burning tokens in a branch
 *     with no reader. Both therefore hang a cascade on the spawning turn's own
 *     `signal`, and both drop it again the instant they resolve — cascading into a
 *     child that already finished would flip a completed branch to `interrupted`
 *     and erase a report that was already persisted.
 *   - `spawn()` is not. It answers the program with a handle and the child runs on
 *     **regardless of what the spawner does next** — keeps working, ends the turn,
 *     or is interrupted mid-program (spec §7). So it never touches `ctx.signal`.
 *     The one thing that does reach it is an explicit stop of the spawner session,
 *     through the registry's cascade hooks — which exist for exactly this and
 *     nothing else (`turn/queue.ts`: "a normal turn ending does not fire those
 *     hooks… only an explicit stop cascades"). Without that hook a runaway detached
 *     child in a deep tree has no stop path but its own session.
 *
 * THERE IS NO DONE-GATE. `agent()` returns `{sessionId, title, ok, status, report,
 * changedFiles}` — and no `checkPassed`. The port had one, derived from a committed
 * check the harness re-ran; the acceptance gate is gone (spec §17), so `ok` now says
 * only whether the child's TURN completed. `status` rides alongside it because
 * "failed" is not one fact: errored, interrupted and orphaned call for different
 * moves from the spawner, and a bare boolean makes all three look the same.
 *
 * WHAT THIS FILE DOES NOT DECIDE. It does not launch — `agents/subagent.ts` owns
 * lineage, naming, the child's ctx and the depth cap, and this module is four ways
 * of awaiting it. It does not enforce the width caps (T4.3, in that same module) and
 * it does not format or post the completion note (T4.4): `deps.deliver` is the seam
 * that receives a detached child's result, and until `agents/notes.ts` lands the
 * result is still claimable in-band by `join()` after the fact.
 *
 * TIERS, AND WHY THEY ARE DERIVED. Which verbs a turn is bridged is a function of
 * where the session sits in the lineage, not of a flag somebody set: a top-level
 * session gets all four, a subagent gets blocking `agent()` (plus `adopt`) only —
 * a detached grandchild would outlive the turn whose report had already gone upward
 * — and a depth-2 subagent or a workflow agent gets none. `delegationTier` reads
 * that off the database, and `delegationTurnDeps` pairs each tier with the matching
 * `granted` list, so the prompt sections and the bridge cannot disagree about what
 * exists (spec §6: a host function exists only when the prompt grants it).
 *
 * Ported from `src/subagent.ts` (`runSubagent`, `spawnSubagentDetached`,
 * `joinSubagent`, `adoptSubagent`). Deltas are marked `NOTE:`.
 */
import { z } from "zod";
import { AgentError } from "../errors.ts";
import type { HostFnName } from "../harness/protocol.ts";
import type { Message, Session } from "../schema/parts.ts";
import type { AppCtx, Db, HostFns, TurnCtx } from "../types.ts";
import { cappedLaunch, type SpawnCaps } from "../agents/caps.ts";
import {
  type LaunchDeps,
  launchSubagent,
  MAX_SUBAGENT_DEPTH,
  subagentDepth,
  type SubagentLaunch,
  type SubagentOptions,
  type SubagentResult,
} from "../agents/subagent.ts";
import { TurnRegistry, turns as defaultRegistry } from "../turn/queue.ts";
import {
  BASE_HOST_FNS,
  baseHostFns,
  createTurnStarter,
  defaultProgramRunner,
  interruptTurn,
  type TurnDeps,
} from "../turn/runner.ts";

// ---------------------------------------------------------------------------
// Tiers
// ---------------------------------------------------------------------------

/**
 * How much delegation a session may do.
 *
 *   - `top`    — root, fork, compaction: all four verbs, detaching included.
 *   - `nested` — a subagent one hop down: blocking `agent()` and `adopt()` only.
 *   - `none`   — a depth-2 subagent (the nesting cap, spec §7) or a workflow agent,
 *                which gets no context beyond its prompt and no delegation with it.
 */
export type DelegationTier = "top" | "nested" | "none";

/**
 * Everything a top-level session may call.
 *
 * `adopt` is STILL BRIDGED but no longer documented in `prompt/delegation.md`. It is a
 * vestige of the era when each subagent had its own workspace and its work had to be
 * taken over; since subagents share their spawner's checkout there is nothing to take —
 * both of its branches now just explain that. Walked it live: haiku called it, got a
 * paragraph back, and spent a whole round relaying that paragraph to the user. Left
 * callable so an old transcript replays unchanged; taken out of the prompt so no round is
 * ever spent on it again.
 */
export const TOP_LEVEL_DELEGATION: readonly HostFnName[] = ["agent", "spawn", "join", "adopt"];

/**
 * What a subagent may call: blocking only.
 *
 * `spawn` and `join` are withheld deliberately. A detached child of a subagent would
 * still be running — and still writing to the shared checkout — after its spawner's
 * report had already been handed upward, mutating a branch the top-level session
 * believes is final.
 */
export const NESTED_DELEGATION: readonly HostFnName[] = ["agent", "adopt"];

/** The verbs a tier is bridged, and therefore the prompt sections it earns. */
export function delegationFnsFor(tier: DelegationTier): readonly HostFnName[] {
  return tier === "top" ? TOP_LEVEL_DELEGATION : tier === "nested" ? NESTED_DELEGATION : [];
}

/**
 * A session's tier, read off its lineage.
 *
 * Derived rather than passed, for the same reason the depth cap is: only the
 * `originId` chain knows how far down a session actually is. `TurnCtx.depth` is a
 * tier flag the runner sets from `kind` alone (1 for any subagent, however deeply
 * nested), so a depth-2 subagent looks identical to a depth-1 one there.
 *
 * A `workflow_agent` gets `none`: spec §8 gives it its prompt string and nothing
 * else, and the prompt assembler grants it neither delegation section — bridging a
 * verb it is never told about would be exactly the guess spec §6 forbids.
 */
export function delegationTier(db: Db, sessionId: string): DelegationTier {
  const session = db.getSession(sessionId);
  if (!session || session.kind === "workflow_agent") return "none";
  const depth = subagentDepth(db, sessionId);
  if (depth === 0) return "top";
  return depth < MAX_SUBAGENT_DEPTH ? "nested" : "none";
}

/** What a child launched from `tier` may itself do. One hop down, never sideways. */
export function childTierOf(tier: DelegationTier): DelegationTier {
  return tier === "top" ? "nested" : "none";
}

// ---------------------------------------------------------------------------
// The detached register
// ---------------------------------------------------------------------------

/** One live-or-finished detached child, from its spawner's point of view. */
export interface DetachedRecord {
  spawnerId: string;
  sessionId: string;
  title: string;
  /** Settles with the child's assembled result; never rejects (see `launchSubagent`). */
  result: Promise<SubagentResult>;
  /** `join()` (or `adopt()`) took it in-band, so no completion note is owed. */
  claimed: boolean;
}

/**
 * Detached children, by child session id.
 *
 * Memory-only and process-scoped, like the turn registry it sits beside: a server
 * restart orphans the running turn (`turn/state.ts` surfaces it) and the record goes
 * with it, which is why `join`'s refusal says so rather than implying the id was
 * wrong. A class rather than a module-level `Map` so a test gets its own and two
 * tests in one file cannot claim each other's children.
 *
 * Finished records are kept, not dropped: `join()` after completion is a normal move
 * (spawn three, do other work, claim them all), and a record dropped at completion
 * would turn the ordinary race into an error.
 */
export class DetachedSubagents {
  readonly #byChild = new Map<string, DetachedRecord>();

  register(record: Omit<DetachedRecord, "claimed">): DetachedRecord {
    const entry: DetachedRecord = { ...record, claimed: false };
    this.#byChild.set(entry.sessionId, entry);
    return entry;
  }

  get(sessionId: string): DetachedRecord | undefined {
    return this.#byChild.get(sessionId);
  }

  /** The children this session detached, newest last. The refusal message names them. */
  idsFor(spawnerId: string): string[] {
    return [...this.#byChild.values()]
      .filter((r) => r.spawnerId === spawnerId)
      .map((r) => r.sessionId);
  }

  /**
   * Take a child in-band. Idempotent: claiming twice returns the same record and
   * the same promise, because "spawn, join, then join again in a later round" is a
   * program being careful, not a program being wrong.
   */
  claim(spawnerId: string, sessionId: string): DetachedRecord {
    const record = this.#byChild.get(sessionId);
    if (!record || record.spawnerId !== spawnerId) {
      const mine = this.idsFor(spawnerId);
      throw new AgentError(
        400,
        `join("${sessionId}"): this session has no detached subagent by that id. ` +
          (mine.length > 0
            ? `Its detached subagents are: ${mine.join(", ")}.`
            : `It has not spawn()ed any — join() only claims a child THIS session ` +
              `detached with spawn(), and the register is memory-only, so a server ` +
              `restart clears it. Use agent(task, {name}) to run one to completion.`),
      );
    }
    record.claimed = true;
    return record;
  }

  forget(sessionId: string): void {
    this.#byChild.delete(sessionId);
  }

  get size(): number {
    return this.#byChild.size;
  }
}

/** The process-wide register. Injected everywhere; this is the production instance. */
export const detachedSubagents: DetachedSubagents = new DetachedSubagents();

// ---------------------------------------------------------------------------
// Options, validated at the bridge
// ---------------------------------------------------------------------------

/**
 * `agent(task, {name})`'s options bag, as it arrives over the string-only wire.
 *
 * Validated here because the bridge IS a boundary (plan §0: Zod at the boundary):
 * the program is arbitrary model-written JavaScript, so `{name: 42}` is a thing that
 * happens, and it must become a message the next round can act on rather than a
 * branch titled "42".
 */
const DelegationOptions = z.object({
  name: z.string().optional(),
  model: z.string().optional(),
  effort: z.enum(["low", "medium", "high", "xhigh", "max"]).optional(),
});

function parseOptions(verb: string, optsJson: string): SubagentOptions {
  let raw: unknown;
  try {
    raw = optsJson.trim() === "" ? {} : JSON.parse(optsJson);
  } catch (err) {
    throw new AgentError(
      400,
      `${verb}(task, opts): the options could not be read as JSON ` +
        `(${err instanceof Error ? err.message : String(err)}). Pass a plain object, ` +
        `e.g. ${verb}(task, {name: "audit auth handlers"}).`,
    );
  }
  if (raw === null || raw === undefined) return {};
  const parsed = DelegationOptions.safeParse(raw);
  if (!parsed.success) {
    throw new AgentError(
      400,
      `${verb}(task, opts): ${
        parsed.error.issues
          .map((i) => `${i.path.join(".") || "opts"}: ${i.message}`)
          .join("; ")
      }. It takes {name?: string, model?: string, effort?: "low"|"medium"|"high"|"xhigh"|"max"} — ` +
        `always pass a name, it labels the branch everywhere the user sees it.`,
    );
  }
  return parsed.data;
}

// ---------------------------------------------------------------------------
// The host functions
// ---------------------------------------------------------------------------

/** How a launch happens. `launchSubagent` satisfies this. */
export type LaunchFn = (
  ctx: TurnCtx,
  task: string,
  opts: SubagentOptions,
  deps: LaunchDeps,
) => SubagentLaunch;

/** The seams, so all four verbs are drivable with no worker, no key and no server. */
export interface DelegationDeps {
  /** Absent = derived from the session's lineage (`delegationTier`). */
  tier?: DelegationTier;
  /** Absent = the process registry, which is what the turn runner also defaults to. */
  registry?: TurnRegistry;
  /** Absent = the process-wide detached register. */
  detached?: DetachedSubagents;
  /** Absent = `launchSubagent`. */
  launch?: LaunchFn;
  /** The width-cap ledger (T4.3). Absent = the process ledger. */
  caps?: SpawnCaps;
  /**
   * Skip the width caps. Workflows only (spec §8: their own semaphore bounds the
   * fan-out, and a whole run would not fit under the per-turn cap). The nesting
   * rule still applies.
   */
  exempt?: boolean;
  /** The child's launch deps — its turn deps, its timeout, its diff seam. */
  child?: (ctx: TurnCtx) => LaunchDeps;
  /**
   * Where a detached child's result goes when nobody claimed it: T4.4 posts it to
   * the spawner as a `[subagent finished]` system note, waking an idle spawner.
   *
   * Absent = nothing is posted. The branch is still in the tree and the result is
   * still claimable by a later `join()`, so an unwired seam degrades to "the report
   * is not pushed" rather than to lost work.
   */
  deliver?: (ctx: TurnCtx, result: SubagentResult) => void;
  /** Where a launch's internal failure is logged. Tests pass a collector. */
  reportError?: (error: unknown, sessionId: string) => void;
}

/** The delegation subset of `HostFns` — exactly the four names, all optional. */
export type DelegationHostFns = Pick<HostFns, "agent" | "spawn" | "join" | "adopt">;

/**
 * Build the delegation host functions for one turn.
 *
 * Returns only the verbs the tier allows, because **absence is the capability
 * denial**: a name the turn does not bridge is simply not on the host object, and
 * calling it rejects with the bridge's "not available in this turn — the system
 * prompt lists the host functions this session was granted" (`harness/vm.ts`). A
 * `none` tier therefore returns `{}`, not four functions that throw.
 */
export function createDelegationHostFns(
  ctx: TurnCtx,
  deps: DelegationDeps = {},
): DelegationHostFns {
  const tier = deps.tier ?? delegationTier(ctx.db, ctx.sessionId);
  if (tier === "none") return {};

  const registry = deps.registry ?? defaultRegistry;
  const detached = deps.detached ?? detachedSubagents;
  const launch = deps.launch ?? launchSubagent;
  const childDeps = deps.child ?? (() => ({}));
  const reportError = deps.reportError ??
    ((err, sessionId) => console.error(`detached subagent ${sessionId} failed:`, err));

  /**
   * Cascade a stop into a child that is STILL running.
   *
   * The guard is the whole point: a blocking child that already resolved has its
   * report and its outcome persisted on its own branch, and interrupting it now
   * would flip a finished session to `interrupted` and overwrite work that was
   * already accepted.
   */
  const stopIfRunning = (sessionId: string) => {
    if (registry.isRunning(sessionId)) interruptTurn(sessionId, registry);
  };

  /** Await a child as part of THIS turn: the spawner's stop reaches it. */
  const awaitAsOwnWork = async (
    sessionId: string,
    result: Promise<SubagentResult>,
  ): Promise<string> => {
    const cascade = () => stopIfRunning(sessionId);
    ctx.signal.addEventListener("abort", cascade, { once: true });
    try {
      return JSON.stringify(await result);
    } finally {
      ctx.signal.removeEventListener("abort", cascade);
    }
  };

  /** Refuse before creating a branch nobody will read. */
  const assertLive = (verb: string): void => {
    if (ctx.signal.aborted) {
      throw new AgentError(
        409,
        `${verb}(): this turn was interrupted, so nothing was launched. ` +
          `Anything already done stands; the branches that were running have been stopped.`,
      );
    }
  };

  /**
   * Every launch goes through the width caps (T4.3): the nesting rule, the
   * per-turn budget and the tree's concurrency slot, with the slot released when
   * the child settles. A refusal throws `SpawnCapError` naming WHICH cap, and
   * costs the siblings already running nothing — which is what makes the
   * documented `Promise.allSettled` fan-out idiom lossless (plan §6.9).
   */
  const capped = (
    task: string,
    opts: SubagentOptions,
    mode: "blocking" | "detached",
    verb: string,
  ) =>
    cappedLaunch(
      ctx,
      {
        mode,
        verb,
        ...(deps.caps ? { caps: deps.caps } : {}),
        ...(deps.exempt ? { exempt: true } : {}),
      },
      () => launch(ctx, task, opts, childDeps(ctx)),
    );

  const agent = async (task: string, optsJson = "{}"): Promise<string> => {
    assertLive("agent");
    const opts = parseOptions("agent", optsJson);
    const child = capped(task, opts, "blocking", "agent()");
    return await awaitAsOwnWork(child.sessionId, child.result);
  };

  /**
   * Detached delegation. Returns the handle immediately and deliberately keeps the
   * child off `ctx.signal`: it survives the spawner's turn ending, and it survives
   * the spawner's program being wound down. Only an explicit stop of the spawner
   * session reaches it, through the registry's cascade hook.
   */
  // The bridge contract is promise-in, promise-out: a refusal must reach the program
  // as a rejection, like every other one, rather than as a synchronous throw.
  // deno-lint-ignore require-await
  const spawn = async (task: string, optsJson = "{}"): Promise<string> => {
    assertLive("spawn");
    const opts = parseOptions("spawn", optsJson);
    const child = capped(task, opts, "detached", "spawn()");
    const record = detached.register({
      spawnerId: ctx.sessionId,
      sessionId: child.sessionId,
      title: child.title,
      result: child.result,
    });
    const unhook = registry.onInterrupt(ctx.sessionId, () => stopIfRunning(child.sessionId));
    child.result
      .then((result) => {
        // Claimed in-band by `join()` — the program already has it, and posting a
        // note as well would tell the spawner the same thing twice.
        if (!record.claimed) deps.deliver?.(ctx, result);
      })
      .catch((err) => reportError(err, child.sessionId))
      .finally(unhook);
    return JSON.stringify({ sessionId: child.sessionId, title: child.title });
  };

  /**
   * Claim a detached child in-band. From this point the child IS this turn's work,
   * so the spawner's stop reaches it — same containment as the blocking mode.
   */
  const join = async (sessionId: string): Promise<string> => {
    const record = detached.claim(ctx.sessionId, sessionId);
    return await awaitAsOwnWork(record.sessionId, record.result);
  };

  /**
   * Take over a subagent's session.
   *
   * NOTE: there is nothing to move, and saying so IS the implementation. Subagents
   * share their spawner's checkout (spec §7, §17: no per-agent worktrees), so a
   * child's writes are already in this session's tree the moment it makes them —
   * the honest answer is the one that stops the model looking for a merge step that
   * does not exist. What it still does is real: it validates the lineage (adopting
   * a stranger's session is a mistake worth naming), reports the branch's live
   * status, and re-announces the branch so the rail and the Changes view refresh.
   *
   * It deliberately does NOT mark a detached child claimed. A child adopted while
   * still running would then finish with its report going nowhere.
   */
  // deno-lint-ignore require-await -- promise-in, promise-out, as above.
  const adopt = async (sessionId: string): Promise<string> => {
    const child = ctx.db.getSession(sessionId);
    if (!child || child.kind !== "subagent" || child.originId !== ctx.sessionId) {
      throw new AgentError(
        400,
        `adopt("${sessionId}"): that is not a subagent of this session. adopt() only ` +
          `takes over a branch THIS session spawned; you cannot adopt a sibling, a ` +
          `grandchild, or an ordinary session.`,
      );
    }
    ctx.bus.publish({ type: "session.updated", sessionId: child.id, data: child });

    const running = registry.isRunning(child.id);
    const state = running
      ? `is still running`
      : `finished (${child.outcomeOk === false ? "its turn failed" : "its turn completed"})`;
    const next = running
      ? detached.get(child.id)
        ? `await join("${child.id}") to take its report in-band, or end your turn and let ` +
          `its "[subagent finished]" note arrive.`
        : `wait for the call that started it rather than polling.`
      : `read the working tree for what it changed.`;
    return `subagent "${child.title}" (${child.id}) ${state}. It works in THIS session's ` +
      `checkout, so its writes are already here — there is no worktree and nothing to ` +
      `merge. ${next}`;
  };

  return tier === "top" ? { agent, spawn, join, adopt } : { agent, adopt };
}

// ---------------------------------------------------------------------------
// Turn wiring
// ---------------------------------------------------------------------------

/**
 * How delegation is wired into turns, once, at boot.
 *
 * `base` is whatever the process already wants on every turn (`survivingJobs`, a
 * test registry); `extend` is the composition seam for the M6 verbs, which bridge
 * their own host functions into the same turn. Both are threaded down into the
 * CHILD's turn deps too, so a subagent's turn behaves like any other turn — same
 * registry, same job reporting, one tier shallower.
 */
export interface DelegationWiring {
  base?: TurnDeps;
  /** Host functions another task bridges, merged under the delegation verbs. */
  extend?: (ctx: TurnCtx) => Partial<HostFns>;
  /** Launch-level seams for every child: its wall clock, its changed-files source. */
  launch?: Omit<LaunchDeps, "turn">;
  detached?: DetachedSubagents;
  deliver?: (ctx: TurnCtx, result: SubagentResult) => void;
  /** The width-cap ledger. Absent = the process ledger (T4.3). */
  caps?: SpawnCaps;
}

/**
 * The `TurnDeps` for a turn at `tier`: the delegation verbs bridged into its
 * programs, and the matching `granted` list so the prompt documents exactly those.
 *
 * The two must be built together — this function is the only place that knows both
 * halves. `granted` is the prompt's capability grant and the bridge is the runtime
 * one; a turn told about `spawn()` that cannot call it wastes a round, and a turn
 * that can call one it was never told about will not.
 */
export function delegationTurnDeps(
  tier: DelegationTier,
  wiring: DelegationWiring = {},
): TurnDeps {
  const base = wiring.base ?? {};
  return {
    ...base,
    granted: [...(base.granted ?? BASE_HOST_FNS), ...delegationFnsFor(tier)],
    programFor: (turnCtx) =>
      defaultProgramRunner(turnCtx, {
        ...baseHostFns(turnCtx),
        ...(wiring.extend?.(turnCtx) ?? {}),
        ...createDelegationHostFns(turnCtx, {
          tier,
          registry: base.registry,
          detached: wiring.detached,
          deliver: wiring.deliver,
          caps: wiring.caps,
          // Lazily, per launch: a child's turn is a turn like any other, one tier
          // shallower. Building it eagerly would recurse at construction time.
          child: () => ({
            ...wiring.launch,
            turn: delegationTurnDeps(childTierOf(tier), wiring),
          }),
        }),
      }),
  };
}

/**
 * The `TurnStarter` for `server/sessions.ts`, with delegation wired in.
 *
 * One starter per tier, chosen per session at start time. That indirection is
 * required rather than tidy: `TurnDeps.granted` is a fixed array read once inside
 * the runner, so a single starter cannot vary the grant by session — and the grant
 * MUST vary, because a depth-2 subagent and a root are the same code path with
 * different capabilities.
 *
 * Typed structurally rather than importing `TurnStarter` from `server/sessions.ts`:
 * `hostfn/` never imports from `server/` (plan §3).
 */
export function createDelegatingTurnStarter(
  wiring: DelegationWiring = {},
): (ctx: AppCtx, session: Session, message: Message) => void {
  const starters: Record<DelegationTier, ReturnType<typeof createTurnStarter>> = {
    top: createTurnStarter(delegationTurnDeps("top", wiring)),
    nested: createTurnStarter(delegationTurnDeps("nested", wiring)),
    none: createTurnStarter(delegationTurnDeps("none", wiring)),
  };
  return (ctx, session, message) => {
    starters[delegationTier(ctx.db, session.id)](ctx, session, message);
  };
}
