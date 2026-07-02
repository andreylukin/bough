/**
 * The worker provider: a small local model given one self-contained task as a single
 * system+user exchange. Just the OpenAI-compatible `/v1/chat/completions` shape,
 * the same contract whether it's served by bough's own `llama-server`, a
 * llamafile, or a remote endpoint.
 */

export interface WorkerParams {
  system: string;
  user: string;
  maxTokens: number;
  /** Left off unless set, so the server's default sampling applies. Reasoning-tuned
   * workers need their recommended decoding (often temp 1.0 / top_p 0.95). */
  temperature?: number;
  topP?: number;
}

/** Send one system+user exchange and return the assistant's text. */
export async function workerComplete(baseUrl: string, params: WorkerParams): Promise<string> {
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
  });
  return await sendWithRetry(`${baseUrl}/v1/chat/completions`, body, 4);
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
  if (res.status === 200) return parse(await res.text());
  const text = await res.text();
  if (res.status === 503 && attempts > 1) return retryAfter(url, body, attempts);
  throw new Error(`worker ${res.status}: ${text}`);
}

async function retryAfter(url: string, body: string, attempts: number): Promise<string> {
  await new Promise((r) => setTimeout(r, 500));
  return sendWithRetry(url, body, attempts - 1);
}

function parse(body: string): string {
  let content: unknown;
  try {
    const parsed = JSON.parse(body) as { choices?: { message?: { content?: unknown } }[] };
    content = parsed.choices?.[0]?.message?.content;
  } catch {
    throw new Error(`could not parse worker response: ${body.slice(0, 300)}`);
  }
  if (typeof content !== "string") throw new Error("worker returned no choices");
  return content;
}
