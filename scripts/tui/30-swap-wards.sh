#!/usr/bin/env bash
# SWAP (§17 Phase 6) — the ward HOST row is disabled by a patch while the binary runs, and a ward
# file written into the watched directory is picked up without a restart. Both halves are the same
# claim: runtime code is rows, and a row is config.
#
# The wards the host loads are its CHILD ENTRIES, so "the host went away" is observable in the
# composed tree, and "the file was picked up" is observable as the process still being the same one
# after the directory changed under it.
source "$(dirname "$0")/lib.sh"

[ -n "$BOUGH_LIVE" ] && { skip the_ward_host_row_is_enabled_before_the_patch "the swap gate is composition, not a model"; exit 0; }

USER_PATCH="$HOME_DIR/bough.patch.yml"
WARD_DIR="$HOME_DIR/wards"
mkdir -p "$WARD_DIR"

# The host is pointed at THIS script's scratch directory: the shipped bundle defaults it to the
# developer's real `~/.bough/wards`, and a suite that read those would be neither hermetic nor safe.
WARDS_PATCH="$HOME_DIR/wards.hermetic.yml"
cat > "$WARDS_PATCH" <<YML
entries:
  wards:
    config:
      dir: $WARD_DIR
      glob: "*.rhai"
      watch: true
      debounce_ms: 400
      max_ops: 200000
      max_depth: 32
      max_string_bytes: 65536
      max_array_size: 4096
      eval_timeout_ms: 2000
      max_firings_per_minute: 60
      limits: { max_actions: 16, max_spawns: 2, max_text_bytes: 8192 }
YML

dump() { BOUGH_HOME="$HOME_DIR" "$BOUGH_BIN" --profile tui --patch "$WARDS_PATCH" --dump-format json --dump-config 2>/dev/null; }

row_state() {
  dump | python3 -c '
import json, sys
rows = (json.load(sys.stdin) or {}).get("rows") or []
def walk(rs):
    for r in rs:
        yield r
        yield from walk(r.get("group") or [])
want, state = sys.argv[1], sys.argv[2]
hit = [r for r in walk(rows) if r.get("id") == want]
if not hit:
    sys.exit(f"no row `{want}` in the composed tree")
disabled = bool(hit[0].get("disabled"))
if (state == "disabled") != disabled:
    sys.exit(f"row `{want}` disabled={disabled}, expected {state}")
' "$1" "$2"
}

tui_open
tui_start "$WARDS_PATCH"

t the_tui_is_up_before_the_patch \
  see "sol" --timeout 20000

t the_ward_host_row_is_enabled_before_the_patch \
  row_state wards enabled

pid_before="$(pgrep -f "$BOUGH_BIN" | head -1)"

# A ward file appearing under the watched directory is a hot reload, not a restart.
cat > "$WARD_DIR/nudge.rhai" <<'RHAI'
fn triggers() { [] }
fn ward(event, cx) { [] }
RHAI
sleep 2

t writing_a_ward_file_does_not_restart_the_process \
  bash -c "[ \"\$(pgrep -f '$BOUGH_BIN' | head -1)\" = \"$pid_before\" ]"

t the_surface_still_answers_with_a_ward_loaded \
  bash -c 'shell-use submit "still there?" >/dev/null; shell-use wait idle --timeout 30000 >/dev/null; see "the first fragment" --timeout 20000'

cat > "$USER_PATCH" <<'YML'
entries:
  wards:
    disabled: true
YML
sleep 2

t the_patch_disables_the_ward_host \
  row_state wards disabled

t the_process_still_never_restarted \
  bash -c "[ \"\$(pgrep -f '$BOUGH_BIN' | head -1)\" = \"$pid_before\" ]"

rm -f "$USER_PATCH"
sleep 2
t removing_the_patch_brings_the_ward_host_back \
  row_state wards enabled

tui_quit
