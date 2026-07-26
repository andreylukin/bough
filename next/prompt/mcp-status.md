## MCP state (mcpStatus)

await mcpStatus() returns this session's MCP management state — {registry, auth,
active, connections}. MCP servers are managed through bough itself, NOT through
other tools' config files.

Answer every MCP question from a FRESH mcpStatus() call, never from memory of an
earlier turn: registry entries, grants and connections change between turns (UI
toggles, other sessions, expiring auth), and a cached answer is a wrong one.

For changes — registering, enabling, authenticating — tell the human to type /mcp
rather than improvising a config edit.

A server listed as failed or unauthorized in the status is not a tool you can call;
say what the status reports and move on with the rest of the task.
