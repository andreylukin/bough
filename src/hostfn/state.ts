/**
 * `state.get / set / list / delete` — the durable notes one line of work keeps for
 * itself across turns.
 *
 * WHY IT EXISTS. The only thing carrying a fact from one round to the next is the
 * transcript, and the transcript is exactly what compaction, forking and the context
 * cap eventually eat. A long task ("port 40 files, one per turn") therefore
 * re-derives its own bookkeeping every round, out of a history that is getting
 * shorter. `state.set()` puts that bookkeeping in SQLite instead, at a cost of one
 * line of context.
 *
 * THE INVARIANT THIS HOLDS: **the store is keyed by the LINEAGE ROOT, never by the
 * session id.** A fork, a compaction child and a subagent are one piece of work from
 * the user's point of view (spec §6), and a note written before a fork has to still
 * be there after it — otherwise the store is useless for exactly the long tasks it
 * exists for, because every branch silently starts empty. Scoping by session id
 * would look correct in every single-session test and fail the first time anyone
 * forked.
 *
 * `lineageRoot` is therefore the load-bearing function here, and it is *wider* than
 * `Db.ancestorChain` on purpose. NOTE (delta from `src/state.ts`, which resolved the
 * root as `db.ancestorChain(sessionId)[0]`): `ancestorChain` walks `parentId`, and a
 * subagent has `parentId: null` — that null is what gives it a fresh, task-only
 * thread (`agents/subagent.ts`). Walking parents alone therefore makes every
 * subagent its own root, which contradicts the sentence the store is specified by.
 * So the walk hops one more edge: at the top of a parent chain that is a
 * `subagent`/`workflow_agent`, it continues from `originId` — the delegation edge —
 * and lands on the spawner's root. Forks and compactions are unaffected: for them
 * this is `ancestorChain[0]` exactly.
 *
 * SECOND INVARIANT: **it is notes, not storage.** 16KB per value and 200 keys per
 * lineage, both hard. A program that dumps a build log in here would re-inflate the
 * very context this exists to spare, and the value comes back into the model's
 * window in full on the next `get()`. So an oversized value is REJECTED, never
 * truncated — a silently shortened note is a wrong note, and the message says to put
 * the payload in a file and store its path.
 *
 * Cross-conversation memory is deliberately not offered: there is no semantic recall
 * (spec §17), and this store is per lineage on purpose.
 *
 * `hostfn/` imports nothing from `server/` (plan §3): everything here takes a `Db`
 * or a `TurnCtx`, so the whole module is testable with an in-memory database and no
 * server in sight.
 *
 * Ported from `src/state.ts`. Deltas are marked `NOTE:`.
 */
import { StateError } from "../errors.ts";
import { HOST_FN_VERBS } from "../harness/protocol.ts";
import type { Db, HostFns, TurnCtx } from "../types.ts";

// ---------------------------------------------------------------------------
// Limits
// ---------------------------------------------------------------------------

/** Per-value ceiling. Notes, not blobs — read the file yourself for anything bigger. */
export const MAX_VALUE_BYTES = 16_384;

/** Per-lineage ceiling on distinct keys, so a runaway loop cannot fill the database. */
export const MAX_KEYS = 200;

/** Keys are labels, not payloads. A 4KB key is a value in the wrong slot. */
export const MAX_KEY_CHARS = 200;

/** The verbs, from the canonical list the worker rebuilds `state.*` from. */
export const STATE_VERBS: readonly string[] = HOST_FN_VERBS.state;

/** What `state.list()` answers with: keys and sizes, never values. */
export interface StateEntry {
  key: string;
  bytes: number;
  updatedAt: number;
}

// ---------------------------------------------------------------------------
// Scope
// ---------------------------------------------------------------------------

/**
 * The session whose store this session shares — the root of its lineage.
 *
 * Walks `parentId` to the top (that is `Db.ancestorChain`), then hops the delegation
 * edge: a `subagent` or `workflow_agent` at the top of a parent chain continues from
 * its `originId`, because it is work its spawner started and shares the store with.
 * The `seen` set is not paranoia about a well-formed tree — it is what stops a cycle
 * introduced by a bad write from hanging every `state.*` call in the process.
 *
 * An unknown session is its own root, which is the only answer available and keeps
 * the verbs usable in a fixture that never created a session row.
 */
