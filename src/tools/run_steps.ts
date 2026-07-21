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
import { bash, inflightForegroundOutput } from "./bash.ts";
import * as bg from "./bash_bg.ts";
import { readFile } from "./read_file.ts";
import { writeFile } from "./write_file.ts";
import { editFile } from "./edit_file.ts";
import { runProgram } from "../harness/vm.ts";
import { digestOutput } from "../worker/digest.ts";

/** Appears in the tool output when the harness accepts `done` (turn may end). */
export const DONE_ACCEPTED = "[done] accepted";
/** Appears when `done` was requested but the committed check failed. */
export const DONE_REJECTED = "[done] rejected";

// A program that can call agent() blocks on whole subagent turns — give it a far
// larger wall-clock budget than the plain 3-minute program cap in harness/vm.ts.
const DELEGATING_TIMEOUT_MS = 45 * 60_000;

const schema = z.object({
  code: z.string().describe(
    "One JavaScript program for this round. It runs in a sealed V8 sandbox; the core " +
      "capability surface is async host functions with these RETURN TYPES: " +
      "bash(cmd) → string (combined stdout+stderr — NOT a {stdout} object; " +
      "`const {stdout} = await bash(...)` yields undefined), read(path) → string, " +
      "write(path, content), edit(path, oldText, newText), and background jobs (a slow " +
      "bash auto-backgrounds after ~60s) — bashBg(cmd) → {id, pid}, bashOutput(id) → " +
      "string (progress, safe while running), bashWait(id) → string (block until done), bashKill(id) " +
      "— plus mcpStatus() (always available: this session's MCP management state), " +
      "recall(query, k?) → {hits, indexed} (semantic search over past bough conversations), " +
      "ask(question, {options?: string[]}) → string (pause and ask the USER a clarifying " +
      "question; blocks until they answer in the TUI — they pick an option or type freely — " +
      "and throws a catchable 'user declined' error if dismissed) and any " +
      "oracle(question) → string, delegation (agent/spawn/join/adopt), mcp(server, tool, args), and lsp.* symbol " +
      "navigation host functions your system prompt grants. Node globals (process, " +
      "require) do not exist. Use console.log(...) to see " +
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
  todo: z.string().optional().describe(
    "STRONGLY RECOMMENDED when the request states several rules or steps: copy them " +
      "here as a numbered list, VERBATIM, in your first program — it is echoed back " +
      "after every round so no stated rule falls out of view. Re-declare with " +
      "completed items pruned; an unpruned item is work you have not verified.",
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
    "Execute one JavaScript program in the sealed sandbox (host functions: bash/read/write/edit, " +
    "background shells via bashBg/bashOutput/bashKill, plus delegation and mcp() when granted), " +
    "optionally committing a `check` command and/or requesting `done`. This is your only way to act.",
  schema,
  async run(input: unknown, ctx: ToolRunCtx): Promise<string> {
    const { code, check, done, todo } = input as z.infer<typeof schema>;
    if (check && ctx.turn) ctx.turn.check = check;
    if (todo !== undefined && ctx.turn) ctx.turn.todo = todo;
    let wrote = false;

    const result = await runProgram(
      code,
      {
        bash: (command) => bash.run({ command }, ctx),
        // Background shells: detached from the turn on purpose (no ctx.signal) —
        // they persist across rounds and turns of this session until killed.
        bashBg: (command) => {
          if (ctx.turn) ctx.turn.ranParallel = true; // honesty gate input (turn.ts)
          return bg.bashBg(command, ctx);
        },
        bashOutput: (id) => Promise.resolve(bg.bashOutput(id, ctx)),
        bashWait: (id) => bg.bashWait(id, ctx),
        bashKill: (id) => bg.bashKill(id, ctx),
        read: (path) => readFile.run({ path }, ctx),
        write: (path, content) => {
          wrote = true;
          return writeFile.run({ path, content }, ctx);
        },
        edit: (path, old_string, new_string) => {
          wrote = true;
          return editFile.run({ path, old_string, new_string }, ctx);
        },
        // Delegation (present only when the turn runner allows it): agent()/join() can
        // block on whole subagent turns, so the program gets a far larger wall-clock
        // budget. spawn/join are absent in subagent turns (blocking delegation only).
        ...(ctx.delegate
          ? {
            agent: async (task: string) => JSON.stringify(await ctx.delegate!.run(task)),
            adopt: (sessionId: string) => ctx.delegate!.adopt(sessionId),
            ...(ctx.delegate.spawn
              ? { spawn: async (task: string) => JSON.stringify(await ctx.delegate!.spawn!(task)) }
              : {}),
            ...(ctx.delegate.join
              ? {
                join: async (sessionId: string) =>
                  JSON.stringify(await ctx.delegate!.join!(sessionId)),
              }
              : {}),
          }
          : {}),
        // The oracle (wired for supervisor turns): plain strings both ways.
        ...(ctx.oracle ? { oracle: (question: string) => ctx.oracle!(question) } : {}),
        // ask() (wired for supervisor turns): park the program on a question to
        // the human; options travel as JSON like mcp() args. User-paced — the
        // delegating wall-clock budget below covers the wait.
        ...(ctx.ask
          ? {
            ask: (question: string, optsJson: string) =>
              ctx.ask!(question, JSON.parse(optsJson || "{}")),
          }
          : {}),
        // MCP (wired only for turns whose skills/activations granted servers): the
        // JSON round-trip keeps the postMessage protocol string-only, like agent().
        ...(ctx.mcp
          ? {
            mcp: async (server: string, tool: string, argsJson: string) =>
              JSON.stringify(await ctx.mcp!.call(server, tool, JSON.parse(argsJson))) ??
                "null",
          }
          : {}),
        // MCP management state — read-only, wired for every supervisor turn.
        ...(ctx.mcpStatus ? { mcpStatus: async () => JSON.stringify(await ctx.mcpStatus!()) } : {}),
        // Recall (wired for supervisor turns): semantic search over past
        // conversations; the RecallResult round-trips as JSON.
        ...(ctx.recall
          ? {
            recall: async (query: string, k?: number) =>
              JSON.stringify(await ctx.recall!(query, k)),
          }
          : {}),
        // LSP symbol verbs (wired when the backing server is registered): same
        // JSON round-trip as mcp(); the worker side fans this out as lsp.*.
        ...(ctx.lsp
          ? {
            lsp: async (verb: string, argsJson: string) =>
              JSON.stringify(await ctx.lsp!.call(verb, JSON.parse(argsJson))) ?? "null",
          }
          : {}),
        // Artifacts (wired for supervisor turns): write + host a file for browser
        // viewing; the JSON round-trip returns the artifact object to the program.
        ...(ctx.artifact
          ? {
            artifact: async (name: string, content: string) =>
              JSON.stringify(await ctx.artifact!(name, content)),
          }
          : {}),
        // Ship (wired for root-session repo turns): commit + optional push into the
        // origin repo; options and result travel as JSON like mcp().
        ...(ctx.ship
          ? {
            ship: async (optsJson: string) =>
              JSON.stringify(await ctx.ship!(JSON.parse(optsJson || "{}"))),
          }
          : {}),
        // Recurring runs (wired for supervisor turns): verb-dispatched like lsp();
        // the worker side fans this out as schedule.*.
        ...(ctx.schedule
          ? {
            schedule: async (verb: string, argsJson: string) =>
              JSON.stringify(await ctx.schedule!.call(verb, JSON.parse(argsJson))) ?? "null",
          }
          : {}),
      },
      // agent() blocks on whole subagent turns; a held mcp()/lsp() call blocks on a
      // human approval; an oracle() consult can reason for many minutes; an ask()
      // waits on the human — all need far more wall-clock than the 3-minute cap.
      ctx.delegate || ctx.mcp || ctx.lsp || ctx.oracle || ctx.ask
        ? DELEGATING_TIMEOUT_MS
        : undefined,
      ctx.signal,
      ctx.onLog,
    );

    const out: string[] = [];
    // Oversized printed output is digested (head + local-worker summary + tail)
    // before it reaches the model; the program itself always saw the full text.
    if (result.logs.length) out.push(await digestOutput(result.logs.join("\n")));
    if (!result.ok) {
      // An interrupt terminates the worker mid-host-call, so a foreground bash's
      // result never comes back — surface what it had already printed instead of
      // discarding it under the bare interrupt marker.
      if (ctx.signal?.aborted) {
        const partial = inflightForegroundOutput(ctx.sessionId);
        if (partial) out.push(partial);
      }
      out.push(`[program error] ${result.error}`);
    }

    if (done) {
      const committed = ctx.turn?.check ?? check;
      if (!committed && ctx.turn && !ctx.turn.checkNudged) {
        // First check-less done bounces: a completion claim with no committed
        // check is unverified. The second one is accepted (some work has no
        // natural check), so this can never loop.
        ctx.turn.checkNudged = true;
        out.push(
          `${DONE_REJECTED} — no check committed. Commit a \`check\` command that ` +
            `exits 0 iff the request's acceptance criteria hold, then set done:true ` +
            `again. If the work is genuinely uncheckable, set done:true once more.`,
        );
      } else if (!committed) {
        out.push(`${DONE_ACCEPTED} — no check declared`);
      } else {
        const checkOut = await bash.run({ command: committed }, ctx).catch(
          (e: Error) => `check did not run: ${e.message}\n[exit code 1]`,
        );
        const exit = exitCodeOf(checkOut);
        out.push(`[check] ${committed}\n${checkOut}`);
        if (exit === 0 && ctx.turn?.requestText && !ctx.turn.specEchoed) {
          // Multi-rule requests get one spec replay at the decisive moment: a
          // passing check can still be weaker than the spec (it usually is when
          // a prose sub-clause got dropped rounds ago). Second done accepts.
          ctx.turn.specEchoed = true;
          out.push(
            `${DONE_REJECTED} — spec recheck. Your check passed, but re-read the ` +
              `request below and confirm EACH numbered rule is implemented and ` +
              `covered by your check. If any rule is not tested by the check, fix ` +
              `that first; when every rule is verified, set done:true again.\n` +
              `--- original request ---\n${ctx.turn.requestText}`,
          );
        } else {
          out.push(
            exit === 0
              ? `${DONE_ACCEPTED} — check passed`
              : `${DONE_REJECTED} — check failed (exit ${exit}); keep working`,
          );
        }
      }
    }
    // Probe-round meter: verify-by-eyeball shows up as runs of rounds that
    // change nothing and commit nothing (bench evidence: up to 19 such rounds
    // re-confirming a finished implementation by hand). Deterministic and
    // advisory — after 3 consecutive probe-only rounds, nudge toward the check.
    if (ctx.turn) {
      // Armed only after the first write: pre-implementation exploration rounds
      // are legitimate; the waste pattern is the post-implementation probe tail.
      if (wrote) ctx.turn.everWrote = true;
      const probeOnly = ctx.turn.everWrote === true && !wrote && !check && !done;
      ctx.turn.probeRounds = probeOnly ? (ctx.turn.probeRounds ?? 0) + 1 : 0;
      if (ctx.turn.probeRounds >= 3) {
        ctx.turn.probeRounds = 0;
        out.push(
          "[verification note] 3 rounds without a file change or a committed check. " +
            "If behavior is confirmed, encode that verification as your `check` command " +
            "and set done:true; if something is wrong, fix it now instead of probing further.",
        );
      }
    }
    // Context reinforcement: the turn's todo rides on every result so stated
    // rules survive long turns (weak models drop prose sub-clauses otherwise).
    if (ctx.turn?.todo?.trim()) out.push(`[todo — prune as items complete]\n${ctx.turn.todo}`);
    return out.join("\n") || "(no output)";
  },
};
