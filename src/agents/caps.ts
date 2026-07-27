/**
 * The delegation width caps: how many subagents one turn may launch, and how many
 * may be running at once across the whole tree (spec §7).
 *
 * THE INVARIANT THIS HOLDS: **a refused launch costs nothing.** Not the slot it
 * asked for, not a sibling that already started, not the budget of the turn it was
 * launched from. Fan-out is written as `Promise.allSettled` over N launches
 * precisely because some of them are expected to be refused (plan §6.9), so the
 * cap has to behave like a `Promise.reject` for exactly one element of that array
 * — every other launch continues, and the ledger afterwards reflects the launches
 * that actually happened and no others. A cap that unwound a sibling, or that
 * charged a refused launch against the per-turn budget, would turn the harness's
 * own recommended fan-out idiom into a lossy one.
 *
 * WHY A LEDGER AND NOT A QUERY. The port derived both counts by scanning
 * `db.listSessions()` — spawns of this turn by `originMessageId`, running children
 * by walking the lineage and asking the turn registry. That reads correctly and
 * counts wrongly: check-then-create is two steps with an `await` between them, and
 * a program that fires twelve `spawn()` calls without awaiting produces twelve
 * checks that all see zero running before the first session row exists. The budget
 * would then be advisory. Here the check and the take are ONE synchronous function
 * (`reserve`), which on a single-threaded runtime is atomic by construction: no
 * two launches can observe the same free slot. That is the whole reason this
 * module owns a mutable counter instead of a predicate over the database.
 *
 * WHY THE CONCURRENCY COUNTER IS KEYED BY TREE AND NOT BY SESSION. "Four running
 * at once" is a property of a *piece of work*, not of a conversation. A root that
 * spawns four children has used the budget; so has a root that spawns two children
 * which each spawn one, even though no single session launched more than two. The
 * key is therefore the tree root — the top non-subagent session of the lineage
 * (`treeRootOf`) — so nested and sibling launches share one budget and a launch
 * from any session in the tree sees the true total. Sessions in *different* trees
 * hold independent budgets: they are different work, and one user's long fan-out
 * must not refuse another session's first delegation.
 *
 * THE SLOT IS TAKEN AT RESERVATION, NOT WHEN A TURN IS SEEN RUNNING. A child's
 * turn row does not exist until after its session is created, and its status is
 * not `running` until the runner gets to it. A budget that could only be measured
 * after the fact is not a budget, so the slot is held from `reserve()` until the
 * lease is released — which the launch path ties to the child's turn settling, and
 * which the bus attachment backstops (see `attachBus`) so a caller that drops a
 * lease on the floor cannot leak a slot out of the tree's budget forever.
 *
 * WHAT IS NOT HERE. The *depth* cap (a subagent may delegate one level further;
 * depth 2 is terminal) is derived from lineage and lives with the code that writes
 * that lineage, in `subagent.ts`. What this module owns of nesting is the narrower
 * rule that follows from the same sentence in spec §7: a subagent's further
 * delegation is **blocking only** — `assertMayDelegate` refuses a detached
 * `spawn()` from inside a subagent turn, and says why.
 *
 * Ported from `src/subagent.ts` (`MAX_TREE_CONCURRENT`, `MAX_SPAWNS_PER_TURN`,
 * `treeRootOf`, `runningInTree`). Deltas from that port are marked `NOTE:`.
 */
import { AgentError, SpawnCapError } from "../errors.ts";
import type { BoughEvent, TurnFinishedData } from "../schema/events.ts";
import type { Bus, Db, TurnCtx } from "../types.ts";

// ---------------------------------------------------------------------------
// The caps
// ---------------------------------------------------------------------------

/**
 * Total launches — blocking and detached alike — permitted from one turn.
 *
 * Bounds a *sequential* loop, which the concurrency cap cannot: a program that
 * awaits each child in turn never has two running at once and could otherwise fork
 * forever. Work that genuinely needs a wider fan-out is a workflow (spec §8),
 * which is exempt from both caps and has its own semaphore.
 */
export const MAX_SPAWNS_PER_TURN = 8;

/** Subagent turns permitted in flight at once across one tree (spec §7). */
export const MAX_TREE_CONCURRENT = 4;

/**
 * How a launch awaits its child, which is the only thing the nesting rule cares
 * about: `"blocking"` is `agent()`, `"detached"` is `spawn()`.
 */
export type DelegationMode = "blocking" | "detached";

/**
 * The lineage hop limit for the walks below.
 *
 * Lineage is written once, at spawn, so a cycle can only come from a bad write.
 * The limit is not paranoia about a well-formed tree — it is what stops such a
 * write from hanging every later launch on an infinite walk.
 */
