/**
 * Remote MCP servers — registry entries with a `url` — over the official SDK's
 * Streamable HTTP transport, authenticated by the OAuth provider in `oauth.ts`.
 *
 * THE INVARIANT THIS HOLDS is the same one the stdio client holds, with one
 * addition that is the whole point of this file: **a server that does not work
 * fails, by name, in bounded time — and a server that is merely UNAUTHORIZED fails
 * as a question, not as a fault.** A 401 becomes "not authorized — /mcp auth
 * <name>" in the turn's catalog. Never a hang, never a stack trace, never an entry
 * that reads as "this server is broken" when the truth is "nobody has approved it
 * yet" (spec §10, plan T7.2).
 *
 * Three properties make that true, and each one is load-bearing:
 *
 * **Every HTTP request is bounded.** The SDK's transport calls `fetch` with no
 * deadline of its own, and the auth flow adds three more round trips (discovery,
 * registration, token) that the transport's request timeout does not cover. So
 * every request goes through {@link boundedFetch}: a per-request timeout, plus a
 * connection-wide abort that `close()` fires. The one exception is the server→client
 * SSE stream, which is long-lived BY DESIGN — timing it out would be a reconnect
 * loop, so it carries only the connection abort.
 *
 * **A 401 is remembered even when the auth flow fails afterwards.** The transport
 * answers a 401 by running `auth()`, and if THAT fails — no OAuth metadata, no
 * registration endpoint, a rejected refresh token — the error that escapes is about
 * discovery or registration and no longer mentions 401 at all. A server whose only
 * problem is that nobody has authorized it would then surface as a broken server.
 * The fetch wrapper records a 401 from the MCP endpoint, and the error mapper trusts
 * that over the shape of whatever escaped.
 *
 * **Refresh is the transport's job, not ours.** An expired access token is not an
 * error path: the transport gets a 401, `auth()` exchanges the refresh token, the
 * provider persists the new pair, and the request is retried — all inside one
 * `callTool`. An expired REFRESH token degrades to the same authorization prompt as
 * a server that was never authorized (`oauth.ts`'s `invalidateCredentials`), which
 * is exactly right: in both cases the human must approve access again.
 *
 * **The JSON-RPC channel goes DIRECT.** There is no egress proxy in this design and
 * no call-layer gate (spec §17: no sandbox, no egress proxy, no credential gating).
 * The transport talks to the remote server, with no interception anywhere. Stated
 * because the previous tree argued the point at length; here it is simply how it works.
 *
 * Only the TRANSPORT comes from the SDK. Spawning stays hand-rolled in `client.ts`
 * for reasons that do not apply to HTTP (composing a child's whole environment,
 * tracking it for shutdown), and both present the same `McpConnection` to the layer
 * above, so nothing upstream branches on which one a registry entry produced.
 *
 * Ported from `src/mcp/remote.ts`. Deltas from that port are marked `NOTE:`.
 */
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StreamableHTTPClientTransport } from "@modelcontextprotocol/sdk/client/streamableHttp.js";
import {
  type OAuthClientProvider,
  UnauthorizedError,
} from "@modelcontextprotocol/sdk/client/auth.js";
import type { FetchLike } from "@modelcontextprotocol/sdk/shared/transport.js";
import { ErrorCode, McpError as JsonRpcError } from "@modelcontextprotocol/sdk/types.js";
import { z } from "zod";
import { McpError } from "../errors.ts";
import type {
  McpCallResult,
  McpConnection,
  McpServerInfo,
  McpTimeouts,
  McpToolInfo,
} from "./client.ts";
import { BoughOAuthProvider, type TokenStoreOptions } from "./oauth.ts";

const DEFAULT_TIMEOUTS: Required<McpTimeouts> = {
  connectMs: 30_000,
  requestMs: 30_000,
  /** Longer on purpose: browser automation and long fetches are legitimate. */
  callMs: 5 * 60_000,
};

/** `tools/list` pages followed before giving up — a self-referential cursor is a hang. */
const MAX_TOOL_PAGES = 50;

