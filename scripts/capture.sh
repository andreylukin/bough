#!/usr/bin/env bash
# Capture screenshots of the bough TUI with VHS for visual iteration.
# Ensures a server is running, pre-builds the TUI, then renders the tape.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
mkdir -p /tmp/boughwork

if ! curl -fsS http://127.0.0.1:4096/health >/dev/null 2>&1; then
  ( cd "$root/packages/bough_server" && gleam run ) >/tmp/bough_server.log 2>&1 &
  for _ in $(seq 1 60); do
    curl -fsS http://127.0.0.1:4096/health >/dev/null 2>&1 && break
    sleep 0.5
  done
fi

# Pre-build so `gleam run` inside the tape starts promptly.
( cd "$root/packages/bough_tui" && gleam build >/dev/null 2>&1 )

vhs "$root/scripts/capture.tape"
echo "screenshots: /tmp/bough_1_empty.png /tmp/bough_2_typed.png /tmp/bough_3_thinking.png /tmp/bough_4_result.png"
