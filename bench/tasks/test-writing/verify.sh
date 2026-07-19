#!/usr/bin/env bash
# Pass iff slugify.py is untouched, the written tests are green against the real
# implementation, and they FAIL against two mutants (discriminating power).
set -euo pipefail
WS="$1"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cmp -s "$HERE/fixture/slugify.py" "$WS/slugify.py"
[ -f "$WS/test_slugify.py" ]

V="$(mktemp -d "${TMPDIR:-/tmp}/bench-verify.XXXXXX")"
trap 'rm -rf "$V"' EXIT
cp -R "$WS/." "$V/"
cd "$V"

python3 -m unittest -q

# Mutant A: forgets to trim leading/trailing hyphens.
cat > slugify.py <<'EOF'
import re


def slugify(text):
    text = text.lower()
    return re.sub(r"[^a-z0-9]+", "-", text)
EOF
if python3 -m unittest -q 2>/dev/null; then exit 1; fi

# Mutant B: replaces each bad character separately (no run collapsing).
cat > slugify.py <<'EOF'
import re


def slugify(text):
    text = text.lower()
    text = re.sub(r"[^a-z0-9]", "-", text)
    return text.strip("-")
EOF
if python3 -m unittest -q 2>/dev/null; then exit 1; fi

exit 0