/** Transport diagnostics kept for `/mcp` status ("why is it failing"). */
const ERROR_TAIL_BYTES = 4_096;

// ---------------------------------------------------------------------------
// The authorization prompt
// ---------------------------------------------------------------------------

/**
 * The sentence a 401 turns into. Exported so the catalog, the `/mcp` panel and the
 * error all say the same words — a prompt the user recognizes in one place and not
 * in another is a prompt they do not act on.
 */
export function authPrompt(server: string): string {
  return `not authorized — /mcp auth ${server}`;
}

/**
 * The server answered 401 and bough has no usable credentials for it.
 *
 * A distinct class, and not just a message, because the catalog renders it
 * differently: this is a PROMPT (one command fixes it), while every other `McpError`
 * from this module is a fault. `status` is 401 so the same distinction survives the
 * HTTP boundary.
 */
export class McpAuthRequiredError extends McpError {
  /** Discriminator for a catalog that has only the error, not its class. */
  readonly authRequired = true;

  constructor(readonly server: string, detail?: string) {
    super(
      401,
      `MCP server "${server}": ${authPrompt(server)}. The server answered 401 and bough ` +
        `has no token it can use or refresh for it; that command returns the URL to ` +
        `approve access in your browser, and the tools appear on the next turn. ` +
        `The rest of the turn is unaffected.` + (detail ? ` (${detail})` : ""),
    );
  }
}

/** True when the failure is "a human must approve access", not "it is broken". */
export function isAuthRequired(error: unknown): error is McpAuthRequiredError {
  return error instanceof McpAuthRequiredError ||
    (error instanceof McpError && (error as { authRequired?: boolean }).authRequired === true);
}

// ---------------------------------------------------------------------------
// Bounded HTTP
// ---------------------------------------------------------------------------

/**
 * `fetch` with a deadline and a kill switch.
 *
 * `onStatus` is how a 401 survives an auth flow that fails for some later reason —
 * see the module comment. The SSE carve-out is by method + Accept, because that GET
 * is a subscription: a request timeout on it would tear the stream down on a
 * schedule and reconnect forever.
 */
function boundedFetch(opts: {
  base: FetchLike;
  timeoutMs: number;
  abort: AbortSignal;
  onStatus?: (url: string, status: number) => void;
}): FetchLike {
  return async (url: string | URL, init?: RequestInit) => {
    const isSseStream = (init?.method ?? "GET").toUpperCase() === "GET" &&
      String(new Headers(init?.headers).get("accept") ?? "").includes("text/event-stream");
    const signals: AbortSignal[] = [opts.abort];
    if (init?.signal) signals.push(init.signal);
    if (!isSseStream) signals.push(AbortSignal.timeout(opts.timeoutMs));
    const response = await opts.base(url, { ...init, signal: AbortSignal.any(signals) });
    opts.onStatus?.(String(url), response.status);
    return response;
  };
}

// ---------------------------------------------------------------------------
// Connecting
// ---------------------------------------------------------------------------

export interface RemoteConnectOptions extends TokenStoreOptions {
  /** The registry name. Every error message carries it, and it keys the tokens. */
  name: string;
  /** The Streamable HTTP endpoint (`ServerConfig.url`). */
  url: string;
  /** Static headers from the registry. OAuth is NOT one of these. */
  headers?: Record<string, string>;
  timeouts?: McpTimeouts;
  /**
   * The OAuth provider. Absent = a `BoughOAuthProvider` over the token store, which
   * is what production wants; a caller passes its own to read back the captured
   * authorization URL, or `null` to talk to a server that needs no auth at all.
   */
  authProvider?: OAuthClientProvider | null;
  /** HTTP, injected in tests. Absent = the global `fetch`. */
  fetchFn?: FetchLike;
}

/**
 * One connected remote server.
 *
 * Reads as the stdio client's twin on purpose: same `McpConnection` surface, same
 * bounded pagination, same "a tool error is DATA, not an exception" rule, same
 * lenient result reading. The differences are all in how it fails, which is the
 * half this module exists for.
 */
