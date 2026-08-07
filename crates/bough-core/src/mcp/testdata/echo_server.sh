#!/bin/sh
# Test fixture: a tiny MCP server over stdio (newline-delimited JSON-RPC).
#
# The Rust port of `src/mcp/testdata/echo_server.ts`. It is a POSIX shell script
# rather than a JS file on purpose: the client's tests must run with nothing on
# the machine but `sh` and `sed` — a fixture that needed a JS runtime would make
# the no-hang suite skippable, which is the one thing it must never be.
#
# Speaks just enough protocol for `client.rs` — initialize, tools/list in TWO
# pages (so the cursor loop is exercised), tools/call — and deliberately
# misbehaves on demand, because the client's whole contract is what happens when
# a server does not cooperate.
#
# Tools:
#   echo   {text}  — readOnlyHint; returns the text plus structuredContent
#   scream {text}  — annotated as a write; uppercases
#   boom   {}      — returns an isError RESULT (a tool failure is data)
#   die    {}      — writes to stderr and exits the process MID-CALL, never replying
#   slow   {}      — never replies at all, and stays alive doing it
#   loose  {q}     — inputSchema missing `type: "object"`, so only the lenient path keeps it
#
# Flags:
#   --deaf   read stdin forever, answer nothing (a server that starts and hangs)
#   --noise  print a non-JSON banner to stdout before the handshake

DEAF=0
NOISE=0
for arg in "$@"; do
  case "$arg" in
    --deaf) DEAF=1 ;;
    --noise) NOISE=1 ;;
  esac
done

PAGE1='{"tools":[{"name":"echo","description":"Echo the text back.\nSecond line that the prompt section must drop.","inputSchema":{"type":"object","properties":{"text":{"type":"string"}},"required":["text"]},"annotations":{"readOnlyHint":true}}],"nextCursor":"page2"}'
# The last entry has no name at all: not callable even in principle, so the
# client drops it. `loose` is missing `type: "object"`, which the strict schema
# rejects and the lenient fallback keeps.
PAGE2='{"tools":[{"name":"scream","description":"Echo the text back, LOUDLY.","inputSchema":{"type":"object","properties":{"text":{"type":"string"}},"required":["text"]},"annotations":{"readOnlyHint":false,"destructiveHint":true}},{"name":"boom","description":"Always fails.","inputSchema":{"type":"object","properties":{}}},{"name":"die","description":"Kills the server.","inputSchema":{"type":"object","properties":{}}},{"name":"slow","description":"Never answers.","inputSchema":{"type":"object","properties":{}}},{"name":"loose","description":"Advertised with a sloppy schema.","inputSchema":{"properties":{"q":{"type":"string"}}}},{"description":"an entry with no name"}]}'

if [ "$NOISE" = 1 ]; then
  echo "echo-fixture starting up (this line is not JSON)"
fi

while IFS= read -r line; do
  if [ "$DEAF" = 1 ]; then
    continue
  fi
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"initialize"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"echo-fixture","version":"0"}}}\n' "$id"
      ;;
    *'"method":"tools/list"'*)
      case "$line" in
        *'"cursor":"page2"'*)
          printf '{"jsonrpc":"2.0","id":%s,"result":%s}\n' "$id" "$PAGE2" ;;
        *)
          printf '{"jsonrpc":"2.0","id":%s,"result":%s}\n' "$id" "$PAGE1" ;;
      esac
      ;;
    *'"method":"tools/call"'*)
      tool=$(printf '%s' "$line" | sed -n 's/.*"name":"\([^"]*\)".*/\1/p')
      text=$(printf '%s' "$line" | sed -n 's/.*"text":"\([^"]*\)".*/\1/p')
      q=$(printf '%s' "$line" | sed -n 's/.*"q":"\([^"]*\)".*/\1/p')
      case "$tool" in
        die)
          # Mid-call death: no reply, a diagnostic on stderr, gone.
          echo "echo-fixture: asked to die, taking the server down" >&2
          exit 3
          ;;
        slow)
          : # alive, and never answering
          ;;
        echo)
          printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"%s"}],"structuredContent":{"echoed":"%s"}}}\n' "$id" "$text" "$text"
          ;;
        scream)
          loud=$(printf '%s' "$text" | tr '[:lower:]' '[:upper:]')
          printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"%s"}]}}\n' "$id" "$loud"
          ;;
        boom)
          printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"kaboom"}],"isError":true}}\n' "$id"
          ;;
        loose)
          printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"q=%s"}]}}\n' "$id" "$q"
          ;;
        ping)
          # NOT advertised in tools/list (the catalog is pinned by a test): this
          # is how the suite makes the server send a REQUEST of its own, which
          # the client must refuse rather than ignore. The refusal comes back on
          # this pipe and is echoed to stderr, where the test can see it.
          printf '{"jsonrpc":"2.0","id":9001,"method":"sampling/createMessage","params":{}}\n'
          printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"pinged"}]}}\n' "$id"
          ;;
        *)
          printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"no such tool: %s"}],"isError":true}}\n' "$id" "$tool"
          ;;
      esac
      ;;
    *'"code":-32601'*)
      # The client's refusal of the server-initiated request above, echoed where
      # the test can assert it: a server left waiting forever is the same hang
      # seen from the other end of the pipe.
      printf 'refused: %s\n' "$line" >&2
      ;;
    *'"method":'*)
      : # notifications (initialized) need no reply
      ;;
  esac
done
