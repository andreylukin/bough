/**
 * The command memory, PUSHED — what it already knows, delivered without being asked.
 *
 * WHY THIS EXISTS. Every bash() has been recorded since the tag memory shipped, and
 * three days of live use measured what that bought: 100% of commands written, ~60
 * recall reads against ~3,900 writes. A 1.5% read rate. The memory is not weak — it
 * is unconsulted, because consulting it is a decision the model has to remember to
 * make mid-task, and it does not.
 *
 * The sharpest evidence: one session ran `gh search prs --state merged` ONE HUNDRED
 * times in eighty-three seconds, each returning `invalid argument "merged"`. All one
 * hundred failures were written to the table. The table was never asked. No prompt
 * sentence fixes that, because the prompt was already asking; the read has to happen
 * whether or not the model chooses it.
 *
 * So this module is the memory speaking first, at the only two moments it can:
 *
 *   - `note()` — AFTER a command fails, append what the same command did here
 *     before, and the nearest thing that exited 0. It lands in the tool result, at
 *     the exact point the mistake was made, costing one indexed SELECT.
 *   - `guard()` — BEFORE a command runs, refuse a command already failing in a
 *     tight loop and hand back the error it is about to produce again.
 *
 * WHY A GUARD AT ALL, given a refusal is a real cost when it is wrong. Because a
 * note only helps a model that reads its output and changes course, and the loop
 * above proves that is not guaranteed. The thresholds are therefore set where the
 * evidence is unambiguous rather than merely suggestive: the SAME session, the
 * SAME byte-identical command, three failures already, inside two minutes. Any
 * edit to the command — a changed flag, a different argument — is a different
 * string and resets it. A command that failed yesterday still runs today.
 *
 * And the refusal never lies about what happened: it says the command was not run,
 * says why, and quotes the error it would have produced. A skipped command reports
 * as skipped.
 */

import type { Db, PriorFailures } from "../types.ts";
import { attributeCommand } from "./record.ts";

/** How far back a repeat failure is worth mentioning at all. */
const ECHO_WINDOW_MS = 14 * 24 * 60 * 60 * 1000;
/** The loop window: failures this close together are one runaway, not history. */
const LOOP_WINDOW_MS = 2 * 60 * 1000;
/** Failures of the identical command, in this session, inside the loop window. */
const LOOP_THRESHOLD = 3;
/** Enough of the last failure to recognise it; a full 2k head would bury the note. */
const ERROR_CHARS = 220;
/** Leading tokens that define "the same kind of command" for a success lookup. */
const PREFIX_TOKENS = 2;

export interface EchoCtx {
  db: Db;
  sessionId: string;
  workspace: string;
  now?: () => number;
}

export interface CommandEcho {
  /**
   * The note to append to a finished command's output, or null. Called with what
   * the command actually did, so a success is cheap: it returns immediately.
   */
  note(command: string, exitCode: number | null): string | null;
  /**
   * The output to return INSTEAD of running the command, or null to run it. A
   * non-null answer means nothing was spawned.
   */
  guard(command: string): string | null;
}

/** The first line of what a command printed, trimmed of a trailing `[exit code N]`. */
function firstErrorLine(outputHead: string): string {
  const line = outputHead
    .split("\n")
    .map((l) => l.trim())
    .find((l) => l !== "" && !/^\[exit code -?\d+\]$/.test(l));
  if (!line) return "";
  return line.length > ERROR_CHARS ? `${line.slice(0, ERROR_CHARS)}…` : line;
}

/** `4m ago`, `2s ago` — the same vocabulary `bough tags show` prints. */
function ago(ms: number): string {
  const s = Math.max(0, Math.round(ms / 1000));
  if (s < 60) return `${s}s ago`;
  const m = Math.round(s / 60);
  if (m < 60) return `${m}m ago`;
  const h = Math.round(m / 60);
  return h < 48 ? `${h}h ago` : `${Math.round(h / 24)}d ago`;
}

/**
 * The LIKE pattern for "a command of this kind": the first couple of tokens, with
 * LIKE's own wildcards escaped so a command containing `%` cannot widen its own
 * search. Empty when there is nothing distinctive to match on.
 */
function successPrefix(command: string): string | null {
  const tokens = command.trim().split(/\s+/).slice(0, PREFIX_TOKENS);
  if (tokens.length === 0) return null;
  const prefix = tokens.join(" ");
  if (prefix.length < 2) return null;
  return prefix.replace(/[\\%_]/g, (c) => `\\${c}`);
}

/**
 * Build the per-turn echo. Every failure is swallowed for the same reason the
 * recorder swallows its own: recall is a side channel, and a broken lookup must
 * never be a broken round.
 */
export function createCommandEcho(ctx: EchoCtx): CommandEcho {
  const now = ctx.now ?? Date.now;
  /** Repo attribution is not free (it stats paths), and both entry points want it. */
  const repoFor = (command: string): string => attributeCommand(command, ctx.workspace).repo;

  const lookup = (command: string): { repo: string; prior: PriorFailures } | null => {
    const at = now();
    const repo = repoFor(command);
    const prior = ctx.db.priorFailures(repo, command, at - ECHO_WINDOW_MS, ctx.sessionId);
    return prior ? { repo, prior } : null;
  };

  return {
    note(command, exitCode) {
      // A success has nothing to be warned about, and this is the common case.
      if (exitCode === 0 || exitCode === null) return null;
      try {
        const hit = lookup(command);
        if (!hit) return null;
        const { repo, prior } = hit;
        const times = prior.count === 1 ? "once" : `${prior.count}×`;
        const lines = [
          `[history] this exact command already failed here ${times} ` +
          `(last ${ago(now() - prior.lastTs)}): ${firstErrorLine(prior.outputHead)}`,
        ];
        const prefix = successPrefix(command);
        const worked = prefix &&
          ctx.db.lastSuccessLike(repo, prefix, command, now() - ECHO_WINDOW_MS);
        if (worked) lines.push(`          this exited 0 here: ${worked}`);
        return lines.join("\n");
      } catch {
        return null;
      }
    },

    guard(command) {
      try {
        const at = now();
        const repo = repoFor(command);
        // Scoped to the loop window, not the echo window: an old failure must not
        // count toward a runaway, and this is the query that decides a refusal.
        const prior = ctx.db.priorFailures(repo, command, at - LOOP_WINDOW_MS, ctx.sessionId);
        if (!prior || prior.inSession < LOOP_THRESHOLD) return null;
        return [
          `[not run] this identical command has failed ${prior.inSession} times in this ` +
          `session in the last ${Math.round(LOOP_WINDOW_MS / 60000)} minutes, so it was ` +
          `skipped rather than run a ${prior.inSession + 1}th time.`,
          ``,
          `Its last error, ${ago(at - prior.lastTs)}:`,
          `  ${firstErrorLine(prior.outputHead)}`,
          ``,
          `Change the command and it runs — any edit makes it a different command. ` +
          `To see what has worked here: bough tags show <tag>, or bough tags sql ` +
          `"SELECT cmd FROM command_history WHERE exit_code = 0 AND cmd LIKE '…' ` +
          `ORDER BY ts DESC LIMIT 5".`,
        ].join("\n");
      } catch {
        return null;
      }
    },
  };
}
