#!/usr/bin/env bash
# Drive the bough TUI in a tmux pane for visual iteration and dump it as text.
#
# tmux panes are real PTYs, which OTP 29's tty stack (io:columns/io:rows)
# accepts — unlike VHS/ttyd, which now makes shore crash with {error, enotsup}.
# Capturing the pane as text is also faster and cheaper to inspect than images.
#
# Usage:
#   scripts/capture.sh start [PROJECT]   launch a fresh TUI (default /tmp/boughwork)
#   scripts/capture.sh send "<keys>"     tmux send-keys to the TUI (e.g. "Tab", text)
#   scripts/capture.sh snap              print the current pane as text
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
session="bough"
server="http://127.0.0.1:4096"

ensure_server() {
  curl -fsS "$server/health" >/dev/null 2>&1 && return
  ( cd "$root/packages/bough_server" && gleam run ) >/tmp/bough_server.log 2>&1 &
  for _ in $(seq 1 60); do
    curl -fsS "$server/health" >/dev/null 2>&1 && break
    sleep 0.5
  done
}

case "${1:-start}" in
  start)
    project="${2:-/tmp/boughwork}"
    mkdir -p "$project"
    ensure_server
    ( cd "$root/packages/bough_tui" && gleam build >/dev/null 2>&1 )
    tmux kill-session -t "$session" 2>/dev/null || true
    tmux new-session -d -s "$session" -x 200 -y 50
    tmux send-keys -t "$session" \
      "cd $root/packages/bough_tui && BOUGH_PROJECT=$project BOUGH_SERVER=$server gleam run" Enter
    sleep 9
    tmux capture-pane -t "$session" -p
    ;;
  send)
    tmux send-keys -t "$session" "${2:?keys required}"
    ;;
  snap)
    tmux capture-pane -t "$session" -p
    ;;
  *)
    echo "usage: $0 {start [PROJECT]|send <keys>|snap}" >&2
    exit 2
    ;;
esac
