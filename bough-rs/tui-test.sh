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
# THE DEFECT THIS SECTION EXISTS FOR: the picker listed palettes, previewed on
# ↑↓ and accepted with ⏎ — and changed NO PIXEL, because every component painted
# from `const`s. Text assertions passed the whole time, so these read COLOURS.
# The panel is a raised surface (`panel`), and Midnight's whole note is "deeper
# surfaces": its panel is #101216 against the built-in #14161a.
PANEL_Y=$(SU text 2>/dev/null | grep -n "\[theme\]" | head -1 | cut -d: -f1)
if [ -n "$PANEL_Y" ]; then
  PY=$((PANEL_Y - 1))
  BASE_PANEL=$(cell_bg 6 "$PY")
  check_cmd "the panel paints the built-in surface before any preview" \
    test "$BASE_PANEL" = "#14161a"
  for _ in $(seq 1 7); do SU press Down >/dev/null 2>&1; done; sleep 0.8
  PREVIEWED=$(cell_bg 6 "$PY")
  check "↑↓ names what is being previewed"          expect text "previewing Midnight"
  check_cmd "arrowing onto a preset repaints the product, not a swatch" \
    test "$PREVIEWED" = "#101216"
  SU press Escape >/dev/null; sleep 0.8
  SU keys "Control+y" >/dev/null; sleep 0.8
  RESTORED=$(cell_bg 6 "$PY")
  check_cmd "leaving the tab restores the baseline byte for byte" \
    test "$RESTORED" = "$BASE_PANEL"
  # ⏎ keeps it, and the server is told — as a PUT, since Midnight is a theme.
  for _ in $(seq 1 7); do SU press Down >/dev/null 2>&1; done; sleep 0.6
  SU press Enter >/dev/null; sleep 1.2
  check_cmd "⏎ keeps the palette it was previewing" \
    test "$(cell_bg 6 "$PY")" = "#101216"
  check_cmd "⏎ persists it to the server" \
    bash -c "curl -sf http://127.0.0.1:$PORT/theme | jq -e '.theme.name == \"Midnight\"'"
  # …and Default round-trips as a DELETE: no stored theme, not an empty PUT.
  for _ in $(seq 1 9); do SU press Up >/dev/null 2>&1; done; sleep 0.6
  SU press Enter >/dev/null; sleep 1.2
  check_cmd "choosing Default repaints the built-ins" \
    test "$(cell_bg 6 "$PY")" = "#14161a"
  check_cmd "choosing Default persists as a DELETE, never an empty PUT" \
    bash -c "curl -sf http://127.0.0.1:$PORT/theme | jq -e '.theme == null'"
else
  printf 'FAIL  the theme panel never rendered\n'; fail=$((fail + 1))
fi
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

echo "── paste, recall and the take-back ──────────────"
# FIVE DEFECTS THAT PASSED A GREEN SUITE and failed on a real terminal. Every
# check here drives the gesture a hand makes: the bracketed-paste burst the
# terminal really sends, the chord the `?` overlay really promises.
clear_composer
# A real paste arrives as ONE `Event::Paste`, not as N keystrokes. Nothing
# handled it, so every paste into this TUI was silently swallowed.
SU write "$(printf '\033[200~pasted words\033[201~')" >/dev/null; sleep 0.8
check "a bracketed paste lands in the composer"    expect text "pasted words"
clear_composer
# Over `QUEUE_ABOVE_CHARS` it is held aside and MARKED where the cursor was, so
# a 400-line stack trace does not bury the sentence being written (`paste.rs`).
SU write "$(printf '\033[200~a paste far longer than the fifty characters that hold one aside\033[201~')" >/dev/null; sleep 0.8
check "a long paste is held aside behind one mark" expect text "[Pasted text #1]"
check "…and its text is not inlined into the box"  expect text "far longer than the fifty" --not
clear_composer
# A pasted PATH to an image is a picture, not prose about a file the model
# cannot open. It is read and uploaded, and the composer says so.
printf '\211PNG\r\n\032\n' > "$HOME_DIR/pasted.png"
SU write "$(printf '\033[200~%s\033[201~' "$HOME_DIR/pasted.png")" >/dev/null
SU wait text "image:" --timeout 8000 >/dev/null 2>&1
check "a pasted image path attaches instead of typing itself" expect text "[image: "
check "…and the path was never inserted as text"   expect text "pasted.png" --not
clear_composer

