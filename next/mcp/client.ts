/**
 * The stdio MCP client: newline-delimited JSON-RPC 2.0 to a child process bough
 * spawns itself.
 *
 * THE INVARIANT THIS HOLDS: **a server that does not work fails, by name, in
 * bounded time.** Never a hang (plan T7.1). A hung MCP server is worse than an
 * absent one — the turn it is attached to stalls behind it, the user sees a
 * spinner, and there is nothing in the transcript that says why. So every path out
 * of this module terminates:
 *
 *   - a binary that does not exist fails at spawn, naming the command;
 *   - a process that starts and never answers `initialize` fails on the connect
 *     deadline, with its stderr attached;
 *   - a process that dies — at any point, including mid-call — fails everything in
 *     flight immediately from its exit handler rather than waiting for timeouts;
 *   - every request carries its own deadline;
 *   - `tools/list` pagination is bounded, so a server that returns the same cursor
 *     forever is an error and not an infinite loop;
 *   - a server-initiated request (sampling, roots, elicitation) is REFUSED with a
 *     JSON-RPC error rather than ignored, because a server waiting on a reply that
 *     never comes is the same hang seen from the other end.
 *
 * Every failure is an `McpError` carrying a status and a sentence that names the
 * server, what failed, and the move that resolves it — the text reaches the model
 * as a caught exception inside its program (spec §6, errors.ts).
 *
 * WHY HAND-ROLLED, given `@modelcontextprotocol/sdk` is a dependency. Owning the
 * spawn is the point: bough composes the child's ENTIRE environment (`clearEnv`,
 * `config.childEnv`) so a third-party binary does not inherit the user's provider
 * keys, and it tracks the child so shutdown can kill it (`killAllMcpServers`). The
 * SDK's `StdioClientTransport` spawns for you and neither property survives that.
 *
 * The SDK is used where it fits and nowhere it does not:
 *
 *   - `LATEST_PROTOCOL_VERSION` / `SUPPORTED_PROTOCOL_VERSIONS` / `JSONRPC_VERSION`
 *     — version negotiation against the real, current constants instead of a
 *     hardcoded date that rots;
 *   - `InitializeResultSchema` — the handshake is validated strictly, because a
 *     process that answers something else is not an MCP server and saying so at
 *     connect is far better than an empty tool list later;
 *   - `ToolSchema` — tried first for each advertised tool, with a LENIENT fallback
 *     that keeps any entry carrying a usable name. Dropping a callable tool from
 *     the catalog over a schema nit (an `inputSchema` missing `type: "object"` is
 *     the common one) is a worse outcome than listing it with a thin signature.
 *
 * `tools/call` results are read leniently for the same reason: an unrecognized
 * content block must not turn a successful call into a failure. Resources,
 * prompts, sampling and completions are out of scope (spec §10).
 *
 * Ported from `src/mcp/client.ts`. Deltas from that port are marked `NOTE:`.
 */
import {
  InitializeResultSchema,
  JSONRPC_VERSION,
  LATEST_PROTOCOL_VERSION,
  SUPPORTED_PROTOCOL_VERSIONS,
  ToolSchema,
} from "@modelcontextprotocol/sdk/types.js";
import { z } from "zod";
import { McpError } from "../errors.ts";

// ---------------------------------------------------------------------------
// Shapes
// ---------------------------------------------------------------------------

/**
 * What the layer above needs from any transport. The stdio client below and the
 * remote client (T7.2) both satisfy it, so nothing upstream branches on which one
 * a registry entry produced.
 */
export interface McpConnection {
  /** The registry name this connection is for — every error message carries it. */
  readonly name: string;
  listTools(): Promise<McpToolInfo[]>;
  callTool(name: string, args: unknown): Promise<McpCallResult>;
  close(): Promise<void>;
  readonly alive: boolean;
  /** Recent diagnostics ("why is it failing") — stderr for stdio, last transport error for remote. */
  readonly stderrTail: string;
}

/** One tool as advertised by `tools/list`. */
export interface McpToolInfo {
  name: string;
  description?: string;
  /** JSON Schema for the arguments — only the parts the catalog renders. */
  inputSchema?: { properties?: Record<string, unknown>; required?: string[] };
  /**
   * Behavior hints (`readOnlyHint`, `destructiveHint`, …). Server-supplied and
   * therefore untrusted: they may SEED a classification, they never grant one.
   */
  annotations?: Record<string, unknown>;
}

/** A `tools/call` result, read leniently — see the module comment. */
export interface McpCallResult {
  content?: Array<{ type: string; text?: string }>;
  structuredContent?: unknown;
  /** The tool itself failed. A tool error is DATA, not a transport failure. */
  isError?: boolean;
}

