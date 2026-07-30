/**
 * Where everything lives, and the two constants an experiment must never vary
 * by accident.
 *
 * THE MODEL IS PINNED AND THE HARNESS IS NOT. That is the entire experimental
 * design: AHE holds the base model fixed and edits only the harness, so a pass-rate
 * change has one candidate cause. A sweep that quietly picked up a different model
 * would be measuring the model, and would look exactly like a successful harness
 * edit while doing it.
 */
import { join } from "node:path";

/** The repo root — this file's parent's parent. Nothing here reads `cwd`. */
export const REPO = join(import.meta.dir, "..");

/** Frozen for every sweep. Cheap enough to run 40 tasks nightly, weak enough to discriminate. */
export const MODEL = "claude-haiku-4-5";

/** The meta-agents (analyze, evolve) run on a DIFFERENT harness than the one under test. */
export const META_CMD = "claude";

/**
 * The bench server is isolated from the user's own bough: its own home, its own
 * database, its own port. A sweep that wrote into ~/.bough would put hundreds of
 * throwaway sessions into the daily driver's history, and a crashed sweep would
 * take the real server's port with it.
 */
export const PORT = Number(process.env["AHE_PORT"] ?? 4599);
export const HOME = process.env["AHE_HOME"] ?? join(REPO, "ahe", "state", "home");
export const TRACE_DIR = join(REPO, "ahe", "state", "trace");

export const TASKS_DIR = join(REPO, "ahe", "tasks");
export const RUNS_DIR = join(REPO, "ahe", "runs");
/** Scratch workspaces. Outside the repo: a trial's checkout is not the user's diff. */
export const WORK_DIR = process.env["AHE_WORK"] ?? "/tmp/ahe-work";

/** One trial's wall-clock ceiling. A hung turn must cost one trial, not the sweep. */
export const TRIAL_TIMEOUT_S = Number(process.env["AHE_TIMEOUT"] ?? 300);

/** The editable action space: the prompt sections, and nothing else. */
export const PROMPT_DIR = join(REPO, "src", "prompt");

export const iterDir = (n: number): string =>
  join(RUNS_DIR, `iteration_${String(n).padStart(3, "0")}`);
