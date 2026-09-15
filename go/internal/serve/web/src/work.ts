import { useCallback, useMemo, useState } from "react";
import { execNote, groupSubs, type SubAgent, type Turn } from "./render";
import type { Job, Line, Row } from "./types";

/*
 * The work index: every job, subagent and background agent a session
 * started, as one list the transcript, the Work button and the sidebar
 * all read. A state comes only from that worker's own records — never
 * from the parent's live flag, a wait limit or an aggregate count.
 */

export type WorkKind = "subagent" | "job" | "agent";
export type WorkLife = "queued" | "running" | "finished" | "failed" | "stopped" | "unknown";

export interface Worker {
  key: string;
  kind: WorkKind;
  /** The session that controls it: the owner for a job or subagent, the child itself for an agent. */
  session: string;
  /** Job id, subagent lane ("2") or child session id. */
  id: string;
  label: string;
  task: string;
  life: WorkLife;
  exit?: number;
  exitNote?: string;
  /** Only from a recorded duration or matching event timestamps; absent, never 0. */
  ms?: number;
  startedAt?: string;
  endedAt?: string;
  steps?: number;
  recordedSteps?: number;
  stepErrors: number;
  notRun: number;
  result?: string;
  error?: string;
  output?: string;
  outputState: "recorded" | "empty" | "none" | "not-recorded";
  cmd?: string;
  seq: number;
  subrunSeq?: number;
  live: boolean;
  canStop: boolean;
}

const TERMINAL = new Set<WorkLife>(["finished", "failed", "stopped", "unknown"]);

/** "4s" / "1m 5s" / "2h 3m" / "350ms" → milliseconds; null when it is not a duration. */
function parseDuration(s: string): number | null {
  const parts = s.trim().match(/\d+(?:\.\d+)?(?:ms|h|m|s)/g);
  if (!parts || parts.join("") !== s.trim().replace(/\s+/g, "")) return null;
  let ms = 0;
  for (const p of parts) {
    const n = parseFloat(p);
    ms += p.endsWith("ms") ? n : p.endsWith("h") ? n * 3600e3 : p.endsWith("m") ? n * 60e3 : n * 1e3;
  }
  return ms;
}

/**
 * The loop's notice for a job that ended: "job 7 [exited 1] make check (4s)\noutput"
 * or "job 7 [failed] make check (4s): exit status 1\noutput". A [failed] note
 * carries no exit code, so none is invented.
 */
export function parseLegacyJob(text: string): { id?: number; life: WorkLife; exit?: number; cmd: string; ms?: number; output: string } | null {
  const nl = text.indexOf("\n");
  const head = nl < 0 ? text : text.slice(0, nl);
  const output = nl < 0 ? "" : text.slice(nl + 1).trim();
  const m = /^job (\d+) \[([^\]]+)\] (.*?)(?: \(([^)]*)\))?(?:: .*)?$/.exec(head);
  if (!m) return null;
  const status = m[2];
  const ex = /^exited (-?\d+)$/.exec(status);
  const life: WorkLife = ex ? (+ex[1] === 0 ? "finished" : "failed") : status === "failed" ? "failed" : status === "running" ? "running" : "unknown";
  const ms = m[4] ? parseDuration(m[4]) : null;
  return { id: +m[1], life, ...(ex ? { exit: +ex[1] } : {}), cmd: m[3], ...(ms != null && ms > 0 ? { ms } : {}), output };
}

/**
 * A job's one-line title: its first command line that says something. The
 * loop's notice keeps only a first line plus "…", so a script opening with a
 * comment read as "# …"; a comment or a bare ellipsis falls through to "Job N".
 */
export function jobTitle(cmd: string, id: string | number): string {
  for (const raw of cmd.split("\n")) {
    const l = raw.trim().replace(/\s*(?:…|\.\.\.)$/, "").trim();
    if (l && !l.startsWith("#")) return l;
  }
  return `Job ${id}`;
}

