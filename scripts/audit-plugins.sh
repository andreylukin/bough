#!/usr/bin/env bash
# REQUIREMENTS §17 Phase 8 — the plugin audit.
#
# Invariant this script holds: **no row of `bundles/bough-base.yml` is load-bearing by accident.**
# For every row, the tree is booted with that row DISABLED by a generated patch layer, and the
# result must be one of two honest outcomes:
#
#   ok        the tree settled with every other row ACTIVE;
#   pending   the rows that DEPEND on the disabled one never activated, and the launcher says so
#             by naming their unmet keys.
#
# A row whose absence makes another row FAIL (rather than wait for a key it declared), or that
# takes the boot down with an unrelated error, is the finding this audit exists to print.
#
# Every profile is booted whole first: an enabled row that never activates is a boot failure
# (§0.2), so `--check` — compose, mount, quiesce, assert, tear down — IS the audit.
#
# Usage: scripts/audit-plugins.sh [profile...]   (default: every profiles/*.yml)
set -uo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

profiles=("$@")
if [ ${#profiles[@]} -eq 0 ]; then
  for f in profiles/*.yml; do profiles+=("$(basename "$f" .yml)"); done
fi

cargo build --quiet -p bough
bin="${CARGO_TARGET_DIR:-$root/target}/debug/bough"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

fail=0
printf '%-28s %-10s %s\n' "SUBJECT" "RESULT" "DETAIL"
printf '%-28s %-10s %s\n' "----------------------------" "----------" "------"

row() { printf '%-28s %-10s %s\n' "$1" "$2" "$3"; }

# --- every profile composes AND boots whole. --------------------------------------------------
for p in "${profiles[@]}"; do
  if ! out="$("$bin" --profile "$p" --dump-config 2>&1 >/dev/null)"; then
    row "profile:$p" FAIL "does not compose"; fail=1; continue
  fi
  mkdir -p "$work/home-$p"
  if ! out="$(BOUGH_HOME="$work/home-$p" "$bin" --profile "$p" --check --no-watch 2>&1)"; then
    row "profile:$p" FAIL "does not boot: $(printf '%s' "$out" | tail -1)"; fail=1; continue
  fi
  row "profile:$p" ok "composes and boots"
done

# --- and every base row can be taken out. ------------------------------------------------------
#
# The headless profile is the subject: it is the one every integration gate boots, and it carries
# the base bundle without the terminal on top.
ids="$(grep -E '^- id: ' bundles/bough-base.yml | sed 's/^- id: //')"
for id in $ids; do
  patch="$work/disable-$id.yml"
  printf 'entries:\n  %s:\n    disabled: true\n' "$id" > "$patch"
  home="$work/home-off-$id"
  mkdir -p "$home"
  out="$(BOUGH_HOME="$home" "$bin" --profile headless --check --no-watch --patch "$patch" 2>&1)"
  rc=$?
  if [ $rc -eq 0 ]; then
    row "off:$id" ok "the tree settled without it"
    continue
  fi
  # The launcher prints one line per row that never activated, with its unmet keys. A dependent
  # WAITING on a key the disabled row provided is the honest outcome; anything else is a finding.
  if printf '%s' "$out" | grep -q 'unmet: [^-]'; then
    n="$(printf '%s' "$out" | grep -c 'unmet: [^-]')"
    row "off:$id" pending "$n dependent row(s) waiting on a key it provided"
    continue
  fi
  row "off:$id" FAIL "$(printf '%s' "$out" | grep -E '^bough:|is Failed' | head -1)"
  fail=1
done

if [ "$fail" -ne 0 ]; then
  echo
  echo "audit-plugins: FAILURES above."
fi
exit "$fail"
