#!/usr/bin/env bash
# env: one persona's own bough serve on a copy of the fixture HOME.
#
#   go/tests/ux/env.sh up NAME FIXTURE   copy FIXTURE to $UX_RUN/NAME/home,
#                                        start serve on a free port, print its URL
#   go/tests/ux/env.sh down NAME         stop that serve and NAME's agent-browser session
#
# BOUGH_BIN is the binary under test (a worktree's build when verifying a
# fix); UX_RUN is the run's directory. Everything a persona touches lives
# under $UX_RUN/NAME, and down stops only the pid up wrote there, so
# parallel testers and other bough processes are never swept up.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
: "${UX_RUN:?UX_RUN unset}"
cmd="${1:-}" name="${2:-}"
[ -n "$name" ] || { echo "usage: $0 up NAME FIXTURE | down NAME" >&2; exit 2; }
dir="$UX_RUN/$name"

case "$cmd" in
up)
  [ $# -eq 3 ] || { echo "usage: $0 up NAME FIXTURE" >&2; exit 2; }
  : "${BOUGH_BIN:?BOUGH_BIN unset}"
  mkdir -p "$UX_RUN"
  python3 "$here/fixture.py" clone "$3" "$dir/home"
  mkdir -p "$dir/shots"
  port="$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1])')"
  # The fixture's own env file holds the only provider key serve sees.
  (cd "$dir/home" && exec env -u ANTHROPIC_API_KEY -u OPENAI_API_KEY -u OPENROUTER_API_KEY -u CEREBRAS_API_KEY \
    HOME="$dir/home" "$BOUGH_BIN" serve --run "127.0.0.1:$port") >"$dir/serve.log" 2>&1 &
  echo $! >"$dir/serve.pid"
  url="http://127.0.0.1:$port"
  token="$(cat "$dir/home/.bough/serve.token")"
  for _ in $(seq 1 150); do
    curl -sf "$url/api/health" -H "Authorization: Bearer $token" -H "Origin: $url" >/dev/null && { echo "$url" | tee "$dir/url"; exit 0; }
    sleep 0.2
  done
  cat "$dir/serve.log" >&2
  echo "env: serve for $name did not start" >&2
  exit 1
  ;;
down)
  agent-browser --session "$name" close >/dev/null 2>&1 || true
  if [ -f "$dir/serve.pid" ]; then
    pid="$(cat "$dir/serve.pid")"
    kill "$pid" 2>/dev/null || true
    for _ in $(seq 1 50); do kill -0 "$pid" 2>/dev/null || break; sleep 0.1; done
    kill -9 "$pid" 2>/dev/null || true
    rm -f "$dir/serve.pid"
  fi
  ;;
*) echo "usage: $0 up NAME FIXTURE | down NAME" >&2; exit 2 ;;
esac
