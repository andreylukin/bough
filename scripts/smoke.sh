#!/usr/bin/env bash
# Smoke: boot the Rust server on a scratch BOUGH_HOME, assert the API answers,
# then drive the Rust TUI headless via shell-use. With SMOKE_MODEL set (e.g.
# SMOKE_MODEL=openai/gpt-5.6-luna) the TUI leg runs a live turn through that
# model; without it the leg stops at boot + first render.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="$ROOT/target/release/bough"
PORT="${SMOKE_PORT:-43219}"
HOME_DIR="$(mktemp -d)"
[ -x "$BIN" ] || { echo "smoke: $BIN missing — run make release first" >&2; exit 2; }

BOUGH_HOME="$HOME_DIR" BOUGH_PORT="$PORT" BOUGH_MODEL="${SMOKE_MODEL:-}" "$BIN" start &
SERVER_PID=$!
cleanup() { kill "$SERVER_PID" 2>/dev/null || true; wait "$SERVER_PID" 2>/dev/null || true; }
trap cleanup EXIT

for i in $(seq 1 50); do
  curl -sf "http://127.0.0.1:$PORT/sessions" >/dev/null 2>&1 && break
  kill -0 "$SERVER_PID" 2>/dev/null || { echo "smoke: server died during boot" >&2; exit 1; }
  sleep 0.2
done
curl -sf "http://127.0.0.1:$PORT/sessions" | grep -q '\[' || { echo "smoke: GET /sessions did not answer a list" >&2; exit 1; }
echo "smoke: server up on :$PORT (BOUGH_HOME=$HOME_DIR)"

if ! command -v shell-use >/dev/null; then
  echo "smoke: shell-use not installed — skipping TUI leg" >&2
  exit 0
fi

SID="bough-smoke-$$"
SU() { shell-use --session "$SID" "$@"; }
SU run env BOUGH_HOME="$HOME_DIR" BOUGH_PORT="$PORT" "$BIN" tui
trap 'SU close 2>/dev/null || true; cleanup' EXIT
# wait idle never settles (the TUI repaints on timers) — wait for the boot hint.
SU wait text "type a message" --timeout 15000
SU expect text "panicked" --not
SU expect text "OfflineError" --not
echo "smoke: TUI booted and rendered"

if [ -n "${SMOKE_MODEL:-}" ]; then
  : "${OPENROUTER_API_KEY:?smoke: SMOKE_MODEL set but OPENROUTER_API_KEY missing}"
  # The expected string must NOT appear in the prompt itself, or the wait
  # matches the echoed user message and the smoke passes on a dead loop.
  SU type "combine the words RUST and PARITY with a single hyphen and reply with only that"
  SU press Enter
  SU wait text "RUST-PARITY" --timeout 120000
  echo "smoke: live turn on $SMOKE_MODEL completed"
fi

SU screenshot
echo "smoke: PASS"
