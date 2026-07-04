/**
 * `run_steps` — the supervisor's ONLY tool: one JavaScript program per
 * round, executed by the deterministic harness in a sealed V8 sandbox (harness/vm.ts).
 * The supervisor plans and writes; it never touches the machine. The program's whole
 * capability surface is the four host functions, which run here on the host through
 * the same confined tool implementations as before (Seatbelt bash, workspace-rooted
 * file ops).
 *
 * Completion is CHECK-gated, not self-reported: `check` commits a shell command that
 * exits 0 iff the task's acceptance criteria hold; `done: true` asks the harness to
 * finish, and the harness re-runs the committed check and accepts only on exit 0.
 * The verdict markers below are the contract with the turn loop.
 */
import { z } from "zod/v4";
import type { ToolDef, ToolRunCtx } from "./types.ts";
import { bash } from "./bash.ts";
import { readFile } from "./read_file.ts";
import { writeFile } from "./write_file.ts";
import { editFile } from "./edit_file.ts";
import { runProgram } from "../harness/vm.ts";

/** Appears in the tool output when the harness accepts `done` (turn may end). */
export const DONE_ACCEPTED = "[done] accepted";
/** Appears when `done` was requested but the committed check failed. */
export const DONE_REJECTED = "[done] rejected";

// A program that can call agent() blocks on whole subagent turns — give it a far
// larger wall-clock budget than the plain 3-minute program cap in harness/vm.ts.
const DELEGATING_TIMEOUT_MS = 45 * 60_000;

const schema = z.object({
  code: z.string().describe(
    "One JavaScript program for this round. It runs in a sealed V8 sandbox; the entire " +
      "capability surface is four async host functions: bash(cmd), read(path), " +
      "write(path, content), edit(path, oldText, newText). Use console.log(...) to see " +
      "anything — printed output is returned to you. Cover inspect → change → verify in " +
      "one program.",
  ),
  check: z.string().optional().describe(
    "Shell command that exits 0 iff the task's literal acceptance criteria hold. " +
      "Committed for the rest of the turn (re-declaring replaces it). Declare it as early " +
      "as you can.",
  ),
  done: z.boolean().optional().describe(
    "Set true when you believe the task is complete. The harness re-runs the committed " +
      "check and accepts done only if it passes.",
  ),
});

/** "cmd output…[exit code N]" (bash tool format) → N; absent marker = 0. */
function exitCodeOf(bashOutput: string): number {
  const m = /\[exit code (\d+)\]\s*$/.exec(bashOutput);
  return m ? Number(m[1]) : 0;
}

export const runSteps: ToolDef = {
  name: "run_steps",
  description:
    "Execute one JavaScript program in the sealed sandbox (host functions: bash/read/write/edit), " +
    "optionally committing a `check` command and/or requesting `done`. This is your only way to act.",
  schema,
  async run(input: unknown, ctx: ToolRunCtx): Promise<string> {
    const { code, check, done } = input as z.infer<typeof schema>;
    if (check && ctx.turn) ctx.turn.check = check;

    const result = await runProgram(
      code,
      {
        bash: (command) => bash.run({ command }, ctx),
        read: (path) => readFile.run({ path }, ctx),
        write: (path, content) => writeFile.run({ path, content }, ctx),
        edit: (path, old_string, new_string) => editFile.run({ path, old_string, new_string }, ctx),
        // Delegation (present only when the turn runner allows it): agent()/join() can
        // block on whole subagent turns, so the program gets a far larger wall-clock budget.
        ...(ctx.delegate
          ? {
            agent: async (task: string) => JSON.stringify(await ctx.delegate!.run(task)),
            spawn: async (task: string) => JSON.stringify(await ctx.delegate!.spawn(task)),
            join: async (sessionId: string) => JSON.stringify(await ctx.delegate!.join(sessionId)),
            adopt: (sessionId: string) => ctx.delegate!.adopt(sessionId),
          }
          : {}),
      },
      ctx.delegate ? DELEGATING_TIMEOUT_MS : undefined,
      ctx.signal,
    );

    const out: string[] = [];
    if (result.logs.length) out.push(result.logs.join("\n"));
    if (!result.ok) out.push(`[program error] ${result.error}`);

    if (done) {
      const committed = ctx.turn?.check ?? check;
      if (!committed) {
        out.push(`${DONE_ACCEPTED} — no check declared`);
      } else {
        const checkOut = await bash.run({ command: committed }, ctx).catch(
          (e: Error) => `check did not run: ${e.message}\n[exit code 1]`,
        );
        const exit = exitCodeOf(checkOut);
        out.push(`[check] ${committed}\n${checkOut}`);
        out.push(
          exit === 0
            ? `${DONE_ACCEPTED} — check passed`
            : `${DONE_REJECTED} — check failed (exit ${exit}); keep working`,
        );
      }
    }
    return out.join("\n") || "(no output)";
  },
};