const TREE = /[─-▟]+/g;
const ABS_PATH = /(?:~|\$HOME|\/)[^\s:'"()]*\/([^\s/:'"()]+)/g;

/**
 * The line a failed job's row shows: the last printed line with real text,
 * without tree glyphs ("└──") and with absolute paths cut to their basename.
 * `full` keeps the untouched line for a tooltip.
 */
export function jobSummaryLine(output: string): { text: string; full: string } | null {
  const lines = output.split("\n");
  for (let i = lines.length - 1; i >= 0; i--) {
    const bare = lines[i].replace(TREE, " ").replace(/\s+/g, " ").trim();
    if (!/[\p{L}\p{N}]/u.test(bare)) continue;
    return { text: bare.replace(ABS_PATH, "…/$1"), full: lines[i].trim() };
  }
  return null;
}

function blankWorker(kind: WorkKind, session: string, id: string, label: string, live: boolean, seq: number): Worker {
  return { key: `${session}:${kind}:${id}`, kind, session, id, label, task: "", life: "unknown", stepErrors: 0, notRun: 0, outputState: "none", seq, live, canStop: false };
}

/**
 * Jobs from typed entries ({id, event, cmd, until?, exit?}) merged by id
 * with the loop's legacy text notices, plus jobs the row says still run.
 */
export function jobsFromLines(lines: Line[], session: string, live: boolean, running?: Job[] | null): Worker[] {
  const byId = new Map<number, Worker & { startAt?: string; endAt?: string; typedEnd?: boolean }>();
  const get = (id: number, seq: number) => {
    let w = byId.get(id);
    if (!w) { w = blankWorker("job", session, String(id), `Job ${id}`, live, seq); byId.set(id, w); }
    return w;
  };
  for (const l of lines) {
    if (l.kind !== "job") continue;
    const d = l.data ?? {};
    if (typeof d.event === "string" && typeof d.id === "number") {
      const w = get(d.id, l.seq);
      if (typeof d.cmd === "string" && !w.cmd) w.cmd = d.cmd;
      if (d.event === "started") {
        w.startAt = l.at;
        if (w.life === "unknown" && !w.typedEnd && w.outputState === "none") w.life = "running";
      } else if (d.event === "finished") {
        w.endAt = l.at;
        w.typedEnd = true;
        w.seq = Math.max(w.seq, l.seq);
        if (typeof d.exit === "number") {
          w.exit = d.exit;
          w.life = d.exit === 0 ? "finished" : "failed";
          w.exitNote = undefined;
        } else if (w.exit === undefined && w.life !== "failed") {
          w.life = "unknown";
          w.exitNote = "Exit not recorded.";
        }
      }
      continue;
    }
    // "job N matched … while running" and other notices are not outcomes.
    const p = parseLegacyJob(String(d.text ?? l.text ?? ""));
    if (!p || p.id === undefined || p.life === "running") continue;
    const w = get(p.id, l.seq);
    w.seq = Math.max(w.seq, l.seq);
    w.cmd ||= p.cmd;
    if (!w.endAt) w.endAt = l.at;
    if (p.ms !== undefined) w.ms = p.ms;
    w.output = p.output;
    w.outputState = p.output ? "recorded" : "empty";
    // A typed exit is the better record; the notice fills what it lacks.
    if (w.exit === undefined) {
      w.life = p.life;
      if (p.exit !== undefined) w.exit = p.exit;
      w.exitNote = undefined;
    }
  }
  for (const j of running ?? []) {
    const w = get(j.id, 0);
    w.cmd ||= j.cmd;
    w.startedAt = j.started;
    if (!w.endAt) w.life = "running";
  }
  const out: Worker[] = [];
  for (const w of byId.values()) {
    const { startAt, endAt, typedEnd, ...rest } = w;
    void typedEnd;
    const r: Worker = rest;
    r.task = r.cmd ?? "";
    r.startedAt ??= startAt;
    if (endAt) r.endedAt = endAt;
    // A started job with no outcome is running only while its session is.
    if (r.life === "running" && !live) r.life = "unknown";
    if (r.ms === undefined && startAt && endAt) {
      const ms = Date.parse(endAt) - Date.parse(startAt);
      if (ms > 0) r.ms = ms;
    }
    if (r.outputState === "none" && TERMINAL.has(r.life)) r.outputState = "not-recorded";
    r.canStop = live && r.life === "running";
    out.push(r);
  }
  return out.sort((a, b) => +a.id - +b.id);
}

function text(v: unknown): string {
  return typeof v === "string" ? v : "";
}

function subLife(a: SubAgent, turn: Turn, live: boolean): WorkLife {
  const s = a.status;
  if (s === "ok" || s === "done" || s === "finished") return "finished";
  if (s === "stopped" || s === "cancelled") return "stopped";
  if (s) return "failed";
  // Running needs the worker's own start or activity and its run still open.
  const evidence = !!a.task || a.lines.length > 0;
  return live && !turn.done && evidence ? "running" : "unknown";
}

/** Subagents of one turn, one per lane per start, keyed by their SubRun. */
export function subagentsFromTurn(turn: Turn, session: string, live: boolean): Worker[] {
  const out: Worker[] = [];
  for (const it of groupSubs(turn.body)) {
    if (it.kind !== "sub") continue;
    for (const a of it.agents) {
      const w = blankWorker("subagent", session, a.worker, `Subagent ${a.worker}`, live, a.seq);
      w.key = `${session}:subagent:${it.seq}:${a.worker}:${a.seq}`;
      w.subrunSeq = it.seq;
      w.task = a.task;
      w.life = subLife(a, turn, live);
      w.startedAt = a.from;
      // The lane is recorded as a number or a string; groupSubs reads both.
      const done = turn.body.find((l) => l.kind === "sub:done" && l.seq > a.seq && (String(l.data?.worker ?? "") || "1") === a.worker);
      if (done) {
        w.seq = done.seq;
        w.endedAt = done.at;
        const ms = Date.parse(done.at) - Date.parse(a.from);
        if (ms > 0) w.ms = ms;
        const r = text(done.data?.text) || done.text;
        if (r) w.result = r;
        if (a.steps) w.steps = a.steps;
      }
      w.recordedSteps = a.lines.filter((l) => l.kind === "sub:code").length;
      const errs = a.lines.filter((l) => l.kind === "sub:error");
      w.stepErrors = errs.length;
      if (errs.length) w.error = errs[errs.length - 1].text;
      for (const l of a.lines) w.notRun += execNote(l.text)?.notRun ?? 0;
      w.canStop = false;
      out.push(w);
    }
  }
  return out;
}

const AGENT_LIFE: Partial<Record<Row["status"], WorkLife>> = {
  queued: "queued", running: "running", "needs-you": "running", error: "failed",
  stopped: "stopped", interrupted: "stopped", done: "finished",
};

/** Direct background agents of `parent`: from /children when loaded, else the session list. */
export function agentsFromRows(parent: Row, rows: Row[], children?: Row[] | null): Worker[] {
  const byId = new Map<string, Row>();
  for (const r of rows) if (r.spawnedBy === parent.id) byId.set(r.id, r);
  for (const r of children ?? []) byId.set(r.id, r);
  const out: Worker[] = [];
  for (const c of byId.values()) {
    const queued = c.status === "queued" || !!c.queued;
    const w = blankWorker("agent", c.id, c.id, c.title || (queued ? "Queued agent" : "Background agent"), c.live, 0);
    w.key = `agent:${c.id}`;
    w.task = c.title || c.summary || "";
    // An idle child that is no longer live ended its work.
    w.life = queued ? "queued" : AGENT_LIFE[c.status] ?? (c.status === "idle" ? (c.live ? "running" : "finished") : "unknown");
    if (TERMINAL.has(w.life)) w.endedAt = c.lastAt;
    w.canStop = c.live && w.life === "running";
    out.push(w);
  }
  return out;
}

/** One session's work: its jobs, its subagents, its direct child agents. */
export function workIndex(input: { session: string; lines: Line[]; turns: Turn[]; row: Row; rows: Row[]; children?: Row[] | null; live: boolean }): Worker[] {
  const { session, lines, turns, row, rows, children, live } = input;
  const subs = turns.flatMap((t) => subagentsFromTurn(t, session, live));
  return [...jobsFromLines(lines, session, live, row.jobs), ...subs, ...agentsFromRows(row, rows, children)];
}

export type WorkCounts = { running: number; queued: number; failed: number; unknown: number; stopped: number; finished: number; newResults: number; total: number };

export function workCounts(ws: Worker[], isNew?: (w: Worker) => boolean): WorkCounts {
  const c: WorkCounts = { running: 0, queued: 0, failed: 0, unknown: 0, stopped: 0, finished: 0, newResults: 0, total: ws.length };
  for (const w of ws) {
    c[w.life]++;
    if (isNew?.(w)) c.newResults++;
  }
  return c;
}

/**
 * The Work button's words. While anything runs: the live breakdown. Once
 * all ended: "7 finished", "3 stopped" or "8 workers · 1 failed". Failures
 * are never dropped; narrow splits the top-priority part onto line one.
 */
export function workSummaryText(c: WorkCounts, opts: { loading?: boolean; unavailable?: boolean; paused?: boolean; narrow?: boolean } = {}): { primary: string; secondary?: string; aria: string } {
  const n = (k: number, word: string) => (k ? [`${k} ${word}`] : []);
  const failed = n(c.failed, "failed"), unknown = n(c.unknown, "unknown"), fresh = n(c.newResults, c.newResults === 1 ? "new result" : "new results");
  let parts: string[];
  if (c.total === 0) parts = [];
  else if (c.running || c.queued) parts = [...n(c.running, "running"), ...n(c.queued, "queued"), ...failed, ...unknown, ...fresh, ...n(c.stopped, "stopped")];
  else if (c.finished === c.total) parts = [`${c.total} finished`, ...fresh];
  else if (c.stopped === c.total) parts = [`${c.total} stopped`, ...fresh];
  else parts = [`${c.total} ${c.total === 1 ? "worker" : "workers"}`, ...failed, ...unknown, ...n(c.stopped, "stopped"), ...fresh];
  const qual = opts.loading ? "Loading…" : opts.unavailable ? "Unavailable" : "";
  const aria = ["Work", ...parts, ...(qual ? [qual] : []), ...(opts.paused ? ["Updates paused"] : [])].join(", ");
  if (!parts.length) return { primary: `Work · ${qual || "0 workers"}`, aria };
  if (!opts.narrow) return { primary: ["Work", ...parts].join(" · "), aria };
  const order = [...failed, ...unknown, ...fresh, ...n(c.running, "running"), ...n(c.queued, "queued"), ...n(c.stopped, "stopped"), ...n(c.finished, "finished")];
  const all = c.running || c.queued || c.finished !== c.total ? order : parts;
  const [first, ...rest] = all;
  return { primary: `Work · ${first}`, ...(rest.length ? { secondary: rest.join(" · ") } : {}), aria };
}

/** What a review is pinned to: a worker that ends again (a replayed lane, a new exit) is unreviewed. */
export function workFingerprint(w: Worker): string {
  return [w.kind, w.id, w.life, w.exit ?? w.seq].join(":");
}

type Store = Pick<Storage, "getItem" | "setItem">;

function storage(): Store | null {
  try { return globalThis.localStorage ?? null; } catch { return null; }
}

export function loadReviewed(session: string, s: Store | null = storage()): Record<string, string> {
  try {
    const v = JSON.parse(s?.getItem(`bough:work-reviewed:${session}`) ?? "{}");
    return v && typeof v === "object" && !Array.isArray(v) ? v : {};
  } catch { return {}; }
}

export function saveReviewed(session: string, map: Record<string, string>, s: Store | null = storage()): void {
  try { s?.setItem(`bough:work-reviewed:${session}`, JSON.stringify(map)); } catch { /* private window */ }
}

/**
 * New: a background agent result or a successful job's recorded output
 * that ended after the page loaded, not yet reviewed. History never is.
 */
export function isNewWork(w: Worker, loadedAt: number, reviewed: Record<string, string>): boolean {
  if (reviewed[w.key] === workFingerprint(w)) return false;
  const eligible = w.kind === "agent" ? !!w.result && w.life === "finished" : w.kind === "job" && w.life === "finished" && w.outputState === "recorded";
  if (!eligible || !w.endedAt) return false;
  return Date.parse(w.endedAt) > loadedAt;
}

export function useReviewed(session: string): { isReviewed(w: Worker): boolean; markReviewed(w: Worker): void; isNew(w: Worker): boolean } {
  const [state, setState] = useState(() => ({ session, map: loadReviewed(session), loadedAt: Date.now() }));
  const cur = state.session === session ? state : { session, map: loadReviewed(session), loadedAt: Date.now() };
  if (cur !== state) setState(cur);
  const markReviewed = useCallback((w: Worker) => {
    setState((s) => {
      const fp = workFingerprint(w);
      if (s.map[w.key] === fp) return s;
      const map = { ...loadReviewed(s.session), [w.key]: fp };
      saveReviewed(s.session, map);
      return { ...s, map };
    });
  }, []);
  return useMemo(() => ({
    isReviewed: (w: Worker) => cur.map[w.key] === workFingerprint(w),
    markReviewed,
    isNew: (w: Worker) => isNewWork(w, cur.loadedAt, cur.map),
  }), [cur, markReviewed]);
}

/** The loop's prefix for a turn a finished background job starts on its own. */
export const JOB_WAKE_PREFIX = "[background job] ";

/**
 * The job notes of a background-job wake-up turn, or null for a prompt a
 * person typed. That turn is recorded as an input, so it rendered as if you
 * had typed the model's instruction and the raw "job 8 [failed] …" lines.
 */
export function jobWakeNotes(text: string): string[] | null {
  if (!text.startsWith(JOB_WAKE_PREFIX)) return null;
  const notes: string[] = [];
  for (const l of text.split("\n")) {
    if (/^job \d+ \[/.test(l)) notes.push(l);
    else if (notes.length && l.trim()) notes[notes.length - 1] += "\n" + l;
  }
  return notes.map((n) => n.trimEnd());
}
