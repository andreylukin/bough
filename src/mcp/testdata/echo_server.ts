/**
 * Test fixture: a tiny MCP server over stdio (newline-delimited JSON-RPC).
 *
 * Speaks just enough protocol for `client.test.ts` — initialize, tools/list in TWO
 * pages (so the cursor loop is exercised), tools/call — and deliberately misbehaves
 * on demand, because the client's whole contract is what happens when a server
 * does not cooperate.
 *
 * Run with `bun`: it reads stdin and writes stdout/stderr, nothing else.
 *
 * Tools:
 *   echo   {text}  — readOnlyHint; returns the text plus structuredContent
 *   scream {text}  — annotated as a write; uppercases
 *   boom   {}      — returns an isError RESULT (a tool failure is data)
 *   die    {}      — writes to stderr and exits the process MID-CALL, never replying
 *   slow   {}      — never replies at all, and stays alive doing it
 *   loose  {q}     — inputSchema missing `type: "object"`, so only the lenient path keeps it
 *
 * Flags:
 *   --deaf   read stdin forever, answer nothing (a server that starts and hangs)
 *   --noise  print a non-JSON banner to stdout before the handshake
 */

// This fixture imports nothing, and top-level `await` is only legal in a module —
// so say so explicitly. Deno called every file a module; tsc does not.
export {};

const args = process.argv.slice(2);
const DEAF = args.includes("--deaf");
const NOISE = args.includes("--noise");

function reply(message: unknown): void {
  console.log(JSON.stringify(message));
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
  { name: "boom", description: "Always fails.", inputSchema: { type: "object", properties: {} } },
  {
    name: "die",
    description: "Kills the server.",
    inputSchema: { type: "object", properties: {} },
  },
  { name: "slow", description: "Never answers.", inputSchema: { type: "object", properties: {} } },
  // Missing `type: "object"` — the SDK's ToolSchema rejects it; the client's
  // lenient fallback must still list it, because the tool is callable.
  {
    name: "loose",
    description: "Advertised with a sloppy schema.",
    inputSchema: { properties: { q: { type: "string" } } },
  },
  // No name at all: not callable even in principle, so the client drops it.
  { description: "an entry with no name" },
];

function callTool(id: number | undefined, name: string, args: Record<string, unknown>): void {
  if (name === "die") {
    // Mid-call death: no reply, a diagnostic on stderr, gone.
    console.error("echo-fixture: asked to die, taking the server down");
    process.exit(3);
  }
  if (name === "slow") return; // alive, and never answering

  const result = name === "echo"
    ? {
      content: [{ type: "text", text: String(args.text) }],
      structuredContent: { echoed: args.text },
    }
    : name === "scream"
    ? { content: [{ type: "text", text: String(args.text).toUpperCase() }] }
    : name === "boom"
    ? { content: [{ type: "text", text: "kaboom" }], isError: true }
    : name === "loose"
    ? { content: [{ type: "text", text: `q=${String(args.q)}` }] }
    : { content: [{ type: "text", text: `no such tool: ${name}` }], isError: true };
  reply({ jsonrpc: "2.0", id, result });
}

function handle(message: {
  id?: number;
  method?: string;
  params?: { cursor?: string; name?: string; arguments?: Record<string, unknown> };
}): void {
  if (DEAF) return;
  if (message.method === "initialize") {
    reply({
      jsonrpc: "2.0",
      id: message.id,
      result: {
        protocolVersion: "2025-06-18",
        capabilities: { tools: {} },
        serverInfo: { name: "echo-fixture", version: "0" },
      },
    });
  } else if (message.method === "tools/list") {
    const second = message.params?.cursor === "page2";
    reply({
      jsonrpc: "2.0",
      id: message.id,
      result: second ? { tools: PAGE2 } : { tools: PAGE1, nextCursor: "page2" },
    });
  } else if (message.method === "tools/call") {
    callTool(message.id, message.params?.name ?? "", message.params?.arguments ?? {});
  } else if (message.id !== undefined) {
    reply({ jsonrpc: "2.0", id: message.id, error: { code: -32601, message: "method not found" } });
  }
  // notifications (initialized) need no reply
}

if (NOISE) console.log("echo-fixture starting up (this line is not JSON)");

let buffer = "";
for await (const chunk of Bun.stdin.stream().pipeThrough(new TextDecoderStream())) {
  buffer += chunk;
  for (;;) {
    const newline = buffer.indexOf("\n");
    if (newline < 0) break;
    const line = buffer.slice(0, newline).trim();
    buffer = buffer.slice(newline + 1);
    if (line) handle(JSON.parse(line));
  }
}
