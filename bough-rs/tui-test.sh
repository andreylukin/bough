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
# check_cmd <name> <shell command…> — same tally, for an assertion that is not
# an `expect` (reading cell colours needs `cells` + `jq`).
check_cmd() {
  local name="$1"; shift
  if "$@" >/dev/null 2>&1; then
    printf 'ok    %s\n' "$name"; pass=$((pass + 1))
  else
    printf 'FAIL  %s\n' "$name"; fail=$((fail + 1))
  fi
}
# Clear the composer between probes: a leftover draft changes what the next key
# means. `End` and not `^e` for the trip to end-of-line — `^e` is `cursor.end`
# only while something is typed, and on an ALREADY-empty draft the same chord is
# `fold.all`, so clearing with it silently toggled every fold in the transcript
# and the fold assertions further down failed on state this helper had flipped.
clear_composer() {
  SU press End >/dev/null 2>&1
  SU press ctrl+u >/dev/null 2>&1
  for _ in $(seq 1 40); do SU press BackSpace >/dev/null 2>&1; done
}

# The bg colour of one cell, as `#rrggbb` or `default`. NO `--json`: `cells`
# already answers in JSON and the flag reshapes it into an envelope where
# `.cells` is absent, so every colour read back comes out `null` and every
# comparison between two of them trivially passes.
cell_bg() { SU cells "$1" "$2" 1 1 2>/dev/null | jq -r '.cells[0].bg'; }
# Left press / left-drag motion / left release as SGR reports (1-BASED cells).
# `shell-use mouse move` sends a BUTTONLESS motion, which is `Moved` and not
# `Drag(Left)` — dragging must be spelled out or the selection never grows.
sgr_down() { SU write "$(printf '\033[<0;%d;%dM' "$1" "$2")" >/dev/null 2>&1; }
sgr_drag() { SU write "$(printf '\033[<32;%d;%dM' "$1" "$2")" >/dev/null 2>&1; }
sgr_up()   { SU write "$(printf '\033[<0;%d;%dm' "$1" "$2")" >/dev/null 2>&1; }

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
# THE FAILURE MODE OF THE STYLED TRANSCRIPT: `lines.rs` bakes SGR into every
# row, so a renderer that paints them raw prints the escapes as text. These are
# the litter that shows up first.
check "no escape sequence is painted as text"     expect text "[0m" --not
check "no dim escape is painted as text"          expect text "[2m" --not

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

echo "── line editing ─────────────────────────────────"
# EVERY ONE OF THESE WAS DEAD ON THE REAL BINARY while the unit suite was green:
# `app.rs` hand-rolled a raw `KeyCode` match that knew ^a/^e/home/end/backspace/
# ←/→ and nothing else, so the word motions and the kills resolved to a command
# and were dropped on the floor. The chords are in the binding table and printed
# in the help overlay, which is exactly what made it a lie rather than a gap.
clear_composer
SU type "hello world there" >/dev/null; sleep 0.5
SU press alt+b >/dev/null; sleep 0.3
SU press alt+b >/dev/null; sleep 0.3
SU type "MARK" >/dev/null; sleep 0.6
check "⌥b moves a word back (twice), and typing lands there" \
  expect text "hello MARKworld there"
clear_composer

SU type "alpha beta gamma" >/dev/null; sleep 0.5
SU press ctrl+w >/dev/null; sleep 0.5
check "^w deletes the word before the cursor"      expect text "gamma" --not
check "^w leaves the rest of the line alone"       expect text "alpha beta"
SU press alt+b >/dev/null; sleep 0.3
SU type "<" >/dev/null; sleep 0.5
check "⌥b lands inside what is left"               expect text "alpha <beta"
clear_composer

SU type "kill from here to the end" >/dev/null; sleep 0.5
SU press alt+b >/dev/null; sleep 0.3
SU press ctrl+k >/dev/null; sleep 0.5
check "^k kills to end of line"                    expect text "end" --not
check "^k keeps everything before the cursor"      expect text "kill from here to the"
SU press ctrl+u >/dev/null; sleep 0.5
check "^u kills the whole line"                    expect text "type a message"

