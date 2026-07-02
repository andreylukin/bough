/**
 * net_events persistence for the Network rail. Lives apart from db.ts (which the
 * turn-runner owns this wave) as its own thin store over a DatabaseSync handle on the
 * same file — net_events has no foreign keys, so a second connection is safe and the
 * two stores never contend (node:sqlite is synchronous; writes serialize on the event
 * loop). The table shape mirrors db.ts's net_events exactly; CREATE IF NOT EXISTS makes
 * either owner's creation a no-op for the other.
 *
 * A request may be written twice — first as `pending` (held), then resolved to
 * allowed/denied — so we upsert by id (INSERT OR REPLACE): the row always reflects the
 * request's latest verdict, which is what the rail and the approval card render.
 */
import { DatabaseSync } from "node:sqlite";
import type { NetRequest } from "../schema/parts.ts";

const SCHEMA = `
CREATE TABLE IF NOT EXISTS net_events (
  id           TEXT PRIMARY KEY,
  session_id   TEXT,
  host         TEXT NOT NULL,
  verb         TEXT,
  action       TEXT NOT NULL,
  verdict      TEXT NOT NULL,
  reason       TEXT,
  requested_by TEXT,
  ts           INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS net_events_session ON net_events(session_id, ts);
`;

type NetRow = {
  id: string;
  session_id: string | null;
  host: string;
  verb: string | null;
  action: string;
  verdict: string;
  reason: string | null;
  requested_by: string | null;
  ts: number;
};

function toNetRequest(r: NetRow): NetRequest {
  const out: NetRequest = {
    id: r.id,
    host: r.host,
    action: r.action,
    verdict: r.verdict as NetRequest["verdict"],
    ts: r.ts,
  };
  if (r.verb != null) out.verb = r.verb;
  if (r.reason != null) out.reason = r.reason;
  if (r.requested_by != null) out.requestedBy = r.requested_by;
  return out;
}

export class NetStore {
  #db: DatabaseSync;

  constructor(path: string) {
    this.#db = new DatabaseSync(path);
    this.#db.exec(SCHEMA);
  }

  close(): void {
    this.#db.close();
  }

  /** Insert or update (by id) the row for a net request. */
  upsert(sessionId: string | undefined, r: NetRequest): void {
    this.#db
      .prepare(
        `INSERT OR REPLACE INTO net_events
           (id, session_id, host, verb, action, verdict, reason, requested_by, ts)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)`,
      )
      .run(
        r.id,
        sessionId ?? null,
        r.host,
        r.verb ?? null,
        r.action,
        r.verdict,
        r.reason ?? null,
        r.requestedBy ?? null,
        r.ts,
      );
  }

  /** Recent rows, newest first. Filtered by session when given. */
  recent(sessionId?: string, limit = 100): NetRequest[] {
    const rows = (sessionId
      ? this.#db
        .prepare(
          `SELECT * FROM net_events WHERE session_id = ? ORDER BY ts DESC, rowid DESC LIMIT ?`,
        )
        .all(sessionId, limit)
      : this.#db
        .prepare(`SELECT * FROM net_events ORDER BY ts DESC, rowid DESC LIMIT ?`)
        .all(limit)) as NetRow[];
    return rows.map(toNetRequest);
  }
}

export function openNetStore(path: string): NetStore {
  return new NetStore(path);
}