# THE POPUP'S CURSOR IS RESET BY A NARROWING QUERY, not merely clamped: a
# clamped cursor still points at a row nobody highlighted, and ⏎ on the `/` list
# RUNS it. Walk down five rows, then narrow to a two-row list.
SU type "/" >/dev/null; sleep 0.8
for _ in $(seq 1 4); do SU press Down >/dev/null 2>&1; done; sleep 0.5
check "↓ moves the popup's highlight"              expect text "❯ /mcp"
SU type "th" >/dev/null; sleep 0.8
check "narrowing the list resets the highlight to its first row" expect text "❯ /theme"
SU press Escape >/dev/null; clear_composer

# ^n is printed in the generated `?` overlay as "start a fresh conversation" and
# resolved to a command NOTHING answered.
SU type "half a thought" >/dev/null; sleep 0.4
SU keys "Control+n" >/dev/null; sleep 1.2
check "^n clears the screen for a fresh conversation" expect text "half a thought" --not
check "…and the composer is back to its invitation"   expect text "type a message"

# ↑ on an EMPTY draft is history recall (with a multiline draft it walks lines,
# which the line-editing section above pins). A `!` line is in the same ring,
# sigil and all, so re-running the last command is ↑⏎.
SU type '!echo recall-probe' >/dev/null; SU press Enter >/dev/null
SU wait text "recall-probe" --timeout 12000 >/dev/null 2>&1
clear_composer
SU press Up >/dev/null; sleep 0.8
check "↑ on an empty draft recalls the last line sent" expect text "!echo recall-probe" --no-strict
SU press Down >/dev/null; sleep 0.6
check "↓ off the end returns to the empty draft"       expect text "type a message"
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

  # THE 3-SECOND TAKE-BACK, IN ITS DOCUMENTED USAGE: esc immediately after
  # Enter. That is exactly the window in which the message still wears its
  # optimistic `local-N` id, and handing that name to the unsend route made the
  # server answer "message local-N is not one of this session's own messages,
  # so it cannot be unsent" — a refusal the user reads as a broken feature.
  clear_composer
  SU type "this one is going straight back" >/dev/null; sleep 0.4
  SU press Enter >/dev/null; sleep 1.0
  SU press Escape >/dev/null; sleep 3
  check "esc right after enter takes the message back" expect text "took that back" --no-strict
  check "…and never names a local id at the server"    expect text "local-" --not
  check "…and the text comes back to the composer"     expect text "this one is going straight back"
  clear_composer

  # `!cmd` BORROWS the workspace's one `shell` conversation so the job has a
  # home; it must not become the thread you are chatting in. Typing `!echo hi`
  # on a fresh screen used to leave every later turn in a conversation
  # permanently titled "shell" and typed `kind:"shell"`.
  check_cmd "the ! sigil left exactly one shell conversation" \
    bash -c "curl -sf http://127.0.0.1:$PORT/sessions | jq -e '[.[]|select(.kind==\"shell\")]|length == 1'"
  check_cmd "no chat turn was typed into it" \
    bash -c "curl -sf http://127.0.0.1:$PORT/sessions | jq -e '[.[]|select(.kind==\"shell\")][0].lastTurnStatus == null'"
  check_cmd "the conversation actually chatted in is an ordinary root" \
    bash -c "curl -sf http://127.0.0.1:$PORT/sessions | jq -e '[.[]|select(.kind==\"root\" and .lastTurnStatus==\"done\")]|length >= 1'"

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

  # ---- the drill-in: a subagent must be VISIBLE ---------------------------
  #
  # `GET /sessions` DELIBERATELY excludes the collapsing kinds (subagent,
  # workflow_agent, schedule_run) — they surface only via `?originId=`. This
  # client only ever asked for the plain listing, so a running subagent got no
  # rail row, a finished one got no card, and the tree showed zero agent nodes:
  # invisible on every surface at once, while the server said it was busy.
  echo "── a spawned subagent, on every surface ─────────"
  SU type 'Use spawn() to start ONE detached subagent named "railcheck" whose task is: run bash '"'"'sleep 90'"'"' then report OK. Do not join it.' >/dev/null
  SU press Enter >/dev/null
  SU wait text "railcheck" --timeout 180000 >/dev/null 2>&1
  sleep 6
  # The server's own view first, so a green rail cannot be green over nothing.
  ROOT=$(curl -sf "http://127.0.0.1:$PORT/sessions" | jq -r '[.[]|select(.kind=="root")][0].id')
  check_cmd "the server has a busy subagent under the root" \
    bash -c "curl -sf 'http://127.0.0.1:$PORT/sessions?originId=$ROOT' | jq -e '[.[]|select(.kind==\"subagent\")]|length >= 1'"
  check_cmd "…and the plain listing still excludes it (derived visibility)" \
    bash -c "curl -sf 'http://127.0.0.1:$PORT/sessions' | jq -e '[.[]|select(.kind==\"subagent\")]|length == 0'"
  check "the running subagent takes a rail row"        expect text "railcheck" --no-strict
  check "…and the rail says how to reach it"           expect text "agent running" --no-strict
  check "…and the status bar counts it"                expect text "1 agent" --no-strict

  # ⏎ opens it, ← comes back. Both are printed in the `?` overlay and both were
  # unreachable: the rail had no row to stand on, and `SessionOut` had no
  # handler and a guard nothing ever set.
  SU press Down >/dev/null; sleep 1
  check "↓ enters the rail and offers the verbs"       expect text "stop" --no-strict
  SU press Enter >/dev/null; sleep 4
  check "⏎ opens the agent's own conversation"         expect text "back" --no-strict
  SU press Left >/dev/null; sleep 4
  check "← returns to the session that spawned it"     expect text "agent running" --no-strict

  # `x x` stops it, and the SERVER agrees — a rail that lies about stopping is
  # worse than a rail with no stop on it.
  SU press Down >/dev/null; sleep 1
  SU type "x" >/dev/null; sleep 1
  check "x arms the stop rather than firing it"        expect text "x again stops it" --no-strict
  SU type "x" >/dev/null; sleep 6
  check_cmd "…and x x actually interrupted it at the server" \
    bash -c "curl -sf 'http://127.0.0.1:$PORT/sessions?originId=$ROOT' | jq -e '[.[]|select(.kind==\"subagent\" and .busy==false)]|length >= 1'"
  clear_composer

  # ---- the tree can expand a conversation it did not open ------------------
  #
  # `panel.threads` was filled exclusively from the OPEN session and `→` on any
  # other row inserted an id and fetched nothing, so every other conversation
  # expanded to zero turns — and ⏎-fork, `e` split and `m` were unreachable
  # there. The caret flipped and the next row was the legend.
  echo "── the tree expands any conversation ────────────"
  SU keys "Control+f" >/dev/null; sleep 2
  SU press Down >/dev/null; sleep 1
  SU press Right >/dev/null; sleep 3
  check "a row that is not the open conversation expands to its turns" \
    expect text "├─" --no-strict
  # …and a subagent is a NODE in it, not only a rail row.
  check "the collapsed fan-out is offered"             expect text "spawned" --no-strict
  SU press Escape >/dev/null; sleep 1
  clear_composer

  # ---- a streaming message reaches the tree --------------------------------
  #
  # `mirror_thread` refreshed only when the LENGTH differed, and a streaming
  # message is already in the thread with empty parts — so arriving text never
  # changed the count and the tree printed `bough (no text)` over a turn full
  # of words.
  echo "── the tree shows text that streamed in ─────────"
  SU type "Say the word PEBBLE and nothing else." >/dev/null
  SU press Enter >/dev/null
  SU wait text "PEBBLE" --timeout 150000 >/dev/null 2>&1
  sleep 3
  SU keys "Control+f" >/dev/null; sleep 2
  SU press Right >/dev/null; sleep 2
  check "an assistant turn in the tree never reads (no text)" expect text "(no text)" --not
  SU press Escape >/dev/null; sleep 1
  clear_composer
fi

# ---- notices expire, and the quit row retracts with the confirm ------------
#
# `NOTICE_TTL_MS` has been defined in the ported store all along and nothing
# used it: `notice` was set-only, so every row leaked forever and rode a
# session switch into a conversation it said nothing about.
echo "── a notice is a flash, not a fixture ───────────"
SU keys "Control+c" >/dev/null; sleep 1
check "^c arms the quit and says so"                   expect text "again to quit" --no-strict
SU type "a" >/dev/null; sleep 1
check "…and typing retracts what the confirm promised" expect text "again to quit" --not
clear_composer
SU type "/nosuchthing" >/dev/null
SU press Enter >/dev/null; sleep 2
check "an unknown command explains itself"             expect text "there is no /nosuchthing" --no-strict
sleep 12
check "…and the row retires on its own ten seconds later" \
  expect text "there is no /nosuchthing" --not
clear_composer

SU screenshot "$RS_DIR/target/tui-test.svg" >/dev/null 2>&1 \
  && echo "(screenshot: $RS_DIR/target/tui-test.svg)"

echo
echo "tui-test: $pass passed, $fail failed"
exit $((fail > 0))
