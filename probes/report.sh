#!/usr/bin/env bash
# Metrics report: per-session usability metrics for recent root sessions, from
# GET /sessions/:id/metrics. Run before and after a harness change and diff.
# usage: report.sh [N]   (default 10 most recent)
set -euo pipefail
PORT="${BOUGH_PORT:-4321}"
API="http://127.0.0.1:$PORT"
N="${1:-10}"

curl -sf "$API/sessions" | python3 -c "
import json, sys, urllib.request

sessions = [s for s in json.load(sys.stdin) if s['kind'] == 'root']
sessions.sort(key=lambda s: s['createdAt'], reverse=True)
rows = []
for s in sessions[:$N]:
    with urllib.request.urlopen('$API/sessions/' + s['id'] + '/metrics') as r:
        m = json.load(r)
    fo = m['firstOutput']
    td = m['turnDuration']
    rows.append((
        s['title'][:34],
        m['userTurns'], m['toolCalls'], m['approvalPrompts'],
        m['interrupted'], m['failed'],
        f\"{fo['medianMs']/1000:.1f}s\" if fo else '—',
        f\"{td['medianMs']/1000:.0f}s\" if td else '—',
    ))
hdr = ('session', 'turns', 'tools', 'asks', 'stops', 'fails', 'p50 first-out', 'p50 turn')
w = [max(len(str(r[i])) for r in rows + [hdr]) for i in range(len(hdr))]
for r in [hdr] + rows:
    print('  '.join(str(v).ljust(w[i]) for i, v in enumerate(r)))
"