const MAX_LINEAGE_HOPS = 16;

// ---------------------------------------------------------------------------
// Tree identity
// ---------------------------------------------------------------------------

/**
 * The top session of a subagent tree: walk `originId` up while the session is a
 * subagent, and stop at the first thing that is not.
 *
 * A fork or a compaction is therefore its own tree even though it also carries an
 * `originId` — it is a branch of a conversation, not a delegated child, and it
 * gets its own delegation budget. Pure over the database, so a caller may ask
 * before it commits to a launch.
 */
export function treeRootOf(db: Db, sessionId: string): string {
  let id = sessionId;
  let cur = db.getSession(id);
  for (let hops = 0; cur?.kind === "subagent" && cur.originId && hops < MAX_LINEAGE_HOPS; hops++) {
    const origin = db.getSession(cur.originId);
    // A dangling origin (the parent was never written, or the row is gone) leaves
    // this session as the top of the tree it can actually see. Better a budget
    // scoped slightly too narrowly than a walk that throws inside a launch.
    if (!origin) break;
    id = origin.id;
    cur = origin;
  }
  return id;
}

// ---------------------------------------------------------------------------
// Leases
// ---------------------------------------------------------------------------

/**
 * One taken concurrency slot, released when the child's turn ends.
 *
 * `release()` is idempotent, and that is load-bearing rather than defensive: the
 * launch path releases when the child's result settles, and the bus backstop
 * releases when the child's `turn.finished` arrives. Both fire for a normal child,
 * so a second release must be a no-op — a lease that decremented twice would hand
 * the tree a fifth concurrent slot, which is the same bug as having no cap.
 */
export interface SpawnLease {
  /** The tree whose concurrency budget this slot came out of. */
  readonly treeId: string;
  /** The spawning turn whose per-turn budget was charged. */
  readonly turnId: string;
  readonly released: boolean;
  /** The child session this slot is for, once it exists. */
  readonly sessionId: string | null;
  /**
   * Point the lease at the child session the moment its id is known, so the bus
   * backstop can release it if the holder never does.
   */
  bind(sessionId: string): void;
  /** Give the slot back. Idempotent. */
  release(): void;
}

/**
 * The lease a launch that is exempt from the caps carries (workflows, spec §8).
 *
 * A distinct no-op object rather than `null`, so every call site has a lease to
 * bind and release unconditionally — an exemption expressed as an optional lease
 * would put an `if` in the launch path, and the branch that forgets to release is
 * the one that leaks.
 */
export function exemptLease(): SpawnLease {
  let bound: string | null = null;
  let released = false;
  return {
    treeId: "",
    turnId: "",
    get released() {
      return released;
    },
    get sessionId() {
      return bound;
    },
    bind(sessionId: string) {
      if (!released) bound = sessionId;
    },
    release() {
      released = true;
    },
  };
}

// ---------------------------------------------------------------------------
// The ledger
// ---------------------------------------------------------------------------

/** Test seam: both caps are injectable so a test need not launch eight of anything. */
export interface CapLimits {
  perTurn?: number;
  concurrent?: number;
}

/**
 * The counters behind both caps.
 *
 * In memory, like the turn registry and the job registry, and for the same reason:
 * a persisted count would always be a lie after a restart. A restart ends every
 * running turn (they are recovered as `orphaned`, `turn/state.ts`), so an empty
 * ledger at boot is the truth, not a loss.
 */
export class SpawnCaps {
  readonly perTurn: number;
  readonly concurrent: number;

  /** turnId → launches this turn has been charged for. Never decremented. */
  readonly #spawns = new Map<string, number>();
  /** treeId → slots currently held. */
  readonly #running = new Map<string, number>();
  /** child sessionId → the leases the bus backstop may release. */
  readonly #bound = new Map<string, Set<SpawnLease>>();
  #detach: (() => void) | null = null;

  constructor(limits: CapLimits = {}) {
    this.perTurn = limits.perTurn ?? MAX_SPAWNS_PER_TURN;
    this.concurrent = limits.concurrent ?? MAX_TREE_CONCURRENT;
  }

