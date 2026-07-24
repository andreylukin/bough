/**
 * Durable key/value notes the program keeps for itself across turns — the
 * `state.*` host function (get/set/list/delete), fanned out like schedule.*.
 *
 * Why it exists: the only thing carrying facts from one round to the next today is
 * the transcript, and the transcript is exactly what compaction, forking and the
 * context cap eventually eat. A long task ("port 40 files, one per turn") therefore
 * re-derives its bookkeeping every round. state.set() puts that bookkeeping in
 * SQLite instead, at a cost of one line of context.
 *
 * Scope is the ROOT session of the lineage (db.ancestorChain), not the session id:
 * a fork, a compaction child and a subagent are all the same piece of work from the
 * user's point of view, and the store should follow it. Cross-conversation memory is
 * deliberately NOT offered here — that was scoped out; recall() covers "what did I
 * do before".
 *
 * Values are arbitrary JSON, capped hard (see MAX_VALUE_BYTES) because a program
 * that dumps a build log in here would re-inflate the very context this is meant to
 * spare. Verb errors are HttpError like schedules.ts, so the message the program
 * catches names the verb and what was wrong.
 */
import { HttpError } from "./errors.ts";
import type { Db } from "./db/db.ts";

/** Per-value ceiling: notes, not blobs. Read the file yourself for anything bigger. */
const MAX_VALUE_BYTES = 16_384;
/** Per-lineage ceiling on distinct keys — a runaway loop can't fill the DB. */
const MAX_KEYS = 200;

function requireKey(verb: string, args: unknown): string {
  const key = typeof args === "string" ? args : (args as { key?: unknown })?.key;
  if (typeof key !== "string" || !key.trim()) {
    throw new HttpError(400, `state.${verb}: key (non-empty string) required`);
  }
  if (key.length > 200) throw new HttpError(400, `state.${verb}: key too long (max 200 chars)`);
  return key;
}

/**
 * One state verb against a lineage's store. `rootId` is resolved by the caller
 * (turn.ts) from the session's ancestor chain.
 */
export function stateVerb(db: Db, rootId: string, verb: string, args: unknown): unknown {
  switch (verb) {
    case "get": {
      const key = requireKey("get", args);
      const raw = db.getState(rootId, key);
      // Unset reads as null rather than throwing: `?? default` is the natural idiom.
      return raw === undefined ? null : JSON.parse(raw);
    }
    case "set": {
      const a = args as { key?: unknown; value?: unknown };
      if (typeof a?.key !== "string" || !a.key.trim()) {
        throw new HttpError(400, "state.set: {key, value} required (key: non-empty string)");
      }
      const key = requireKey("set", a.key);
      if (a.value === undefined) {
        throw new HttpError(400, "state.set: value required (use state.delete to unset)");
      }
      const value = JSON.stringify(a.value);
      const bytes = new TextEncoder().encode(value).length;
      if (bytes > MAX_VALUE_BYTES) {
        throw new HttpError(
          400,
          `state.set: value too large (${bytes} bytes, max ${MAX_VALUE_BYTES}) — ` +
            `state holds notes, not payloads; write the payload to a file and store its path`,
        );
      }
      if (
        db.getState(rootId, key) === undefined && db.listState(rootId).length >= MAX_KEYS
      ) {
        throw new HttpError(400, `state.set: too many keys (max ${MAX_KEYS}) — delete some first`);
      }
      db.setState(rootId, key, value, Date.now());
      return { ok: true, key, bytes };
    }
    case "list":
      return db.listState(rootId);
    case "delete": {
      const key = requireKey("delete", args);
      return { ok: true, key, removed: db.deleteState(rootId, key) };
    }
    default:
      throw new HttpError(400, `unknown state verb: ${verb} (get|set|list|delete)`);
  }
}
