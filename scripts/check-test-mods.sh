#!/usr/bin/env bash
# Every crate's integration tests are ONE target, tests/main.rs (Cargo.toml: autotests = false).
# The price of that is a trap this script closes: a new tests/foo.rs that main.rs does not declare
# is silently NEVER COMPILED OR RUN. Fail the gate instead.
set -euo pipefail
cd "$(dirname "$0")/.."
bad=0
for main in plugins/*/tests/main.rs crates/*/tests/main.rs bench/*/tests/main.rs; do
  [ -f "$main" ] || continue
  dir=$(dirname "$main")
  for f in "$dir"/*.rs; do
    stem=$(basename "$f" .rs)
    [ "$stem" = main ] && continue
    grep -Eq "^mod $stem;" "$main" || { echo "$f is not declared in $main"; bad=1; }
  done
  for d in "$dir"/*/; do
    [ -f "$d/mod.rs" ] || continue
    h=$(basename "$d")
    grep -Eq "^mod $h;" "$main" || { echo "$d/mod.rs is not declared in $main"; bad=1; }
  done
done
for t in plugins/*/tests crates/*/tests bench/*/tests; do
  [ -d "$t" ] || continue
  ls "$t"/*.rs >/dev/null 2>&1 || continue
  [ -f "$t/main.rs" ] || { echo "$t has test files but no main.rs (add one; see AGENTS.md)"; bad=1; }
done
exit $bad