/** Identity the server reported at handshake, for `/mcp` status. */
export interface McpServerInfo {
  name?: string;
  version?: string;
  protocolVersion: string;
}

interface Pending {
  resolve: (value: unknown) => void;
  reject: (error: Error) => void;
}

/**
 * Deadlines. Injected so a test asserts the no-hang property in milliseconds
 * instead of waiting out a production timeout.
 */
export interface McpTimeouts {
  /** Spawn → `initialize` answered. */
  connectMs?: number;
  /** Ordinary requests (`tools/list`). */
  requestMs?: number;
  /** `tools/call`. Longer on purpose: browser automation and long fetches are legitimate. */
  callMs?: number;
}

const DEFAULT_TIMEOUTS: Required<McpTimeouts> = {
  connectMs: 30_000,
  requestMs: 30_000,
  callMs: 5 * 60_000,
};

/** How long a killed child gets to exit before SIGKILL. */
const KILL_GRACE_MS = 2_000;
const STDERR_TAIL_BYTES = 4_096;
/** Stderr included in an error message — enough to diagnose, not enough to flood a turn. */
const STDERR_NOTE_BYTES = 500;
/**
 * `tools/list` pages followed before giving up. A server that returns a cursor
 * pointing at itself would otherwise loop forever inside a turn.
 */
const MAX_TOOL_PAGES = 50;

export interface McpStdioOptions {
  /** The registry name. Absent = the executable, so errors are never anonymous. */
  name?: string;
  /** argv, composed by the caller (`[command, ...args]`). */
  argv: string[];
  cwd?: string;
  /**
   * The child's ENTIRE environment (`clearEnv: true`). The caller composes it —
   * `config.childEnv()` — so a third-party binary never inherits the user's keys.
   */
  env: Record<string, string>;
  timeouts?: McpTimeouts;
}

// ---------------------------------------------------------------------------
// Live children
// ---------------------------------------------------------------------------

/**
 * Every connected stdio client in this process.
 *
 * MCP servers are children of the server process, and the same trap background
 * shells have applies (plan §6.3, `hostfn/jobs.ts`): a chatty server dies of
 * SIGPIPE when our end of its stdout closes, but a silent one — an idle HTTP
 * bridge, a server between requests — survives, reparented and invisible, with
 * nothing left that knows it exists. So shutdown kills them explicitly.
 */
const liveClients = new Set<McpStdioClient>();

/**
 * SIGTERM every connected MCP server. Synchronous and best-effort, because the
 * caller is a signal handler on its way to `Deno.exit` and has no await to give.
 * Returns how many were signalled.
 */
export function killAllMcpServers(): number {
  let killed = 0;
  for (const client of [...liveClients]) {
    if (client.terminate()) killed++;
  }
  return killed;
}

/** How many stdio servers this process currently holds open. */
export function liveMcpServerCount(): number {
  return liveClients.size;
}

// ---------------------------------------------------------------------------
// The client
// ---------------------------------------------------------------------------

export class McpStdioClient implements McpConnection {
  readonly name: string;
  #child: Deno.ChildProcess;
  #writer: WritableStreamDefaultWriter<Uint8Array>;
  #pending = new Map<number, Pending>();
  #timeouts: Required<McpTimeouts>;
  #seq = 0;
  #stderrTail = "";
  #exited = false;
  #closed = false;
  #info: McpServerInfo | undefined;

