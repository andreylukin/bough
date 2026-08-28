#!/usr/bin/env bash
# REQUIREMENTS §17 Phase 8, §16 — THE PLUGIN AUDIT.
#
# Invariant this script holds: **no row of a shipped bundle is load-bearing by accident, and no
# seam with two Providers is secretly welded to one of them.** Four phases, one table:
#
#   A  every shipped profile composes AND boots whole (`--check`: compose, mount, quiesce, assert,
#      tear down). An enabled row that never activates is a boot failure (§0.2), so `--check` IS
#      the audit of a profile.
#   B  every row of the audited bundle disabled ONE AT A TIME, and the result classified by §3.3's
#      rule: `ok` (the tree settled without it) or `pending` (only the rows that DEPEND on it
#      waited, and the launcher named their unmet keys). A `Failed` row, a boot that falls over, a
#      report that never arrives, or a patch the launcher IGNORED is a FAIL — that last one is the
#      case the previous version of this script could not see: a patch naming an id no layer
#      created is reported and skipped, so the "disabled" boot was really the ordinary tree.
#   C  the LEAK half, which no launcher output can answer (D-C6): `cargo test -p bough --test
#      audit_leaks` boots, disables a row through the live recompose path, re-enables it, and
#      asserts every binding and listener count returns to its baseline. Its verdict fills the
#      `leaked` column for every phase-B row.
#   D  every seam with two Providers on this branch, BOOTED under each Provider by a patch that
#      changes the row's `plugin`, and then that seam's suite run under it.
#
# Exit status is non-zero if any row of the table is FAIL. `--json` prints the same table as JSON.
#
# Usage:
#   scripts/audit-plugins.sh [--json] [--bundle <name>] [--phases ABCD] [--no-build] [profile...]
#   scripts/audit-plugins.sh --self-test        # the classification rule, against recorded reports
set -uo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

json=0
bundle="bough-base"
phases="ABCD"
build=1
self_test=0
profiles=()
while [ $# -gt 0 ]; do
  case "$1" in
    --json) json=1 ;;
    --bundle) bundle="$2"; shift ;;
    --bundle=*) bundle="${1#--bundle=}" ;;
    --phases) phases="$2"; shift ;;
    --phases=*) phases="${1#--phases=}" ;;
    --no-build) build=0 ;;
    --self-test) self_test=1 ;;
    -h|--help) sed -n '2,30p' "$0"; exit 0 ;;
    -*) echo "audit-plugins: unknown flag $1" >&2; exit 2 ;;
    *) profiles+=("$1") ;;
  esac
  shift
done

# ---------------------------------------------------------------------------------------------
# the classification rule (§3.3). PURE: it reads a `--check` exit status and its output, and says
# what the table should print. `--self-test` runs it against recorded reports in
# `scripts/fixtures/check-reports/`, so the rule is tested without booting anything.
#
# Echoes: <verdict>|<dependents pending>|<failed>|<detail>
# ---------------------------------------------------------------------------------------------
classify() {
  local rc="$1" file="$2" pending failed detail

  # A patch the launcher IGNORED never disabled anything: the boot that follows is the ordinary
  # tree wearing the disabled row's name, and reporting it as `ok` would be this audit lying.
  if grep -q 'which no layer created' "$file"; then
    echo "FAIL|0|0|the patch named a row no layer created: nothing was disabled"
    return
  fi
  if grep -qE 'panicked at|SIGSEGV|core dumped' "$file"; then
    echo "FAIL|0|0|the launcher panicked"
    return
  fi
  if [ "$rc" -eq 0 ]; then
    echo "ok|0|0|the tree settled without it"
    return
  fi
  pending="$(grep -cE '^  [^ ].* is Pending; unmet: ' "$file")"
  failed="$(grep -cE '^  [^ ].* is Failed' "$file")"
  detail="$(grep -E '^  [^ ].* is Failed' "$file" | head -1 | sed 's/^  //')"
  if [ "${failed:-0}" -gt 0 ]; then
    echo "FAIL|${pending:-0}|${failed}|${detail:-a row FAILED}"
    return
  fi
  if [ "${pending:-0}" -gt 0 ]; then
    echo "pending|${pending}|0|${pending} dependent row(s) waiting on a key it provided"
    return
  fi
  echo "FAIL|0|0|exit $rc with no unresolved-row report: $(grep -E '^bough:' "$file" | head -1)"
}

