/**
 * `run_steps` — the supervisor's ONLY tool: one JavaScript program per round, executed
 * by the deterministic harness in a Deno worker (harness/vm.ts) that inherits the
 * server's permissions — the program can do anything the user can. The host functions
 * bridged below are not a boundary; they are where the harness behavior lives (the
 * turn's interrupt, auto-backgrounding, output digestion, and the write/check signals
 * that feed the done-gate), which is why ordinary work should still flow through them.
 *
 * Completion is CHECK-gated, not self-reported: `check` commits a shell command that
 * exits 0 iff the task's acceptance criteria hold; `done: true` asks the harness to
 * finish, and the harness re-runs the committed check and accepts only on exit 0.
 * The verdict markers below are the contract with the turn loop.
 */
import { z } from "zod/v4";
import type { ToolDef, ToolRunCtx } from "./types.ts";
import { bash, inflightForegroundOutput, shConcurrent } from "./bash.ts";
import * as bg from "./bash_bg.ts";
import { extractFrom } from "../worker/extract.ts";
import { fetchUrl } from "./fetch_url.ts";
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
    "One JavaScript program for this round. It runs in a Deno worker with the user's " +
      "own permissions — the full Deno runtime (Deno.readTextFile, Deno.Command, " +
      'Deno.env, sockets, `await import("npm:…")`) is available when you need it. ' +
      "The host functions below are the convenient path and carry harness behavior " +
      "(interrupts, auto-backgrounding, output digestion), so prefer them for " +
      "ordinary work. Their RETURN TYPES: " +
      "bash(cmd) → string (combined stdout+stderr — NOT a {stdout} object; " +
      "`const {stdout} = await bash(...)` yields undefined), " +
      "sh(...cmds) → [{code, out}, …] (the SAME shell, but runs every command " +
      "CONCURRENTLY and never throws on a non-zero exit — prefer it over sequential " +
      "bash() calls for independent commands), read(path) → string, " +
      "write(path, content), edit(path, oldText, newText), and background jobs (a slow " +
      "bash auto-backgrounds after ~60s) — bashBg(cmd) → {id, pid}, bashOutput(id) → " +
      "string (progress, safe while running), bashWait(id) → string (block until done), bashKill(id) " +
      "— plus extract(text, instruction, schema?) → string, or the schema-shaped object " +
      "when a JSON Schema is passed (a cheap local model pulls one value out of text you " +
      "already hold, so a big blob never enters your context; throws if no worker is " +
      "reachable, so read the text yourself then), " +
      "fetch(url, {method?, headers?, body?}) → {status, ok, url, contentType, body, " +
      "truncated} (http/https only; body capped at 1MB, 30s deadline, throws on " +
      "transport failure — a non-2xx status is DATA, not a throw), " +
      "image(path, note?) (attach an image file — a screenshot, a rendered chart — so " +
      "you SEE it; it reaches you on the next turn, not inside this program), " +
      "mcpStatus() (always available: this session's MCP management state), " +
      "recall(query, k?) → {hits, indexed} (semantic search over past bough conversations), " +
      "state.get(key) → the stored value or null / state.set({key, value}) / state.list() / " +
      "state.delete(key) (durable notes for THIS conversation — they survive rounds and " +
      "compaction, so long tasks keep bookkeeping there instead of re-deriving it), " +
      "ask(question, {options?: string[]}) → string (pause and ask the USER a clarifying " +
      "question; blocks until they answer in the TUI — they pick an option or type freely — " +
      "and throws a catchable 'user declined' error if dismissed) and any " +
      "delegation (agent/spawn/join/adopt), mcp(server, tool, args), and lsp.* symbol " +
      "navigation host functions your system prompt grants. `require` does not exist " +
      "(use npm: specifiers) and process.exit()/Deno.exit() throw — a program ends by " +
      "returning. Use console.log(...) to see " +
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
    "Execute one JavaScript program in a Deno worker running with the user's own permissions " +
    "(host functions: bash/sh/read/write/edit, " +
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
        bash: async (command) => {
          const out = await bash.run({ command }, ctx);
          // Remember the turn's latest passing command: it is the ready-made
          // `check` candidate a check-less done auto-adopts (and the done-gate
          // messages cite). Pure navigation (ls/cat/…) is skipped — a vacuous
          // check; mutating prefixes (rm/mv/git/…) are skipped — the adopted
          // command re-runs at done-time, so it must be safe to repeat.
          if (
            ctx.turn && exitCodeOf(out) === 0 &&
            !/^\s*(ls|cat|head|tail|find|pwd|echo|which|wc)\b[^|&;]*$/.test(command) &&
            !/^\s*(rm|mv|cp|mkdir|touch|chmod|chown|ln|git|kill)\b/.test(command)
          ) ctx.turn.lastGreenCmd = command;
          return out;
        },
        // Concurrent shells: the parallel sibling of bash(). The commands run at the
        // same time (the host bridge is id-keyed and nothing in bash.ts serializes
        // shells) and every result carries its own exit code, so a failing command
        // is data rather than an exception. JSON both ways, like agent()/mcp().
        sh: async (cmdsJson: string) => {
          const cmds = JSON.parse(cmdsJson) as string[];
          if (cmds.length > 1 && ctx.turn) ctx.turn.ranParallel = true; // honesty gate (turn.ts)
          return JSON.stringify(await shConcurrent(cmds, ctx));
        },
        // Cheap-model extraction (worker/extract.ts): the program hands over text it
        // already holds and gets back one value, so the blob never has to enter the
        // supervisor's context. null schema = free-form string. Throws (catchably)
        // when no worker is reachable rather than inventing an answer.
        extract: async (text: string, instruction: string, schemaJson: string) => {
          const schema = JSON.parse(schemaJson) as Record<string, unknown> | null;
          return JSON.stringify(await extractFrom(text, instruction, schema ?? undefined)) ??
            "null";
        },
        // HTTP (fetch_url.ts): host-side egress for a program that has none of its
        // own. Options in and the response object out travel as JSON; the turn's
        // interrupt is passed down so an aborted turn doesn't leave a request live.
        fetch: async (url: string, optsJson: string) =>
          JSON.stringify(await fetchUrl(url, JSON.parse(optsJson || "{}"), ctx.signal)),
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
        // image() (wired for supervisor turns): attach an image file so the model
        // can SEE it. Plain string in, plain confirmation out — the bytes stay
        // host-side (attachment store), they never cross the bridge.
        ...(ctx.image ? { image: (path: string, note?: string) => ctx.image!(path, note) } : {}),
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
        // Recurring runs (wired for supervisor turns): verb-dispatched like lsp();
        // the worker side fans this out as schedule.*.
        ...(ctx.schedule
          ? {
            schedule: async (verb: string, argsJson: string) =>
              JSON.stringify(await ctx.schedule!.call(verb, JSON.parse(argsJson))) ?? "null",
          }
          : {}),
        // Durable notes (wired for supervisor turns): verb-dispatched like
        // schedule; the worker side fans this out as state.*.
        ...(ctx.state
          ? {
            state: async (verb: string, argsJson: string) =>
              JSON.stringify(await ctx.state!.call(verb, JSON.parse(argsJson))) ?? "null",
          }
          : {}),
        // Workflows (wired for delegating root-session turns): verb-dispatched
        // like schedule; the worker side fans this out as workflow.*.
        ...(ctx.workflow
          ? {
            workflow: async (verb: string, argsJson: string) =>
              JSON.stringify(await ctx.workflow!.call(verb, JSON.parse(argsJson))) ?? "null",
          }
          : {}),
      },
      // agent() blocks on whole subagent turns; a held mcp()/lsp() call blocks on a
      // human approval; an ask() waits on the human — all need far more wall-clock
      // than the 3-minute cap.
      ctx.delegate || ctx.mcp || ctx.lsp || ctx.ask ? DELEGATING_TIMEOUT_MS : undefined,
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
      // Measured and rejected: auto-adopting lastGreenCmd as the check here
      // (skipping the bounce) cost pass rate — the check-writing rounds are
      // where the model re-compares its output against the literal spec, and
      // removing that forcing function shipped format bugs. Cite, don't adopt.
      const committed = ctx.turn?.check ?? check;
      if (!committed && ctx.turn && !ctx.turn.checkNudged) {
        // First check-less done bounces: a completion claim with no committed
        // check is unverified. The second one is accepted (some work has no
        // natural check), so this can never loop.
        ctx.turn.checkNudged = true;
        out.push(
          `${DONE_REJECTED} — no check committed. Commit a \`check\` command that ` +
            `exits 0 iff the request's acceptance criteria hold, then set done:true ` +
            `again.` +
            (ctx.turn.lastGreenCmd
              ? ` Your last passing command is a ready-made candidate — commit it ` +
                `as-is, do NOT re-run it first: check: ${ctx.turn.lastGreenCmd}`
              : "") +
            ` If the work is genuinely uncheckable, set done:true once more.`,
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
      // Threshold 3, measured: firing on the first probe round cut cost ~20%
      // but dropped pass rate 91%→81% on the bench (premature finishes) —
      // the probe tail is partly productive settling time for a weak model.
      if (ctx.turn.probeRounds >= 3) {
        ctx.turn.probeRounds = 0;
        out.push(
          "[verification note] 3 rounds without a file change or a committed check. " +
            "STOP re-verifying — you have already seen the result. " +
            (ctx.turn.lastGreenCmd
              ? `THIS round, commit your last passing command verbatim as the check ` +
                `and finish: { check: ${JSON.stringify(ctx.turn.lastGreenCmd)}, done: true }. `
              : "If behavior is confirmed, encode that verification as your `check` " +
                "command and set done:true this round. ") +
            "If something is actually wrong, fix it now instead of probing further.",
        );
      }
    }
    // Context reinforcement: the turn's todo rides on every result so stated
    // rules survive long turns (weak models drop prose sub-clauses otherwise).
    if (ctx.turn?.todo?.trim()) out.push(`[todo — prune as items complete]\n${ctx.turn.todo}`);
    return out.join("\n") || "(no output)";
  },
};
