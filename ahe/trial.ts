/**
 * One trial: a fresh copy of the fixture, one headless turn, one verdict.
 *
 * THE VERDICT COMES FROM THE WORKSPACE, NEVER THE TRANSCRIPT. `verify.sh` reads the
 * code the agent left behind and nothing else — not its summary, not whether it
 * said "done", not whether the tests it chose to run passed. An agent that reports
 * success on broken code and one that reports failure on working code must land on
 * opposite sides of this line, and only the environment can put them there.
 *
 * Every trial gets a NEW workspace and a NEW session. Sharing either would let one
 * trial's leftovers decide the next one's outcome, which is the failure mode that
 * makes a bench quietly stop measuring anything.
 */
import { spawn } from "node:child_process";
import { existsSync, mkdirSync, rmSync } from "node:fs";
import { join } from "node:path";
import { MODEL, PORT, REPO, TASKS_DIR, TRIAL_TIMEOUT_S, WORK_DIR } from "./config.ts";

export interface TrialRow {
  task: string;
  trial: number;
  /** Which harness iteration produced this row. */
  iteration: number;
  sessionId: string | null;
  pass: boolean;
  /** The verifier's first line on failure — the taxonomy, not a stack trace. */
  failReason: string | null;
  status: string;
  durationMs: number;
  costUsd: number | null;
  outputTokens: number | null;
  workspace: string;
}

function run(
  cmd: string[],
  opts: { cwd?: string; env?: Record<string, string>; timeoutMs?: number } = {},
): Promise<{ code: number; stdout: string; stderr: string }> {
  return new Promise((resolve) => {
    const child = spawn(cmd[0], cmd.slice(1), {
      cwd: opts.cwd ?? REPO,
      env: { ...process.env, ...opts.env },
    });
    let stdout = "";
    let stderr = "";
    child.stdout.on("data", (d) => (stdout += d));
    child.stderr.on("data", (d) => (stderr += d));
    const timer = opts.timeoutMs
      ? setTimeout(() => child.kill("SIGKILL"), opts.timeoutMs)
      : undefined;
    child.on("close", (code) => {
      if (timer) clearTimeout(timer);
      resolve({ code: code ?? -1, stdout, stderr });
    });
  });
}

/** Lay down a pristine copy of the task's fixture. */
function stageWorkspace(task: string, trial: number, iteration: number): string {
  const ws = join(WORK_DIR, `${task}-i${iteration}-t${trial}`);
  rmSync(ws, { recursive: true, force: true });
  mkdirSync(ws, { recursive: true });
  const fixture = join(TASKS_DIR, task, "fixture");
  if (!existsSync(fixture)) throw new Error(`task ${task} has no fixture/`);
  // `cp -R src/. dst` copies CONTENTS including dotfiles, without nesting.
  spawn("cp", ["-R", `${fixture}/.`, ws]).unref();
  return ws;
}

export async function runTrial(
  task: string,
  trial: number,
  iteration: number,
): Promise<TrialRow> {
  const ws = stageWorkspace(task, trial, iteration);
  await Bun.sleep(150); // the cp above is detached; the fixtures are tiny
  const prompt = await Bun.file(join(TASKS_DIR, task, "prompt.md")).text();

  const started = Date.now();
  const exec = await run([
    "bun",
    "src/cli/exec.ts",
    "-w",
    ws,
    "-m",
    MODEL,
    "--json",
    "--timeout",
    String(TRIAL_TIMEOUT_S),
    prompt,
  ], {
    env: { BOUGH_PORT: String(PORT) },
    // The CLI raises its own interrupt on `--timeout`; this is the outer backstop
    // for the case where the CLI itself is what wedged.
    timeoutMs: (TRIAL_TIMEOUT_S + 60) * 1000,
  });
  const durationMs = Date.now() - started;

  const line = exec.stdout.trim().split("\n").filter((l) => l.startsWith("{")).pop();
  const envelope = line ? JSON.parse(line) : null;

  const verdict = await run(["bash", join(TASKS_DIR, task, "verify.sh"), ws], {
    timeoutMs: 120_000,
  });
  const pass = verdict.code === 0;

  return {
    task,
    trial,
    iteration,
    sessionId: envelope?.session ?? null,
    pass,
    // First line only: "the spec is still violated" is the bucket, and the diff
    // underneath it belongs in the trace, not in a results row.
    failReason: pass ? null : (verdict.stdout.trim().split("\n")[0] || "verifier crashed"),
    status: envelope?.status ?? (exec.code === 0 ? "unknown" : "exec-failed"),
    durationMs,
    costUsd: envelope?.usage?.costUsd ?? null,
    outputTokens: envelope?.usage?.outputTokens ?? null,
    workspace: ws,
  };
}