  /**
   * Check both caps and take both slots, or throw having taken NEITHER.
   *
   * Synchronous from first read to last write — no `await` inside — which is what
   * makes it atomic on this runtime and what makes twelve simultaneous launches
   * settle as four takes and eight refusals rather than twelve takes.
   *
   * The per-turn budget is charged first because it is the one no amount of
   * waiting clears; but it is charged only on the path that also takes the
   * concurrency slot, so a launch refused for concurrency has spent nothing and
   * the turn can still use its full eight once children finish.
   */
  reserve(key: { turnId: string; treeId: string }): SpawnLease {
    const { turnId, treeId } = key;
    const spawned = this.#spawns.get(turnId) ?? 0;
    if (spawned >= this.perTurn) {
      throw new SpawnCapError(
        `spawn cap reached: this turn has already launched ${spawned} subagents, which is the ` +
          `per-turn limit (${this.perTurn}). Waiting will not clear it — it counts launches, ` +
          `not running children. Do the rest of the work in this turn, split it across the ` +
          `children you already have, or hand the fan-out to a workflow ` +
          `(workflow.start), which has no per-turn cap. Launches that already ` +
          `started are unaffected.`,
      );
    }

    const running = this.#running.get(treeId) ?? 0;
    if (running >= this.concurrent) {
      throw new SpawnCapError(
        `subagent concurrency cap reached: ${running} subagents are already running across this ` +
          `tree, which is the tree-wide limit (${this.concurrent}) — it counts every branch, ` +
          `not just this session's own children. Await or join() the ones in flight, then ` +
          `launch the rest as a second batch. This launch alone was refused; the ones ` +
          `already running are untouched.`,
      );
    }

    this.#spawns.set(turnId, spawned + 1);
    this.#running.set(treeId, running + 1);
    return this.#lease(treeId, turnId);
  }

  /** Slots held by one tree, or by every tree when called with no argument. */
  running(treeId?: string): number {
    if (treeId !== undefined) return this.#running.get(treeId) ?? 0;
    let total = 0;
    for (const n of this.#running.values()) total += n;
    return total;
  }

  /** Launches charged to one turn. */
  spawnedInTurn(turnId: string): number {
    return this.#spawns.get(turnId) ?? 0;
  }

  /**
   * Wire the ledger to the event stream. Returns the unsubscribe thunk.
   *
   * Two jobs, both about not leaking:
   *
   *   1. **A dropped lease.** The launch path releases when the child's result
   *      settles, but a launch that threw between `reserve()` and wiring that
   *      release — or a detached child whose promise nobody kept — would hold a
   *      slot for the life of the process. `turn.finished` for the bound session
   *      is the authoritative "this child is no longer running", on every path
   *      (done, error, interrupted), so it releases the lease too. Releases are
   *      idempotent, so the two paths racing is a no-op, not a double free.
   *   2. **The per-turn map.** Its entries are meaningless once the spawning turn
   *      ends, and without this the map would grow by one entry per delegating
   *      turn for as long as the server runs.
   */
  attachBus(bus: Bus): () => void {
    this.#detach?.();
    const off = bus.subscribe((event) => this.#onEvent(event));
    this.#detach = off;
    return off;
  }

  /** Drop every count. Tests only — production never un-caps a running tree. */
  reset(): void {
    this.#spawns.clear();
    this.#running.clear();
    this.#bound.clear();
  }

  #onEvent(event: BoughEvent): void {
    if (event.type !== "turn.finished") return;
    const data = event.data as TurnFinishedData | undefined;
    if (!data) return;
    if (data.sessionId) {
      for (const lease of this.#bound.get(data.sessionId) ?? []) lease.release();
    }
    // The turn that did the spawning is over; its budget entry is dead weight.
    if (data.turnId) this.#spawns.delete(data.turnId);
  }

  #lease(treeId: string, turnId: string): SpawnLease {
    let released = false;
    let bound: string | null = null;
    const lease: SpawnLease = {
      treeId,
      turnId,
      get released() {
        return released;
      },
      get sessionId() {
        return bound;
      },
      bind: (sessionId: string) => {
        // Binding after release would register a lease nothing will ever clean up.
        if (released || !sessionId || bound === sessionId) return;
        if (bound) this.#unbind(bound, lease);
        bound = sessionId;
        let set = this.#bound.get(sessionId);
        if (!set) this.#bound.set(sessionId, set = new Set());
        set.add(lease);
      },
      release: () => {
        if (released) return;
        released = true;
        const held = this.#running.get(treeId) ?? 0;
        // `- 1` guarded rather than assumed: a count that went negative would make
        // the cap silently unenforceable for the rest of the process.
        if (held <= 1) this.#running.delete(treeId);
        else this.#running.set(treeId, held - 1);
        if (bound) this.#unbind(bound, lease);
      },
    };
    return lease;
  }

  #unbind(sessionId: string, lease: SpawnLease): void {
    const set = this.#bound.get(sessionId);
    if (!set) return;
    set.delete(lease);
    if (set.size === 0) this.#bound.delete(sessionId);
  }
}

/**
 * The process-wide ledger.
 *
 * A singleton for the same reason the job registry is one: the thing it counts —
 * subagent turns in flight across a tree — spans sessions, turns and HTTP
 * requests, and a per-request instance would count nothing. `server/main.ts`
 * attaches the bus to it at boot; tests construct their own `SpawnCaps` and never
 * touch this one.
 */
