// The work index a Thread builds, without the Thread around it: stories
// put a turn, a job row or the Work dialog inside it and get the same
// states, review and Stop behaviour the app has.
import { useEffect, useMemo } from "react";
import { groupTurns } from "../render";
import type { Line, Row } from "../types";
import { WorkContext, agentReports, jobIdOf, useStopStore, useWork, type WorkCtx } from "../work-ui";
import { useReviewed, workIndex, type Worker } from "../work";

export function WorkFrame({ row, lines, rows = [], kids = null, children }: {
  row: Row; lines: Line[]; rows?: Row[]; kids?: Row[] | null; children: React.ReactNode;
}) {
  const turns = useMemo(() => groupTurns(lines), [lines]);
  const review = useReviewed(row.id);
  const reports = useMemo(() => agentReports(lines), [lines]);
  const workers = useMemo(() => workIndex({ session: row.id, lines, turns, row, rows, children: kids, live: row.live })
    .map((w) => (w.kind === "agent" && reports.has(w.id) ? { ...w, result: reports.get(w.id) } : w)), [row, lines, turns, rows, kids, reports]);
  const byKey = useMemo(() => new Map(workers.map((w) => [w.key, w])), [workers]);
  const { stops, requestStop } = useStopStore(byKey, review);
  const ctx = useMemo<WorkCtx>(() => {
    const jobs = new Map(workers.filter((w) => w.kind === "job").map((w) => [w.id, w]));
    const jobFirst = new Map<string, number>();
    for (const l of lines) {
      const id = l.kind === "job" ? jobIdOf(l) : undefined;
      if (id !== undefined && !jobFirst.has(String(id))) jobFirst.set(String(id), l.seq);
    }
    return { session: row.id, live: row.live, workers, byKey, jobs, jobFirst, review, stops, requestStop };
  }, [row.id, row.live, workers, byKey, lines, review, stops, requestStop]);
  return <WorkContext.Provider value={ctx}>{children}</WorkContext.Provider>;
}

/** Presses Stop on the first worker `pick` finds, once, after `delay` ms. */
export function AutoStop({ pick, delay = 800, fromWork }: { pick: (w: Worker) => boolean; delay?: number; fromWork?: boolean }) {
  const ctx = useWork();
  const target = ctx?.workers.find(pick);
  const key = target?.key;
  useEffect(() => {
    if (!key) return;
    const t = setTimeout(() => { const w = ctx?.byKey.get(key); if (w?.canStop) ctx?.requestStop(w, fromWork); }, delay);
    return () => clearTimeout(t);
  }, [key]); // eslint-disable-line react-hooks/exhaustive-deps
  return null;
}
