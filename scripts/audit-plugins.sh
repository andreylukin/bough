#!/usr/bin/env bash
# Invariant: every shipped profile composes AND boots. An enabled row that never activates is a
# boot failure (REQUIREMENTS §0.2), so `--check` is the audit: compose, mount, quiesce, assert,
# tear down, exit.
#
# Usage: scripts/audit-plugins.sh [profile...]   (default: every profiles/*.yml)
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

profiles=("$@")
if [ ${#profiles[@]} -eq 0 ]; then
  for f in profiles/*.yml; do profiles+=("$(basename "$f" .yml)"); done
fi

cargo build --quiet -p bough
bin="${CARGO_TARGET_DIR:-$root/target}/debug/bough"

fail=0
for p in "${profiles[@]}"; do
  printf '== profile %s\n' "$p"
  if ! "$bin" --profile "$p" --dump-config >/dev/null; then
    printf '   FAIL: does not compose\n'; fail=1; continue
  fi
  if ! "$bin" --profile "$p" --check --no-watch; then
    printf '   FAIL: does not boot\n'; fail=1; continue
  fi
  printf '   ok\n'
done
exit "$fail"
