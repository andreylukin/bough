#!/usr/bin/env bash
# mcp-eval: the programmatic MCP surface (tools.mcp) against the shell
# one (bough mcp call over bash), on the same tasks, model and servers.
# Each task runs headless twice; the table counts code blocks, MCP
# discovery calls (list/tools/search/status, or tools.mcp.search /
# describe), MCP calls, and cost from the run's history. It costs
# tokens and is never part of `go test`.
#
#   scripts/mcp-eval.sh tasks.txt        # one task per line
#   BOUGH=~/.local/bin/bough MODEL=... scripts/mcp-eval.sh tasks.txt
set -euo pipefail
tasks=${1:?usage: mcp-eval.sh tasks.txt}
BOUGH=${BOUGH:-bough}
extra=()
[ -n "${MODEL:-}" ] && extra+=(--set "llm.model=$MODEL")
hist=$HOME/.bough/history
printf '%-6s %-40s %6s %6s %6s %8s %s\n' mode task blocks discov calls cost outcome
while IFS= read -r task; do
  [ -z "$task" ] && continue
  for mode in shell prog; do
    set=()
    [ "$mode" = shell ] && set=(--set mcp.programmatic=false)
    before=$(ls -t "$hist"/*.jsonl 2>/dev/null | head -1)
    out=$(printf '%s\n' "$task" | timeout 600 "$BOUGH" --headless "${extra[@]}" "${set[@]}" 2>&1 || true)
    f=$(ls -t "$hist"/*.jsonl | head -1)
    [ "$f" = "$before" ] && { echo "$mode: no history written"; continue; }
    python3 - "$f" "$mode" "$task" <<'PY'
import json, re, sys
f, mode, task = sys.argv[1:4]
L = [json.loads(x) for x in open(f) if x.strip()]
blocks = sum(1 for l in L if l["kind"] in ("code",))
disc = calls = 0
for l in L:
    if l["kind"] not in ("code", "call"): continue
    d = l.get("data") or {}
    s = str(d.get("text") or d.get("code") or d.get("cmd") or "")
    disc += len(re.findall(r"bough mcp (?:list|tools|search|status)|tools\.mcp\.(?:search|describe)\(|mcp_(?:search|describe)", s))
    calls += len(re.findall(r"bough mcp call|tools\.mcp\.(?!search|describe|call|servers)\w+\.\w+\(|tools\.mcp\.call\(|mcp_call", s))
cost = 0.0
for l in L:
    u = (l.get("data") or {}).get("usage") or {}
    cost += float(u.get("cost_usd") or u.get("cost") or 0)
last = next((str((l.get("data") or {}).get("text", ""))[:60].replace("\n", " ") for l in reversed(L) if l["kind"] == "assistant"), "")
print(f"{mode:<6} {task[:40]:<40} {blocks:6d} {disc:6d} {calls:6d} {cost:8.3f} {last}")
PY
  done
done < "$tasks"