export class McpRemoteClient implements McpConnection {
  readonly name: string;
  readonly url: string;

  #client: Client;
  #transport: StreamableHTTPClientTransport;
  #abort: AbortController;
  #timeouts: Required<McpTimeouts>;
  #alive = true;
  #closed = false;
  #errorTail = "";
  /** Live view of the fetch wrapper's 401 flag — see the module comment. */
  #sawUnauthorized: () => boolean;
  #info: McpServerInfo | undefined;

  private constructor(init: {
    name: string;
    url: string;
    client: Client;
    transport: StreamableHTTPClientTransport;
    abort: AbortController;
    timeouts: Required<McpTimeouts>;
    sawUnauthorized: () => boolean;
  }) {
    this.name = init.name;
    this.url = init.url;
    this.#client = init.client;
    this.#transport = init.transport;
    this.#abort = init.abort;
    this.#timeouts = init.timeouts;
    this.#sawUnauthorized = init.sawUnauthorized;
    init.client.onclose = () => {
      this.#alive = false;
    };
    init.client.onerror = (error: Error) => {
      this.#errorTail = (this.#errorTail + "\n" + describe(error)).slice(-ERROR_TAIL_BYTES);
    };
  }

  /**
   * Connect and run the MCP handshake.
   *
   * Rejects — never hangs, never resolves half-connected — on an unreachable host,
   * a server that accepts the connection and never answers, an HTTP error, or an
   * authorization the human has not granted. The last one rejects with
   * {@link McpAuthRequiredError} so the caller can render a prompt instead of a
   * fault.
   */
  static async connect(opts: RemoteConnectOptions): Promise<McpRemoteClient> {
    const name = opts.name;
    const timeouts = { ...DEFAULT_TIMEOUTS, ...opts.timeouts };
    let endpoint: URL;
    try {
      endpoint = new URL(opts.url);
    } catch {
      throw new McpError(
        400,
        `MCP server "${name}" has an unusable \`url\` (${JSON.stringify(opts.url)}). ` +
          `A remote server needs an absolute http(s) URL pointing at its MCP endpoint.`,
      );
    }

    const abort = new AbortController();
    let sawUnauthorized = false;
    const fetchFn = boundedFetch({
      base: opts.fetchFn ?? ((url: string | URL, init?: RequestInit) => fetch(url, init)),
      timeoutMs: timeouts.requestMs,
      abort: abort.signal,
      onStatus: (url, status) => {
        // Only the MCP endpoint itself: a 401 from the token endpoint is the auth
        // flow's own business and the SDK maps it to a typed OAuth error.
        if (status === 401 && url.startsWith(endpoint.origin) && !url.includes("/.well-known/")) {
          sawUnauthorized = true;
        }
      },
    });

    const authProvider = opts.authProvider === null
      ? undefined
      : opts.authProvider ?? new BoughOAuthProvider(name, { dir: opts.dir });

    const transport = new StreamableHTTPClientTransport(endpoint, {
      ...(authProvider ? { authProvider } : {}),
      ...(opts.headers && Object.keys(opts.headers).length > 0
        ? { requestInit: { headers: { ...opts.headers } } }
        : {}),
      fetch: fetchFn,
    });
    const client = new Client({ name: "bough", version: "0" });

    try {
      // The timeout covers the whole handshake including any auth round trips it
      // triggers; `boundedFetch` bounds each individual request under it. Both, so
      // neither a slow server nor a slow authorization server can park a turn.
      await client.connect(transport, { timeout: timeouts.connectMs });
    } catch (error) {
      abort.abort();
      await transport.close().catch(() => {});
      await client.close().catch(() => {});
      throw mapError(error, { name, url: opts.url, sawUnauthorized, what: "connect" });
    }

    const version = client.getServerVersion();
    const remote = new McpRemoteClient({
      name,
      url: opts.url,
      client,
      transport,
      abort,
      timeouts,
      sawUnauthorized: () => sawUnauthorized,
    });
    remote.#info = {
      ...(version?.name !== undefined ? { name: version.name } : {}),
      ...(version?.version !== undefined ? { version: version.version } : {}),
      protocolVersion: transport.protocolVersion ?? "",
    };
    return remote;
  }

  /** Identity from the handshake, for `/mcp` status. */
  get serverInfo(): McpServerInfo | undefined {
    return this.#info;
  }

  /**
   * Every tool the server advertises, following pagination cursors.
   *
   * Bounded twice, exactly like the stdio client: a repeated cursor and a runaway
   * page count are both errors, because either one inside a turn is a hang with
   * extra steps.
   */
  async listTools(): Promise<McpToolInfo[]> {
    const tools: McpToolInfo[] = [];
    const seen = new Set<string>();
    let cursor: string | undefined;
    for (let page = 0; page < MAX_TOOL_PAGES; page++) {
      const result = await this.#request(
        { method: "tools/list", params: cursor ? { cursor } : {} },
        LooseListResult,
        this.#timeouts.requestMs,
        "tools/list",
      );
      for (const raw of result.tools) {
        const tool = toolInfo(raw);
        if (tool) tools.push(tool);
      }
      const next = result.nextCursor;
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
   * data the program reads, not an exception. Only transport, protocol, deadline
   * and authorization failures throw.
   *
   * An access token that expired since `connect` is invisible here: the transport
   * refreshes it and retries inside this call.
   */
  async callTool(name: string, args: unknown): Promise<McpCallResult> {
    return await this.#request(
      {
        method: "tools/call",
        params: { name, arguments: (args ?? {}) as Record<string, unknown> },
      },
      LooseCallResult,
      this.#timeouts.callMs,
      `tools/call ${name}`,
    );
  }

  get alive(): boolean {
    return this.#alive && !this.#closed;
  }

  /** Recent transport errors — the remote analogue of stdio's stderr tail. */
  get stderrTail(): string {
    return this.#errorTail.trim();
  }

  /**
   * Close the session and cancel anything in flight. Safe to call twice, never
   * throws — teardown that can fail is teardown that leaks a connection.
   */
  async close(): Promise<void> {
    if (this.#closed) return;
    this.#closed = true;
    this.#alive = false;
    await this.#client.close().catch(() => {});
    await this.#transport.close().catch(() => {});
    // After the graceful close, so an in-flight SSE stream is torn down rather
    // than left to reconnect.
    this.#abort.abort();
  }

  // -- JSON-RPC ------------------------------------------------------------

  /**
   * NOTE (port): requests go through `client.request` with LENIENT schemas rather
   * than through `client.listTools`/`client.callTool`, which validate against the
   * SDK's closed unions. Dropping a callable tool from the catalog over a schema nit
   * — an `inputSchema` missing `type: "object"` is the common one — or turning a
   * successful call into a failure over one unrecognized content block is a worse
   * outcome than a thin signature. Same rule as the stdio client.
   */
  async #request<S extends z.ZodTypeAny>(
    request: { method: string; params?: Record<string, unknown> },
    schema: S,
    timeoutMs: number,
    what: string,
  ): Promise<z.infer<S>> {
    if (!this.alive) {
      throw new McpError(
        502,
        `MCP server "${this.name}" is disconnected, so ${what} could not be sent` +
          `${this.#note()}. Reconnect it (POST /mcp/servers/${this.name}/enable).`,
      );
    }
    try {
      return await this.#client.request(
        request as Parameters<Client["request"]>[0],
        schema,
        { timeout: timeoutMs },
      ) as z.infer<S>;
    } catch (error) {
      throw mapError(error, {
        name: this.name,
        url: this.url,
        sawUnauthorized: this.#sawUnauthorized() || isUnauthorized(error),
        what,
        note: this.#note(),
      });
    }
  }

  #note(): string {
    const tail = this.stderrTail;
    return tail ? ` — last transport error: ${tail.slice(-500)}` : "";
  }
}

