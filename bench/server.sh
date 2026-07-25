#!/usr/bin/env bash
# Isolated bough server for the bench: own port, DB, and state
# root under bench/state/ — it never touches ~/.bough or the daily-driver server.
# The model is pinned via BOUGH_MODEL at launch, so nothing is persisted anywhere.
#
# usage: bench/server.sh start|stop|status
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

PID_FILE="$STATE/server.pid"
LOG_FILE="$STATE/server.log"

server_pid() {
  [ -f "$PID_FILE" ] && kill -0 "$(cat "$PID_FILE")" 2>/dev/null && cat "$PID_FILE" || true
}

case "${1:-status}" in
  start)
    if [ -n "$(server_pid)" ]; then
      echo "already running (pid $(server_pid)) on :$PORT"
      exit 0
    fi
    # shellcheck source=/dev/null
    [ -f "$HOME/.bough/env" ] && set -a && source "$HOME/.bough/env" && set +a
    [ -n "${ANTHROPIC_API_KEY:-}" ] || { echo "ANTHROPIC_API_KEY not set (checked ~/.bough/env)" >&2; exit 1; }
    # Python lsp needs basedpyright on the host (leta's default python server);
    # without it every lsp.* call on python tasks fails and wastes agent rounds.
    command -v basedpyright-langserver >/dev/null 2>&1 ||
      echo "warn: basedpyright not installed — python lsp will fail (npm i -g basedpyright)" >&2
    mkdir -p "$STATE"
    (
      cd "$BENCH/.."
      BOUGH_PORT="$PORT" \
      BOUGH_DB="$STATE/bough.db" \
      BOUGH_SUBAGENT_BASE="$STATE/workspaces" \
      BOUGH_SNAPSHOT_BASE="$STATE/snapshots" \
      BOUGH_MODEL="$MODEL_BOUGH" \
      BOUGH_EFFORT="" \
      BOUGH_PROMPT_DIR="${BOUGH_PROMPT_DIR:-}" \
      nohup deno run --allow-net --allow-env --allow-read --allow-write \
        --allow-ffi --allow-sys --allow-run src/server/main.ts >"$LOG_FILE" 2>&1 &
      echo $! >"$PID_FILE"
    )
    for _ in $(seq 1 30); do
      curl -sf "$API/sessions" >/dev/null 2>&1 && { echo "bench server up on :$PORT (model $MODEL_BOUGH)"; exit 0; }
      sleep 0.5
    done
    echo "server did not come up — tail of $LOG_FILE:" >&2
    tail -20 "$LOG_FILE" >&2
    exit 1
    ;;
  stop)
    pid="$(server_pid)"
    [ -n "$pid" ] && kill "$pid" && echo "stopped (pid $pid)" || echo "not running"
    rm -f "$PID_FILE"
    ;;
  status)
    pid="$(server_pid)"
    [ -n "$pid" ] && echo "up (pid $pid) on :$PORT" || echo "down"
    ;;
  *)
    echo "usage: server.sh start|stop|status" >&2
    exit 2
    ;;
esac
