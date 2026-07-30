/**
 * Turn a trial's raw trace into a directory an agent can navigate.
 *
 * WHY A FILESYSTEM AND NOT A BLOB. A sweep produces millions of tokens of trace. No
 * analyzer reads that in one context, and summarizing it up front throws away
 * exactly the detail the analysis is supposed to discover. AHE's answer — and this
 * is its "experience observability" pillar — is to make the trace a place instead of
 * a payload: one file per message, an index at the top, so an agent greps, opens the
 * three files that matter, and drills into the raw round when it needs to. Every
 * claim it later makes has a path underneath it.
 *
 * WHAT IS DERIVED HERE, AND WHY IT IS NOT LEFT TO THE ANALYZER. `hostfn_events.jsonl`
 * pairs each host-function call with its result. bough is code-mode — the model
 * writes a program, so there is no tool call per verb to count, and the fact that
 * `patch()` was rejected four times before the agent gave up is buried inside a code
 * string in one round and a result string in the next. That pairing is mechanical,
 * so a script does it exactly rather than an LLM doing it approximately. It is also
 * the ONLY thing that maps a failure back to a single prompt section: `patch()`
 * failing repeatedly is evidence about `patch-grammar.md`, and without this file the
 * analyzer can only say "the agent struggled with editing".
 */
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { HOST_FN_NAMES } from "../src/harness/protocol.ts";
import { manifestPath, tracePath } from "../src/llm/trace.ts";
import type { RoundRecord, TurnManifest } from "../src/llm/trace.ts";
import { TRACE_DIR } from "./config.ts";
import type { TrialRow } from "./trial.ts";

/** One host-function call and what came back. */
export interface HostFnEvent {
  round: number;
  fn: string;
  /** Null when the round's result never arrived (the turn ended first). */
  ok: boolean | null;
  /** The result text, truncated — enough to classify, not enough to bury. */
  result: string | null;
}

/**
 * A bare call to a host function.
 *
 * The lookbehind is load-bearing: host functions are free identifiers in the
 * program's scope, so `join(...)` is the delegation verb but `parts.join(",")` is
 * Array#join, and several host-function names — `join`, `state`, `write`, `view`,
 * `fetch`, `image` — are ordinary method and property names too. Without it the
 * first sweep credited `join()` calls to a program that only ever joined a list,
 * which is the kind of quiet false positive that sends an analyzer looking for a
 * delegation problem that never existed.
 */
const CALL_SITE = new RegExp(`(?<![.\\w$])(${HOST_FN_NAMES.join("|")})\\s*\\(`, "g");

/** Which host functions a program calls, in source order, with repeats kept. */
export function callSites(code: string): string[] {
  // Deliberately syntactic: a real parse would be more precise about strings and
  // comments, and would still be a heuristic about which calls actually EXECUTED.
  // The next round's result is what settles that; this only has to name candidates.
  return [...code.matchAll(CALL_SITE)].map((m) => m[1]);
}

function textOf(value: unknown): string {
  if (typeof value === "string") return value;
  if (Array.isArray(value)) return value.map(textOf).join("\n");
  if (value && typeof value === "object") {
    const v = value as Record<string, unknown>;
    if (typeof v["text"] === "string") return v["text"];
    if ("content" in v) return textOf(v["content"]);
  }
  return value === undefined ? "" : JSON.stringify(value);
}

/**
 * Pair each round's calls with the tool result that lands in the NEXT round's
 * request. A program's outcome is not in the round that wrote it — the provider
 * only shows it back on the following turn of the loop.
 */
export function hostFnEvents(rounds: RoundRecord[]): HostFnEvent[] {
  const events: HostFnEvent[] = [];
  for (const [i, round] of rounds.entries()) {
    for (const block of round.response?.content ?? []) {
      if (block.type !== "tool_use" || block.name !== "run_steps") continue;
      const code = (block.input as { code?: string } | null)?.code ?? "";
      const next = rounds[i + 1];
      const result = next?.request.messages.flatMap((m) =>
        Array.isArray(m.content) ? m.content : []
      ).find((c) => (c as { toolUseId?: string; tool_use_id?: string }).toolUseId === block.id ||
        (c as { tool_use_id?: string }).tool_use_id === block.id
      ) as { isError?: boolean; is_error?: boolean; content?: unknown } | undefined;
      const isError = result ? (result.isError ?? result.is_error ?? false) : null;
      const body = result ? textOf(result.content).slice(0, 2000) : null;
      for (const fn of callSites(code)) {
        events.push({ round: round.n, fn, ok: isError === null ? null : !isError, result: body });
      }
    }
  }
  return events;
}

function readJsonl(path: string): Record<string, unknown>[] {
  return readFileSync(path, "utf8").trim().split("\n").filter(Boolean).map((l) => JSON.parse(l));
}

/** What a trial cost itself, beyond whether it passed. */
export interface TrialStats {
  rounds: number;
  /** Host-function calls whose result came back an error. */
  hostFnErrors: number;
  /**
   * Rounds the program never even ran — a syntax error, or a name collision with
   * an injected binding. These are pure harness waste: the model paid for a round
   * and learned nothing about the task, only about the harness. They are also the
   * clearest evidence a prompt section can act on, which is why they are counted
   * apart from ordinary tool failures.
   */
  parseErrors: number;
}

