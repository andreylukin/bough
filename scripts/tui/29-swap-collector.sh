#!/usr/bin/env bash
# SWAP (§17 Phase 6) — a Phase-6 row is DISABLED by a patch file while the binary runs. The
# launcher recomposes, the collector's schedule job goes with its row, the rest of the tree stays
# consistent and keeps answering, and putting the patch back brings the row back. No compile, no
# restart.
#
# The collector has no pane of its own, so "the tree stayed consistent" is asserted the only way it
# honestly can be from outside: the process is the same one, the surface still answers a turn, and
# the composed tree really does carry the row as disabled.
source "$(dirname "$0")/lib.sh"

[ -n "$BOUGH_LIVE" ] && { skip the_collector_row_is_enabled_before_the_patch "the swap gate is composition, not a model"; exit 0; }

USER_PATCH="$HOME_DIR/bough.patch.yml"

# `--dump-config` of the tree the running process would compose, read from outside it.
dump() { BOUGH_HOME="$HOME_DIR" "$BOUGH_BIN" --profile tui --dump-format json --dump-config 2>/dev/null; }

# `row_disabled <id>`: the composed tree says this row is disabled.
# NOTE ON WHAT THESE TWO PROVE. `row_disabled`/`row_enabled` shell out to a SEPARATE
# `bough --dump-config` process reading the same $BOUGH_HOME patch file this script just wrote.
# That asserts THE COMPOSER re-reads the file — Phase-0 behaviour — not that the RUNNING TUI
# process recomposed and dropped the collector's schedule job. The SWAP claim
# ("schedule.jobs() lists no job for it, every other row's fingerprint is unchanged") is carried
# by `crates/bough/tests/phase6_swap.rs`, which asserts the live job table and per-row
# fingerprints. The live evidence in THIS script is `the_surface_still_answers_after_the_swap`
# and `the_process_never_restarted`; the two bullets below are a cheap consistency check beside
# them and must not be read as independent confirmation.
row_disabled() {
  dump | python3 -c '
import json, sys
rows = (json.load(sys.stdin) or {}).get("rows") or []
def walk(rs):
    for r in rs:
        yield r
        yield from walk(r.get("group") or [])
want = sys.argv[1]
hit = [r for r in walk(rows) if r.get("id") == want]
if not hit:
    sys.exit(f"no row `{want}` in the composed tree")
if not hit[0].get("disabled"):
    sys.exit(f"row `{want}` is not disabled")
' "$1"
}

row_enabled() {
  dump | python3 -c '
import json, sys
rows = (json.load(sys.stdin) or {}).get("rows") or []
def walk(rs):
    for r in rs:
        yield r
        yield from walk(r.get("group") or [])
want = sys.argv[1]
hit = [r for r in walk(rows) if r.get("id") == want]
if not hit:
    sys.exit(f"no row `{want}` in the composed tree")
if hit[0].get("disabled"):
    sys.exit(f"row `{want}` is disabled")
' "$1"
}

tui_open
tui_start

t the_tui_is_up_before_the_patch \
  see "sol" --timeout 20000

t the_collector_row_is_enabled_before_the_patch \
  row_enabled collect.github

pid_before="$(pgrep -f "$BOUGH_BIN" | head -1)"

cat > "$USER_PATCH" <<'YML'
entries:
  collect.github:
    disabled: true
YML

# The watch debounces; give the recompose a moment before reading the tree back.
sleep 2

t the_patch_disables_the_collector_row \
  row_disabled collect.github

t the_surface_still_answers_after_the_swap \
  bash -c 'shell-use submit "still there?" >/dev/null; see "the first fragment" --timeout 20000'

t the_process_never_restarted \
  bash -c "[ \"\$(pgrep -f '$BOUGH_BIN' | head -1)\" = \"$pid_before\" ]"

rm -f "$USER_PATCH"
sleep 2
t removing_the_patch_brings_the_collector_back \
  row_enabled collect.github

tui_quit
