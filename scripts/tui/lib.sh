# scripts/tui/lib.sh — the shared harness for the Phase 3 shell-use suite (§17 Phase 3).
#
# Invariant this file holds: every assertion in the suite is NAMED, and the name is what the
# verification map cites. `t <name> <cmd…>` prints `ok - <name>` or `not ok - <name>`, and the
# first failure aborts the script with a non-zero status — so `make tui-test` fails on the first
# broken bullet rather than on a wall of output.
#
# The caller (the Makefile) exports:
#   BOUGH_BIN    the RELEASE binary under test
#   BOUGH_HOME   a scratch home; each script gets its own subdirectory of it
#   BOUGH_PATCH  a generated patch that swaps `llm.anthropic` for `llm-replay` (empty when live)
#   BOUGH_LIVE   `1` for the live half of the suite (no replay patch, a real haiku answer)

set -u

: "${BOUGH_BIN:?scripts/tui: BOUGH_BIN is not set (run through \`make tui-test\`)}"
: "${BOUGH_HOME:?scripts/tui: BOUGH_HOME is not set (run through \`make tui-test\`)}"
BOUGH_PATCH="${BOUGH_PATCH:-}"
BOUGH_LIVE="${BOUGH_LIVE:-}"

SCRIPT_NAME="$(basename "$0" .sh)"
# One home PER SCRIPT: the ledgers these scripts seed must not collide, and a failed script's
# home is left behind for inspection.
HOME_DIR="$BOUGH_HOME/$SCRIPT_NAME"
rm -rf "$HOME_DIR"
mkdir -p "$HOME_DIR"

SESSION="bough-tui-$SCRIPT_NAME-$$"
export SHELL_USE_SESSION="$SESSION"

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

# ---------------------------------------------------------------------------
# assertions
# ---------------------------------------------------------------------------

# `t <name> <cmd…>`: run the command, print the TAP-shaped line, abort on failure.
t() {
  local name="$1"; shift
  local out
  if out="$("$@" 2>&1)"; then
    echo "ok - $name"
  else
    echo "not ok - $name"
    echo "# command: $*"
    [ -n "$out" ] && echo "$out" | sed 's/^/# /'
    echo "# --- screen ---"
    shell-use text 2>/dev/null | sed 's/^/# /' || true
    tui_close
    exit 1
  fi
}

# `skip <name> <why>`: a bullet this half of the suite deliberately does not run (the live half
# skips the replay-only timing assertions). Prints an `ok` line so the count stays honest.
skip() {
  echo "ok - $1 # SKIP $2"
}

# `skip_all <why> <name>...`: every bullet of a script this half deliberately does not run.
#
# A whole-script `skip one-name; exit 0` guard printed ONE `ok` line for a script carrying eight
# or ten named assertions, which is exactly the dishonest count `skip` exists to avoid. One line
# per bullet, and the names are the same ones the other half prints.
skip_all() {
  local why="$1"
  shift
  local name
  for name in "$@"; do
    echo "ok - $name # SKIP $why"
  done
}

# ---------------------------------------------------------------------------
# the binary under test
# ---------------------------------------------------------------------------

# Every script boots with the old-feed row pointed at ITS OWN scratch home.
#
# The shipped bundle defaults `bough_db` to the developer's REAL `~/.bough/bough.db` and
# `jungler_db` to `~/.jungler/jungler.db`. A suite that reads those is neither hermetic — the
# priming query's answer would depend on whose machine it runs on — nor safe. `07-old-feed.sh`
# layers its own copy of this config afterwards and wins.
#
# The WHOLE config, because a patch layer replaces an entry's `config` map rather than merging.
OLD_FEED_PATCH="$HOME_DIR/old-feed.hermetic.yml"
cat > "$OLD_FEED_PATCH" <<YML
entries:
  old-feed:
    config:
      jungler_db: $HOME_DIR/jungler.db
      bough_db: $HOME_DIR/bough.db
      state_db: $HOME_DIR/old-feed-state.db
      poll_ms: 30000
      batch: 200
      deliver_to: sol
      priming_limit: 40
      tier1: true
YML

