/**
 * Evaluate: run every task k times, materialize every trace, report.
 *
 * TRIALS ARE SEQUENTIAL. Parallel trials would finish a sweep faster and would also
 * put several turns through one server, one machine and one rate limit at once —
 * which shows up as timeouts and truncated streams on exactly the hard tasks, i.e.
 * as harness failures that are really contention. A sweep that is fast and wrong is
 * worse than one that takes an hour.
 *
 * `k` MATTERS MORE THAN IT LOOKS. A single trial per task cannot distinguish an edit
 * that helped from a task that flips on its own; the loop's whole falsification step
 * rests on being able to tell those apart. k=3 is the floor.
 */
import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { iterDir, TASKS_DIR } from "./config.ts";
import { materialize } from "./materialize.ts";
import { start } from "./server.ts";
import { runTrial, type TrialRow } from "./trial.ts";

export interface SweepResult {
  rows: TrialRow[];
  /** task → how many of its trials passed. */
  byTask: Record<string, { pass: number; of: number }>;
  passRate: number;
  costUsd: number;
}

export function tasksInBank(): string[] {
  return [...new Bun.Glob("*/prompt.md").scanSync({ cwd: TASKS_DIR })]
    .map((p) => p.split("/")[0])
    .sort();
}

export async function sweep(
  iteration: number,
  opts: { tasks?: string[]; trials?: number } = {},
): Promise<SweepResult> {
  const tasks = opts.tasks?.length ? opts.tasks : tasksInBank();
  const trials = opts.trials ?? 3;
  const dir = iterDir(iteration);
  mkdirSync(dir, { recursive: true });

  await start();

  const rows: TrialRow[] = [];
  for (const task of tasks) {
    for (let trial = 1; trial <= trials; trial++) {
      const row = await runTrial(task, trial, iteration);
      rows.push(row);
      // Materialize immediately: a sweep that dies at trial 30 still leaves 29
      // readable traces, and the trace directory is the expensive artifact.
      materialize(row, join(dir, "traces", task, `trial-${trial}`));
      writeFileSync(
        join(dir, "results.jsonl"),
        rows.map((r) => JSON.stringify(r)).join("\n") + "\n",
      );
      console.log(
        `  ${task} t${trial}: ${row.pass ? "PASS" : `FAIL (${row.failReason})`} ` +
          `${(row.durationMs / 1000).toFixed(0)}s $${(row.costUsd ?? 0).toFixed(4)}`,
      );
    }
  }

  const byTask: SweepResult["byTask"] = {};
  for (const row of rows) {
    const b = byTask[row.task] ??= { pass: 0, of: 0 };
    b.of++;
    if (row.pass) b.pass++;
  }
  const result: SweepResult = {
    rows,
    byTask,
    passRate: rows.filter((r) => r.pass).length / (rows.length || 1),
    costUsd: rows.reduce((sum, r) => sum + (r.costUsd ?? 0), 0),
  };
  writeFileSync(join(dir, "summary.json"), JSON.stringify({ ...result, rows: undefined }, null, 2));
  return result;
}

if (import.meta.main) {
  const args = process.argv.slice(2);
  const trials = Number(args.find((a) => a.startsWith("-n"))?.slice(2) ?? 3);
  const tasks = args.filter((a) => !a.startsWith("-"));
  const iteration = Number(process.env["AHE_ITERATION"] ?? 0);
  const result = await sweep(iteration, { tasks, trials });
  console.log(
    `\npass ${(result.passRate * 100).toFixed(1)}% · $${result.costUsd.toFixed(3)} · ` +
      Object.entries(result.byTask).map(([t, b]) => `${t} ${b.pass}/${b.of}`).join(" · "),
  );
}
