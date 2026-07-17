# Shared helpers for the TUI usability probes. Source, don't run.
#
# Every probe drives the REAL bough TUI against the LIVE server via shell-use,
# from a scratch workspace OUTSIDE the repo (live runs must never pollute the
# working tree — the server builds from it). Probe conversations are archived
# on cleanup so they don't linger in the session tree.
set -euo pipefail

PORT="${BOUGH_PORT:-4321}"
API="http://127.0.0.1:$PORT"
SU_SESSION="${PROBE_SESSION:-bough-probe}"
su() { shell-use --session "$SU_SESSION" "$@"; }

now_ms() { python3 -c 'import time; print(int(time.time()*1000))'; }

# Spawn the TUI in a fresh scratch workspace. Sets $WS.
probe_start() {
  curl -sf "$API/sessions" >/dev/null || {
    echo "bough server not reachable on :$PORT — bough start" >&2
    exit 4
  }
  WS="$(mktemp -d "${TMPDIR:-/tmp}/bough-probe.XXXXXX")"
  su run bough --cwd "$WS" --cols 100 --rows 32
  su wait text "›" --timeout 15000
  su wait idle
}

# The bough session id the probe's conversation landed in: newest session whose
# workspace is the probe's scratch dir.
probe_session_id() {
  curl -sf "$API/sessions" | python3 -c "
import json, os, sys
ws = os.path.realpath('$WS')  # server normalizes /var → /private/var on macOS
rows = [
    s for s in json.load(sys.stdin)
    if s.get('workspace') and os.path.realpath(s['workspace']) == ws
]
rows.sort(key=lambda s: s['createdAt'])
print(rows[-1]['id'] if rows else '')
"
}

probe_metrics() { # $1 = session id
  curl -sf "$API/sessions/$1/metrics"
}

# Close the TUI + shell-use session, archive the probe conversation.
probe_cleanup() {
  local sid="${1:-}"
  su close >/dev/null 2>&1 || true
  if [ -n "$sid" ]; then
    curl -sf -X POST "$API/sessions/$sid/archive" >/dev/null 2>&1 || true
  fi
  [ -n "${WS:-}" ] && rm -rf "$WS"
}
