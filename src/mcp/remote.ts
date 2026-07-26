/**
 * Remote MCP servers (registry entries with `url`): the official SDK's Client +
 * StreamableHTTPClientTransport, authenticated by the OAuth provider in oauth.ts.
 * Only the transport comes from the SDK — spawning (stdio) stays hand-rolled in
 * client.ts, and both present the same McpConnection surface to the manager.
 *
 * Auth posture: a server that answers 401 surfaces as "not authorized — /mcp auth
 * <name>" in the turn's catalog (the SDK's UnauthorizedError), never as a hang.
 * Token refresh happens inside the transport via the provider; an expired refresh
 * token degrades the same way.
 *
 * The JSON-RPC channel goes DIRECT. It once had to be argued for — there was an
 * egress proxy and a call-layer gate (mcp/gate.ts) that would have double-gated
 * every tools/call POST — but both are gone, so this is simply how it works now:
 * the transport talks to the remote server, with no interception anywhere.
 */
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StreamableHTTPClientTransport } from "@modelcontextprotocol/sdk/client/streamableHttp.js";
import { UnauthorizedError } from "@modelcontextprotocol/sdk/client/auth.js";
import { BoughOAuthProvider } from "./oauth.ts";
import type { McpCallResult, McpConnection, McpToolInfo } from "./client.ts";

export class McpRemoteClient implements McpConnection {
  #client: Client;
  #alive = true;
  #lastError = "";

  private constructor(client: Client) {
    this.#client = client;
    client.onclose = () => {
      this.#alive = false;
    };
    client.onerror = (e: Error) => {
      this.#lastError = String(e?.message ?? e);
    };
  }

  /** Connect + initialize. Throws a readable "not authorized" error on 401. */
  static async connect(opts: { server: string; url: string }): Promise<McpRemoteClient> {
    const transport = new StreamableHTTPClientTransport(new URL(opts.url), {
      authProvider: new BoughOAuthProvider(opts.server),
    });
    const client = new Client({ name: "bough", version: "0" });
    try {
      await client.connect(transport);
    } catch (e) {
      await transport.close().catch(() => {});
      if (e instanceof UnauthorizedError) {
        throw new Error(
          `not authorized — run \`/mcp auth ${opts.server}\` and approve access in the browser`,
        );
      }
      throw e;
    }
    return new McpRemoteClient(client);
  }

  async listTools(): Promise<McpToolInfo[]> {
    const tools: McpToolInfo[] = [];
    let cursor: string | undefined;
    do {
      const page = await this.#client.listTools(cursor ? { cursor } : {});
      tools.push(...(page.tools as McpToolInfo[]));
      cursor = page.nextCursor;
    } while (cursor);
    return tools;
  }

  async callTool(name: string, args: unknown): Promise<McpCallResult> {
    return await this.#client.callTool({
      name,
      arguments: (args ?? {}) as Record<string, unknown>,
    }) as McpCallResult;
  }

  get alive(): boolean {
    return this.#alive;
  }

  get stderrTail(): string {
    return this.#lastError;
  }

  async close(): Promise<void> {
    this.#alive = false;
    await this.#client.close().catch(() => {});
  }
}