# The suite's status line does not ANIMATE.
#
# `tui.status` repaints a spinner frame every `spinner_ms` (80) and an elapsed second on top of it
# for as long as a turn runs. A repainting screen is never idle, so `shell-use wait idle` — 99 of
# them across these scripts — could only ever run out its whole timeout whenever it was called
# mid-turn. That is where the suite's 39 minutes went: tens of seconds of nothing, tens of times.
#
# `static_status: true` is the row's own validated field for exactly this: the running turn is the
# WORD `running`, unchanged frame to frame, so the PTY goes quiet the moment the answer has landed.
# The human default is `false` and stays `false` (the bundle is untouched) — M32's finding was a
# running turn that showed nothing moving, and this suite is not a human.
#
# The WHOLE config, because a patch layer replaces an entry's `config` map rather than merging it,
# and it must stay in step with `bundles/bough-tui-app.yml`'s `tui.status` row.
#
# `24-honesty.sh` sets `TUI_STATIC_STATUS=0` before sourcing this file: its
# `a_running_turn_shows_a_spinner_and_an_elapsed_clock` bullet is ABOUT the animation, and a suite
# that switched it off everywhere would have deleted the assertion rather than sped it up.
TUI_STATIC_STATUS="${TUI_STATIC_STATUS:-1}"
STATUS_PATCH="$HOME_DIR/status.static.yml"
cat > "$STATUS_PATCH" <<YML
entries:
  tui.status:
    config:
      cwd_max: 40
      spinner: "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"
      spinner_ms: 80
      static_status: true
      hints: ["? = help", "^f = search", "tab = panes"]
YML

# The arguments this half of the suite boots with. The live half omits the replay patch so the
# real `llm.anthropic` row answers. An argument beginning with `-` is passed to the binary
# VERBATIM (that is how `--root` reaches it); anything else is a patch file.
bough_patch_args() {
  # `--local`: the suite drives THIS process's screen in THIS PTY. Without it a bare `bough` on a
  # tty is the attach client (§11 "The resident") — and every script here passes patch layers, so
  # this is belt over braces; `39-attach.sh` is the script whose SUBJECT is the attach path.
  local args="--local"
  if [ -z "$BOUGH_LIVE" ] && [ -n "$BOUGH_PATCH" ]; then
    args="$args --patch $BOUGH_PATCH"
  fi
  args="$args --patch $OLD_FEED_PATCH"
  # Code mode is the DEFAULT consumer since 2026-08-28, and it CONCEALS the typed tools from the
  # agent — a replayed `bash` call under it is refused with "no tool named `bash` is available". A
  # script whose subject IS the typed tool surface says so with `TYPED_TOOLS=1` before sourcing
  # this file, and gets the shipped fallback layer (`bundles/bough-typed.yml`,
  # `docs/configuration.md`).
  if [ "${TYPED_TOOLS:-}" = 1 ]; then
    args="$args --patch $REPO_ROOT/bundles/bough-typed.yml"
  fi
  if [ "$TUI_STATIC_STATUS" = 1 ]; then
    args="$args --patch $STATUS_PATCH"
  fi
  local extra
  for extra in "$@"; do
    case "$extra" in
      -*) args="$args $extra" ;;
      *) args="$args --patch $extra" ;;
    esac
  done
  echo "$args"
}

# `add_row <root-name> <<'YML' … YML`: add a ROW to this script's own copy of the tui bundle, and
# echo the `--root` argument that boots it.
#
# A `--patch` layer can only modify a row some layer already created — a patch naming a new id is
# reported as "names row `x`, which no layer created" and then IGNORED, so a script that mounts a
# fixture row through `--patch` boots the ordinary tree and asserts nothing. Rows come from a
# bundle, and `--root` is what points the binary at a bundle directory it may edit.
add_row() {
  local name="$1" root="$HOME_DIR/root-$1"
  if [ ! -d "$root" ]; then
    mkdir -p "$root"
    cp -R "$REPO_ROOT/profiles" "$root/profiles"
    cp -R "$REPO_ROOT/bundles" "$root/bundles"
  fi
  cat >> "$root/bundles/bough-tui-app.yml"
  echo "--root $root"
}

