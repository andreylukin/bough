#!/bin/sh
# A RECORDING `gh` shim (AGENTS.md: tests never call the real one).
#
# One line per invocation into `$BOUGH_TEST_GH_LOG`, appended with `>>` so a line is never lost to
# a truncating reopen; the whole argv is on the line, which is what carries the action MARKER that
# `crash_reconcile.rs` counts by. Anything printed on stdout becomes the artifact's locator.
if [ -z "$BOUGH_TEST_GH_LOG" ]; then
  echo "gh-shim: BOUGH_TEST_GH_LOG is unset; refusing to act unrecorded" >&2
  exit 64
fi
printf '%s\n' "$*" >> "$BOUGH_TEST_GH_LOG"
echo "https://example.invalid/pr/1"