  private constructor(child: Deno.ChildProcess, name: string, timeouts: Required<McpTimeouts>) {
    this.name = name;
    this.#child = child;
    this.#timeouts = timeouts;
    this.#writer = child.stdin.getWriter();
    liveClients.add(this);
    void this.#readLoop();
    void this.#drainStderr();
    // The whole point of this handler: when the child dies, everything in flight
    // fails NOW with a message that says it died, instead of sitting until its
    // deadline and reporting a timeout for a process that is already gone.
    child.status.then((status) => {
      this.#exited = true;
      liveClients.delete(this);
      this.#failAll(
        new McpError(
          502,
          `MCP server "${this.name}" exited${
            status.signal ? ` on ${status.signal}` : ` with code ${status.code}`
          }${this.#stderrNote()}. Check the command in the registry ` +
            `(GET /mcp/servers) and run it by hand to see why it stopped.`,
        ),
      );
    });
  }

  /**
   * Spawn the server and run the MCP initialize handshake.
   *
   * Rejects — never hangs, never resolves a half-connected client — on a missing
   * binary, a process that exits during the handshake, a process that never
   * answers, a reply that is not an MCP handshake, or a protocol version bough
   * does not speak.
   */
  static async connect(opts: McpStdioOptions): Promise<McpStdioClient> {
    const name = opts.name ?? opts.argv[0] ?? "(unnamed)";
    const timeouts = { ...DEFAULT_TIMEOUTS, ...opts.timeouts };
    if (opts.argv.length === 0) {
      throw new McpError(400, `MCP server "${name}" has no command to run — set \`command\`.`);
    }

    let child: Deno.ChildProcess;
    try {
      child = new Deno.Command(opts.argv[0], {
        args: opts.argv.slice(1),
        cwd: opts.cwd,
        env: opts.env,
        clearEnv: true,
        stdin: "piped",
        stdout: "piped",
        stderr: "piped",
      }).spawn();
    } catch (error) {
      // NOTE (port): the old client let a spawn failure surface as a raw
      // `Deno.errors.NotFound`, which reads as a missing FILE and says nothing
      // about which server or which command.
      throw new McpError(
        502,
        `MCP server "${name}" failed to start: could not run ${JSON.stringify(opts.argv[0])} ` +
          `(${describe(error)}). Check \`command\` in the registry and that it is on PATH.`,
      );
    }

    const client = new McpStdioClient(child, name, timeouts);
    try {
      const raw = await client.#request(
        "initialize",
        {
          protocolVersion: LATEST_PROTOCOL_VERSION,
          capabilities: {},
          clientInfo: { name: "bough", version: "0" },
        },
        timeouts.connectMs,
      );
      const parsed = InitializeResultSchema.safeParse(raw);
      if (!parsed.success) {
        throw new McpError(
          502,
          `MCP server "${name}" answered initialize with something that is not an MCP ` +
            `handshake${client.#stderrNote()}. It is probably not an MCP server, or it ` +
            `logs to stdout — MCP requires stdout to carry JSON-RPC only.`,
        );
      }
      const version = parsed.data.protocolVersion;
      if (!(SUPPORTED_PROTOCOL_VERSIONS as readonly string[]).includes(version)) {
        throw new McpError(
          502,
          `MCP server "${name}" speaks protocol version ${version}; bough speaks ` +
            `${SUPPORTED_PROTOCOL_VERSIONS.join(", ")}. Upgrade the server, or pin an ` +
            `older release of it.`,
        );
      }
      client.#info = {
        name: parsed.data.serverInfo?.name,
        version: parsed.data.serverInfo?.version,
        protocolVersion: version,
      };
      await client.#notify("notifications/initialized");
      return client;
    } catch (error) {
      await client.close();
      throw error instanceof McpError ? error : new McpError(
        502,
        `MCP server "${name}" failed to start: ${describe(error)}${client.#stderrNote()}`,
      );
    }
  }

  /** Identity from the handshake — `undefined` until `connect` resolves. */
  get serverInfo(): McpServerInfo | undefined {
    return this.#info;
  }

  /**
   * Every tool the server advertises, following pagination cursors.
   *
   * Bounded twice: a cursor that repeats and a page count that runs away are both
   * errors, because either one inside a turn is a hang with extra steps.
   */
  async listTools(): Promise<McpToolInfo[]> {
    const tools: McpToolInfo[] = [];
    const seen = new Set<string>();
    let cursor: string | undefined;
    for (let page = 0; page < MAX_TOOL_PAGES; page++) {
      const result = await this.#request("tools/list", cursor ? { cursor } : {}) as {
        tools?: unknown[];
        nextCursor?: unknown;
      };
      for (const raw of result?.tools ?? []) {
        const tool = toolInfo(raw);
        if (tool) tools.push(tool);
      }
      const next = typeof result?.nextCursor === "string" ? result.nextCursor : undefined;
      if (!next) return tools;
      if (seen.has(next)) {
        throw new McpError(
          502,
          `MCP server "${this.name}" repeated the tools/list cursor ${JSON.stringify(next)}, ` +
            `so its tool list never ends. Reporting ${tools.length} tools and stopping.`,
        );
      }
      seen.add(next);
      cursor = next;
    }
    throw new McpError(
      502,
      `MCP server "${this.name}" paginated tools/list past ${MAX_TOOL_PAGES} pages. ` +
        `Reporting ${tools.length} tools and stopping.`,
    );
  }

  /**
   * Invoke one tool. A tool that FAILS comes back as `{isError: true}` — that is
   * data the program reads, not an exception. Only transport, protocol and
   * deadline failures throw.
   */
  async callTool(name: string, args: unknown): Promise<McpCallResult> {
    const raw = await this.#request(
      "tools/call",
      { name, arguments: args ?? {} },
      this.#timeouts.callMs,
    );
    const parsed = CallResultShape.safeParse(raw);
    // Not a shape we recognize: hand it to the caller as structured content
    // rather than failing a call the server considered successful.
    if (!parsed.success) return { structuredContent: raw };
    return parsed.data;
  }

  /** Last stderr from the child — "why did it die", for `/mcp` status. */
  get stderrTail(): string {
    return this.#stderrTail;
  }

  get alive(): boolean {
    return !this.#exited && !this.#closed;
  }

  /**
   * SIGTERM the child without awaiting it. For process shutdown, which has no
   * await to give. Returns whether a signal was actually sent.
   */
  terminate(): boolean {
    liveClients.delete(this);
    if (this.#exited) return false;
    try {
      this.#child.kill("SIGTERM");
      return true;
    } catch {
      return false; // already gone
    }
  }

  /**
   * Terminate the child and release its pipes. Safe to call twice, safe to call on
   * a client whose child already died, and never throws — teardown that can fail is
   * teardown that leaks a process.
   */
  async close(): Promise<void> {
    if (this.#closed) {
      // Still wait for the child, so a second caller does not race the exit.
      await this.#child.status.catch(() => {});
      return;
    }
    this.#closed = true;
    liveClients.delete(this);
    this.#failAll(
      new McpError(502, `MCP server "${this.name}" was disconnected while the call was in flight.`),
    );
    try {
      await this.#writer.close();
    } catch { /* already closed, or the child is gone */ }
    try {
      this.#child.kill("SIGTERM");
    } catch { /* already exited */ }
    // Grace, then force: a wedged server must not outlive its connection.
    const grace = setTimeout(() => {
      try {
        this.#child.kill("SIGKILL");
      } catch { /* raced its own exit */ }
    }, KILL_GRACE_MS);
    await this.#child.status.catch(() => {});
    clearTimeout(grace);
  }

  // -- JSON-RPC ------------------------------------------------------------

  #request(
    method: string,
    params: unknown,
    timeoutMs = this.#timeouts.requestMs,
  ): Promise<unknown> {
    if (!this.alive) {
      return Promise.reject(
        new McpError(
          502,
          `MCP server "${this.name}" is not running, so ${method} could not be sent` +
            `${this.#stderrNote()}. Reconnect it (POST /mcp/servers/${this.name}/enable) ` +
            `or check the command in the registry.`,
        ),
      );
    }
    const id = ++this.#seq;
    return new Promise<unknown>((resolve, reject) => {
      // NOTE (port): the timer is CLEARED when the request settles. The old client
      // left one armed per request, which kept a timer alive for the full timeout
      // after every successful call — invisible in the server, fatal in a test,
      // where Deno's sanitizer reports the leak as the test's own.
      const timer = setTimeout(() => {
        if (!this.#pending.delete(id)) return;
        reject(
          new McpError(
            504,
            `MCP ${method} on server "${this.name}" timed out after ${timeoutMs}ms` +
              `${this.#stderrNote()}. The server is running but did not answer.`,
          ),
        );
      }, timeoutMs);
      this.#pending.set(id, {
        resolve: (value) => {
          clearTimeout(timer);
          resolve(value);
        },
        reject: (error) => {
          clearTimeout(timer);
          reject(error);
        },
      });
      void this.#send({ jsonrpc: JSONRPC_VERSION, id, method, params });
    });
  }

  #notify(method: string, params?: unknown): Promise<void> {
    return this.#send({
      jsonrpc: JSONRPC_VERSION,
      method,
      ...(params !== undefined ? { params } : {}),
    });
  }

  async #send(message: unknown): Promise<void> {
    try {
      await this.#writer.write(new TextEncoder().encode(JSON.stringify(message) + "\n"));
    } catch {
      // stdin gone = the child is dead; its status handler fails the pending
      // request with a message that says so.
    }
  }

  async #readLoop(): Promise<void> {
    let buffer = "";
    try {
      for await (const chunk of this.#child.stdout.pipeThrough(new TextDecoderStream())) {
        buffer += chunk;
        for (;;) {
          const newline = buffer.indexOf("\n");
          if (newline < 0) break;
          const line = buffer.slice(0, newline).trim();
          buffer = buffer.slice(newline + 1);
          if (line) this.#dispatch(line);
        }
      }
    } catch { /* stream torn down by close or exit */ }
  }

  #dispatch(line: string): void {
    let message: {
      id?: number | string;
      method?: string;
      result?: unknown;
      error?: { code?: number; message?: string };
    };
    try {
      message = JSON.parse(line);
    } catch {
      return; // a server that logs to stdout — skip the noise rather than die on it
    }

    // A reply to one of ours.
    if (message.id !== undefined && message.method === undefined) {
      const pending = this.#pending.get(Number(message.id));
      if (!pending) return; // already timed out, or never ours
      this.#pending.delete(Number(message.id));
      if (message.error) {
        const code = message.error.code !== undefined ? ` (code ${message.error.code})` : "";
        pending.reject(
          new McpError(
            502,
            `MCP server "${this.name}": ${message.error.message ?? "unspecified error"}${code}`,
          ),
        );
      } else {
        pending.resolve(message.result);
      }
      return;
    }

    // A server-initiated REQUEST (sampling, roots, elicitation): refuse it
    // explicitly. Ignoring it leaves the server blocked on a reply forever, which
    // is the same hang from the other side of the pipe. Notifications need none.
    if (message.id !== undefined && message.method !== undefined) {
      void this.#send({
        jsonrpc: JSONRPC_VERSION,
        id: message.id,
        error: { code: -32601, message: `bough does not support ${message.method}` },
      });
    }
  }

  async #drainStderr(): Promise<void> {
    try {
      for await (const chunk of this.#child.stderr.pipeThrough(new TextDecoderStream())) {
        this.#stderrTail = (this.#stderrTail + chunk).slice(-STDERR_TAIL_BYTES);
      }
    } catch { /* stream torn down by close or exit */ }
  }

  #stderrNote(): string {
    const tail = this.#stderrTail.trim();
    return tail ? ` — stderr: ${tail.slice(-STDERR_NOTE_BYTES)}` : "";
  }

  #failAll(error: Error): void {
    for (const [, pending] of this.#pending) pending.reject(error);
    this.#pending.clear();
  }
}