# Open a PTY running a SHELL (not the binary directly): 08-restore asserts that the shell prompt
# echoes typed characters again after the binary exits, which is what "raw mode is off" means.
tui_open() {
  # The PTY's cwd is a SCRATCH working directory, never the checkout: V2's `write_file` intent
  # writes a real file through `tools-baseline`, and pointing it at $REPO_ROOT left `notes/demo.rs`
  # behind in the repo on every run.
  mkdir -p "$HOME_DIR/work"
  shell-use open --cols 120 --rows 40 --cwd "$HOME_DIR/work" >/dev/null
  shell-use submit "export BOUGH_HOME=$HOME_DIR" >/dev/null
  wait_for "export BOUGH_HOME" 5000
}

# `wait_for <text> [ms]`: block until the text the NEXT assertion needs is on screen.
#
# This is what replaced `wait idle` everywhere a turn could still be running. `wait idle` asks the
# PTY to stop repainting, which is a question about the CLOCK — an animated status line, a
# streaming answer, a spinner — and never about the fact a bullet is waiting for; mid-turn it could
# only ever run its whole timeout out. `wait text` asks the question the assertion actually has.
#
# Failure is swallowed on purpose: this is a WAIT, not an assertion. The named bullet after it
# still polls and still decides, so a missed needle here reports as that bullet failing with its
# own message rather than as an unnamed harness abort.
wait_for() {
  shell-use wait text "$1" --timeout "${2:-30000}" >/dev/null 2>&1 || true
}

# The composer's placeholder: what is on screen once the TUI owns the terminal, in every profile
# that has a composer. `tui_start` waits for THIS rather than for the screen to go quiet — boot
# ends when the shell has drawn, and "the bytes stopped" is a different and much slower claim.
BOOT_MARK="Type a message"

# Start the binary in the open session. Extra arguments become additional `--patch` layers.
tui_start() {
  shell-use submit "$BOUGH_BIN $(bough_patch_args "$@")" >/dev/null
  # The strip pane is the first thing on screen in every profile that has one.
  wait_for "$BOOT_MARK" 20000
  # …and then for the ROSTER. The composer's placeholder is drawn on the FIRST frame, while the
  # rest of the tree is still activating: a command submitted into that window comes back mangled
  # (`10-memory.sh` sent `/seal` and the palette answered `usage: /seal`) or is answered by a tree
  # that is not up yet (`05-commands.sh` asked `/agents` and was told "no agents are running").
  # The first rail row is the tree saying it has finished. Best-effort, like every `wait_for`: a
  # boot that deliberately never activates (`08-restore.sh`) has no rail and must still return.
  wait_for "sol" 15000
  sleep 0.5
}

# Ask the running TUI to quit, then wait for the shell prompt back.
tui_quit() {
  # A draft the test left in the composer would make `/quit` a suffix of it, so the composer is
  # emptied first (phase ux1: nothing the user typed is destroyed, so nothing clears itself).
  #
  # Ctrl+U kills the current LINE and is a no-op at column 0, so ONE of them cannot empty a
  # MULTI-LINE draft — `18-draft.sh` ends with a three-line one, and `/quit` was being appended
  # as a fourth line and SENT AS A MESSAGE. Every failure here is swallowed, so the script still
  # reported all-ok and the process was killed by the EXIT trap instead of exiting cleanly.
  local i
  for i in $(seq 1 12); do
    shell-use press "Ctrl+u" >/dev/null 2>&1 || true
    shell-use press Backspace >/dev/null 2>&1 || true
  done
  shell-use submit "/quit" >/dev/null 2>&1 || true
  # The binary is GONE: the shell prompt is back and the alt screen is not. A text wait, because
  # the thing being waited for is an event and not a quiet clock.
  wait_for "export BOUGH_HOME" 10000
  # …and the PROCESS is gone. The terminal is restored BEFORE the tree is torn down, so a prompt
  # back on screen is not a shutdown finished: the ledger's closing checkpoint runs after it, and
  # `24-honesty.sh` reads the WAL from outside the moment this returns.
  local i
  for i in $(seq 1 60); do
    pgrep -f "$BOUGH_BIN" >/dev/null 2>&1 || break
    sleep 0.25
  done
}