export function lineageRoot(db: Db, sessionId: string): string {
  const seen = new Set<string>();
  let id = sessionId;
  for (;;) {
    if (seen.has(id)) return id;
    seen.add(id);
    const root = db.ancestorChain(id)[0];
    if (!root) return id;
    const origin = root.originId ?? null;
    const delegated = root.kind === "subagent" || root.kind === "workflow_agent";
    if (delegated && origin && !seen.has(origin)) {
      id = origin;
      continue;
    }
    return root.id;
  }
}

// ---------------------------------------------------------------------------
// The verbs
// ---------------------------------------------------------------------------

/**
 * The key an argument names. `state.get(key)` sends the bare string and
 * `state.set({key, value})` sends the object, so both shapes are legal input and
 * the difference is the caller's convenience, not a contract.
 */
function requireKey(verb: string, args: unknown): string {
  const key = typeof args === "string" ? args : (args as { key?: unknown } | null)?.key;
  if (typeof key !== "string" || key.trim() === "") {
    throw new StateError(
      400,
      `state.${verb}: a non-empty string key is required — call it as ` +
        `state.${verb}("some-key").`,
    );
  }
  if (key.length > MAX_KEY_CHARS) {
    throw new StateError(
      400,
      `state.${verb}: key too long (${key.length} chars, max ${MAX_KEY_CHARS}). ` +
        `Keys are labels; put the long text in the value, or in a file.`,
    );
  }
  return key;
}

/**
 * One state verb against one lineage's store. `rootId` is resolved by the caller
 * (`lineageRoot`), so this function is pure with respect to lineage and the clock is
 * injected — the two things that make it testable without a turn.
 *
 * Every failure is a `StateError`, which the router renders as a 400 and the program
 * catches as an ordinary exception whose message names the verb, the state that
 * caused it, and the move that resolves it (spec §6).
 */
export function stateVerb(
  db: Db,
  rootId: string,
  verb: string,
  args: unknown,
  now: () => number = Date.now,
): unknown {
  switch (verb) {
    case "get": {
      const key = requireKey("get", args);
      const raw = db.getState(rootId, key);
      // An unset key reads as null rather than throwing: `(await state.get(k)) ??
      // fallback` is the natural idiom and a throw would make every read need a
      // try/catch.
      if (raw === undefined) return null;
      try {
        return JSON.parse(raw);
      } catch {
        // A row that is not JSON can only come from something outside this module
        // writing the table. Say so rather than crashing the program with a parse
        // error it cannot act on.
        throw new StateError(
          500,
          `state.get("${key}"): the stored value is not valid JSON — it was not ` +
            `written by state.set(). Overwrite it with state.set({key, value}) or ` +
            `remove it with state.delete().`,
        );
      }
    }

    case "set": {
      const a = (args ?? {}) as { key?: unknown; value?: unknown };
      const key = requireKey("set", a);
      // `undefined` does not survive JSON, so an omitted value and an explicit
      // `undefined` arrive identically. Both mean "unset", and unsetting has its own
      // verb — saying so beats writing the string "undefined".
      if (a.value === undefined) {
        throw new StateError(
          400,
          `state.set("${key}"): a value is required — call it as ` +
            `state.set({key, value}). Use state.delete("${key}") to unset it.`,
        );
      }
      let value: string;
      try {
        value = JSON.stringify(a.value);
      } catch (err) {
        throw new StateError(
          400,
          `state.set("${key}"): the value could not be serialized as JSON ` +
            `(${err instanceof Error ? err.message : String(err)}). state holds plain ` +
            `JSON — no functions, no class instances, no cycles.`,
        );
      }
      // A value that stringifies to `undefined` (a function, a symbol) is not a
      // value; `JSON.stringify` reports it by returning nothing at all.
      if (value === undefined) {
        throw new StateError(
          400,
          `state.set("${key}"): the value is not JSON (a function, a symbol, or ` +
            `undefined). state holds plain JSON — describe the thing, or write it to ` +
            `a file and store the path.`,
        );
      }
      const bytes = new TextEncoder().encode(value).length;
      if (bytes > MAX_VALUE_BYTES) {
        throw new StateError(
          400,
          `state.set("${key}"): value too large (${bytes} bytes, max ` +
            `${MAX_VALUE_BYTES}) — state holds notes, not payloads. Write the payload ` +
            `to a file and store its path here instead. Nothing was stored.`,
        );
      }
      // Counted only for a key that does not exist yet: overwriting an existing note
      // must keep working at the cap, or a lineage that filled up could not even
      // correct itself.
      if (db.getState(rootId, key) === undefined) {
        const used = db.listState(rootId).length;
        if (used >= MAX_KEYS) {
          throw new StateError(
            400,
            `state.set("${key}"): too many keys (${used}, max ${MAX_KEYS}). ` +
              `state.list() shows what is stored; state.delete(key) frees a slot. ` +
              `Nothing was stored.`,
          );
        }
      }
      db.setState(rootId, key, value, now());
      return { ok: true, key, bytes };
    }

    case "list":
      return db.listState(rootId) satisfies StateEntry[];

    case "delete": {
      const key = requireKey("delete", args);
      // `removed: false` rather than an error: "there was none" is an answer, and a
      // delete that has to be guarded by a get is two round-trips for nothing.
      return { ok: true, key, removed: db.deleteState(rootId, key) };
    }

    default:
      throw new StateError(
        400,
        `state: unknown verb "${verb}" — it is one of ${STATE_VERBS.join(", ")}.`,
      );
  }
}