// ---------------------------------------------------------------------------
// Failure mapping
// ---------------------------------------------------------------------------

function isUnauthorized(error: unknown): boolean {
  return error instanceof UnauthorizedError;
}

/**
 * The JSON-RPC error code carried by an SDK protocol error, if it is one.
 *
 * Read structurally rather than through `instanceof JsonRpcError`: Deno resolves
 * this SDK's declarations to `any`, so the class narrows nothing and a property
 * access on the caught `unknown` would not compile. The `instanceof` is kept as a
 * guard on the shape, not as a type narrowing.
 */
function jsonRpcCode(error: unknown): number | undefined {
  if (!(error instanceof Error) || !(error instanceof JsonRpcError)) return undefined;
  const code = (error as unknown as { code?: unknown }).code;
  return typeof code === "number" ? code : undefined;
}

/**
 * Turn whatever escaped into the one sentence the model or the user reads.
 *
 * The authorization case is checked FIRST and from the recorded 401 rather than
 * from the error's class, because the error that escapes an auth flow is usually
 * about the step that failed after the 401 — see the module comment.
 */
function mapError(
  error: unknown,
  ctx: { name: string; url: string; sawUnauthorized: boolean; what: string; note?: string },
): McpError {
  if (error instanceof McpError) return error;
  if (ctx.sawUnauthorized || isUnauthorized(error)) {
    // The underlying reason is kept as a parenthetical: "not authorized" is the
    // move, but "registration_endpoint missing" is what a maintainer needs.
    const detail = isUnauthorized(error) ? undefined : describe(error);
    return new McpAuthRequiredError(ctx.name, detail);
  }
  if (jsonRpcCode(error) === ErrorCode.RequestTimeout) {
    return new McpError(
      504,
      `MCP ${ctx.what} on server "${ctx.name}" timed out — ${ctx.url} accepted the ` +
        `connection but did not answer${ctx.note ?? ""}. The server is up and stuck, or ` +
        `the URL is not an MCP endpoint.`,
    );
  }
  if (isAbort(error)) {
    return new McpError(
      504,
      `MCP ${ctx.what} on server "${ctx.name}" was cut off before ${ctx.url} answered` +
        `${ctx.note ?? ""}. The request deadline passed, or the connection was closed.`,
    );
  }
  return new McpError(
    502,
    `MCP server "${ctx.name}" failed ${ctx.what}: ${describe(error)}${ctx.note ?? ""}. ` +
      `Check \`url\` in the registry (GET /mcp/servers) and that ${ctx.url} is reachable.`,
  );
}