if [ "$self_test" -eq 1 ]; then
  # Every fixture is named `<case>.rc<status>.<expected verdict>.txt`, so the expectation is in
  # the corpus rather than in a second list that can drift from it.
  bad=0
  for f in scripts/fixtures/check-reports/*.txt; do
    base="$(basename "$f")"
    rc="$(printf '%s' "$base" | sed -E 's/.*\.rc([0-9]+)\..*/\1/')"
    want="$(printf '%s' "$base" | sed -E 's/.*\.rc[0-9]+\.([a-zA-Z]+)\.txt/\1/')"
    got="$(classify "$rc" "$f" | cut -d'|' -f1)"
    if [ "$got" = "$want" ]; then
      echo "ok - classify $base -> $want"
    else
      echo "not ok - classify $base -> $got, expected $want"
      bad=1
    fi
  done
  exit "$bad"
fi

if [ ${#profiles[@]} -eq 0 ]; then
  for f in profiles/*.yml; do profiles+=("$(basename "$f" .yml)"); done
fi

if [ "$build" -eq 1 ]; then
  cargo build --quiet -p bough || { echo "audit-plugins: the workspace does not build" >&2; exit 2; }
fi
bin="${CARGO_TARGET_DIR:-$root/target}/debug/bough"
[ -x "$bin" ] || { echo "audit-plugins: no binary at $bin (drop --no-build)" >&2; exit 2; }

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

fail=0
# The table, accumulated as `phase|subject|disabled|pending|failed|leaked|verdict|detail` and
# printed once at the end, so `--json` and the text table are the same data.
rows=()
record() { rows+=("$1|$2|$3|$4|$5|$6|$7|$8"); case "$7" in FAIL) fail=1 ;; esac; }

has_phase() { case "$phases" in *"$1"*) return 0 ;; *) return 1 ;; esac; }

# ---- A: every shipped profile composes and boots whole. --------------------------------------
if has_phase A; then
  for p in "${profiles[@]}"; do
    if ! out="$("$bin" --profile "$p" --dump-config 2>&1 >/dev/null)"; then
      record A "profile:$p" "-" 0 0 "-" FAIL "does not compose: $(printf '%s' "$out" | tail -1)"
      continue
    fi
    home="$work/home-$p"; mkdir -p "$home"
    BOUGH_HOME="$home" "$bin" --profile "$p" --check --no-watch >"$work/out" 2>&1
    rc=$?
    IFS='|' read -r verdict pending failed detail <<<"$(classify "$rc" "$work/out")"
    if [ "$verdict" = "ok" ]; then
      record A "profile:$p" "-" 0 0 "-" ok "composes and boots"
    else
      # A WHOLE profile is not allowed to leave rows pending: every enabled row must activate.
      record A "profile:$p" "-" "$pending" "$failed" "-" FAIL "$detail"
    fi
  done
fi

# ---- C first (its verdict is a COLUMN of B): the in-process leak assertion. -------------------
leak_col="-"
if has_phase C; then
  if cargo test --quiet -p bough --test audit_leaks >"$work/leaks" 2>&1; then
    leak_col="no"
    record C "audit_leaks" "-" 0 0 "no" ok "binding and listener counts return to baseline"
  else
    leak_col="?"
    record C "audit_leaks" "-" 0 0 "?" FAIL "$(grep -E '^(test |thread |assertion)' "$work/leaks" | head -1)"
  fi
fi

# ---- B: every row of the bundle, taken out one at a time. ------------------------------------
#
# The subject profile is the one that carries the audited bundle: `headless` for `bough-base`
# (the profile every integration gate boots), `tui` for the terminal bundle.
if has_phase B; then
  case "$bundle" in
    bough-tui-app) subject_profile="tui" ;;
    *) subject_profile="headless" ;;
  esac
  ids="$(grep -E '^- id: ' "bundles/$bundle.yml" | sed 's/^- id: //')"
  for id in $ids; do
    patch="$work/disable-$id.yml"
    printf 'entries:\n  %s:\n    disabled: true\n' "$id" > "$patch"
    home="$work/home-off-$id"; mkdir -p "$home"
    BOUGH_HOME="$home" "$bin" --profile "$subject_profile" --check --no-watch --patch "$patch" \
      >"$work/out" 2>&1
    rc=$?
    IFS='|' read -r verdict pending failed detail <<<"$(classify "$rc" "$work/out")"
    record B "$bundle" "$id" "$pending" "$failed" "$leak_col" "$verdict" "$detail"
  done
fi

# ---- D: every two-Provider seam, booted under each Provider, with that seam's suite. ----------
#
# One line per seam × provider. The patch is a real boot (`--check` with the provider swapped in
# by a `plugin:` field), and the suite is the seam's own. A seam with ONE provider on this branch
# is printed as a named SKIP with the reason — never as an `ok` it did not earn (§16).
seam_case() {
  # <seam> <provider> <profile> <patch-or-> <suites…>
  local seam="$1" provider="$2" profile="$3" patch="$4"; shift 4
  local home="$work/home-seam-$seam-$provider"; mkdir -p "$home"
  local args=(--profile "$profile" --check --no-watch)
  [ "$patch" != "-" ] && args+=(--patch "$patch")
  BOUGH_HOME="$home" "$bin" "${args[@]}" >"$work/out" 2>&1
  local rc=$?
  IFS='|' read -r verdict pending failed detail <<<"$(classify "$rc" "$work/out")"
  if [ "$verdict" != "ok" ]; then
    record D "seam:$seam" "$provider" "$pending" "$failed" "-" FAIL "boot: $detail"
    return
  fi
  local suite
  for suite in "$@"; do
    if cargo test --quiet -p bough --test "$suite" >"$work/suite" 2>&1; then
      record D "seam:$seam" "$provider" 0 0 "-" ok "boots; --test $suite green"
    else
      record D "seam:$seam" "$provider" 0 0 "-" FAIL \
        "--test $suite: $(grep -E '^(test .* FAILED|thread |assertion)' "$work/suite" | head -1)"
    fi
  done
}

if has_phase D; then
  seam_case ledger ledger-sqlite headless - ledger_swap ledger_invariants
  printf 'entries:\n  ledger:\n    plugin: ledger-memory\n    config: {}\n' > "$work/ledger-memory.yml"
  seam_case ledger ledger-memory headless "$work/ledger-memory.yml" ledger_swap

  seam_case agent_loop agent-loop headless - loop_swap agent_invariants
  seam_case agent_loop agent-loop-scripted headless crates/bough/tests/fixtures/loop-scripted.yml loop_swap

  seam_case rollups rollups-summarizer headless - rollups_swap memory_invariants
  seam_case rollups rollups-none headless scripts/tui/fixtures/rollups-none.patch.yml rollups_swap

  seam_case llm llm-replay headless crates/bough/tests/fixtures/llm-replay.yml agent_scripted
  if [ -n "${BOUGH_LIVE:-}" ]; then
    seam_case llm llm-anthropic headless - agent_scripted
  else
    record D "seam:llm" "llm-anthropic" 0 0 "-" SKIP "live Provider: re-run with BOUGH_LIVE=1 and a key"
  fi

  # Seams whose second Provider does not exist on this branch. Named, with the reason, because a
  # seam silently left out of the sweep is exactly what this audit is for.
  record D "seam:projection" "projection-assembler" 0 0 "-" SKIP \
    "one Provider: projection-probe is a CONSUMER (it contributes sections), not a second Provider"
  record D "seam:tui" "tui-shell" 0 0 "-" SKIP \
    "one Provider: tui-probe registers a PANE into tui, it does not provide the key"
  record D "seam:workers" "worker-spawn" 0 0 "-" SKIP \
    "one Provider: worker-fork is a second WorkerKind on the same Provider seam, not a swap"
  record D "seam:actions" "actions-shim" 0 0 "-" SKIP \
    "one Provider today; the second arrives in Phase 6 (§7)"
fi

# ---- the table. -------------------------------------------------------------------------------
if [ "$json" -eq 1 ]; then
  printf '%s\n' "${rows[@]}" | python3 -c '
import json, sys
out = []
for line in sys.stdin.read().splitlines():
    if not line.strip():
        continue
    phase, subject, disabled, pending, failed, leaked, verdict, detail = line.split("|", 7)
    out.append({"phase": phase, "subject": subject, "disabled": disabled,
                "dependents_pending": int(pending or 0), "failed": int(failed or 0),
                "leaked": leaked, "verdict": verdict, "detail": detail})
json.dump(out, sys.stdout, indent=2)
print()
'
else
  printf '%-2s %-22s %-22s %5s %5s %6s %-8s %s\n' \
    "" "SUBJECT" "DISABLED" "PEND" "FAIL" "LEAKED" "VERDICT" "DETAIL"
  printf '%-2s %-22s %-22s %5s %5s %6s %-8s %s\n' \
    "--" "----------------------" "----------------------" "-----" "-----" "------" "--------" "------"
  for r in "${rows[@]}"; do
    IFS='|' read -r phase subject disabled pending failed leaked verdict detail <<<"$r"
    printf '%-2s %-22s %-22s %5s %5s %6s %-8s %s\n' \
      "$phase" "$subject" "$disabled" "$pending" "$failed" "$leaked" "$verdict" "$detail"
  done
fi

if [ "$fail" -ne 0 ]; then
  echo
  echo "audit-plugins: FAILURES above."
fi
exit "$fail"
