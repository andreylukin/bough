#!/usr/bin/env bash
# Pass iff Account.balance → current_balance (every caller), with the unrelated
# "balance" decoys (Report class, dict key, string, comments) untouched and tests
# green. This task ISOLATES semantic rename: a text replace (sed/global edit) that
# hits every "balance" breaks the Report decoy, so test_report_decoy_unchanged
# fails — lsp.rename (or a careful symbol-scoped edit) is the way through.
set -euo pipefail
WS="$1"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# Tests are the spec — must be byte-identical (no gaming the decoy guard).
cmp -s "$HERE/fixture/test_bank.py" "$WS/test_bank.py"
cd "$WS"
# The method was renamed on Account…
grep -q "def current_balance" models.py
! grep -qn "def balance" models.py
# …and no caller still invokes the old method name.
! grep -rn "\.balance()" --include='*.py' .
# Decoys intact (also enforced by the byte-identical test suite below).
grep -q '"balance": 0' models.py
grep -q 'f"balance:' models.py
python3 -m unittest -q
