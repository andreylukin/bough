import type { Line } from "./types";

/** A command that runs a test suite, by the runners people actually type. */
export const TEST_CMD = /\b(go test|(?:npm|pnpm|yarn|bun)(?: run)? test|pytest|cargo (?:test|nextest)|vitest|jest|make (?:test|check)|mvn test|gradle test|rspec|phpunit)\b/;

/** seq is the result's when there is one (the transcript block carries it), else the call's. */
export type TestRun = { cmd: string; seq: number; at: string; exit?: number };

/**
 * The latest test run and how it ended, from the exit code the loop
 * recorded on its result — never from what the reply said. A result
 * belongs to the call before it (lines carry no call id). A test call with no result yet is running; one whose result has no exit
 * code (recorded before exit codes were) has exit undefined. Later turns
 * that ran no tests leave it standing.
 */
export function lastTestRun(lines: Line[], running: boolean): (TestRun & { state: "passed" | "failed" | "running" | "unrecorded" }) | null {
  for (let i = lines.length - 1; i >= 0; i--) {
    const c = lines[i];
    if (c.kind !== "code" || !TEST_CMD.test(c.text)) continue;
    const cmd = (TEST_CMD.exec(c.text)?.[0] ?? "tests");
    // Its result is the first one before the next call.
    let r: Line | undefined;
    for (let j = i + 1; j < lines.length && lines[j].kind !== "code"; j++) if (lines[j].kind === "result") { r = lines[j]; break; }
    if (!r) return { cmd, seq: c.seq, at: c.at, state: running && !lines.slice(i + 1).some((l) => l.kind === "done") ? "running" : "unrecorded" };
    const exit = r.data?.exit;
    if (typeof exit !== "number") return { cmd, seq: r.seq, at: r.at, state: "unrecorded" };
    return { cmd, seq: r.seq, at: r.at, exit, state: exit === 0 ? "passed" : "failed" };
  }
  return null;
}

export type FinishedJob = { id: number; cmd: string; exit: string; took: string; output: string; seq: number; at: string };

/** Jobs that ended, newest last, from the loop's `job N [exited …] cmd (took)` notes. */
export function finishedJobs(lines: Line[]): FinishedJob[] {
  const out: FinishedJob[] = [];
  for (const l of lines) {
    if (l.kind !== "job") continue;
    const text = String(l.data?.text ?? l.text ?? "");
    const m = /^job (\d+) \[([^\]]+)\] (.*?)(?: \(([^)]*)\))?\n([\s\S]*)$/.exec(text + (text.includes("\n") ? "" : "\n"));
    if (m) out.push({ id: +m[1], exit: m[2], cmd: m[3], took: m[4] ?? "", output: m[5].trim(), seq: l.seq, at: l.at });
  }
  return out;
}
