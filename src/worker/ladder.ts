/**
 * The delegation ladder — a small, self-contained, VERIFIABLE unit of work runs on
 * the local worker instead of costing frontier tokens. The supervisor dictates the
 * unit ReDAct-style (location + behavior + example — specificity is safely better)
 * and commits a CHECK; completion is decided by the check's exit code, never
 * self-reported.
 *
 * Ladder shape (the benchmarked one): tier1 = local worker one-shot; on CHECK fail
 * a single fresh sample (best-of-2 harvests the 3B's per-shot variance ~100× cheaper
 * than escalating); then a small frontier backstop. The relay/reasoner tier from the
 * experiments is deliberately absent — it earned 6/45 solves at a heavy latency
 * cost. BOUGH_WORKER_LOCAL_ONLY=1 removes the backstop for privacy-sensitive runs.
 *
 * The action channel is the PRIMITIVES surface the experiments proved (fenced
 * write/edit/sh blocks — block body IS the content); code-mode indirection
 * collapses small models. Ops apply through the same confined tools as the
 * supervisor's own programs (workspace-rooted paths, Seatbelt bash), and a failed
 * edit gets the fast-apply reconciliation for free.
 *
 * Known limitation (v1): a failed attempt's writes are not rolled back — the
 * result reports every file it touched so the caller can inspect or re-dictate.
 */
import type { ToolRunCtx } from "../tools/types.ts";
import { bash } from "../tools/bash.ts";
import { writeFile } from "../tools/write_file.ts";
import { editFile } from "../tools/edit_file.ts";
import { workerIfRunning } from "./runtime.ts";
import { workerComplete } from "./client.ts";
import { anthropicClient } from "../supervisor/llm.ts";

/** Injectable completions for tests: (system, user, temperature) → raw reply. */
export type UnitCompleter = (system: string, user: string, temperature: number) => Promise<string>;

export interface UnitResult {
  /** True iff the check exited 0 after some tier's ops were applied. */
  solved: boolean;
  /** Which tier's attempt passed the check ("none" when all failed). */
  tier: "worker" | "backstop" | "none";
  /** Total model attempts spent (worker samples + backstop). */
  attempts: number;
  /** Files written or edited across ALL attempts, including failed ones. */
  touched: string[];
  /** Last check output (or op-apply error) — the caller's evidence either way. */
  report: string;
}

/** Best-of-2 at tier1: first sample near-greedy, the retry explores. */
const WORKER_TEMPERATURES = [0.2, 0.8];
const WORKER_MAX_TOKENS = 2048;

export function backstopModel(): string {
  return Deno.env.get("BOUGH_WORKER_BACKSTOP") ?? "claude-haiku-4-5";
}

const SYSTEM = [
  "You are a code-editing worker. You get ONE small task with everything you need:",
  "location, desired behavior, and how it will be checked. Act using ONLY fenced",
  "blocks, applied in order:",
  "",
  "```write <path>",
  "<the file's entire new content>",
  "```",
  "",
  "```edit <path>",
  "<<<<<<<",
  "<exact text currently in the file>",
  "=======",
  "<replacement text>",
  ">>>>>>>",
  "```",
  "",
  "```sh",
  "<one shell command>",
  "```",
  "",
  "Prefer edit for surgical changes and write for small files. Do not explain,",
  "do not add prose outside blocks, do not invent files the task didn't mention.",
].join("\n");

/**
 * Run one delegated unit up the ladder. Never throws — every failure mode lands
 * in the result so the calling program can react.
 */