# ↑/↓ walk the DRAFT's lines once there is more than one — and a multi-line
# draft needs `^j`, which was itself unreachable: ratatui enables raw mode on
# crossterm 0.28 while this crate's 0.29 parses the bytes, and 0.29 therefore
# decoded 0x0a as Enter and SENT the message instead of inserting a newline.
SU type "first line" >/dev/null; sleep 0.4
SU press ctrl+j >/dev/null; sleep 0.5
SU type "second line" >/dev/null; sleep 0.6
check "^j inserts a newline instead of sending"    expect text "first line"
check "the second line is its own row"             expect text "second line"
SU press Up >/dev/null; sleep 0.4
SU type "<UP>" >/dev/null; sleep 0.6
check "↑ moves to the line above in a multiline draft" expect text "first line<UP>"
SU press Down >/dev/null; sleep 0.4
SU type "<DOWN>" >/dev/null; sleep 0.6
check "↓ moves back down"                          expect text "second line<DOWN>"
clear_composer

echo "── drag selection ───────────────────────────────"
# `self.sel` was tracked by the mouse handler and the copy worked, but NO render
# path read it — so a drag highlighted nothing and the cells under it reported
# the untouched background the whole time. The copy passing is not the feature.
SU type "highlight me please" >/dev/null; sleep 0.6
COMPOSER_Y=$(SU text 2>/dev/null | grep -n "highlight me please" | head -1 | cut -d: -f1)
if [ -n "$COMPOSER_Y" ]; then
  Y0=$((COMPOSER_Y - 1))          # `text` is 1-based; `cells` is 0-based
  PLAIN=$(cell_bg 4 "$Y0")
  check_cmd "the dragged span is not highlighted before the drag" \
    test "$PLAIN" != "#4ec98f"
  sgr_down 5 "$COMPOSER_Y"; sleep 0.4
  sgr_drag 20 "$COMPOSER_Y"; sleep 0.7
  INSIDE=$(cell_bg 4 "$Y0")
  OUTSIDE=$(cell_bg 20 "$Y0")
  check_cmd "a cell inside the drag is repainted mid-drag" \
    test "$INSIDE" != "$PLAIN"
  check_cmd "the highlight is the accent, not a no-op restyle" \
    test "$INSIDE" = "#4ec98f"
  check_cmd "the cell past the drag is untouched" \
    test "$OUTSIDE" = "$PLAIN"
  check "the highlight recolours the text, never overwrites it" \
    expect text "highlight me please"
  sgr_up 20 "$COMPOSER_Y"; sleep 0.8
  AFTER=$(cell_bg 4 "$Y0")
  check_cmd "the highlight clears when the selection is dropped" \
    test "$AFTER" = "$PLAIN"
  check "a real drag copies on release and says how much" \
    expect text "copied" --no-strict
else
  printf 'FAIL  the drag target never rendered\n'; fail=$((fail + 1))
fi
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
  clear_composer

  echo "── the transcript's folds and cards ─────────────"
  # A turn that actually RUNS a program: the tool fold, its `↳ output` row and
  # the per-call status are what `build_lines` renders and the v1 miniature
  # could not draw at all.
  SU type "run: echo FOLD-PROVEN" >/dev/null
  SU press Enter >/dev/null
  SU wait text "FOLD-PROVEN" --timeout 150000 >/dev/null 2>&1
  sleep 3
  check "the tool call renders as a collapsed fold"         expect text "1 step" --no-strict
  SU keys "Control+e" >/dev/null; sleep 1
  check "^e unfolds the group's program"                    expect text "output" --no-strict
  check "the unfolded call reports its status"              expect text "done" --no-strict
  SU keys "Control+e" >/dev/null; sleep 1
  check "^e folds it back"                                  expect text "output" --not
  check "the margin row names the project rules"            expect text "# rules:" --no-strict
  check "still no escape sequence on a rich screen"         expect text "[0m" --not
  clear_composer
fi

SU screenshot "$RS_DIR/target/tui-test.svg" >/dev/null 2>&1 \
  && echo "(screenshot: $RS_DIR/target/tui-test.svg)"

echo
echo "tui-test: $pass passed, $fail failed"
exit $((fail > 0))
