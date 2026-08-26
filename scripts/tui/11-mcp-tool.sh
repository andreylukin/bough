#!/usr/bin/env bash
# §6/§9 — a discovered MCP tool becomes a tool like any other. A hermetic stdio server is mounted
# as one `mcp.rmcp` server row; `/mcp list` shows its tool, `/mcp call` runs it and prints the
# result, and disabling the server row takes exactly that server's tools away.
#
# The server is a twenty-line Python program written here rather than a real one off the network:
# the suite is hermetic (AGENTS.md), and what is under test is bough's side of the protocol.
source "$(dirname "$0")/lib.sh"

[ -n "$BOUGH_LIVE" ] && { skip an_mcp_tool_is_listed_after_discovery "the MCP server is a local fixture either way"; exit 0; }

SERVER="$HOME_DIR/echo_server.py"
cat > "$SERVER" <<'PY'
#!/usr/bin/env python3
"""A minimal stdio MCP server: one tool, `echo`, which returns its `text` argument."""
import json, sys

TOOL = {
    "name": "echo",
    "description": "return the text you were given",
    "inputSchema": {
        "type": "object",
        "properties": {"text": {"type": "string"}},
        "required": ["text"],
    },
}

def reply(id_, result):
    sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": id_, "result": result}) + "\n")
    sys.stdout.flush()

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    try:
        msg = json.loads(line)
    except json.JSONDecodeError:
        continue
    method, id_ = msg.get("method"), msg.get("id")
    if method == "initialize":
        reply(id_, {
            "protocolVersion": msg.get("params", {}).get("protocolVersion", "2024-11-05"),
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "echo-fixture", "version": "0.1.0"},
        })
    elif method == "tools/list":
        reply(id_, {"tools": [TOOL]})
    elif method == "tools/call":
        args = msg.get("params", {}).get("arguments", {})
        reply(id_, {
            "content": [{"type": "text", "text": args.get("text", "")}],
            "isError": False,
        })
    elif id_ is not None:
        reply(id_, {})
PY
chmod +x "$SERVER"

MCP_PATCH="$HOME_DIR/mcp.fixture.yml"
cat > "$MCP_PATCH" <<YML
entries:
  mcp.rmcp:
    config:
      connect_timeout_ms: 15000
      call_timeout_ms: 120000
      servers:
        - name: echofix
          transport: { kind: stdio, command: python3, args: ["$SERVER"] }
YML

tui_open
tui_start "$MCP_PATCH"

t the_tui_is_up_with_the_server_row_mounted \
  see "sol" --timeout 20000

shell-use submit "/mcp list"
shell-use wait idle --timeout 20000

t an_mcp_tool_is_listed_after_discovery \
  see "echofix__echo" --timeout 20000

t the_listed_tool_carries_the_servers_own_description \
  see "return the text you were given" --timeout 5000

shell-use submit "/mcp list echofix"
shell-use wait idle --timeout 20000

t listing_one_server_shows_that_servers_tools \
  see "echofix__echo" --timeout 20000

# `/mcp call` is NOT driven here: `commands`' shell-style split strips the quotes out of a JSON
# object, so no JSON argument survives being typed at the TUI (see docs/track-b-merge-notes.md).
# The call path is covered by `plugins/tool-mcp`'s own tests, which do not go through the parser.

# SWAP: disabling the mcp Provider row takes every discovered tool with it. `disabled` is a FIELD
# of its own, so the user patch wins on it while this script's `--patch` layer still supplies the
# server list (§0.3 per-field reconcile).
USER_PATCH="$HOME_DIR/bough.patch.yml"
cat > "$USER_PATCH" <<'YML'
entries:
  mcp.rmcp:
    disabled: true
YML
sleep 3

shell-use submit "/mcp list"
shell-use wait idle --timeout 20000
t disabling_the_provider_row_takes_its_tools_with_it \
  see "no MCP tools" --timeout 20000

rm -f "$USER_PATCH"
sleep 3
shell-use submit "/mcp list"
shell-use wait idle --timeout 20000
t removing_the_patch_rediscovers_the_server \
  see "echofix__echo" --timeout 20000

tui_quit