export async function runUnit(
  instruction: string,
  check: string,
  ctx: ToolRunCtx,
  hooks: { worker?: UnitCompleter; backstop?: UnitCompleter } = {},
): Promise<UnitResult> {
  const touched: string[] = [];
  let attempts = 0;
  let report = "";

  const unique = () => [...new Set(touched)];
  const tryReply = async (reply: string): Promise<string | null> => {
    const applied = await applyOps(reply, ctx, touched);
    if (applied !== null) return applied; // op-level failure, not a check verdict
    const out = await bash.run({ command: check }, ctx).catch((e: Error) =>
      `check did not run: ${e.message}\n[exit code 1]`
    );
    return exitCodeOf(out) === 0 ? null : out;
  };

  const worker = hooks.worker ?? localWorker;
  for (const temperature of WORKER_TEMPERATURES) {
    let reply: string;
    try {
      reply = await worker(SYSTEM, instruction, temperature);
    } catch (e) {
      report = `worker unavailable: ${(e as Error).message}`;
      break; // no local tier — fall through to the backstop
    }
    attempts++;
    const failure = await tryReply(reply);
    if (failure === null) {
      return { solved: true, tier: "worker", attempts, touched: unique(), report: "check passed" };
    }
    report = failure;
  }

  if (Deno.env.get("BOUGH_WORKER_LOCAL_ONLY") !== "1") {
    const backstop = hooks.backstop ?? frontierBackstop;
    try {
      const reply = await backstop(SYSTEM, instruction, 0);
      attempts++;
      const failure = await tryReply(reply);
      if (failure === null) {
        return {
          solved: true,
          tier: "backstop",
          attempts,
          touched: unique(),
          report: "check passed",
        };
      }
      report = failure;
    } catch (e) {
      report = `backstop failed: ${(e as Error).message}`;
    }
  }

  return { solved: false, tier: "none", attempts, touched: unique(), report };
}

interface Op {
  kind: "write" | "edit" | "sh";
  arg: string;
  body: string;
}

/** Fenced blocks → ops, in reply order. Prose outside blocks is ignored. */
export function parseOps(reply: string): Op[] {
  const ops: Op[] = [];
  const fence = /```(write|edit|sh)[ \t]*([^\n]*)\n([\s\S]*?)```/g;
  for (let m = fence.exec(reply); m; m = fence.exec(reply)) {
    ops.push({ kind: m[1] as Op["kind"], arg: m[2].trim(), body: m[3] });
  }
  return ops;
}

/** Apply ops through the confined tools. Returns an error description, or null. */
async function applyOps(reply: string, ctx: ToolRunCtx, touched: string[]): Promise<string | null> {
  const ops = parseOps(reply);
  if (ops.length === 0) return "worker reply contained no write/edit/sh blocks";
  for (const op of ops) {
    try {
      if (op.kind === "write") {
        if (!op.arg) return "write block without a path";
        // Block body is the file content; strip the fence's trailing newline.
        await writeFile.run({ path: op.arg, content: op.body.replace(/\n$/, "") }, ctx);
        touched.push(op.arg);
      } else if (op.kind === "edit") {
        if (!op.arg) return "edit block without a path";
        const parts = splitEdit(op.body);
        if (!parts) return "edit block without <<<<<<< / ======= / >>>>>>> markers";
        await editFile.run({ path: op.arg, old_string: parts[0], new_string: parts[1] }, ctx);
        touched.push(op.arg);
      } else {
        const out = await bash.run({ command: op.body.trim() }, ctx);
        const exit = exitCodeOf(out);
        if (exit !== 0) return `sh op failed (exit ${exit}): ${out.slice(0, 500)}`;
      }
    } catch (e) {
      return `${op.kind} ${op.arg} failed: ${(e as Error).message}`;
    }
  }
  return null;
}

/** "<<<<<<<\nold\n=======\nnew\n>>>>>>>" → [old, new]; tolerant of marker spacing. */
function splitEdit(body: string): [string, string] | null {
  const m = /<{7}[^\n]*\n([\s\S]*?)\n={7}[^\n]*\n([\s\S]*?)\n>{7}/.exec(body);
  return m ? [m[1], m[2]] : null;
}

/** "…[exit code N]" (bash tool format) → N; absent marker = 0. */
function exitCodeOf(bashOutput: string): number {
  const m = /\[exit code (\d+)\]\s*$/.exec(bashOutput);
  return m ? Number(m[1]) : 0;
}

async function localWorker(system: string, user: string, temperature: number): Promise<string> {
  const url = await workerIfRunning();
  if (!url) throw new Error("no local worker running");
  return await workerComplete(url, {
    system,
    user,
    maxTokens: WORKER_MAX_TOKENS,
    temperature,
    cachePrompt: true,
  });
}

async function frontierBackstop(system: string, user: string): Promise<string> {
  const result = await anthropicClient().run(
    {
      model: backstopModel(),
      maxTokens: WORKER_MAX_TOKENS,
      messages: [{ role: "user", content: [{ type: "text", text: `${system}\n\n${user}` }] }],
      tools: [],
    },
    () => {},
  );
  const text = result.content.find((b) => b.type === "text")?.text;
  if (!text) throw new Error("backstop returned no text");
  return text;
}
