#!/usr/bin/env python3
"""A minimal resident MCP server over stdio, for the `mcp-subprocess` tests.

Speaks line-delimited JSON-RPC 2.0: `initialize`, `tools/list`, `tools/call`. It never reaches the
network and never writes outside `MCP_FIXTURE_RECORD`.

Environment knobs, so one fixture covers every case the host must survive:
  MCP_FIXTURE_RECORD   append one line per process start (the spawn counter a test reads)
  MCP_FIXTURE_DIE_MS   exit(1) this many milliseconds after start — the crash-loop fixture
  MCP_FIXTURE_ACTIONS  emit one `bough/actions` notification right after `initialize`
  MCP_FIXTURE_TOOL     the name of the one tool listed (default `echo`)
"""

import json
import os
import sys
import threading
import time


def send(msg):
    sys.stdout.write(json.dumps(msg) + "\n")
    sys.stdout.flush()


def main():
    record = os.environ.get("MCP_FIXTURE_RECORD")
    if record:
        with open(record, "a") as f:
            f.write("start %d\n" % os.getpid())

    die_ms = os.environ.get("MCP_FIXTURE_DIE_MS")
    if die_ms:
        def die():
            time.sleep(int(die_ms) / 1000.0)
            os._exit(1)
        threading.Thread(target=die, daemon=True).start()

    tool = os.environ.get("MCP_FIXTURE_TOOL", "echo")

    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            req = json.loads(line)
        except ValueError:
            continue
        method, rid = req.get("method"), req.get("id")
        if method == "initialize":
            send({"jsonrpc": "2.0", "id": rid, "result": {
                "protocolVersion": "2025-06-18",
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "echo-fixture", "version": "0.1.0"},
            }})
            if os.environ.get("MCP_FIXTURE_ACTIONS"):
                send({"jsonrpc": "2.0", "method": "bough/actions", "params": {"actions": [
                    {"kind": "hint", "agent": "sol", "text": "a resident process said so"}
                ]}})
        elif method == "tools/list":
            send({"jsonrpc": "2.0", "id": rid, "result": {"tools": [{
                "name": tool,
                "description": "echoes its argument back",
                "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}},
            }]}})
        elif method == "tools/call":
            args = (req.get("params") or {}).get("arguments") or {}
            send({"jsonrpc": "2.0", "id": rid, "result": {
                "content": [{"type": "text", "text": args.get("text", "")}],
                "isError": False,
            }})
        elif rid is not None:
            send({"jsonrpc": "2.0", "id": rid,
                  "error": {"code": -32601, "message": "no such method: %s" % method}})


if __name__ == "__main__":
    main()
