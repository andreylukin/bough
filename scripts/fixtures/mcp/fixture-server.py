#!/usr/bin/env python3
"""A minimal stdio MCP server, for the WP-5 tests.

Invariant: it exposes EXACTLY two tools — `echo`, which succeeds, and `boom`, which returns an
MCP result with `isError: true`. The second one is what gives `McpCallResult::is_error` a path
through a real transport rather than a stub.

Newline-delimited JSON-RPC on stdin/stdout, which is what the MCP stdio transport speaks.
Nothing here reaches the network or the filesystem.
"""

import json
import sys

PROTOCOL_VERSION = "2025-06-18"

TOOLS = [
    {
        "name": "echo",
        "description": "Echo the text back.",
        "inputSchema": {
            "type": "object",
            "properties": {"text": {"type": "string"}},
            "required": ["text"],
        },
    },
    {
        "name": "boom",
        "description": "Always fails, so is_error has a path.",
        "inputSchema": {"type": "object", "properties": {}},
    },
]


def call(name, args):
    if name == "echo":
        return {"content": [{"type": "text", "text": "echo: " + str(args.get("text", ""))}]}
    if name == "boom":
        return {"content": [{"type": "text", "text": "boom"}], "isError": True}
    return {"content": [{"type": "text", "text": "no such tool: " + name}], "isError": True}


def handle(req):
    method = req.get("method")
    if method == "initialize":
        return {
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "fixture", "version": "0.1.0"},
        }
    if method == "tools/list":
        return {"tools": TOOLS}
    if method == "tools/call":
        p = req.get("params") or {}
        return call(p.get("name"), p.get("arguments") or {})
    if method == "ping":
        return {}
    return None


def main():
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            req = json.loads(line)
        except ValueError:
            continue
        if "id" not in req:  # a notification: acknowledged by silence
            continue
        result = handle(req)
        if result is None:
            out = {
                "jsonrpc": "2.0",
                "id": req["id"],
                "error": {"code": -32601, "message": "method not found: %s" % req.get("method")},
            }
        else:
            out = {"jsonrpc": "2.0", "id": req["id"], "result": result}
        sys.stdout.write(json.dumps(out) + "\n")
        sys.stdout.flush()


if __name__ == "__main__":
    main()
