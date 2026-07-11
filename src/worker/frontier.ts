/**
 * Frontier-worker mode — BOUGH_WORKER_FRONTIER routes every worker micro-task
 * (digestion, annotations, fast-apply, titles) to a small frontier
 * model instead of the local llama-server. Set it to "1" for the default
 * (claude-haiku-4-5) or to a model id. No llama-server is spawned and no GGUF is
 * needed; the embedder (recall) is unaffected — embeddings have no frontier
 * substitute here.
 *
 * BOUGH_WORKER_LOCAL_ONLY=1 wins over this flag: local-only is the privacy tier,
 * and a mode that ships command output to a remote API must never override it.
 */
import { anthropicClient } from "../supervisor/llm.ts";

const DEFAULT_FRONTIER = "claude-haiku-4-5";

// Runtime override set via PATCH /config; undefined = defer to the env. Process-wide
// and in-memory, like setActiveModel — a restart falls back to BOUGH_WORKER_FRONTIER.
let choiceOverride: string | null | undefined;

/** The frontier worker model id, or null when the worker runs locally. */
export function frontierWorkerModel(): string | null {
  if (Deno.env.get("BOUGH_WORKER_LOCAL_ONLY") === "1") return null;
  if (choiceOverride !== undefined) return choiceOverride;
  const v = Deno.env.get("BOUGH_WORKER_FRONTIER");
  if (!v || v === "0") return null;
  return v === "1" ? DEFAULT_FRONTIER : v;
}

/** The worker picker's current value: "local" or a frontier model id. */
export function workerChoice(): string {
  return frontierWorkerModel() ?? "local";
}

/** Switch the worker at runtime: "local" for the llama-server, else a model id. */
export function setWorkerChoice(choice: string): void {
  choiceOverride = choice === "local" ? null : choice;
}

/** Workers offered in the picker (any id is accepted, like the model picker). */
export const WORKER_OPTIONS: { id: string; label: string }[] = [
  { id: "local", label: "Local · Qwen 3B" },
  { id: DEFAULT_FRONTIER, label: "Haiku 4.5" },
];

export interface FrontierParams {
  system: string;
  user: string;
  maxTokens: number;
  /** Expected reply schema. The API has no grammar-constrained decoding, so this
   * only turns on JSON extraction from the reply; the prompt must still ask for
   * JSON, and callers keep validating (they already do — worker replies were
   * never trusted either). */
  jsonSchema?: Record<string, unknown>;
}

/** One system+user exchange against the frontier worker model. */
export async function frontierComplete(params: FrontierParams): Promise<string> {
  const model = frontierWorkerModel();
  if (!model) throw new Error("frontier worker mode is not enabled");
  const result = await anthropicClient().run(
    {
      model,
      system: params.system,
      maxTokens: params.maxTokens,
      messages: [{ role: "user", content: [{ type: "text", text: params.user }] }],
      tools: [],
    },
    () => {},
  );
  const text = result.content.find((b) => b.type === "text")?.text;
  if (!text) throw new Error("frontier worker returned no text");
  return params.jsonSchema ? extractJson(text) : text;
}

/** The outermost {...} span — strips prose/fences a chat model may wrap JSON in. */
function extractJson(text: string): string {
  const start = text.indexOf("{");
  const end = text.lastIndexOf("}");
  return start >= 0 && end > start ? text.slice(start, end + 1) : text;
}
