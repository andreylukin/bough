/**
 * The outer loop: evaluate → analyze → improve → verify the last round's promise.
 *
 * THE STAGGER IS THE POINT. An iteration's edits are not judged by the sweep that
 * motivated them — they cannot be, they did not exist yet. They are judged by the
 * NEXT iteration's sweep, which is the first evidence that exists about them. So
 * iteration N does three things in this order: score the edits N-1 made, analyze
 * what is still broken, and make its own edits for N+1 to score. Every edit is a
 * falsifiable contract with a fixed settlement date.
 *
 * WHAT "REVERTED" MEANS HERE. `git checkout` of the section files an edit touched.
 * The action space is files, so a rollback is exact — no partial undo, no "I removed
 * the sentence I think I added". This is the entire reason component observability
 * is worth the trouble.
 *
 * A NOTE ON WHAT THIS LOOP CANNOT SEE. AHE reports that self-attribution is reliable
 * for fixes and blind to regressions: an edit that fixes one task and quietly breaks
 * another gets credited for the fix. The paired-flip check below is the mitigation —
 * a net-negative round is reverted even if its predicted task did flip — but it is a
 * mitigation, not a solution, and with a small bank noise will sometimes wear the
 * costume of either outcome.
 */
import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { iterDir, PROMPT_DIR, REPO } from "./config.ts";
import { analyze, evolve, readManifest, type ChangeEntry } from "./agents.ts";
import { sweep, type SweepResult } from "./sweep.ts";

const git = (...args: string[]): string =>
  execFileSync("git", args, { cwd: REPO, encoding: "utf8" }).trim();

/** task → pass count, for comparing two sweeps. */
const scores = (r: SweepResult): Record<string, number> =>
  Object.fromEntries(Object.entries(r.byTask).map(([t, b]) => [t, b.pass]));

export interface Verdict {
  file: string;
  predicted_pass: string[];
  /** Tasks that actually improved, and that got worse, between the two sweeps. */
  flipped_to_pass: string[];
  flipped_to_fail: string[];
  /** Did at least one predicted task improve, with no net regression? */
  held: boolean;
  reverted: boolean;
}

/**
 * Settle the previous round's predictions against this round's sweep.
 *
 * The rule is deliberately strict in one direction: a prediction "holds" only if
 * something it named actually improved AND the round did not lose more elsewhere
 * than it gained. An edit that moves nothing is not neutral — it is prompt text that
 * earns no keep, and the growth it adds dilutes everything around it.
 */
export function settle(
  changes: ChangeEntry[],
  before: SweepResult,
  after: SweepResult,
): Verdict[] {
  const a = scores(before);
  const b = scores(after);
  const tasks = [...new Set([...Object.keys(a), ...Object.keys(b)])];
  const up = tasks.filter((t) => (b[t] ?? 0) > (a[t] ?? 0));
  const down = tasks.filter((t) => (b[t] ?? 0) < (a[t] ?? 0));
  const net = up.length - down.length;

  return changes.map((change) => {
    const hit = change.predicted_pass.filter((t) => up.includes(t));
    const held = hit.length > 0 && net >= 0;
    return {
      file: change.file,
      predicted_pass: change.predicted_pass,
      flipped_to_pass: up,
      flipped_to_fail: down,
      held,
      reverted: !held,
    };
  });
}

/** Undo the section files a refuted round touched. */
function revert(files: string[]): void {
  for (const file of files) {
    const path = join(PROMPT_DIR, file);
    if (!existsSync(path)) continue;
    git("checkout", "HEAD", "--", path);
  }
}

export async function runLoop(iterations: number, opts: { trials?: number } = {}): Promise<void> {
  let previous: SweepResult | null = null;
  let pending: ChangeEntry[] = [];

  for (let n = 1; n <= iterations; n++) {
    const dir = iterDir(n);
    mkdirSync(join(dir, "analysis", "detail"), { recursive: true });
    console.log(`\n═══ iteration ${n} ═══`);

    // The prompt this sweep ran with, recorded before anything can change it. The
    // per-turn manifests carry the shas; this carries the text.
    writeFileSync(join(dir, "prompt-sha.txt"), git("rev-parse", "HEAD:src/prompt"));

    console.log("evaluate…");
    const result = await sweep(n, { trials: opts.trials });
    console.log(
      `  ${(result.passRate * 100).toFixed(1)}% · $${result.costUsd.toFixed(3)}`,
    );

    // Settle what the LAST round promised, before proposing anything new — a
    // refuted edit must not still be in the prompt when the next one is written on
    // top of it.
    if (previous && pending.length) {
      const verdicts = settle(pending, previous, result);
      writeFileSync(
        join(dir, "change_evaluation.json"),
        JSON.stringify({ iteration: n - 1, verdicts }, null, 2),
      );
      const refuted = verdicts.filter((v) => v.reverted).map((v) => v.file);
      for (const v of verdicts) {
        console.log(
          `  ${v.held ? "HELD" : "REFUTED"} ${v.file} — predicted ${
            v.predicted_pass.join(",") || "nothing"
          }, gained ${v.flipped_to_pass.join(",") || "nothing"}, lost ${
            v.flipped_to_fail.join(",") || "nothing"
          }`,
        );
      }
      if (refuted.length) {
        revert(refuted);
        console.log(`  reverted ${refuted.join(", ")}`);
      } else if (verdicts.length) {
        git("add", PROMPT_DIR);
        git("commit", "-m", `ahe(iter ${n - 1}): ${pending.map((c) => c.file).join(", ")}`);
      }
    }

    console.log("analyze…");
    await analyze(dir, result);

    console.log("improve…");
    await evolve(dir, result);
    pending = readManifest(dir);
    console.log(
      pending.length
        ? `  ${pending.length} edit(s): ${pending.map((c) => c.file).join(", ")}`
        : "  no edit proposed",
    );
    previous = result;
  }
}

if (import.meta.main) {
  const n = Number(process.argv[2] ?? 3);
  const trials = Number(process.env["AHE_TRIALS"] ?? 3);
  await runLoop(n, { trials });
}