tui_close() {
  shell-use close >/dev/null 2>&1 || true
}

trap tui_close EXIT

# ---------------------------------------------------------------------------
# the ledger, read from outside the process
# ---------------------------------------------------------------------------

LEDGER="$HOME_DIR/ledger.db"

# `sql <query>` against this script's ledger. Absent database ⇒ empty output, never an error: a
# script asserts on the COUNT, and "no ledger yet" is a count of zero.
#
# `.timeout` is NOT decoration. A bare `sqlite3` gives up the instant the binary holds a write
# lock — the ledger's closing checkpoint is exactly that — prints `database is locked (5)` on
# stderr and leaves stdout EMPTY, which `steps_of` then reports as a count of zero. "I could not
# read the ledger" and "the ledger holds nothing" are opposite facts and used to render the same,
# and the merge of track B made the window wide enough to hit (`01-boot-and-turn.sh`'s
# `the_turn_landed_as_ledger_steps`). Waiting is the honest answer for a reader racing a writer.
#
# And the SAME distinction one level up: a `sqlite3` that FAILS is retried rather than reported as
# an empty answer. `.timeout` covers a busy lock; it does not cover the window a live recompose
# opens, where the ledger row is being torn down and rebuilt and the reader gets an error rather
# than a wait. `24-honesty.sh` reads the identity band immediately after a patch reload, and read
# "no request/header in the ledger" over a ledger that held three.
SQL_BUSY_MS=5000
sql() {
  [ -f "$LEDGER" ] || { echo ""; return 0; }
  local out i
  for i in 1 2 3 4 5 6 7 8; do
    if out="$(sqlite3 -cmd ".timeout $SQL_BUSY_MS" "$LEDGER" "$1" 2>/dev/null)"; then
      printf '%s\n' "$out"
      return 0
    fi
    sleep 0.5
  done
  echo ""
  return 0
}

# How many steps of a kind the ledger holds.
steps_of() {
  local n
  n="$(sql "select count(*) from steps where type = '$1';")"
  echo "${n:-0}"
}

# `wait_any <ms> <needle>...`: block until ANY of the needles is on screen.
#
# For the reports that legitimately have two shapes: `/seal` says either `N call(s),` or `nothing
# to seal`, and a wait on the bare word `seal` matches the command`s own echo the instant it is
# typed — which is a wait for nothing at all.
wait_any() {
  local ms="$1"; shift
  local i n
  for i in $(seq 1 $(( ms / 500 ))); do
    for n in "$@"; do
      shell-use text | grep -qF -- "$n" && return 0
    done
    sleep 0.5
  done
  return 0
}

# `wait_steps <kind> <n> [tries]`: block until the ledger holds at least `n` steps of `kind`.
#
# The ledger half of `wait_for`, and the mid-turn replacement for `wait idle` wherever the bullet
# that follows reads the ledger rather than the screen. Like `wait_for` it never fails: it is a
# wait, and the NAMED bullet after it is what decides and what reports.
wait_steps() {
  local kind="$1" want="$2" i
  for i in $(seq 1 "${3:-60}"); do
    [ "$(steps_of "$kind")" -ge "$want" ] && return 0
    sleep 0.5
  done
  return 0
}

# `expect_steps <kind> <min>`: at least `min` steps of `kind` landed.
expect_steps() {
  local kind="$1" min="$2" got
  got="$(steps_of "$kind")"
  if [ "${got:-0}" -ge "$min" ]; then
    return 0
  fi
  echo "expected >= $min steps of kind $kind, found ${got:-0}"
  return 1
}

# `expect_steps_exactly <kind> <n>`
expect_steps_exactly() {
  local kind="$1" want="$2" got
  got="$(steps_of "$kind")"
  if [ "${got:-0}" -eq "$want" ]; then
    return 0
  fi
  echo "expected exactly $want steps of kind $kind, found ${got:-0}"
  return 1
}

# ---------------------------------------------------------------------------
# screen assertions
# ---------------------------------------------------------------------------

