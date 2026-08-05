/** Replay a real incident through the shipped echo logic. usage: bun replay.ts */
import { Database } from "bun:sqlite";
const db = new Database(`${process.env.HOME}/.bough/bough.db`, { readonly: true });
db.run("PRAGMA query_only = ON");

const ERROR_CHARS = 220, WINDOW = 24 * 3600_000, SCAN = 400, SPREAD_MIN = 2, LOOP_MS = 120_000;
const sig = (o: string) => {
  const l = (o ?? "").split("\n").map((x) => x.trim())
    .find((x) => x !== "" && !/^\[exit code -?\d+\]$/.test(x));
  return !l ? "" : l.length > ERROR_CHARS ? l.slice(0, ERROR_CHARS) + "…" : l;
};

type R = { id: number; cmd: string; ts: number; repo: string; session_id: string; output_head: string };
const target = db.query<R, []>(
  `SELECT id, cmd, ts, repo, session_id, output_head FROM command_history
    WHERE output_head LIKE 'invalid argument "merged"%' ORDER BY ts`).all();

let noted = 0, guarded = 0, firstNoteAt = -1;
for (let i = 0; i < target.length; i++) {
  const r = target[i];
  // (1) byte-exact guard: identical cmd, same session, 3+ in the last 2 minutes
  const ident = db.query<{ n: number }, [string, string, string, number]>(
    `SELECT count(*) n FROM command_history WHERE repo = ? AND cmd = ? AND session_id = ?
       AND ts >= ? AND ts < ${r.ts} AND exit_code IS NOT NULL AND exit_code <> 0`)
    .get(r.repo, r.cmd, r.session_id, r.ts - LOOP_MS)!;
  if (ident.n >= 3) { guarded++; continue; }
  // (2) error signature across DISTINCT commands in the last 24h
  const prior = db.query<{ cmd: string; output_head: string }, [string, number, number]>(
    `SELECT cmd, output_head FROM command_history WHERE repo = ? AND ts >= ? AND ts < ?
       AND exit_code IS NOT NULL AND exit_code <> 0 ORDER BY ts DESC LIMIT ${SCAN}`)
    .all(r.repo, r.ts - WINDOW, r.ts);
  const mine = sig(r.output_head);
  const spread = new Set(prior.filter((p) => p.cmd !== r.cmd && sig(p.output_head) === mine).map((p) => p.cmd));
  if (spread.size >= SPREAD_MIN) { noted++; if (firstNoteAt < 0) firstNoteAt = i + 1; }
}
console.log(`incident rows: ${target.length}`);
console.log(`would be SKIPPED by the byte-exact guard: ${guarded}`);
console.log(`would carry the error-signature note:     ${noted}`);
console.log(`first note fires at attempt:              ${firstNoteAt}`);
console.log(`silent:                                   ${target.length - guarded - noted}`);
