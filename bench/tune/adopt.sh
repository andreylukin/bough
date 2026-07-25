#!/usr/bin/env bash
# Adopt a tuned prompt variant into the checked-in default prompt dir.
#
# Copies the variant's section .md files into <root>/src/supervisor/prompt/,
# where promptOverride() reads them in normal operation (see prompt.ts). This
# is the whole adoption step — a reviewable .md diff, no TS-array surgery. Used
# both by hand (after reading bench/tune/report.py) and by nightly.sh's auto-PR.
#
# usage: bench/tune/adopt.sh <variant-name> [target-repo-root]
#   target-repo-root defaults to the repo that contains this script, but
#   nightly.sh points it at a throwaway worktree so the user's checkout is
#   never touched.
set -euo pipefail

VARIANT="${1:?usage: adopt.sh <variant-name> [target-repo-root]}"
TUNE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SRC="$TUNE/variants/$VARIANT"
ROOT="${2:-$(cd "$TUNE/../.." && pwd)}"
DEST="$ROOT/src/supervisor/prompt"

[ -d "$SRC" ] || { echo "adopt: no such variant: $SRC" >&2; exit 1; }
[ -d "$DEST" ] || { echo "adopt: no prompt dir: $DEST" >&2; exit 1; }

# The four override-able sections (must match SECTION_FILES in tune.py and the
# promptOverride calls in prompt.ts). A variant may omit files it didn't change;
# only copy the ones it actually carries.
copied=0
for f in system.md delegation.md delegation-nested.md subagent.md; do
  if [ -f "$SRC/$f" ]; then
    cp "$SRC/$f" "$DEST/$f"
    copied=$((copied + 1))
  fi
done

[ "$copied" -gt 0 ] || { echo "adopt: variant carried no section files" >&2; exit 1; }
echo "adopted $copied section(s) from $VARIANT into $DEST" >&2
