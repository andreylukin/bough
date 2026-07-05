/**
 * Test fixture: a tiny MCP server over stdio (newline-delimited JSON-RPC). Speaks
 * just enough protocol for the client/manager tests — initialize, tools/list (two
 * pages, exercising the cursor loop), tools/call. Run with `deno run --quiet` and
 * NO permissions: it only reads stdin and writes stdout.
 *
 * Tools:
 *   echo   {text}  — readOnlyHint, returns the text + structuredContent
 *   scream {text}  — annotated non-read (write), uppercases
 *   boom   {}      — returns an isError result
 */

function reply(msg: unknown): void {
  console.log(JSON.stringify(msg));
}

const PAGE1 = [
  {
    name: "echo",
    description: "Echo the text back.\nSecond line that the prompt section must drop.",
    inputSchema: {
      type: "object",
      properties: { text: { type: "string" } },
      required: ["text"],
    },
    annotations: { readOnlyHint: true },
  },
];
const PAGE2 = [
  {
    name: "scream",
    description: "Echo the text back, LOUDLY.",
    inputSchema: { type: "object", properties: { text: { type: "string" } }, required: ["text"] },
    annotations: { readOnlyHint: false, destructiveHint: true },
  },
  {
    name: "boom",
    description: "Always fails.",
    inputSchema: { type: "object", properties: {} },
  },
];

function callTool(name: string, args: Record<string, unknown>): unknown {
  if (name === "echo") {
    return {
      content: [{ type: "text", text: String(args.text) }],
      structuredContent: { echoed: args.text },
    };
  }
  if (name === "scream") {
    return { content: [{ type: "text", text: String(args.text).toUpperCase() }] };
  }
  if (name === "boom") {
    return { content: [{ type: "text", text: "kaboom" }], isError: true };
  }
  return { content: [{ type: "text", text: `no such tool: ${name}` }], isError: true };
}

function handle(msg: {
  id?: number;
  method?: string;
  params?: { cursor?: string; name?: string; arguments?: Record<string, unknown> };
}): void {
  if (msg.method === "initialize") {
    reply({
      jsonrpc: "2.0",
      id: msg.id,
      result: {
        protocolVersion: "2025-06-18",
        capabilities: { tools: {} },
        serverInfo: { name: "echo-fixture", version: "0" },
      },
    });
  } else if (msg.method === "tools/list") {
    const page2 = msg.params?.cursor === "page2";
    reply({
      jsonrpc: "2.0",
      id: msg.id,
      result: page2 ? { tools: PAGE2 } : { tools: PAGE1, nextCursor: "page2" },
    });
  } else if (msg.method === "tools/call") {
    reply({
      jsonrpc: "2.0",
      id: msg.id,
      result: callTool(msg.params?.name ?? "", msg.params?.arguments ?? {}),
    });
  } else if (msg.id !== undefined) {
    reply({ jsonrpc: "2.0", id: msg.id, error: { code: -32601, message: "method not found" } });
  }
  // notifications (initialized) need no reply
}

let buf = "";
for await (const chunk of Deno.stdin.readable.pipeThrough(new TextDecoderStream())) {
  buf += chunk;
  for (;;) {
    const nl = buf.indexOf("\n");
    if (nl < 0) break;
    const line = buf.slice(0, nl).trim();
    buf = buf.slice(nl + 1);
    if (line) handle(JSON.parse(line));
  }
}