# `see_anywhere <text>`: the text is on screen, or comes into view when the transcript is scrolled
# up. The view is put back with End either way.
#
# MERGE (track B -> Phase 5/ux1): the merged tree gives the TUI column a `drafts` pane
# (`tui.drafts`, 30% of the height), so the transcript viewport is SHORTER than it was when several
# of these bullets were written, and a long answer can leave the phrase a bullet names above the
# fold. Scrolling asserts the same thing about the same rows — what is being pinned is that the
# renderer PRODUCED them, never that a particular viewport happens to hold them all.
see_anywhere() {
  local text="$1" i
  for i in $(seq 1 12); do
    if shell-use text | grep -qF -- "$text"; then
      shell-use press End >/dev/null
      sleep 0.2
      return 0
    fi
    shell-use press PageUp >/dev/null
    sleep 0.3
  done
  shell-use press End >/dev/null
  echo "'$text' is not on screen, and scrolling up never brought it into view"
  return 1
}

# `see <text> [flags…]`: the text is on screen.
#
# `--no-strict` is deliberate and load-bearing. `shell-use expect text` defaults to STRICT, which
# fails when a phrase matches more than one region — and in this TUI that is the normal case, not
# an anomaly: the rail's about-line, the focus pane's turn and the wake-end summary legitimately
# quote the same words, so an answer landing correctly in two places was being reported as a
# failure. These bullets assert PRESENCE; `expect_absent` asserts the other direction.
see() {
  shell-use expect text --no-strict "$@"
}

# `tui_start_recording_exit <file> [patch…]`: start the binary and have the PTY's own shell write
# its exit status to <file>.
#
# Neither `shell-use get exit-code` nor a submitted `echo exit=$?` can answer this. The first
# tracks the commands the emulator recognises and reported 0 while the binary was still running;
# the second is expanded by the shell that BUILDS the string, whose `$?` is its own. Only the
# shell that actually ran the binary knows, so it is the one asked.
tui_start_recording_exit() {
  local file="$1"; shift
  rm -f "$file"
  shell-use submit "$BOUGH_BIN $(bough_patch_args "$@"); echo \$? > $file" >/dev/null
  wait_for "$BOOT_MARK" 20000
}

# `await_exit_code <file> <code>`: the recorded status arrives and is <code>. Teardown is awaited
# (the launcher shuts the tree down before it exits), so this polls rather than reads once.
await_exit_code() {
  local file="$1" want="$2" i got
  for i in $(seq 1 60); do
    if [ -f "$file" ]; then
      got="$(tr -d '[:space:]' < "$file")"
      if [ "$got" = "$want" ]; then
        return 0
      fi
      echo "the binary exited $got, expected $want"
      return 1
    fi
    sleep 0.5
  done
  echo "the binary never exited"
  return 1
}

# `wheel <x> <y> <up|down> [count]`: a wheel event AT a cell.
#
# `shell-use mouse scroll` always reports the wheel at 1;1 — the top-left cell, which in this TUI
# is the rail. The rail handling it is CORRECT routing (`run.rs::on_mouse` sends the wheel to the
# pane under the pointer and deliberately does not move focus), so a scroll bullet driven that way
# asserts nothing about the trajectory. SGR mouse reports are written directly instead.
wheel() {
  local x="$1" y="$2" dir="$3" count="${4:-1}" btn i
  case "$dir" in
    up) btn=64 ;;
    down) btn=65 ;;
    *) echo "wheel: direction must be up or down (got $dir)"; return 1 ;;
  esac
  for i in $(seq 1 "$count"); do
    shell-use write "$(printf '\033[<%d;%d;%dM' "$btn" "$((x + 1))" "$((y + 1))")" >/dev/null
    # A gap between reports. Written back to back, a burst of SGR sequences intermittently left a
    # partial escape in the parser and the NEXT key (a PageUp) was swallowed with it.
    sleep 0.05
  done
}

