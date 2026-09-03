#!/usr/bin/env bash
# Terminal-Bench 4.0 on Modal with the Go bough.
#
#   bench/harbor/tb4.sh build                       # linux/amd64 binary into bench/harbor/dist
#   bench/harbor/tb4.sh run <job> <k> task [task…]  # k trials per task, Luna via OpenRouter
#   bench/harbor/tb4.sh sum <job> [<job>…]          # per-trial table + pass rates
#
# Env: MODEL (default openrouter/openai/gpt-5.6-luna), TIMEOUT (agent seconds, default 5400),
#      CONC (default 4), CONFIG (an arm: a bough.yml instead of the adapter's default),
#      BIN (an arm binary; `build` writes to it, default dist/bough-go-linux-amd64).
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
JOBS=${JOBS:-$HOME/.cache/bough-tbench/jobs}
MODEL=${MODEL:-openrouter/openai/gpt-5.6-luna}
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
    for t in "$@"; do inc+=(-i "terminal-bench/$t"); done
    set -a; . "$HOME/.bough/env"; set +a
    ak=(--ak "binary=$BIN" --ak "timeout=$TIMEOUT")
    [ -n "${CONFIG:-}" ] && ak+=(--ak "config=$CONFIG")
    mkdir -p "$JOBS"
    PYTHONPATH="$ROOT/bench/harbor" harbor run -d terminal-bench/terminal-bench@4.0.0 --env modal \
      --agent bough_go_agent:BoughGo --model "$MODEL" "${ak[@]}" "${inc[@]}" \
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