function isAbort(error: unknown): boolean {
  return error instanceof DOMException &&
    (error.name === "TimeoutError" || error.name === "AbortError");
}

function describe(error: unknown): string {
  if (error instanceof Error) {
    const cause = error.cause instanceof Error ? `: ${error.cause.message}` : "";
    return `${error.message}${cause}`;
  }
  return String(error);
}

// ---------------------------------------------------------------------------
// Result shapes — lenient by design, see `#request`
// ---------------------------------------------------------------------------

const LooseListResult = z.object({
  tools: z.array(z.unknown()).default([]),
  nextCursor: z.string().optional(),
}).passthrough();

const LooseCallResult = z.object({
  content: z.array(z.object({ type: z.string() }).passthrough()).optional(),
  structuredContent: z.unknown().optional(),
  isError: z.boolean().optional(),
}).passthrough().transform((r): McpCallResult => ({
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
 * One advertised tool. `undefined` means the entry could not be called even in
 * principle, so it is dropped rather than printed as a tool the model may try.
 */
function toolInfo(raw: unknown): McpToolInfo | undefined {
  const parsed = LooseTool.safeParse(raw);
  if (!parsed.success) return undefined;
  const tool = parsed.data;
  return {
    name: tool.name,
    ...(tool.description !== undefined ? { description: tool.description } : {}),
    ...(tool.inputSchema
      ? {
        inputSchema: {
          ...(tool.inputSchema.properties ? { properties: tool.inputSchema.properties } : {}),
          ...(tool.inputSchema.required ? { required: tool.inputSchema.required } : {}),
        },
      }
      : {}),
    ...(tool.annotations !== undefined ? { annotations: tool.annotations } : {}),
  };
}
