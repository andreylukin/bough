#!/usr/bin/env bash
# Scrollback-as-record dump: capture the full rendered scrollback of a live
# probe/TUI session for the reconstruction test — hand the dump to a fresh
# agent with no other context and ask "what did the agent do, and why?".
# Information the reviewer can't recover from the dump is information the
# rendering is losing (folded tool calls, truncation, missing reasoning).
#
# usage: scrollback-dump.sh [shell-use-session] > dump.txt
cd "$(dirname "$0")"
SU_SESSION="${1:-bough-probe}"
exec shell-use --session "$SU_SESSION" text --full
