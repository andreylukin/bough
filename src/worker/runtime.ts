/**
 * Local worker runtime — the worker runs as part of bough: bough owns and supervises
 * a small inference server as a child process, not a separate daemon. This launches
 * `llama-server` over the shared GGUF (fetched by scripts/worker-model.sh) and hands
 * callers a localhost OpenAI-compatible endpoint.
 *
 * Env contract:
 *   BOUGH_WORKER_URL       escape hatch — any endpoint, returned untouched
 *   BOUGH_WORKER_PORT      local port (default 8080)
 *   BOUGH_WORKER_GGUF      model filename under ~/.bough/models
 *   BOUGH_WORKER_GGUF_URL  where to download the GGUF if missing (else error)
 *   BOUGH_LLAMA_SERVER     llama-server binary (default: from PATH)
 *
 * The embedder is a second, tiny llama-server (--embedding) for recall search:
 *   BOUGH_EMBED_URL / BOUGH_EMBED_PORT (8081) / BOUGH_EMBED_GGUF /
 *   BOUGH_EMBED_GGUF_URL — same contract, but the GGUF URL has a default
 *   (nomic-embed v1.5 Q8, ~140MB) since there's no interactive install step.
 *
 * Deferred: graceful shutdown / supervised restart. The
 * child is spawned detached and lives past this process.
 */
import { join } from "node:path";
import { homedir } from "node:os";

const DEFAULT_GGUF = "qwen2.5-coder-3b-instruct-q4_k_m.gguf";
const DEFAULT_PORT = 8080;

const DEFAULT_EMBED_GGUF = "nomic-embed-text-v1.5.Q8_0.gguf";
const DEFAULT_EMBED_GGUF_URL =
  `https://huggingface.co/nomic-ai/nomic-embed-text-v1.5-GGUF/resolve/main/${DEFAULT_EMBED_GGUF}`;
const DEFAULT_EMBED_PORT = 8081;

function envPort(name: string, fallback: number): number {
  const raw = Deno.env.get(name);
  const parsed = raw ? Number.parseInt(raw, 10) : NaN;
  return Number.isFinite(parsed) ? parsed : fallback;
}

function workerPort(): number {
  return envPort("BOUGH_WORKER_PORT", DEFAULT_PORT);
}

/**
 * Ensure a worker endpoint is reachable and return its base URL. Reuses an
 * already-running server (or `BOUGH_WORKER_URL`); otherwise starts `llama-server`
 * over the on-disk model and waits for it to become healthy.
 */
export async function ensureWorker(): Promise<string> {
  const override = Deno.env.get("BOUGH_WORKER_URL");
  if (override) return override;

  const url = `http://127.0.0.1:${workerPort()}`;
  if (await healthy(url)) return url;

  const model = await ensureModel();
  startServer(model, workerPort());
  if (await waitHealthy(url, 90)) return url;
  throw new Error(`worker server did not become healthy at ${url}`);
}

/**
 * Ensure the EMBEDDER endpoint is reachable and return its base URL — the same
 * lifecycle as ensureWorker over a second llama-server started with --embedding.
 * The model is ~140MB, so unlike the worker it downloads by default.
 */
export async function ensureEmbedder(): Promise<string> {
  const override = Deno.env.get("BOUGH_EMBED_URL");
  if (override) return override;

  const url = `http://127.0.0.1:${envPort("BOUGH_EMBED_PORT", DEFAULT_EMBED_PORT)}`;
  if (await healthy(url)) return url;

  const model = await ensureFile(
    Deno.env.get("BOUGH_EMBED_GGUF") ?? DEFAULT_EMBED_GGUF,
    Deno.env.get("BOUGH_EMBED_GGUF_URL") ?? DEFAULT_EMBED_GGUF_URL,
  );
  startServer(model, envPort("BOUGH_EMBED_PORT", DEFAULT_EMBED_PORT), ["--embedding"]);
  if (await waitHealthy(url, 60)) return url;
  throw new Error(`embedder did not become healthy at ${url}`);
}

/**
 * The worker's URL if one is already reachable — never waits on a cold start.
 * For latency-sensitive callers (the edit path) that must fail soft instead of
 * blocking ~90s on a model load. Kicks off `ensureWorker` in the background so
 * the next call finds a live server.
 */
export async function workerIfRunning(): Promise<string | null> {
  const override = Deno.env.get("BOUGH_WORKER_URL");
  if (override) return override;
  const url = `http://127.0.0.1:${workerPort()}`;
  if (await healthy(url)) return url;
  ensureWorker().catch(() => {});
  return null;
}

async function ensureModel(): Promise<string> {
  const filename = Deno.env.get("BOUGH_WORKER_GGUF") ?? DEFAULT_GGUF;
  const ggufUrl = Deno.env.get("BOUGH_WORKER_GGUF_URL");
  if (!ggufUrl) {
    const path = join(homedir(), ".bough", "models", filename);
    try {
      const stat = await Deno.stat(path);
      if (stat.isFile) return path;
    } catch {
      // fall through to the error
    }
    throw new Error(
      `worker model missing at ${path} and BOUGH_WORKER_GGUF_URL is not set ` +
        "(point it at a GGUF to download, or set BOUGH_WORKER_URL to a running endpoint)",
    );
  }
  return await ensureFile(filename, ggufUrl);
}

/** The GGUF's path under ~/.bough/models, downloading it (resumable) if missing. */
async function ensureFile(filename: string, ggufUrl: string): Promise<string> {
  const dir = join(homedir(), ".bough", "models");
  const path = join(dir, filename);
  try {
    const stat = await Deno.stat(path);
    if (stat.isFile) return path;
  } catch {
    // fall through to download
  }
  await Deno.mkdir(dir, { recursive: true });
  const dl = await new Deno.Command("curl", { args: ["-fSL", "-C", "-", "-o", path, ggufUrl] })
    .output();
  if (!dl.success) {
    throw new Error(`model download failed: ${new TextDecoder().decode(dl.stderr)}`);
  }
  return path;
}

function startServer(modelPath: string, port: number, extraArgs: string[] = []): void {
  const bin = Deno.env.get("BOUGH_LLAMA_SERVER") ?? "llama-server";
  // Detached: a crash in inference must not take down the agent, and the server
  // outlives this process so the next boot reuses it via the health check.
  const child = new Deno.Command(bin, {
    args: ["-m", modelPath, "--host", "127.0.0.1", "--port", String(port), ...extraArgs],
    stdin: "null",
    stdout: "null",
    stderr: "null",
  }).spawn();
  child.unref();
}

async function healthy(url: string): Promise<boolean> {
  try {
    const res = await fetch(`${url}/health`, { signal: AbortSignal.timeout(1500) });
    await res.body?.cancel();
    return res.status === 200;
  } catch {
    return false;
  }
}

async function waitHealthy(url: string, retries: number): Promise<boolean> {
  for (let i = 0; i < retries; i++) {
    if (await healthy(url)) return true;
    await new Promise((r) => setTimeout(r, 1000));
  }
  return false;
}
