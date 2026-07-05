/**
 * The system-prompt section for a turn's connected MCP servers: how to call the
 * host function, then a compact per-server tool catalog — name, parameter names
 * (required first, optional marked `?`), first line of the description. Compact by
 * design: no JSON Schema dumps, and a chatty server's catalog is capped so it can't
 * crowd out the task. Failed servers are named with their error so the model
 * doesn't hallucinate tools that never connected.
 */
import type { McpToolInfo } from "./client.ts";
import type { ServerCatalog } from "./manager.ts";

/** Per-server budget for the rendered tool list. */
const SERVER_CHARS = 4_000;

function params(tool: McpToolInfo): string {
  const props = Object.keys(tool.inputSchema?.properties ?? {});
  const required = new Set(tool.inputSchema?.required ?? []);
  const ordered = [
    ...props.filter((p) => required.has(p)),
    ...props.filter((p) => !required.has(p)),
  ];
  if (ordered.length === 0) return "()";
  return `({${ordered.map((p) => (required.has(p) ? p : `${p}?`)).join(", ")}})`;
}

function toolLine(tool: McpToolInfo): string {
  const desc = (tool.description ?? "").split("\n")[0].trim();
  return `- ${tool.name}${params(tool)}${desc ? ` — ${desc}` : ""}`;
}

function serverBlock(server: ServerCatalog): string {
  if (server.error) return `server "${server.name}": UNAVAILABLE — ${server.error}`;
  const lines: string[] = [`server "${server.name}" (${server.tools.length} tools):`];
  let used = 0;
  let shown = 0;
  for (const tool of server.tools) {
    const line = toolLine(tool);
    if (used + line.length > SERVER_CHARS) break;
    lines.push(line);
    used += line.length;
    shown++;
  }
  const omitted = server.tools.length - shown;
  if (omitted > 0) lines.push(`…(${omitted} more tools omitted)`);
  return lines.join("\n");
}

/** The "# MCP tools" section, or "" when nothing is connected or configured. */
export function mcpSection(catalog: ServerCatalog[]): string {
  if (catalog.length === 0) return "";
  return "\n\n# MCP tools\n" +
    "This turn has MCP servers connected. Inside your program, call\n" +
    "`await mcp(server, tool, args)` — `args` is a plain object matching the tool's\n" +
    "parameters; the call returns the tool's result (an object, or its text output)\n" +
    "and throws on failure or when the egress policy denies it (a held call blocks\n" +
    "until the human decides — that is normal, not an error). Only the servers and\n" +
    "tools listed here exist.\n\n" +
    catalog.map(serverBlock).join("\n");
}