export const spawnCaps: SpawnCaps = new SpawnCaps();

// ---------------------------------------------------------------------------
// The nesting rule
// ---------------------------------------------------------------------------

/**
 * Refuse detached delegation from inside a subagent turn (spec §7: subagents may
 * delegate one level further, **blocking only**).
 *
 * The reason is lifetime, not width. A subagent's report goes upward the instant
 * its turn ends; a detached grandchild would still be writing to the shared
 * checkout afterwards, mutating a branch the spawner has already been told is
 * final — and there would be nobody left to receive its own report, since the
 * session that would have been woken is finished.
 *
 * `depth` is `TurnCtx.depth`, which the runner sets to 1 for any subagent or
 * workflow-agent turn. The message says the rule and the move, because a program
 * that reads only "refused" retries the same call (spec §6).
 */
export function assertMayDelegate(
  ctx: Pick<TurnCtx, "depth">,
  mode: DelegationMode,
  verb = mode === "detached" ? "spawn()" : "agent()",
): void {
  if (mode !== "detached" || ctx.depth < 1) return;
  throw new AgentError(
    400,
    `${verb} is not available inside a subagent: nested delegation is blocking-only. ` +
      `Use await agent(task, {name}) instead — it runs the child to completion and returns ` +
      `its report in-band, so your own report can account for it. A detached child would ` +
      `outlive this turn and keep writing to the shared checkout after your report has ` +
      `already gone upward. Retrying ${verb} will fail the same way.`,
  );
}

// ---------------------------------------------------------------------------
// The launch path
// ---------------------------------------------------------------------------

export interface ReserveOptions {
  /** How the caller intends to await the child. Gates the nesting rule. */
  mode: DelegationMode;
  /** The host-function name for error text. Defaults from `mode`. */
  verb?: string;
  /**
   * Skip both width caps. Workflows only (spec §8: "subagent caps do not apply
   * inside a workflow — queue as many calls as needed"); the run's own semaphore
   * bounds it instead. The nesting rule still applies.
   */
  exempt?: boolean;
  /** Defaults to the process ledger. Tests pass their own. */
  caps?: SpawnCaps;
}

/**
 * Check the nesting rule and take a slot for one launch from this turn.
 *
 * Throws `AgentError` for a refused nesting, `SpawnCapError` for a cap — both
 * catchable inside the program, both naming which rule and what to do instead.
 */
export function reserveSpawn(
  ctx: Pick<TurnCtx, "db" | "sessionId" | "turnId" | "depth">,
  opts: ReserveOptions,
): SpawnLease {
  assertMayDelegate(ctx, opts.mode, opts.verb);
  if (opts.exempt) return exemptLease();
  const caps = opts.caps ?? spawnCaps;
  return caps.reserve({ turnId: ctx.turnId, treeId: treeRootOf(ctx.db, ctx.sessionId) });
}

/**
 * What `underLease` needs of a launch: an id now, and a settlement later. Written
 * structurally rather than as `SubagentLaunch` so this module stays independent of
 * the launch module it guards — the caps are about counting, not about how a
 * subagent is built.
 */
export interface LeasedLaunch {
  sessionId: string;
  result: Promise<unknown>;
}

/**
 * Run one launch under a reservation, releasing the slot on every exit.
 *
 * Three endings, and the slot must come back on all of them: the launch throws
 * (release immediately — nothing was started, so nothing is running); the child
 * finishes (release when its result settles, however it settled); or the holder
 * simply forgets, which is what the bus backstop is for. The `bind` in between is
 * what makes that third path possible at all.
 */
export function underLease<T extends LeasedLaunch>(lease: SpawnLease, launch: () => T): T {
  let started: T;
  try {
    started = launch();
  } catch (err) {
    // A refused or failed launch releases what it took and nothing else — in
    // particular it does not disturb the siblings already holding slots.
    lease.release();
    throw err;
  }
  lease.bind(started.sessionId);
  const done = () => lease.release();
  started.result.then(done, done);
  return started;
}

/**
 * The whole capped-launch path in one call: nesting rule, both caps, the slot, and
 * its release. This is what a delegation host function calls.
 *
 * ```ts
 * const launch = cappedLaunch(ctx, { mode: "detached", verb: "spawn()" }, () =>
 *   launchSubagent(ctx, task, opts, deps));
 * ```
 */
export function cappedLaunch<T extends LeasedLaunch>(
  ctx: Pick<TurnCtx, "db" | "sessionId" | "turnId" | "depth">,
  opts: ReserveOptions,
  launch: () => T,
): T {
  return underLease(reserveSpawn(ctx, opts), launch);
}
