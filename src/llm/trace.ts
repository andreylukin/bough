/**
 * Raw provider I/O, on disk, for harness experiments.
 *
 * WHAT THIS IS FOR. Nothing else in the tree records what was actually SENT to the
 * model. `messages` stores rendered parts — the turn as the UI shows it — which is
 * the right store for a conversation and the wrong one for an experiment: it cannot
 * answer "which system prompt bytes produced this round". A loop that edits prompt
 * text and grades the outcome has to be able to answer that, or its attribution is
 * guesswork dressed as measurement. So this decorator writes the request and the
 * response verbatim, per round, including the rounds that FAILED — an error is
 * evidence too, and the retry wrapper would otherwise swallow it.
 *
 * OFF UNLESS ASKED. No `BOUGH_TRACE_DIR`, no sink, no cost: `traceSink` returns
 * null and `withTrace` hands back the inner client unwrapped. This is a diagnostic
 * seam, not a feature — nothing in the product reads what it writes.
 *
 * THE FORMAT is JSONL, one file per turn, one line per round, self-contained:
 *
 *   {"type":"prompt","tier":"system","sha":"…","text":"…"}   first sight of a prefix
 *   {"type":"round","n":1,"systemSha":"…","request":{…},"response":{…}}
 *
 * A prefix is written ONCE and referenced by sha afterwards, because it is
 * byte-identical across every round of a turn and repeating 30KB per round would
 * bury the signal. The file still reconstructs standalone — no reader needs the
 * manifest, and a sha in a round line that has no `prompt` line above it is a bug
 * worth seeing rather than a lookup to resolve elsewhere.
 *
 * WHY THE DECORATOR SITS INSIDE THE RETRIES. `clientFor` composes
 * retries(trace(pricing(provider))): outside the retries it would record one line
 * per `run()` and silently collapse five failed attempts into the sixth's success,
 * which is precisely the pattern an experiment most wants to see. Inside pricing so
 * a recorded round already carries `costUsd`.
 */
import { appendFileSync, mkdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import type { LlmClient, LlmParams, LlmResult } from "../types.ts";
import { sectionSha } from "../prompt/assemble.ts";
import type { SectionSha } from "../prompt/assemble.ts";

/** Which turn a trace belongs to. Resolved by the caller; this module asks nothing. */
export interface TraceLabel {
  dir: string;
  sessionId: string;
  turnId: string;
}

/**
 * Where a turn's trace goes, or null when tracing is off.
 *
 * One file per turn keyed by both ids: concurrent turns write concurrently, and a
 * single shared file would interleave their rounds into nonsense.
 */
export function traceLabel(
  sessionId: string,
  turnId: string,
  env: (k: string) => string | undefined = (k) => process.env[k],
): TraceLabel | null {
  const dir = env("BOUGH_TRACE_DIR")?.trim();
  return dir ? { dir, sessionId, turnId } : null;
}

/** The path a label's rounds are appended to. */
export function tracePath(label: TraceLabel): string {
  return join(label.dir, label.sessionId, `${label.turnId}.jsonl`);
}

/** The path a label's manifest is written to. */
export function manifestPath(label: TraceLabel): string {
  return join(label.dir, label.sessionId, `${label.turnId}.manifest.json`);
}

/**
 * What the turn knew that the provider boundary does not: which prompt sections
 * went in, and what the turn was configured with.
 *
 * `LlmParams` carries the assembled prefix as one opaque string, so section
 * identity has to be written from where assembly happened. That is the whole point
 * of the manifest — an editable component is a SECTION, and without this the trace
 * can only say "the prefix changed", never which file did.
 */
export interface TurnManifest {
  sessionId: string;
  turnId: string;
  model: string;
  effort?: string;
  workspace?: string;
  /** Every included section, in prompt order, with the sha of its exact text. */
  sections: readonly SectionSha[];
  startedAt: number;
}

function writeLine(path: string, value: unknown): void {
  try {
    mkdirSync(dirname(path), { recursive: true });
    appendFileSync(path, `${JSON.stringify(value)}\n`);
  } catch {
    // A trace is diagnostic. A full disk or an unwritable directory must never be
    // the reason a turn dies, and there is no one to tell — the sink has no
    // channel to the user by design.
  }
}

/** Write a turn's manifest. Called once, from where the prompt was assembled. */
export function writeManifest(label: TraceLabel, manifest: TurnManifest): void {
  try {
    const path = manifestPath(label);
    mkdirSync(dirname(path), { recursive: true });
    writeFileSync(path, `${JSON.stringify(manifest, null, 2)}\n`);
  } catch {
    // As above: diagnostic, never fatal.
  }
}

/** A round as it lands in the JSONL. Exported so a reader can type what it parses. */
export interface RoundRecord {
  type: "round";
  /** 1-based within this turn, counting failed attempts. */
  n: number;
  ts: number;
  latencyMs: number;
  model: string;
  effort?: string;
  systemSha: string;
  volatileSha: string;
  request: {
    maxTokens: number;
    toolChoice?: "none";
    /** Tool NAMES only: the schemas are fixed per build and identical every round. */
    tools: string[];
    messages: LlmParams["messages"];
  };
  response?: { content: LlmResult["content"]; stopReason: string; usage?: LlmResult["usage"] };
  /** Present instead of `response` when the attempt threw. */
  error?: { name: string; message: string };
}

/**
 * Record every round this client runs. Returns `inner` untouched when `label` is
 * null, so the non-tracing path pays nothing at all.
 */
export function withTrace(inner: LlmClient, label: TraceLabel | null): LlmClient {
  if (!label) return inner;
  const path = tracePath(label);
  const seen = new Set<string>();
  let n = 0;

  /** Emit a prefix's text the first time this turn sends it; return its sha either way. */
  const prefix = (tier: "system" | "volatile", text: string): string => {
    const sha = sectionSha(text);
    if (!seen.has(sha)) {
      seen.add(sha);
      writeLine(path, { type: "prompt", tier, sha, text });
    }
    return sha;
  };

  return {
    async run(params, onText, signal) {
      const round = ++n;
      const systemSha = prefix("system", params.system ?? "");
      const volatileSha = prefix("volatile", params.systemVolatile ?? "");
      const started = Date.now();
      const base = {
        type: "round" as const,
        n: round,
        ts: started,
        model: params.model,
        effort: params.effort,
        systemSha,
        volatileSha,
        request: {
          maxTokens: params.maxTokens,
          toolChoice: params.toolChoice,
          tools: params.tools.map((t) => t.name),
          messages: params.messages,
        },
      };
      try {
        const result = await inner.run(params, onText, signal);
        writeLine(path, {
          ...base,
          latencyMs: Date.now() - started,
          response: {
            content: result.content,
            stopReason: result.stopReason,
            usage: result.usage,
          },
        } satisfies RoundRecord);
        return result;
      } catch (err) {
        writeLine(path, {
          ...base,
          latencyMs: Date.now() - started,
          error: {
            name: err instanceof Error ? err.constructor.name : typeof err,
            message: err instanceof Error ? err.message : String(err),
          },
        } satisfies RoundRecord);
        throw err;
      }
    },
  };
}
