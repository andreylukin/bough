/**
 * The compaction scout — a bash-capable subagent that reads the CURRENT state of the
 * files a span touched, so the summary describes the checkout rather than the
 * conversation's memory of it.
 *
 * WHY THIS EXISTS. `compact.ts` summarizes from the transcript and nothing else, and a
 * transcript is a record of intentions as much as outcomes: a span says "renamed
 * `foo()` to `bar()`", then three turns later a revert put it back, and the summary
 * that replaces the span asserts a rename that is not in the tree. The compacted
 * branch then CONTINUES from that summary — it is the only thing left of those turns —
 * so a wrong fact there is not a cosmetic blemish, it is the next turn's premise.
 * Letting a scout run `git log`, `ls` and `rg` over the directories in question is how
 * the summary gets to be about what is on disk now.
 *
 * IT IS ENRICHMENT, AND IT NEVER FAILS THE COMPACTION. Every path out of here that is
 * not notes is `null`: no paths found, no key for the scout's model, a provider error,
 * an overrun, a scout that returned nothing. The caller then summarizes exactly as it
 * did before this module existed. That asymmetry is deliberate — compaction is how a
 * user rescues a conversation that has grown too long, and an enrichment step that
 * could take it down would make the rescue less reliable than no rescue at all.
 *
 * SCOPED TO THE DIRECTORIES OF THE FILES THE SPAN TOUCHED, not to the workspace. A
 * scout pointed at a repo root explores the repo; pointed at `src/history` it explores
 * what the span was about. `touchedPaths` mines those paths out of the transcript
 * text — including the `run_steps` program source, which is where a path appears in
 * this harness, since every file verb is a call inside a program rather than a tool
 * call of its own — and keeps only the ones that still exist, because a path that was
 * deleted has nothing to explore and a hallucinated one never had anything.
 *
 * ROUNDS ARE CAPPED AND THE TOOL IS ONE. The scout gets `bash` and nothing else, and
 * `MAX_ROUNDS` rounds to use it in; the round after the cap is asked for its notes with
 * tools forbidden, so an overrun still yields what it learned instead of throwing it
 * away. It runs in the session's own workspace with the user's authority — the same
 * authority every other command in the session already has (spec §6: there is no
 * sandbox) — but it is briefed to read and not to write, and it is not the thing that
 * writes the branch.
 */
import { existsSync } from "node:fs";
import { isAbsolute, join, dirname, relative } from "node:path";
import { clientFor } from "../llm/client.ts";
import { createShellHostFns } from "../hostfn/shell.ts";
import type { Message } from "../schema/parts.ts";
import type { LlmClient, LlmContentBlock, LlmMessage } from "../types.ts";
import { renderSpan } from "./compact.ts";

/**
 * The scout's model.
 *
 * Pinned rather than inherited, and it is the ONE decision here that is not about
 * safety: the session's own model is whatever the user is paying for their real work,
 * and reading three directories to check whether a rename survived is not that work.
 * `gpt-5.6-luna` is fast and cheap enough to be worth spending on every compaction.
 * Overridable because a user holding no OpenAI key needs a way to name a model they
 * can actually reach — and if they do not, `clientFor` throws, this returns null, and
 * compaction proceeds unenriched.
 */
export const DEFAULT_EXPLORE_MODEL = "gpt-5.6-luna";

export function exploreModel(env: NodeJS.ProcessEnv = process.env): string {
  return env["BOUGH_COMPACT_EXPLORE_MODEL"]?.trim() || DEFAULT_EXPLORE_MODEL;
}

/** Rounds the scout may use `bash` in before it is asked to write up. */
const MAX_ROUNDS = 6;
/** Wall clock for the whole scouting run. A compaction waits on this. */
const TIMEOUT_MS = 90_000;
/** Directories named in the brief. Beyond a handful the scope stops being a scope. */
const MAX_DIRS = 6;
/** One command's output as the scout sees it. */
const OUTPUT_CLIP = 4000;
const MAX_TOKENS = 1024;

const SYSTEM =
  "You are scouting a codebase for a summarizer. You will be given a span of a " +
  "coding-agent conversation and the directories of the files it touched. Use bash to " +
  "establish what is TRUE OF THE CHECKOUT NOW — whether the changes the span describes " +
  "are present, what shape the code ended up in, what the recent commits say. Read " +
  "only: ls, cat, rg, git log, git diff, sed -n. Never write, never commit, never run " +
  "a build or a test suite. Then answer with terse notes for the summarizer: what is " +
  "actually in the tree, and anything the conversation claims that the tree contradicts. " +
  "Notes only — no preamble, no offer to continue.";

/**
 * Paths the span touched that still exist, as workspace-relative strings.
 *
 * Mined from the RENDERED transcript rather than from part structure on purpose. A file
 * verb in this harness is `await patch("src/x.ts", …)` inside a `run_steps` program, so
 * the path is a string literal in tool-call input; there is no structured field to read
 * and there never will be while the model acts through programs. A regex over the text
 * plus an `existsSync` filter is the honest version of that: the regex over-matches
 * happily (versions, globs, sentences with dots) and the filesystem is what decides.
 */
