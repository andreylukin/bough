/**
 * Minimal MCP client over the stdio transport: newline-delimited JSON-RPC 2.0 to a
 * child process we spawn ourselves. Hand-rolled (like the proxy and the CA) because
 * owning the spawn is the point — the manager passes the exact argv and a
 * minimal, proxy-routed env, which an SDK's own child_process spawn would bypass.
 *
 * Speaks the v1 slice we use: initialize handshake, tools/list (paginated),
 * tools/call. Server-initiated requests are answered with "method not found";
 * notifications are ignored. Resources/prompts/sampling are out of scope.
 */

/**
 * What the manager needs from any transport — the stdio client below and the
 * remote client (remote.ts) both satisfy it.
 */
export interface McpConnection {
  listTools(): Promise<McpToolInfo[]>;
  callTool(name: string, args: unknown): Promise<McpCallResult>;
  close(): Promise<void>;
  readonly alive: boolean;
  /** Recent diagnostics ("why is it failing") — stderr for stdio, last transport error for remote. */
  readonly stderrTail: string;
}

/** One tool as advertised by tools/list. */
export interface McpToolInfo {
  name: string;
  description?: string;
  inputSchema?: { properties?: Record<string, unknown>; required?: string[] };
  /**
   * Behavior hints (readOnlyHint, destructiveHint, …). Server-supplied and
   * untrusted — the gate uses them to SEED classification, never to grant.
   */
  annotations?: Record<string, unknown>;
}

export interface McpCallResult {
  content?: Array<{ type: string; text?: string }>;
  structuredContent?: unknown;
  isError?: boolean;
}

interface Pending {
  resolve: (v: unknown) => void;
  reject: (e: Error) => void;
}

const PROTOCOL_VERSION = "2025-06-18";
const RPC_TIMEOUT_MS = 30_000;
/** Tool calls can legitimately be slow (browser automation, long fetches). */
const CALL_TIMEOUT_MS = 5 * 60_000;
const STDERR_TAIL_BYTES = 4_096;

export class McpStdioClient implements McpConnection {
  #child: Deno.ChildProcess;
  #writer: WritableStreamDefaultWriter<Uint8Array>;
  #pending = new Map<number, Pending>();
  #seq = 0;
  #stderrTail = "";
  #exited = false;
  #closed = false;

  private constructor(child: Deno.ChildProcess) {
    this.#child = child;
    this.#writer = child.stdin.getWriter();
    void this.#readLoop();
    void this.#drainStderr();
    // When the child dies, fail everything in flight instead of hanging to timeout.
    child.status.then(() => {
      this.#exited = true;
      this.#failAll(new Error(`MCP server exited${this.#stderrNote()}`));
    });
  }