# `select_drag <x1> <y1> <x2> <y2>`: press, drag, RELEASE — as raw SGR reports.
#
# The release is the point: `run.rs::on_mouse` copies on `MouseEventKind::Up`, and a drag that is
# never released selects without ever copying. Written raw for the same reason `wheel` is.
select_drag() {
  local x1="$1" y1="$2" x2="$3" y2="$4"
  shell-use write "$(printf '\033[<0;%d;%dM' "$((x1 + 1))" "$((y1 + 1))")" >/dev/null
  shell-use write "$(printf '\033[<32;%d;%dM' "$((x2 + 1))" "$((y2 + 1))")" >/dev/null
  shell-use write "$(printf '\033[<0;%d;%dm' "$((x2 + 1))" "$((y2 + 1))")" >/dev/null
}

# `expect_selected <x> <y> <w> <bg>`: every cell of the run carries the selection background.
#
# Asserted on the BACKGROUND and not by grepping `shell-use cells` for the word "reverse": that
# word is a FIELD NAME in every cell dump, so the grep matched whether or not anything was ever
# selected.
# Polled: the SGR press/drag reports are handed to the event loop and the highlight is painted on
# a LATER frame, so reading the cells once raced the repaint and reported `bg default`.
expect_selected() {
  local i out
  for i in $(seq 1 25); do
    if out="$(_expect_selected_once "$@" 2>&1)"; then
      return 0
    fi
    sleep 0.2
  done
  echo "$out"
  return 1
}

_expect_selected_once() {
  local x="$1" y="$2" w="$3" want="$4"
  shell-use cells "$x" "$y" "$w" 1 --json | WANT="$want" python3 -c '
import json, os, sys
want = os.environ["WANT"]
cells = json.load(sys.stdin)["data"]["cells"]
bad = [c for c in cells if c["bg"] != want]
if bad:
    c = bad[0]
    sys.exit("cell %s,%s has bg %s, expected %s" % (c["x"], c["y"], c["bg"], want))
'
}

# `expect_diff_gutter <line-substring> <fg>`: on the first screen row containing <line-substring>,
# the leading `+`/`-` GUTTER cell carries <fg>, and the cell to its right does NOT.
#
# This is the colour half of V2 done through `shell-use cells` rather than `expect text --fg`,
# because the two halves of a diff line are deliberately coloured from different sources
# (`tui-render/src/diff.rs`): the gutter is the theme's `added`/`removed` ROLE, the body is
# syntect's highlight for the path's extension. `expect text --fg` can only require ONE colour
# across a whole match, so it cannot express — or prove — that split.
expect_diff_gutter() {
  local needle="$1" want="$2" out i
  # Poll like `shell-use expect` does: the click that expands the call and the redraw that shows
  # its body are not the same frame.
  for i in $(seq 1 25); do
    if out="$(shell-use text | NEEDLE="$needle" WANT="$want" _expect_diff_gutter_py 2>&1)"; then
      return 0
    fi
    sleep 0.2
  done
  echo "$out"
  return 1
}

_expect_diff_gutter_py() {
  python3 -c '
import json, os, subprocess, sys
needle, want = os.environ["NEEDLE"], os.environ["WANT"]
rows = sys.stdin.read().split("\n")
for y, row in enumerate(rows):
    if needle in row:
        x = row.index(needle)
        break
else:
    sys.exit(f"no screen row contains {needle!r}")
def fg(x, y):
    out = subprocess.run(["shell-use", "cells", str(x), str(y), "1", "1", "--json"],
                         capture_output=True, text=True, check=True).stdout
    return json.loads(out)["data"]["cells"][0]["fg"]
gutter, body = fg(x, y), fg(x + 1, y)
if gutter != want:
    sys.exit(f"gutter cell at {x},{y} is {gutter}, expected {want}")
if body == want:
    sys.exit(f"body cell at {x+1},{y} is also {body}: the gutter role leaked into the body")
'
}

# `expect_absent <text> [flags…]`: the text is NOT on screen.
expect_absent() {
  shell-use expect text --not --no-strict "$@"
}

# `row_with <needle…>`: some SINGLE screen row carries EVERY needle. `see` cannot answer this —
# it matches the screen, so two needles on two stacked rows satisfy it — and "these things are on
# one line together" is exactly what a rail row, a picker row and a wrapped paragraph are about.
row_with() {
  local i
  for i in $(seq 1 40); do
    if shell-use text | python3 -c '
import sys
needles = sys.argv[1:]
for line in sys.stdin:
    if all(n in line for n in needles):
        sys.exit(0)
sys.exit(1)
' "$@"; then return 0; fi
    sleep 0.5
  done
  echo "no single screen row carries all of: $*"
  shell-use text | tail -30
  return 1
}

