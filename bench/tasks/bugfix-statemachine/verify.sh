#!/usr/bin/env bash
# Pass iff the tests, dispatcher and cli are untouched, the suite is green,
# and the machine matches the documented rules on event sequences the test
# suite does NOT cover -- where the tempting shallow fixes (reorder only the
# reopen pair, or suppress the symptom in the dispatcher) diverge.
# $1 = final workspace.
set -euo pipefail
WS="$1"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cmp -s "$HERE/fixture/test_ticket.py" "$WS/test_ticket.py"
cmp -s "$HERE/fixture/dispatcher.py" "$WS/dispatcher.py"
cmp -s "$HERE/fixture/cli.py" "$WS/cli.py"
cd "$WS"

python3 -m unittest -q

# Escalated tickets go to review on confirm (uncovered by the test suite).
[ "$(python3 cli.py assign escalate resolve confirm approve)" = "$(printf 'open\nescalated\nsolved\nreview\nclosed')" ]

# Ordinary tickets still close directly on confirm.
[ "$(python3 cli.py assign resolve confirm)" = "$(printf 'open\nsolved\nclosed')" ]

# A reopened escalated ticket still reviews on confirm.
[ "$(python3 cli.py assign escalate resolve customer_reply resolve confirm)" = "$(printf 'open\nescalated\nsolved\nopen\nsolved\nreview')" ]

# The covered auto-close sequence, pinned end to end through the CLI.
[ "$(python3 cli.py assign resolve customer_reply resolve customer_reply resolve customer_reply)" = "$(printf 'open\nsolved\nopen\nsolved\nopen\nsolved\nclosed')" ]
