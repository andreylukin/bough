#!/bin/bash
# PreToolUse guard: this repo is jj-colocated (bough itself drives jj and
# auto-snapshots the working tree). Claude must operate with plain git only —
# jj commands here can move the working copy under the live server's feet,
# and git stash fights jj's index snapshots (push/pop fails messily).
COMMAND=$(jq -r '.tool_input.command // empty')

deny() {
  jq -n --arg reason "$1" '{
    hookSpecificOutput: {
      hookEventName: "PreToolUse",
      permissionDecision: "deny",
      permissionDecisionReason: $reason
    }
  }'
  exit 0
}

# `jj` as a standalone command token (start of line or after ; & | ( ` or space),
# but not paths like `.jj/`.
if echo "$COMMAND" | grep -qE '(^|[;&|(`[:space:]])jj([[:space:]]|$)'; then
  deny "jj is off-limits in this repo: bough owns the jj layer (auto-snapshots the live tree). Use plain git; read-only inspection of jj state must go through git refs (bough/* bookmarks) instead."
fi

if echo "$COMMAND" | grep -qE 'git([[:space:]]+-C[[:space:]]+[^[:space:]]+)?[[:space:]]+stash'; then
  deny "git stash is forbidden in this jj-colocated repo (jj index snapshots make stash push/pop fail messily). Ship changes via a temp worktree instead."
fi

exit 0  # no decision; normal permission flow applies