  /**
   * Spawn `argv` (composed by the caller) and run the
   * MCP initialize handshake. `env` is the child's ENTIRE environment (clearEnv) —
   * the caller composes the minimal set: PATH/HOME, the server's declared env, and
   * the Claw Patrol proxy env.
   */
  static async connect(
    opts: { argv: string[]; cwd?: string; env: Record<string, string> },
  ): Promise<McpStdioClient> {
    const child = new Deno.Command(opts.argv[0], {
      args: opts.argv.slice(1),
      cwd: opts.cwd,
      env: opts.env,
      clearEnv: true,
      stdin: "piped",
      stdout: "piped",
      stderr: "piped",
    }).spawn();
    const client = new McpStdioClient(child);
    try {
      await client.#request("initialize", {
        protocolVersion: PROTOCOL_VERSION,
        capabilities: {},
        clientInfo: { name: "bough", version: "0" },
      });
      await client.#notify("notifications/initialized");
      return client;
    } catch (e) {
      await client.close();
      throw e;
    }
  }

  /** All tools the server advertises, following pagination cursors. */
  async listTools(): Promise<McpToolInfo[]> {
    const tools: McpToolInfo[] = [];
    let cursor: string | undefined;
    do {
      const res = await this.#request("tools/list", cursor ? { cursor } : {}) as {
        tools?: McpToolInfo[];
        nextCursor?: string;
      };
      tools.push(...(res.tools ?? []));
      cursor = res.nextCursor;
    } while (cursor);
    return tools;
  }

  callTool(name: string, args: unknown): Promise<McpCallResult> {
    return this.#request(
      "tools/call",
      { name, arguments: args ?? {} },
      CALL_TIMEOUT_MS,
    ) as Promise<McpCallResult>;
  }

  /** Last stderr output, for "why did it die" diagnostics in /mcp status. */
  get stderrTail(): string {
    return this.#stderrTail;
  }

  get alive(): boolean {
    return !this.#exited && !this.#closed;
  }

  /** Terminate the child and release its pipes. Safe to call twice; never throws. */
  async close(): Promise<void> {
    if (this.#closed) return;
    this.#closed = true;
    this.#failAll(new Error("MCP connection closed"));
    try {
      await this.#writer.close();
    } catch { /* already closed / child gone */ }
    try {
      this.#child.kill("SIGTERM");
    } catch { /* already exited */ }
    // Grace, then force — a wedged server must not leak past the manager.
    const grace = setTimeout(() => {
      try {
        this.#child.kill("SIGKILL");
      } catch { /* raced its exit */ }
    }, 2_000);
    await this.#child.status;
    clearTimeout(grace);
  }

  #request(method: string, params: unknown, timeoutMs = RPC_TIMEOUT_MS): Promise<unknown> {
    if (!this.alive) return Promise.reject(new Error(`MCP server is down${this.#stderrNote()}`));
    const id = ++this.#seq;
    const p = new Promise<unknown>((resolve, reject) => {
      this.#pending.set(id, { resolve, reject });
      setTimeout(() => {
        if (!this.#pending.delete(id)) return;
        reject(new Error(`MCP ${method} timed out after ${timeoutMs}ms`));
      }, timeoutMs);
    });
    void this.#send({ jsonrpc: "2.0", id, method, params });
    return p;
  }

  #notify(method: string, params?: unknown): Promise<void> {
    return this.#send({ jsonrpc: "2.0", method, ...(params !== undefined ? { params } : {}) });
  }

  async #send(msg: unknown): Promise<void> {
    try {
      await this.#writer.write(new TextEncoder().encode(JSON.stringify(msg) + "\n"));
    } catch {
      // stdin gone = child dead; the status handler fails the pending request.
    }
  }

  async #readLoop(): Promise<void> {
    let buf = "";
    try {
      for await (const chunk of this.#child.stdout.pipeThrough(new TextDecoderStream())) {
        buf += chunk;
        for (;;) {
          const nl = buf.indexOf("\n");
          if (nl < 0) break;
          const line = buf.slice(0, nl).trim();
          buf = buf.slice(nl + 1);
          if (line) this.#dispatch(line);
        }
      }
    } catch { /* stream torn down on close/exit */ }
  }

  #dispatch(line: string): void {
    let msg: {
      id?: number | string;
      method?: string;
      result?: unknown;
      error?: { code?: number; message?: string };
    };
    try {
      msg = JSON.parse(line);
    } catch {
      return; // a server that logs to stdout — skip the noise
    }
    if (msg.id !== undefined && msg.method === undefined) {
      const p = this.#pending.get(Number(msg.id));
      if (!p) return;
      this.#pending.delete(Number(msg.id));
      if (msg.error) p.reject(new Error(msg.error.message ?? "MCP error"));
      else p.resolve(msg.result);
      return;
    }
    // Server-initiated request (sampling, roots, …): refuse politely so it doesn't
    // hang; notifications need no reply.
    if (msg.id !== undefined && msg.method !== undefined) {
      void this.#send({
        jsonrpc: "2.0",
        id: msg.id,
        error: { code: -32601, message: `bough does not support ${msg.method}` },
      });
    }
  }

  async #drainStderr(): Promise<void> {
    try {
      for await (const chunk of this.#child.stderr.pipeThrough(new TextDecoderStream())) {
        this.#stderrTail = (this.#stderrTail + chunk).slice(-STDERR_TAIL_BYTES);
      }
    } catch { /* stream torn down on close/exit */ }
  }

  #stderrNote(): string {
    const tail = this.#stderrTail.trim();
    return tail ? ` — stderr: ${tail.slice(-500)}` : "";
  }

  #failAll(err: Error): void {
    for (const [, p] of this.#pending) p.reject(err);
    this.#pending.clear();
  }
}