// ---------------------------------------------------------------------------
// Result shapes
// ---------------------------------------------------------------------------

/**
 * `tools/call`, read leniently: the fields the harness uses, anything else passed
 * through. Deliberately not the SDK's `CallToolResultSchema` — that one validates
 * every content block against a closed union, so one unrecognized block type would
 * turn a successful call into a failed one.
 */
const CallResultShape = z.object({
  content: z.array(z.object({ type: z.string() }).passthrough()).optional(),
  structuredContent: z.unknown().optional(),
  isError: z.boolean().optional(),
}).passthrough().transform((r) => ({
  ...(r.content ? { content: r.content as Array<{ type: string; text?: string }> } : {}),
  ...(r.structuredContent !== undefined ? { structuredContent: r.structuredContent } : {}),
  ...(r.isError !== undefined ? { isError: r.isError } : {}),
}));

/** The minimum an entry needs to be callable at all: a name. */
const LooseTool = z.object({
  name: z.string().min(1),
  description: z.string().optional(),
  inputSchema: z.object({
    properties: z.record(z.string(), z.unknown()).optional(),
    required: z.array(z.string()).optional(),
  }).passthrough().optional(),
  annotations: z.record(z.string(), z.unknown()).optional(),
}).passthrough();

/**
 * One advertised tool: the SDK's `ToolSchema` first, a name-only fallback second.
 * `undefined` means the entry could not be called even in principle, so it is
 * dropped from the catalog rather than printed as a tool the model may try.
 */