/**
 * Explode one trial into `dest`. Returns the manifest so the caller can index the
 * sections this trial was exposed to, and the stats a saturated task still moves on.
 */
export function statsOf(events: HostFnEvent[], rounds: number): TrialStats {
  const failed = events.filter((e) => e.ok === false);
  return {
    rounds,
    hostFnErrors: failed.length,
    parseErrors: failed.filter((e) => (e.result ?? "").includes("program does not parse")).length,
  };
}

export function materialize(row: TrialRow, dest: string): TurnManifest | null {
  if (!row.sessionId) return null;
  const sessionDir = join(TRACE_DIR, row.sessionId);
  // One turn per trial: the trial posts a single message. Any file in the session
  // directory is that turn's.
  const turnId = Array.from(
    new Bun.Glob("*.manifest.json").scanSync({ cwd: sessionDir }),
  )[0]?.replace(".manifest.json", "");
  if (!turnId) return null;
  const label = { dir: TRACE_DIR, sessionId: row.sessionId, turnId };

  const manifest = JSON.parse(readFileSync(manifestPath(label), "utf8")) as TurnManifest;
  const lines = readJsonl(tracePath(label));
  const prompts = lines.filter((l) => l["type"] === "prompt");
  const rounds = lines.filter((l) => l["type"] === "round") as unknown as RoundRecord[];

  mkdirSync(join(dest, "rounds"), { recursive: true });
  writeFileSync(join(dest, "manifest.json"), JSON.stringify(manifest, null, 2));
  writeFileSync(join(dest, "reward.txt"), `${row.pass ? "PASS" : "FAIL"}\n${row.failReason ?? ""}\n`);
  for (const p of prompts) {
    writeFileSync(join(dest, `prompt-${p["tier"]}.md`), String(p["text"]));
  }

  const events = hostFnEvents(rounds);
  writeFileSync(
    join(dest, "hostfn_events.jsonl"),
    events.map((e) => JSON.stringify(e)).join("\n") + (events.length ? "\n" : ""),
  );

  // One file per round, and a readable digest per round beside it. The JSON is the
  // ground truth; the .md is what makes grepping the directory worth doing.
  for (const round of rounds) {
    const n = String(round.n).padStart(3, "0");
    writeFileSync(join(dest, "rounds", `round-${n}.json`), JSON.stringify(round, null, 2));
    const parts: string[] = [
      `# round ${round.n} (${round.latencyMs}ms, stop=${round.response?.stopReason ?? "ERROR"})`,
    ];
    if (round.error) parts.push(`## error\n${round.error.name}: ${round.error.message}`);
    for (const block of round.response?.content ?? []) {
      if (block.type === "text") parts.push(`## assistant text\n${block.text}`);
      if (block.type === "reasoning") parts.push(`## reasoning\n${block.text}`);
      if (block.type === "tool_use") {
        const code = (block.input as { code?: string } | null)?.code;
        parts.push(`## ${block.name}\n${code ? "```js\n" + code + "\n```" : JSON.stringify(block.input)}`);
      }
    }
    const results = events.filter((e) => e.round === round.n && e.result);
    if (results.length) {
      parts.push(`## result\n\`\`\`\n${results[0].result}\n\`\`\``);
    }
    writeFileSync(join(dest, "rounds", `round-${n}.md`), parts.join("\n\n") + "\n");
  }

  const index = [
    `# ${row.task} trial ${row.trial} — ${row.pass ? "PASS" : "FAIL"}`,
    row.failReason ? `\nverifier: ${row.failReason}` : "",
    `\nsession ${row.sessionId} · ${rounds.length} rounds · ${row.durationMs}ms · $${row.costUsd ?? "?"}`,
    `\n## host function calls\n`,
    ...events.map((e) => `- round ${e.round}: ${e.fn}() → ${e.ok === null ? "no result" : e.ok ? "ok" : "ERROR"}`),
    `\n## files\n`,
    `- rounds/round-NNN.md — readable digest per round`,
    `- rounds/round-NNN.json — the raw request and response`,
    `- prompt-system.md — the exact stable prefix this trial ran with`,
    `- manifest.json — which prompt sections were in it, with shas`,
    `- hostfn_events.jsonl — every call paired with its result`,
  ].join("\n");
  writeFileSync(join(dest, "README.md"), index + "\n");
  writeFileSync(
    join(dest, "stats.json"),
    JSON.stringify(statsOf(events, rounds.length), null, 2),
  );
  return manifest;
}

/** Read back the stats a materialized trial wrote. Zeroes if it has none. */
export function readStats(dest: string): TrialStats {
  try {
    return JSON.parse(readFileSync(join(dest, "stats.json"), "utf8")) as TrialStats;
  } catch {
    return { rounds: 0, hostFnErrors: 0, parseErrors: 0 };
  }
}
