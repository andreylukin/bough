/**
 * The oracle — a read-only consult of a stronger reasoning model (Amp's `oracle`
 * pattern). The supervisor's program calls `await oracle(question)` when it hits
 * something genuinely hard: the oracle runs its own bounded agentic loop on the
 * oracle model with exactly two tools — a shell held to a read-only CONTRACT (see
 * shellInvocation readOnly; in VM mode it shares the session guest, where
 * read-only is not yet enforced) and file read — gathers its own context from the
 * workspace, and returns prose advice. It never writes, edits, or delegates, so it
 * cannot conflict with the calling turn's work.
 *
 * The loop is deliberately small: direct tool calls (not code-mode — two tools
 * don't need a program), a hard round cap, and the turn's interrupt signal
 * threaded through both the LLM stream and the shell children.
 */
import {
  clientFor,
  type LlmClient,
  type LlmContentBlock,
  type LlmToolDef,
  type LlmUsage,
} from "../supervisor/llm.ts";
import type { ToolRunCtx } from "./types.ts";
import { shellInvocation } from "./bash.ts";
import { readFile } from "./read_file.ts";

const MAX_ROUNDS = 12;
const MAX_TOKENS = 32_000;
const BASH_TIMEOUT_MS = 60_000;

const SYSTEM = [
  "You are the oracle: a senior read-only analyst consulted by another coding agent",
  "mid-task. Answer its question with the most correct, specific analysis you can.",
  "You have two tools: bash (READ-ONLY shell in the workspace — rg, cat, git log,",
  "head; any write is denied by the sandbox) and read(path). Gather your own context:",
  "read the files in question, search for callers and invariants, check history.",
  "You cannot write, edit, run tests that mutate state, or delegate — you advise.",
  "When you have your answer, reply in plain text: the diagnosis or recommendation",
  "first, then the key evidence (file:line), then concrete next steps for the calling",
  "agent. Be specific — name files, symbols, and exact changes. No filler.",
].join(" ");

const TOOLS: LlmToolDef[] = [
  {
    name: "bash",
    description: "Run a READ-ONLY shell command in the workspace (writes are denied). " +
      "Use rg to search; filter output at the source.",
    inputSchema: {
      type: "object",
      properties: { command: { type: "string" } },
      required: ["command"],
      additionalProperties: false,
    },
  },
  {
    name: "read",
    description: "Read a file (path relative to the workspace).",
    inputSchema: {
      type: "object",
      properties: { path: { type: "string" } },
      required: ["path"],
      additionalProperties: false,
    },
  },
];

/** The oracle's shell: same confinement pipeline as bash, minus write access. */
async function readOnlyBash(command: string, ctx: ToolRunCtx): Promise<string> {
  const { argv, env, cwd } = await shellInvocation(command, ctx, { readOnly: true });
  const timeout = AbortSignal.timeout(BASH_TIMEOUT_MS);
  const out = await new Deno.Command(argv[0], {
    args: argv.slice(1),
    cwd,
    env,
    stdout: "piped",
    stderr: "piped",
    signal: ctx.signal ? AbortSignal.any([timeout, ctx.signal]) : timeout,
  }).output();
  const dec = new TextDecoder();
  const chunks = [dec.decode(out.stdout).trimEnd(), dec.decode(out.stderr).trimEnd()]
    .filter(Boolean);
  if (out.code !== 0) chunks.push(`[exit code ${out.code}]`);
  return chunks.join("\n") || "(no output)";
}

export interface OracleOpts {
  /** The oracle model id (turn.ts oracleModel()). */
  model: string;
  /** Injectable client for tests; defaults to clientFor(model). */
  llm?: LlmClient;
  /** Reports each round's token usage so the calling turn can bill it. */
  onUsage?: (usage: LlmUsage) => void;
}

/**
 * Ask the oracle. Runs a bounded read-only tool loop on the oracle model and
 * returns its final prose answer. Throws on provider errors (missing key, etc.) —
 * the calling program sees an ordinary host-function rejection.
 */
export async function runOracle(
  question: string,
  ctx: ToolRunCtx,
  opts: OracleOpts,
): Promise<string> {
  const llm = opts.llm ?? clientFor(opts.model);
  const messages: { role: "user" | "assistant"; content: LlmContentBlock[] }[] = [
    { role: "user", content: [{ type: "text", text: question }] },
  ];
  // Guest-owned sessions: the shell and file reads see the GUEST clone, so the
  // advertised root must be the guest path, not the host origin.
  const system = `${SYSTEM} The workspace root is ${ctx.guestFs?.root ?? ctx.workspace}.`;

  const answer: string[] = [];
  for (let round = 0; round < MAX_ROUNDS; round++) {
    if (ctx.signal?.aborted) throw new Error("oracle call interrupted");
    const result = await llm.run(
      { model: opts.model, system, maxTokens: MAX_TOKENS, messages, tools: TOOLS },
      () => {},
      ctx.signal,
    );
    if (result.usage) opts.onUsage?.(result.usage);

    const toolUses = result.content.filter((b) => b.type === "tool_use");
    // Only the final round's text is the answer — earlier text is thinking-aloud
    // between tool calls, which the caller doesn't need.
    if (toolUses.length === 0) {
      const text = result.content
        .filter((b) => b.type === "text")
        .map((b) => (b as { text: string }).text)
        .join("\n").trim();
      return text || "(the oracle returned no answer)";
    }

    messages.push({ role: "assistant", content: result.content });
    const results: LlmContentBlock[] = [];
    for (const tu of toolUses) {
      if (tu.type !== "tool_use") continue;
      let output: string;
      let isError = false;
      try {
        const input = tu.input as { command?: string; path?: string };
        if (tu.name === "bash") output = await readOnlyBash(input.command ?? "", ctx);
        else if (tu.name === "read") output = await readFile.run({ path: input.path }, ctx);
        else throw new Error(`unknown tool ${tu.name}`);
      } catch (e) {
        output = (e as Error).message;
        isError = true;
      }
      results.push({ type: "tool_result", toolUseId: tu.id, content: output, isError });
    }
    messages.push({ role: "user", content: results });
    // Keep any text the model emitted alongside its last tool calls: if the round
    // cap trips, this trailing analysis is the best answer we have.
    answer.length = 0;
    answer.push(
      ...result.content.filter((b) => b.type === "text").map((b) => (b as { text: string }).text),
    );
  }
  return [
    ...answer,
    `[oracle stopped at the ${MAX_ROUNDS}-round cap — treat the analysis above as partial]`,
  ].join("\n").trim();
}