# `no_row_is_exactly <text>`: no screen row is that text and nothing else (bar whitespace and any
# leading gutter). The negative half of `row_with`: a paragraph that was joined correctly leaves
# no row holding only its first fragment.
no_row_is_exactly() {
  if shell-use text | python3 -c '
import sys
want = sys.argv[1].strip()
for line in sys.stdin:
    if line.strip().rstrip("\u2502").strip() == want:
        sys.exit(1)
sys.exit(0)
' "$1"; then return 0; fi
  echo "a screen row is exactly, and only: $1"
  return 1
}

# ---------------------------------------------------------------------------
# phase ux1 helpers (§3, WP-8)
# ---------------------------------------------------------------------------

# `cells_have <x> <y> <w> <h> <field> <value> [--all]`: some cell of the region carries
# `<field> == <value>` (`--all`: every cell does). `field` is a `shell-use cells --json` key:
# `fg`, `bg`, `bold`, `char`.
#
# Polled, for the same reason `expect_selected` is: a focus ring, a highlight and a flash are all
# painted on a LATER frame than the key that caused them, and reading the cells once raced the
# repaint. This is the colour half of the phase — a ring or a highlight asserted as TEXT would
# pass on a screen that draws neither.
cells_have() {
  local i out
  for i in $(seq 1 25); do
    if out="$(_cells_have_once "$@" 2>&1)"; then
      return 0
    fi
    sleep 0.2
  done
  echo "$out"
  return 1
}

_cells_have_once() {
  local x="$1" y="$2" w="$3" h="$4" field="$5" want="$6" mode="${7:-any}"
  shell-use cells "$x" "$y" "$w" "$h" --json \
    | FIELD="$field" WANT="$want" MODE="$mode" python3 -c '
import json, os, sys
field, want, mode = os.environ["FIELD"], os.environ["WANT"], os.environ["MODE"]
cells = json.load(sys.stdin)["data"]["cells"]
if not cells:
    sys.exit("the region is empty")
def val(c):
    return str(c.get(field))
if mode == "--all":
    bad = [c for c in cells if val(c) != want]
    if bad:
        c = bad[0]
        sys.exit("cell %s,%s has %s=%s, expected %s" % (c["x"], c["y"], field, val(c), want))
else:
    if not any(val(c) == want for c in cells):
        got = sorted({val(c) for c in cells})
        sys.exit("no cell of the region has %s=%s (saw %s)" % (field, want, ", ".join(got)))
'
}

# `t_cells <name> <x> <y> <w> <h> <field> <value> [--all]`: the NAMED colour assertion the
# verification map cites.
t_cells() {
  local name="$1"; shift
  t "$name" cells_have "$@"
}

# `disk_has <path> [needle]`: the file exists on disk (and contains `needle`). Polled: a tool call
# lands on disk a frame or two after the screen says it did.
#
# The plan calls this "the disk assertion, not a string one" (V10): B5 was a file that the screen
# claimed to have written to the current directory and that was not there. Only the filesystem can
# refute that, so only the filesystem is asked.
disk_has() {
  local path="$1" needle="${2:-}" i
  for i in $(seq 1 40); do
    if [ -f "$path" ]; then
      if [ -z "$needle" ] || grep -qF -- "$needle" "$path"; then
        return 0
      fi
    fi
    sleep 0.5
  done
  if [ ! -f "$path" ]; then
    echo "no file at $path"
    echo "# siblings: $(ls -A "$(dirname "$path")" 2>/dev/null | tr '\n' ' ')"
  else
    echo "$path exists but does not contain: $needle"
    sed 's/^/# /' "$path" | head -20
  fi
  return 1
}

t_disk() {
  local name="$1"; shift
  t "$name" disk_has "$@"
}

