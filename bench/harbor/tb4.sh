#!/usr/bin/env bash
# Terminal-Bench 4.0 on Modal with the Go bough.
#
#   bench/harbor/tb4.sh build                       # linux/amd64 binary into bench/harbor/dist
#   bench/harbor/tb4.sh run <job> <k> task [task…]  # k trials per task, Luna via OpenRouter
#   bench/harbor/tb4.sh sum <job> [<job>…]          # per-trial table + pass rates
#
# Env: MODEL (default openrouter/openai/gpt-5.6-luna), SMALL (llm-small; default = MODEL), EFFORT (reasoning effort),
#      MAXCOST (USD per trial; the loop asks for a final answer past it),
#      JSTOOL=1 (declare the native js(code) function), PROMPT / GUIDANCE (files: the base
#      system prompt / the bench guidance text — the evolve loop's components),
#      DATASET (default terminal-bench/terminal-bench@4.0.0; `terminal-bench@2.0` is the evolve loop's train set),
#      TIMEOUT (agent seconds, default 5400),
#      CONC (default 4), CONFIG (an arm: a bough.yml instead of the adapter's default),
#      BIN (an arm binary; `build` writes to it, default dist/bough-go-linux-amd64).
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
JOBS=${JOBS:-$HOME/.cache/bough-tbench/jobs}
MODEL=${MODEL:-openrouter/openai/gpt-5.6-luna}
DATASET=${DATASET:-terminal-bench/terminal-bench@4.0.0}   # train set: terminal-bench@2.0 (no org prefix)
TIMEOUT=${TIMEOUT:-5400}
CONC=${CONC:-4}
BIN=${BIN:-$ROOT/bench/harbor/dist/bough-go-linux-amd64}
export PATH="$HOME/.local/bin:$PATH"

case "${1:-}" in
  build)
    mkdir -p "$ROOT/bench/harbor/dist"
    (cd "$ROOT/go" && CGO_ENABLED=0 GOOS=linux GOARCH=amd64 go build -o "$BIN" ./cmd/bough)
    ls -la "$BIN"
    ;;
  run)
    job=$2; k=$3; shift 3
    inc=()
    # TB 4.0 task names are namespaced (terminal-bench/<task>); 2.0's are bare.
    ns=""; case "$DATASET" in *@4*) ns="terminal-bench/";; esac
    for t in "$@"; do case "$t" in */*) inc+=(-i "$t");; *) inc+=(-i "$ns$t");; esac; done
    set -a; . "$HOME/.bough/env"; set +a
    ak=(--ak "binary=$BIN" --ak "timeout=$TIMEOUT")
    [ -n "${SMALL:-}" ] && ak+=(--ak "small=$SMALL")
    [ -n "${EFFORT:-}" ] && ak+=(--ak "effort=$EFFORT")
    [ -n "${MAXCOST:-}" ] && ak+=(--ak "max_cost=$MAXCOST")
    [ -n "${JSTOOL:-}" ] && ak+=(--ak "js_tool=$JSTOOL")
    [ -n "${PROMPT:-}" ] && ak+=(--ak "prompt=$PROMPT")
    [ -n "${GUIDANCE:-}" ] && ak+=(--ak "guidance=$GUIDANCE")
    [ -n "${CONFIG:-}" ] && ak+=(--ak "config=$CONFIG")
    mkdir -p "$JOBS"
    PYTHONPATH="$ROOT/bench/harbor" harbor run -d "$DATASET" --env modal \
      --agent bough_go_agent:BoughGo --model "$MODEL" "${ak[@]}" ${inc[@]+"${inc[@]}"} \
      -k "$k" --n-concurrent "$CONC" --jobs-dir "$JOBS" --job-name "$job"
    ;;
  sum)
    shift
    args=()
    for j in "$@"; do args+=("$JOBS/$j"); done
    python3 "$ROOT/bench/harbor/summarize.py" "${args[@]}"
    ;;
  *)
    sed -n 2,10p "$0"; exit 2 ;;
esac
