#!/usr/bin/env bash
# TUI acceptance suite: drive the real ratatui client through a real PTY with
# shell-use and ASSERT on what is on screen. Every check is `shell-use expect`,
# which exits 1 when it fails — so this is a gate, not a transcript.
#
# The whole point is that a TUI surface can compile, have green unit tests, and
# still be unreachable (that failure happened five times during this port). The
# only proof is driving the binary a human would run.
#
#   ./tui-test.sh                                   # offline surfaces only
#   SMOKE_MODEL=openai/gpt-5.6-luna ./tui-test.sh   # + live turn on that model
set -uo pipefail

RS_DIR="$(cd "$(dirname "$0")" && pwd)"
BIN="$RS_DIR/target/release/bough"
PORT="${TUI_TEST_PORT:-43420}"
S="tui-test-$$"
MODEL="${SMOKE_MODEL:-}"
[ -x "$BIN" ] || { echo "tui-test: $BIN missing — run make rs-release first" >&2; exit 2; }

HOME_DIR="$(mktemp -d)"
BOUGH_HOME="$HOME_DIR" BOUGH_PORT="$PORT" BOUGH_MODEL="$MODEL" "$BIN" start >/tmp/tui-test-server.log 2>&1 &
SERVER=$!
cleanup() {
  shell-use --session "$S" close >/dev/null 2>&1 || true
  kill "$SERVER" 2>/dev/null || true
}
trap cleanup EXIT

for _ in $(seq 1 60); do
  curl -sf "http://127.0.0.1:$PORT/sessions" >/dev/null 2>&1 && break
  kill -0 "$SERVER" 2>/dev/null || { echo "tui-test: server died (see /tmp/tui-test-server.log)" >&2; exit 1; }
  sleep 0.3
done

pass=0; fail=0
SU() { shell-use --session "$S" "$@"; }
# check <name> <shell-use args...> — runs an expect and tallies it.
#
# NOTE: `expect text` is a STRICT single-match by default — it FAILS when the
# string appears more than once. Pass --no-strict whenever the string
# legitimately repeats (a tab title in the strip, a palette name echoed in the
# "current:" line, a filename across picker rows), or the assertion tests the
# layout's incidental uniqueness instead of the thing you meant.
check() {
  local name="$1"; shift
  if SU "$@" >/dev/null 2>&1; then
    printf 'ok    %s\n' "$name"; pass=$((pass + 1))
  else
    printf 'FAIL  %s\n' "$name"; fail=$((fail + 1))
    SU text 2>/dev/null | sed 's/^/        | /' | head -20
  fi
}
# Clear the composer between probes: the TUI has no "clear line" chord, and a
# leftover draft changes what the next key means.
clear_composer() { for _ in $(seq 1 40); do SU press BackSpace >/dev/null 2>&1; done; }

# 100x36 so panels have room; at 80x30 the tab strip collapses by design and
# these assertions would be testing the fallback rather than the surface.
SU run --cols 100 --rows 36 env BOUGH_HOME="$HOME_DIR" BOUGH_PORT="$PORT" "$BIN" tui >/dev/null
SU wait text "type a message" --timeout 15000 >/dev/null \
  || { echo "FAIL  the TUI never rendered its composer"; SU text; exit 1; }

echo "── boot ─────────────────────────────────────────"
check "the composer renders its placeholder"      expect text "type a message"
check "the status bar names the workspace"        expect text "? help"
check "no panic reached the screen"               expect text "panicked" --not
check "the client is not offline"                 expect text "unreachable" --not

echo "── panels ───────────────────────────────────────"
SU keys "Control+f" >/dev/null; SU wait text "conversations" --timeout 5000 >/dev/null 2>&1
check "^f opens the tree tab"                     expect text "tree" --no-strict
check "the tree names its sibling tabs"           expect text "changes"
check "an empty tree says so rather than blanking" expect text "no conversations yet"
SU keys "Control+d" >/dev/null; sleep 0.6
check "^d switches to changes"                    expect text "changes" --no-strict
SU keys "Control+y" >/dev/null; sleep 0.6
check "^y opens the theme picker"                 expect text "Default" --no-strict
check "the theme picker previews live"            expect text "preview"
SU keys "Control+k" >/dev/null; sleep 0.6
check "^k opens skills"                           expect text "skills" --no-strict
check "no tab is an empty placeholder"            expect text "nothing to show here yet" --not
SU press Escape >/dev/null; sleep 0.4

echo "── help ─────────────────────────────────────────"
SU type "?" >/dev/null; sleep 0.8
check "? paints the generated overlay"            expect text "esc closes"
check "the overlay documents quitting"            expect text "quit"
SU press Escape >/dev/null; sleep 0.4; clear_composer

echo "── composer ─────────────────────────────────────"
SU type "@Cargo" >/dev/null; SU wait text "Cargo.toml" --timeout 6000 >/dev/null 2>&1
check "@ opens the file picker on real files"     expect text "Cargo.toml" --no-strict
check "the picker explains its keys"              expect text "inserts"
SU press Escape >/dev/null; clear_composer
SU type "/rules" >/dev/null; SU press Enter >/dev/null; sleep 1.2
check "/rules is wired (not refused)"             expect text "not wired" --not
clear_composer
SU type "/saved" >/dev/null; SU press Enter >/dev/null; sleep 1.2
check "/saved is wired"                           expect text "not wired" --not
clear_composer
SU type "/artifacts" >/dev/null; SU press Enter >/dev/null; sleep 1.2
check "/artifacts is wired"                       expect text "not wired" --not
clear_composer
SU type "/nonsense-command" >/dev/null; SU press Enter >/dev/null; sleep 1.0
check "an unknown /word is intercepted, never sent" expect text "nonsense-command" --no-strict
clear_composer

echo "── background shell ─────────────────────────────"
SU type '!sleep 25' >/dev/null; SU press Enter >/dev/null
SU wait text "sleep 25" --timeout 12000 >/dev/null 2>&1
check "! runs a shell and the rail shows it"      expect text "sleep 25"
clear_composer

if [ -n "$MODEL" ]; then
  echo "── live turn on $MODEL ──────────────────────────"
  : "${OPENROUTER_API_KEY:?tui-test: SMOKE_MODEL set but OPENROUTER_API_KEY missing}"
  # The expected string is built by the model from words that never appear
  # hyphenated in the prompt, so a passing wait cannot be the echoed prompt.
  SU type "join the words TUI and PROVEN with one hyphen, reply with only that" >/dev/null
  SU press Enter >/dev/null
  SU wait text "TUI-PROVEN" --timeout 150000 >/dev/null 2>&1
  check "a live turn streams its reply into the transcript" expect text "TUI-PROVEN"
  check "the turn is attributed to the agent"               expect text "bough" --no-strict
  sleep 4
  SU keys "Control+f" >/dev/null; sleep 1.5
  check "the tree lists the conversation it just ran"       expect text "no conversations yet" --not
  SU press Escape >/dev/null
fi

SU screenshot "$RS_DIR/target/tui-test.svg" >/dev/null 2>&1 \
  && echo "(screenshot: $RS_DIR/target/tui-test.svg)"

echo
echo "tui-test: $pass passed, $fail failed"
exit $((fail > 0))
