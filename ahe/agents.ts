/**
 * The two meta-agents: the one that reads traces, and the one that edits prompts.
 *
 * THEY RUN ON A DIFFERENT HARNESS THAN THE ONE UNDER TEST. Both are `claude -p`,
 * deliberately: if bough analyzed its own traces and edited its own prompt, a
 * regression in the harness would degrade the very loop meant to detect it, and a
 * bad round could take the next round's judgement down with it. The thing being
 * measured must not be the thing doing the measuring.
 *
 * THE EVOLVE AGENT'S ACTION SPACE IS `src/prompt/*.md` AND NOTHING ELSE. That is a
 * deliberate narrowing of AHE, which also evolves tools, middleware and memory. It
 * is also the narrowing AHE's own ablation warns about — the system prompt was the
 * one component that regressed alone. The counterweight here is that most of these
 * files are not prose strategy: `shell.md`, `patch-grammar.md` and `files.md` are
 * the host functions' interface documentation, which is the half of "tools" that
 * carried the paper's gain. An edit to `identity.md` is the risky kind; an edit to
 * `patch-grammar.md` because `patch()` was rejected four times is the good kind, and
 * the evidence requirement below is what keeps the agent on the second sort.
 */
import { spawn } from "node:child_process";
import { readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { META_CMD, PROMPT_DIR, REPO } from "./config.ts";
import type { SweepResult } from "./sweep.ts";

/** Run `claude -p` in `cwd` and return its final text. */
export function meta(prompt: string, cwd: string, timeoutMs = 25 * 60_000): Promise<string> {
  return new Promise((resolve, reject) => {
    const child = spawn(META_CMD, [
      "-p",
      prompt,
      "--permission-mode",
      "acceptEdits",
      "--add-dir",
      REPO,
    ], { cwd });
    let out = "";
    child.stdout.on("data", (d) => (out += d));
    child.stderr.on("data", (d) => process.stderr.write(d));
    const timer = setTimeout(() => child.kill("SIGKILL"), timeoutMs);
    child.on("close", (code) => {
      clearTimeout(timer);
      code === 0 ? resolve(out) : reject(new Error(`${META_CMD} exited ${code}`));
    });
  });
}

/** A compact scoreboard — the analyzer gets the numbers, then goes and reads. */
function scoreboard(result: SweepResult): string {
  const waste = `\n\nAcross ${result.rows.length} trials: ${result.rounds} rounds, ` +
    `${result.hostFnErrors} host-function calls came back an error, of which ` +
    `${result.parseErrors} were programs that never parsed at all. A round that never ` +
    `parsed taught the agent nothing about the task — it is pure harness waste, and it ` +
    `is worth investigating even on a task that passed.`;
  return Object.entries(result.byTask)
    .map(([task, b]) => {
      const reasons = result.rows
        .filter((r) => r.task === task && !r.pass)
        .map((r) => r.failReason)
        .filter((v, i, a) => a.indexOf(v) === i);
      return `- ${task}: ${b.pass}/${b.of}${reasons.length ? ` — ${reasons.join(" / ")}` : ""}`;
    })
    .join("\n") + waste;
}

/**
 * Analyze: traces in, root causes out.
 *
 * The contract that matters is the citation requirement. An analysis whose claims
 * cannot be traced to a round is indistinguishable from a plausible story about
 * agent behaviour, and a plausible story is exactly what produces the prompt edits
 * that get refuted three sweeps in a row.
 */
export async function analyze(dir: string, result: SweepResult): Promise<string> {
  const prompt = `You are analyzing rollout traces from a coding agent, to find why it
failed. You are NOT fixing anything and you must not edit any file outside this
directory.

The sweep scored ${(result.passRate * 100).toFixed(1)}%:

${scoreboard(result)}

Traces are at ./traces/<task>/trial-<n>/. Each one has README.md (start here),
rounds/round-NNN.md (readable digest), rounds/round-NNN.json (the raw request and
response), hostfn_events.jsonl (every host-function call paired with its result),
prompt-system.md (the exact system prompt that trial ran with), manifest.json (which
prompt sections were in it) and reward.txt (the verdict).

The agent under test is "code mode": instead of calling tools one at a time it
writes a JavaScript program that calls host functions — view(), patch(), write(),
bash(), lsp() and so on — and gets the program's output back. So its mistakes look
like program bugs, misused host functions, or a plan that never verified itself.

Read the FAILING trials first, then read a passing one for contrast. For each task,
work out the ROOT CAUSE — not what failed, but why the agent did what it did. Was a
host function's contract unclear or misused? Did it never verify its work against
the stated spec? Did it stop early? Did it misread the task? Was it simply not
capable enough, with no harness change that would have helped?

Write two things:
- ./analysis/overview.md — the cross-task patterns, most important first. For each,
  name the failure mode, how many trials show it, and what a harness could do about
  it (or say plainly that nothing could).
- ./analysis/detail/<task>.md — one per task, with the specific sequence of rounds
  that went wrong.

EVERY claim must cite the file it came from, like traces/<task>/trial-1/rounds/round-003.md.
A claim you cannot cite is one you should delete. If a failure looks like the model
simply not being strong enough rather than anything the harness did, say so — that
is a real and useful finding, and inventing a harness cause for it is worse than
reporting nothing.`;
  const text = await meta(prompt, dir);
  writeFileSync(join(dir, "analysis", "agent-final-text.md"), text);
  return text;
}

/** One edit, with the contract AHE requires of it. */
export interface ChangeEntry {
  file: string;
  failure_evidence: string;
  root_cause: string;
  targeted_fix: string;
  predicted_pass: string[];
  predicted_at_risk: string[];
  /**
   * A waste metric this edit predicts will FALL, for the case where the bank is
   * saturated and no task can flip.
   *
   * This is a deviation from AHE, which settles every edit against task-level
   * outcomes alone. It is forced by a small bank: when every task passes there is
   * no outcome delta to settle against, and the alternative is a loop that either
   * cannot act on a real defect it can see, or acts and never checks. The metric is
   * a rate over a fixed bank, so it cannot be gamed by running longer — but it is a
   * weaker claim than a flip, and a verdict that rests on it says so.
   */
  predicted_waste?: "parseErrors" | "hostFnErrors" | null;
}

/**
 * Improve: analysis in, prompt edits plus a falsifiable prediction out.
 *
 * The prediction is not paperwork. It is what converts an edit from an opinion into
 * a claim the next sweep can refute, and it is the only reason this loop does not
 * degenerate into a random walk that drifts wherever the last analysis pointed.
 */
export async function evolve(dir: string, result: SweepResult): Promise<string> {
  const sections = [...new Bun.Glob("*.md").scanSync({ cwd: PROMPT_DIR })].sort();
  const prompt = `You are improving a coding agent's harness. The base model is FROZEN
(claude-haiku-4-5) — you may not change it, and you may not change the agent's code.
The only thing you may edit is its system prompt, which lives as one markdown file
per section at ${PROMPT_DIR}:

${sections.map((s) => `  ${s}`).join("\n")}

Most of these are not strategy prose — they are the interface documentation for the
agent's host functions (shell.md, files.md, patch-grammar.md, lsp.md, fetch.md).
Edits that make a host function's CONTRACT clearer are the ones that transfer. Edits
that add exhortation ("be careful", "always verify") are the ones that get refuted,
and adding several at once measurably degrades this model's instruction-following —
so prefer ONE well-targeted edit over three hopeful ones, and keep total growth
under 5%.

The last sweep scored ${(result.passRate * 100).toFixed(1)}%:

${scoreboard(result)}

Read ./analysis/overview.md and ./analysis/detail/*.md first — they are backed by
traces at ./traces/, which you may drill into.

Then make your edits directly to the files in ${PROMPT_DIR}, and write
./change_manifest.json:

{"changes": [{
  "file": "patch-grammar.md",
  "failure_evidence": "the failing trials and the rounds that show it, with paths",
  "root_cause": "why the agent did what it did — not what failed",
  "targeted_fix": "what you changed and why that addresses the cause",
  "predicted_pass": ["task names you predict will flip to passing"],
  "predicted_at_risk": ["task names your edit could plausibly break"],
  "predicted_waste": "parseErrors" | "hostFnErrors" | null
}]}

Use "predicted_waste" when every task already passes and your edit targets wasted
effort rather than a wrong outcome — a round whose program never parsed, or a host
function called wrongly. It settles only when no task flips either way, so it is the
weaker claim; prefer a predicted flip whenever one is honestly available.

The prediction is a contract: the next sweep checks it, and an edit whose prediction
does not hold is reverted. So predict what you actually believe, not what you hope —
an honest empty "predicted_pass" is better than a wrong one, and an edit you cannot
predict the effect of is an edit you should not make.

If the analysis shows no harness-addressable cause, write {"changes": []} and change
nothing. That is a valid and useful outcome.`;
  return meta(prompt, dir);
}

/** Read the manifest an evolve round wrote, tolerating its absence. */
export function readManifest(dir: string): ChangeEntry[] {
  try {
    const raw = JSON.parse(readFileSync(join(dir, "change_manifest.json"), "utf8"));
    return (raw.changes ?? []) as ChangeEntry[];
  } catch {
    return [];
  }
}
