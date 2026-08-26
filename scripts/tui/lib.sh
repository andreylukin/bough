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

# The arguments this half of the suite boots with. The live half omits the replay patch so the
# real `llm.anthropic` row answers. An argument beginning with `-` is passed to the binary
# VERBATIM (that is how `--root` reaches it); anything else is a patch file.
bough_patch_args() {
  local args=""
  if [ -z "$BOUGH_LIVE" ] && [ -n "$BOUGH_PATCH" ]; then
    args="--patch $BOUGH_PATCH"
  fi
  args="$args --patch $OLD_FEED_PATCH"
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
  shell-use wait idle --timeout 5000 >/dev/null
}

# Start the binary in the open session. Extra arguments become additional `--patch` layers.
tui_start() {
  shell-use submit "$BOUGH_BIN $(bough_patch_args "$@")" >/dev/null
  # The strip pane is the first thing on screen in every profile that has one.
  shell-use wait idle --timeout 20000 >/dev/null
}

# Ask the running TUI to quit, then wait for the shell prompt back.
tui_quit() {
  shell-use submit "/quit" >/dev/null 2>&1 || true
  shell-use wait idle --timeout 10000 >/dev/null 2>&1 || true
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
sql() {
  [ -f "$LEDGER" ] || { echo ""; return 0; }
  sqlite3 "$LEDGER" "$1"
}

# How many steps of a kind the ledger holds.
steps_of() {
  local n
  n="$(sql "select count(*) from steps where type = '$1';")"
  echo "${n:-0}"
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
  shell-use wait idle --timeout 20000 >/dev/null
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
expect_selected() {
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

# Several scripts drive assertions through `bash -c`, which is a FRESH shell: without these the
# helpers above would silently not exist there, and a comparison against their empty output would
# be reported as a failed bullet rather than as the harness bug it is. The variables the ledger
# helpers close over have to travel too.
export LEDGER HOME_DIR BOUGH_BIN
export -f see expect_absent wheel select_drag expect_selected expect_diff_gutter _expect_diff_gutter_py sql steps_of expect_steps expect_steps_exactly await_exit_code
