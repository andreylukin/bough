#!/usr/bin/env bash
# The replay suite, TUI_JOBS scripts at a time. NOT the gate (`make tui-test-replay`, serial, is)
# but the iteration lane: same scripts, same env, per-script logs, and any script that fails in
# the parallel wave is retried ONCE serially before it is called broken.
#
# Why this is safe by construction: every script owns a unique shell-use session (script name +
# pid; shell-use runs one daemon PER SESSION), its own $BOUGH_HOME subdirectory and PTY cwd, the
# binary binds no ports or sockets, and the one shared input is the read-only replay patch. What
# parallelism CAN do is starve the timing-sensitive bullets (01's streaming window, the drag
# timing), which is what the low default width and the serial retry are for. macOS also has a
# system-wide PTY cap, so the width stays in single digits; lib.sh's EXIT trap closes every
# session, pass or fail, so nothing leaks toward that cap.
set -u
: "${BOUGH_BIN:?scripts/tui/parallel.sh: BOUGH_BIN is not set (run through make)}"
: "${BOUGH_HOME:?scripts/tui/parallel.sh: BOUGH_HOME is not set (run through make)}"
JOBS="${TUI_JOBS:-4}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LOGS="$BOUGH_HOME/logs"
mkdir -p "$LOGS"
export BOUGH_BIN BOUGH_HOME BOUGH_PATCH="${BOUGH_PATCH:-}" BOUGH_LIVE="${BOUGH_LIVE:-}" LOGS

# One argument per invocation, carried as $0: BSD xargs refuses -I{} substitution into a script
# this long ("command line cannot be assembled").
printf '%s\n' "$HERE"/[0-9]*.sh | xargs -P "$JOBS" -n 1 bash -c '
  name="$(basename "$0" .sh)"; log="$LOGS/$name.log"
  if bash "$0" >"$log" 2>&1; then
    echo "ok - $name"
  else
    echo "not ok - $name (parallel; retried below)"
    touch "$LOGS/$name.failed"
  fi
'

# The serial retry: contention is the expected way the wave breaks a script, so a failure is
# only a failure if it repeats alone on a quiet machine.
status=0
for f in "$LOGS"/*.failed; do
  [ -e "$f" ] || break
  name="$(basename "$f" .failed)"
  echo "== retry (serial): $name =="
  if bash "$HERE/$name.sh" >"$LOGS/$name.retry.log" 2>&1; then
    echo "ok - $name (serial retry; parallel run was a contention flake, log: $LOGS/$name.log)"
  else
    echo "not ok - $name (fails alone too)"
    sed 's/^/# /' "$LOGS/$name.retry.log" | tail -40
    status=1
  fi
done

# The code-mode arm of the consumer-parameterised scripts, serial and last: it reuses the
# script's name, so it needs its own home to not race the typed arm's.
echo "== code-mode arm: 31-program =="
if BOUGH_HOME="$BOUGH_HOME-codemode" BOUGH_CONSUMER=codemode \
   bash "$HERE/31-program.sh" >"$LOGS/31-program.codemode.log" 2>&1; then
  echo "ok - 31-program (codemode)"
else
  echo "not ok - 31-program (codemode)"
  sed 's/^/# /' "$LOGS/31-program.codemode.log" | tail -40
  status=1
fi

exit "$status"
