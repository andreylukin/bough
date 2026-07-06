/**
 * The worker provider: a small local model given one self-contained task as a single
 * system+user exchange. Just the OpenAI-compatible `/v1/chat/completions` shape,
 * the same contract whether it's served by bough's own `llama-server`, a
 * llamafile, or a remote endpoint.
 *
 * Beyond plain completion this exposes the rest of the llama-server surface bough
 * uses: schema-constrained decoding (`jsonSchema` — conformance is structural, not
 * prompted), mean token logprob (a cheap confidence signal for escalation gating),
 * fill-in-the-middle (`workerInfill`), and embeddings (`workerEmbed`). The non-chat
 * endpoints are llama-server-specific; a remote OpenAI-shaped endpoint only
 * guarantees the chat surface.
 */

export interface WorkerParams {
  system: string;
  user: string;
  maxTokens: number;
  /** Left off unless set, so the server's default sampling applies. Reasoning-tuned
   * workers need their recommended decoding (often temp 1.0 / top_p 0.95). */
  temperature?: number;
  topP?: number;
  /** JSON Schema the reply must conform to. llama-server compiles it to a grammar
   * and constrains decoding — small-model format drift becomes impossible. */
  jsonSchema?: Record<string, unknown>;
  /** Reuse the server-side KV cache across calls sharing a prompt prefix. */
  cachePrompt?: boolean;
}

export interface WorkerReply {
  text: string;
  /** Mean token logprob when the server reports logprobs — low means the worker
   * was guessing; callers can escalate on it instead of paying for a CHECK. */
  avgLogprob?: number;
}

/** Send one system+user exchange and return the assistant's text. */
export async function workerComplete(baseUrl: string, params: WorkerParams): Promise<string> {
  return (await complete(baseUrl, params, false)).text;
}

/** Like `workerComplete`, but asks for logprobs and returns the confidence too. */
export async function workerCompleteMeta(
  baseUrl: string,
  params: WorkerParams,
): Promise<WorkerReply> {
  return await complete(baseUrl, params, true);
}

async function complete(
  baseUrl: string,
  params: WorkerParams,
  wantLogprobs: boolean,
): Promise<WorkerReply> {
  const body = JSON.stringify({
    // llama-server serves a single model and ignores the name; kept for
    // OpenAI-shape compatibility with remote endpoints.
    model: Deno.env.get("BOUGH_WORKER_MODEL") ?? "worker",
    max_tokens: params.maxTokens,
    messages: [
      { role: "system", content: params.system },
      { role: "user", content: params.user },
    ],
    ...(params.temperature !== undefined ? { temperature: params.temperature } : {}),
    ...(params.topP !== undefined ? { top_p: params.topP } : {}),
    ...(params.jsonSchema
      ? {
        response_format: {
          type: "json_schema",
          json_schema: { name: "reply", schema: params.jsonSchema },
        },
      }
      : {}),
    ...(params.cachePrompt !== undefined ? { cache_prompt: params.cachePrompt } : {}),
    ...(wantLogprobs ? { logprobs: true } : {}),
  });
  const raw = await sendWithRetry(`${baseUrl}/v1/chat/completions`, body, 4);
  return parseChat(raw);
}

export interface InfillParams {
  /** Text before the hole. */
  prefix: string;
  /** Text after the hole. */
  suffix: string;
  maxTokens: number;
  temperature?: number;
}

/**
 * Fill-in-the-middle via llama-server's `/infill` — the worker model's FIM
 * training, no chat framing. Returns the completion for the hole between
 * `prefix` and `suffix`.
 */
export async function workerInfill(baseUrl: string, params: InfillParams): Promise<string> {
  const body = JSON.stringify({
    input_prefix: params.prefix,
    input_suffix: params.suffix,
    prompt: "",
    n_predict: params.maxTokens,
    ...(params.temperature !== undefined ? { temperature: params.temperature } : {}),
  });
  const raw = await sendWithRetry(`${baseUrl}/infill`, body, 4);
  let content: unknown;
  try {
    content = (JSON.parse(raw) as { content?: unknown }).content;
  } catch {
    throw new Error(`could not parse infill response: ${raw.slice(0, 300)}`);
  }
  if (typeof content !== "string") throw new Error("infill returned no content");
  return content;
}

/**
 * Embed texts via `/v1/embeddings` (a llama-server started with `--embedding`).
 * Returns one vector per input, in input order.
 */
export async function workerEmbed(baseUrl: string, texts: string[]): Promise<number[][]> {
  const body = JSON.stringify({
    model: Deno.env.get("BOUGH_WORKER_MODEL") ?? "worker",
    input: texts,
  });
  const raw = await sendWithRetry(`${baseUrl}/v1/embeddings`, body, 4);
  let data: { index?: number; embedding?: unknown }[];
  try {
    data = (JSON.parse(raw) as { data?: { index?: number; embedding?: unknown }[] }).data ?? [];
  } catch {
    throw new Error(`could not parse embeddings response: ${raw.slice(0, 300)}`);
  }
  if (data.length !== texts.length) {
    throw new Error(`embeddings returned ${data.length} vectors for ${texts.length} inputs`);
  }
  const vectors: number[][] = new Array(texts.length);
  for (let i = 0; i < data.length; i++) {
    const item = data[i];
    if (!Array.isArray(item.embedding)) throw new Error("embeddings returned no vector");
    vectors[item.index ?? i] = item.embedding as number[];
  }
  return vectors;
}

/**
 * A just-spawned worker can refuse the connection for a beat, and llama-server
 * answers 503 while it loads the model — both transient. Retry a few times with a
 * short pause so a cold worker doesn't read as "worker unavailable".
 */
async function sendWithRetry(url: string, body: string, attempts: number): Promise<string> {
  let res: Response;
  try {
    res = await fetch(url, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body,
      signal: AbortSignal.timeout(60_000),
    });
  } catch (e) {
    if (attempts > 1) return retryAfter(url, body, attempts);
    throw new Error(`worker http error: ${(e as Error).message}`);
  }
  if (res.status === 200) return await res.text();
  const text = await res.text();
  if (res.status === 503 && attempts > 1) return retryAfter(url, body, attempts);
  throw new Error(`worker ${res.status}: ${text}`);
}

async function retryAfter(url: string, body: string, attempts: number): Promise<string> {
  await new Promise((r) => setTimeout(r, 500));
  return sendWithRetry(url, body, attempts - 1);
}

function parseChat(body: string): WorkerReply {
  let choice: { message?: { content?: unknown }; logprobs?: unknown } | undefined;
  try {
    choice = (JSON.parse(body) as { choices?: typeof choice[] }).choices?.[0];
  } catch {
    throw new Error(`could not parse worker response: ${body.slice(0, 300)}`);
  }
  const content = choice?.message?.content;
  if (typeof content !== "string") throw new Error("worker returned no choices");
  const tokens = (choice?.logprobs as { content?: { logprob?: unknown }[] } | undefined)?.content;
  const logprobs = (tokens ?? [])
    .map((t) => t.logprob)
    .filter((l): l is number => typeof l === "number");
  return {
    text: content,
    ...(logprobs.length
      ? { avgLogprob: logprobs.reduce((a, b) => a + b, 0) / logprobs.length }
      : {}),
  };
}