function toolInfo(raw: unknown): McpToolInfo | undefined {
  const strict = ToolSchema.safeParse(raw);
  if (strict.success) {
    const tool = strict.data;
    return {
      name: tool.name,
      ...(tool.description !== undefined ? { description: tool.description } : {}),
      inputSchema: {
        ...(tool.inputSchema.properties
          ? { properties: tool.inputSchema.properties as Record<string, unknown> }
          : {}),
        ...(tool.inputSchema.required ? { required: tool.inputSchema.required } : {}),
      },
      ...(tool.annotations ? { annotations: tool.annotations as Record<string, unknown> } : {}),
    };
  }
  const loose = LooseTool.safeParse(raw);
  if (!loose.success) return undefined;
  const tool = loose.data;
  return {
    name: tool.name,
    ...(tool.description !== undefined ? { description: tool.description } : {}),
    ...(tool.inputSchema
      ? {
        inputSchema: {
          ...(tool.inputSchema.properties
            ? { properties: tool.inputSchema.properties as Record<string, unknown> }
            : {}),
          ...(tool.inputSchema.required ? { required: tool.inputSchema.required } : {}),
        },
      }
      : {}),
    ...(tool.annotations ? { annotations: tool.annotations } : {}),
  };
}

/** A thrown value as a sentence. */
function describe(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
