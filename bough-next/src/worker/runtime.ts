/**
 * Local worker runtime — the bough-next port of the Gleam worker_runtime (SPEC.md
 * §5.6): "the worker runs as part of bough" means bough owns and supervises a small
 * inference server as a child process, not a separate daemon. This launches
 * `llama-server` over the shared GGUF and hands callers a localhost
 * OpenAI-compatible endpoint.
 *
 * The env contract and model directory are IDENTICAL to the Gleam bough, so one
 * install (and one running llama-server) serves both apps:
 *   BOUGH_WORKER_URL       escape hatch — any endpoint, returned untouched
 *   BOUGH_WORKER_PORT      local port (default 8080)
 *   BOUGH_WORKER_GGUF      model filename under ~/.bough/models
 *   BOUGH_WORKER_GGUF_URL  where to download the GGUF if missing (else error)
 *   BOUGH_LLAMA_SERVER     llama-server binary (default: from PATH)
 *
 * Deferred, as in the Gleam version: graceful shutdown / supervised restart. The
 * child is spawned detached and lives past this process.
 */
import { join } from "node:path";
import { homedir } from "node:os";

const DEFAULT_GGUF = "qwen2.5-coder-3b-instruct-q4_k_m.gguf";
const DEFAULT_PORT = 8080;

function workerPort(): number {
  const raw = Deno.env.get("BOUGH_WORKER_PORT");
  const parsed = raw ? Number.parseInt(raw, 10) : NaN;
  return Number.isFinite(parsed) ? parsed : DEFAULT_PORT;
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

async function ensureModel(): Promise<string> {
  const dir = join(homedir(), ".bough", "models");
  const filename = Deno.env.get("BOUGH_WORKER_GGUF") ?? DEFAULT_GGUF;
  const path = join(dir, filename);
  try {
    const stat = await Deno.stat(path);
    if (stat.isFile) return path;
  } catch {
    // fall through to download / error
  }
  const ggufUrl = Deno.env.get("BOUGH_WORKER_GGUF_URL");
  if (!ggufUrl) {
    throw new Error(
      `worker model missing at ${path} and BOUGH_WORKER_GGUF_URL is not set ` +
        "(point it at a GGUF to download, or set BOUGH_WORKER_URL to a running endpoint)",
    );
  }
  await Deno.mkdir(dir, { recursive: true });
  const dl = await new Deno.Command("curl", { args: ["-fSL", "-C", "-", "-o", path, ggufUrl] })
    .output();
  if (!dl.success) {
    throw new Error(`worker model download failed: ${new TextDecoder().decode(dl.stderr)}`);
  }
  return path;
}

function startServer(modelPath: string, port: number): void {
  const bin = Deno.env.get("BOUGH_LLAMA_SERVER") ?? "llama-server";
  // Detached: a crash in inference must not take down the agent, and the server
  // outlives this process so the next boot reuses it via the health check.
  const child = new Deno.Command(bin, {
    args: ["-m", modelPath, "--host", "127.0.0.1", "--port", String(port)],
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