// ---------------------------------------------------------------------------
// The bridged host function
// ---------------------------------------------------------------------------

/** Seams, so the verb is drivable with no server, no worker and no real lineage. */
export interface StateDeps {
  /** Pin the store's scope. Absent = resolved from the session's lineage per call. */
  rootId?: string;
  /** Injected clock. Absent = `ctx.now`, then `Date.now`. */
  now?: () => number;
}

/**
 * Build `state(verb, argsJson)` for one turn.
 *
 * The wire is string-only in both directions (`harness/protocol.ts`), so the worker
 * sends `JSON.stringify(args)` and re-inflates whatever comes back — which is why an
 * unset key must come back as the four characters `null` and not as an empty string.
 *
 * The lineage root is resolved per call rather than captured at construction: a turn
 * can outlive a lineage edit, and one `ancestorChain` walk per `state.*` call is
 * nothing next to the round-trip that carried it.
 */
export function createStateHostFn(
  ctx: TurnCtx,
  deps: StateDeps = {},
): Pick<HostFns, "state"> {
  const now = deps.now ?? ctx.now ?? Date.now;
  return {
    // Promise-in, promise-out. `harness/vm.ts` would catch a synchronous throw here
    // just the same, but a host function whose contract is a promise must FAIL as
    // one: a caller that only awaits (a test, a future direct consumer) would
    // otherwise see a throw where every sibling verb produces a rejection.
    state: (verb: string, argsJson: string): Promise<string> => {
      try {
        const args = parseArgs(verb, argsJson);
        const rootId = deps.rootId ?? lineageRoot(ctx.db, ctx.sessionId);
        const result = stateVerb(ctx.db, rootId, verb, args, now);
        return Promise.resolve(result === undefined ? "null" : JSON.stringify(result));
      } catch (err) {
        return Promise.reject(err);
      }
    },
  };
}

/**
 * The JSON envelope, parsed at the boundary (plan §0).
 *
 * The program is arbitrary model-written JavaScript, so a malformed argument is a
 * thing that happens; it must become a message the next round can act on rather than
 * a `SyntaxError` with no verb in it.
 */
function parseArgs(verb: string, argsJson: string): unknown {
  const text = (argsJson ?? "").trim();
  if (text === "") return null;
  try {
    return JSON.parse(text);
  } catch (err) {
    throw new StateError(
      400,
      `state.${verb}: the arguments could not be read as JSON ` +
        `(${err instanceof Error ? err.message : String(err)}). Pass a plain value, ` +
        `e.g. state.get("key") or state.set({key: "key", value: {…}}).`,
    );
  }
}
