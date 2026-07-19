# Shared helpers for the harness bench. Source, don't run.
set -euo pipefail

BENCH="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
STATE="$BENCH/state"
RESULTS="$BENCH/results"
PORT="${BENCH_PORT:-4599}"
API="http://127.0.0.1:$PORT"
MODEL_CC="${BENCH_MODEL_CC:-claude-haiku-4-5-20251001}"
MODEL_BOUGH="${BENCH_MODEL_BOUGH:-claude-haiku-4-5}"
TRIAL_TIMEOUT="${BENCH_TRIAL_TIMEOUT:-900}"

now_ms() { python3 -c 'import time; print(int(time.time()*1000))'; }

# Stage a fresh copy of a task fixture as a committed git repo; prints the dir.
stage_fixture() { # $1 = task
  local work
  work="$(mktemp -d "${TMPDIR:-/tmp}/bench-$1.XXXXXX")"
  cp -R "$BENCH/tasks/$1/fixture/." "$work/"
  git -C "$work" init -q -b main
  git -C "$work" -c user.email=bench@local -c user.name=bench add -A
  git -C "$work" -c user.email=bench@local -c user.name=bench commit -qm fixture
  echo "$work"
}

# Run the task's oracle against a final workspace; prints 1 (pass) or 0.
verify_task() { # $1 = task, $2 = workspace
  if bash "$BENCH/tasks/$1/verify.sh" "$2" >/dev/null 2>&1; then echo 1; else echo 0; fi
}

# Classify a failed trial: re-run the oracle under -x and bucket the first
# failing assertion. Prints one taxonomy tag (harness-vs-model diagnosis lives
# on top of these): timeout | protected-file-modified | tests-fail |
# missing-file | mutant-not-caught | output-mismatch | content-check | unclassified
fail_reason() { # $1 = task, $2 = workspace, $3 = runner status
  if [ "$3" = "timeout" ]; then echo timeout; return; fi
  local last
  # Last traced command, skipping EXIT-trap cleanup that runs after the failure.
  # PS4 sentinel: unittest assertion diffs also emit ^+ lines on stderr, which
  # would masquerade as trace lines without it.
  last="$(PS4='+BENCHX ' bash -x "$BENCH/tasks/$1/verify.sh" "$2" 2>&1 >/dev/null | grep '^+*BENCHX ' | grep -Ev 'BENCHX (rm -rf|trap )' | tail -1 || true)"
  case "$last" in
    *"cmp -s"*)        echo protected-file-modified ;;
    *unittest*)        echo tests-fail ;;
    *"exit 1"*)        echo mutant-not-caught ;;
    *"'[' -f"*)        echo missing-file ;;
    *"'['"*)           echo output-mismatch ;;
    *grep*)            echo content-check ;;
    *)                 echo unclassified ;;
  esac
}