# `t_size <name> <fn>`: the three-size resize walk (V5). `fn` is called once per size with the
# columns and rows as arguments, at 120x36, then 80x24, then 200x50 — the sizes the audit's
# resize findings (M9, M13, nit 39) were taken at. The terminal is returned to 120x40 afterwards
# whether the walk passed or not, so a later bullet in the same script sees the boot geometry.
#
# It is one NAMED assertion and not three because the claim is about the walk: history that
# re-wraps at every size with nothing injected and nothing lost.
TUI_SIZES="120x36 80x24 200x50"

resize_walk() {
  local fn="$1" spec cols rows out rc=0
  for spec in $TUI_SIZES; do
    cols="${spec%x*}"; rows="${spec#*x}"
    shell-use resize "$cols" "$rows" >/dev/null
    # A resize redraws once; there is no turn to wait out.
    sleep 0.4
    if ! out="$("$fn" "$cols" "$rows" 2>&1)"; then
      echo "at ${cols}x${rows}: $out"
      rc=1
      break
    fi
  done
  shell-use resize 120 40 >/dev/null 2>&1 || true
  sleep 0.4
  return $rc
}

t_size() {
  local name="$1" fn="$2"
  t "$name" resize_walk "$fn"
}

# `no_blank_run <n>`: no run of `n` or more consecutive blank rows inside the drawn frame (nit 39:
# a resize used to inject blank lines between wrapped paragraphs). Trailing blank rows below the
# last content row are the empty transcript, not an injection, so they are trimmed first.
no_blank_run() {
  shell-use text | N="${1:-3}" MARK="${2:-}" python3 -c '
import os, sys
n = int(os.environ["N"])
mark = os.environ.get("MARK") or ""
rows = [r.rstrip() for r in sys.stdin.read().split("\n")]
# Everything below the last CONTENT row is the empty part of the frame — the unfilled transcript,
# the empty search field, the gap above the status line. Only what sits BETWEEN content rows can
# be an injected blank (nit 39), so the tail is trimmed to the last row that carries content.
if mark:
    last = max((i for i, r in enumerate(rows) if mark in r), default=-1)
    rows = rows[: last + 1]
while rows and not rows[-1].strip():
    rows.pop()
run = worst = 0
for r in rows:
    run = run + 1 if not r.strip() else 0
    worst = max(worst, run)
if worst >= n:
    sys.exit("a run of %d blank rows is inside the frame" % worst)
'
}

# `screen_rows`: the drawn screen as one string, trailing blank rows trimmed. Used by the swap
# script to diff two screens row by row.
screen_rows() {
  shell-use text | python3 -c '
import sys
rows = [r.rstrip() for r in sys.stdin.read().split("\n")]
while rows and not rows[-1].strip():
    rows.pop()
print("\n".join(rows))
'
}

# `write_patch <yaml…>` / `clear_patch`: the launcher watches `$BOUGH_HOME/bough.patch.yml` and
# reloads it WHILE the TUI runs (§0.5). The swap scripts of every phase drive that file; this
# phase's swap disables a ROW through it, so the write is here rather than copied into each.
PATCH_FILE="$HOME_DIR/bough.patch.yml"

write_patch() {
  cat > "$PATCH_FILE.tmp"
  mv "$PATCH_FILE.tmp" "$PATCH_FILE"
  sleep 2
}

clear_patch() {
  rm -f "$PATCH_FILE"
  sleep 2
}

# Several scripts drive assertions through `bash -c`, which is a FRESH shell: without these the
# helpers above would silently not exist there, and a comparison against their empty output would
# be reported as a failed bullet rather than as the harness bug it is. The variables the ledger
# helpers close over have to travel too.
export LEDGER HOME_DIR BOUGH_BIN PATCH_FILE TUI_SIZES SQL_BUSY_MS STATUS_PATCH TUI_STATIC_STATUS BOOT_MARK
export -f wait_for wait_any wait_steps see see_anywhere expect_absent row_with no_row_is_exactly wheel select_drag expect_selected _expect_selected_once expect_diff_gutter _expect_diff_gutter_py sql steps_of expect_steps expect_steps_exactly await_exit_code cells_have _cells_have_once disk_has no_blank_run screen_rows write_patch clear_patch resize_walk