export function touchedPaths(span: readonly Message[], workspace: string): string[] {
  const text = renderSpan(span);
  const out: string[] = [];
  const seen = new Set<string>();
  // A path-shaped token: at least one separator or an extension, and none of the
  // characters that would make it prose. Deliberately loose — `existsSync` is the gate.
  for (const m of text.matchAll(/[\w./-]*[\w-]+\.[A-Za-z]\w{0,7}\b/g)) {
    const raw = m[0];
    if (!raw || seen.has(raw)) continue;
    seen.add(raw);
    const abs = isAbsolute(raw) ? raw : join(workspace, raw);
    // Inside the workspace only. An absolute path to /etc or to another checkout may be
    // real and is still not this session's subject, and pointing a scout at it would
    // scope the exploration by whatever the transcript happened to mention.
    const rel = relative(workspace, abs);
    if (rel.startsWith("..") || isAbsolute(rel)) continue;
    if (!existsSync(abs)) continue;
    out.push(rel);
  }
  return out;
}

/** The directories those files live in — the scout's actual scope. */
export function touchedDirs(paths: readonly string[]): string[] {
  const dirs: string[] = [];
  for (const p of paths) {
    const d = dirname(p);
    const norm = d === "." ? "." : d;
    if (!dirs.includes(norm)) dirs.push(norm);
    if (dirs.length >= MAX_DIRS) break;
  }
  return dirs;
}

export interface ExploreCtx {
  sessionId: string;
  workspace: string;
  /** Injected in tests. Absent = the provider-routed client for the scout's model. */
  llm?: LlmClient;
  model?: string;
}

const BASH_TOOL = {
  name: "bash",
  description:
    "Run one read-only shell command in the workspace and get its combined output.",
  inputSchema: {
    type: "object",
    properties: { command: { type: "string", description: "the command to run" } },
    required: ["command"],
  },
};

/**
 * Scout the directories a span touched. Returns notes for the summarizer, or `null`
 * when there is nothing to say or anything at all went wrong (see the header: this
 * step may not fail a compaction).
 */
export async function exploreSpan(
  ctx: ExploreCtx,
  span: readonly Message[],
): Promise<string | null> {
  const paths = touchedPaths(span, ctx.workspace);
  if (paths.length === 0) return null;
  const dirs = touchedDirs(paths);

  try {
    const model = ctx.model ?? exploreModel();
    const llm = ctx.llm ?? clientFor(model);
    const shell = createShellHostFns({ sessionId: ctx.sessionId, workspace: ctx.workspace });
    // The scout's own clock. A compaction is a foreground request the user is waiting
    // on, so a scout that wanders is cut off and the compaction proceeds without it.
    const timer = AbortSignal.timeout(TIMEOUT_MS);

    const messages: LlmMessage[] = [{
      role: "user",
      content: [{
        type: "text",
        text: `Directories to explore: ${dirs.join(", ")}\nFiles the span touched: ${
          paths.slice(0, 40).join(", ")
        }\n\nThe span:\n${renderSpan(span)}`,
      }],
    }];

    for (let round = 0; round <= MAX_ROUNDS; round++) {
      const last = round === MAX_ROUNDS;
      const res = await llm.run(
        {
          model,
          system: SYSTEM,
          maxTokens: MAX_TOKENS,
          messages,
          tools: last ? [] : [BASH_TOOL],
          // The write-up round. Without this the cap would throw away everything the
          // scout learned in the rounds it did get.
          ...(last ? { toolChoice: "none" as const } : {}),
        },
        () => {},
        timer,
      );
      const calls = res.content.filter((b) => b.type === "tool_use");
      if (calls.length === 0) {
        const text = res.content
          .filter((b) => b.type === "text")
          .map((b) => b.text)
          .join("")
          .trim();
        return text || null;
      }
      messages.push({ role: "assistant", content: res.content });
      const results: LlmContentBlock[] = [];
      for (const call of calls) {
        const command = (call.input as { command?: unknown } | null)?.command;
        if (typeof command !== "string" || !command.trim()) {
          results.push({
            type: "tool_result",
            toolUseId: call.id,
            content: "bash needs a non-empty command string",
            isError: true,
          });
          continue;
        }
        // Tagged like any other command in the session, because it IS one: it lands in
        // the tag history where a later session can see what the scout looked at.
        const out = await shell.bash(command, "compact:explore:scout").catch(
          (err: unknown) => `[failed] ${err instanceof Error ? err.message : String(err)}`,
        );
        results.push({
          type: "tool_result",
          toolUseId: call.id,
          content: out.length > OUTPUT_CLIP ? `${out.slice(0, OUTPUT_CLIP)}…` : out,
          isError: false,
        });
      }
      messages.push({ role: "user", content: results });
    }
    return null;
  } catch {
    // Deliberately total. Every failure here — no key, a 429, a timeout, a shell that
    // could not start — is a compaction that summarizes from the transcript alone,
    // which is what it did before this module existed.
    return null;
  }
}
