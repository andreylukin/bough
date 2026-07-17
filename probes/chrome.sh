#!/usr/bin/env bash
# Chrome stability probe: the TUI's fixed furniture (prompt char, status-bar
# hints) must stay put across code changes — wrappers and tools parse terminal
# chrome by these markers, and users find their bearings by them.
cd "$(dirname "$0")"
source ./lib.sh

trap 'probe_cleanup' EXIT
probe_start

fail=0
for marker in "›" "? help"; do
  if su expect text "$marker" --no-strict >/dev/null 2>&1; then
    echo "ok: '$marker'"
  else
    echo "MISSING chrome marker: '$marker'" >&2
    fail=1
  fi
done
exit $fail
