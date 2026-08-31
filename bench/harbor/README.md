# bough-next on Terminal-Bench (Harbor)

`bough_agent.py` is a Harbor `BaseInstalledAgent` for the REBUILD, out-of-tree
(`--agent-import-path`). Simpler than the daily driver's adapter: `bough exec` is in-process
(no server to boot), and the ledger under `$BOUGH_HOME` carries a lane across attempts, so a
turn the clock cut off is continued, not restarted.

```sh
uv tool install harbor
ARCH=x86_64 bench/pier/build-linux-binary.sh      # TB 2.0 images are amd64; Rosetta runs them

# the harness alone, no bough, no API spend
harbor run --dataset terminal-bench@2.0 --agent oracle --include-task-name chess-best-move \
  --jobs-dir ~/.cache/bough-tbench/jobs -y

# bough on one task
export ANTHROPIC_API_KEY=… PYTHONPATH=$PWD/bench/harbor
harbor run --dataset terminal-bench@2.0 \
  --agent-import-path bough_agent:Bough \
  --model anthropic/claude-haiku-4-5-20251001 \
  --ak binary=$PWD/bench/pier/dist/bough-linux-x86_64 \
  --include-task-name chess-best-move \
  --jobs-dir ~/.cache/bough-tbench/jobs -y

# the suite, leaderboard-shaped
harbor run --dataset terminal-bench@2.0 --agent-import-path bough_agent:Bough \
  --model anthropic/claude-sonnet-5 --ak binary=… -k 5 --n-concurrent 4 \
  --jobs-dir ~/.cache/bough-tbench/jobs
```

`--ak` keys: `binary` (required), `timeout` (one turn, s; 1800), `attempts` (2), `budget`,
`cap` (Harbor's phase cap when the task cache cannot be read), `patch` (an ARM: a patch file
over the bundles — projection knobs, rollups, prompts). The model goes in through
`model.policy` as a second patch, with prices for the models `_PRICES` knows.

Per trial, next to Harbor's own logs: `bough-exec.json` (every attempt's envelope: the answer
wake's steps), `ledger.db`, `requests/` (every request the model was sent, verbatim), and
`patch-N.yml`. Tokens and cost come from the ledger's `usage/round` steps, so a turn the clock
killed still counts.

Traps (from the Terminal-Bench campaign on the daily driver): keep `--jobs-dir` under `$HOME`
(Colima shares nothing else — every trial reads `RewardFileNotFound`, oracle included); match
the CONTAINER's arch, not the host's; the three clocks (Harbor's cap → budget → turn) are ordered
so the phase is never shot mid-attempt; `bench/*` is a cargo workspace glob, which is why this
directory is in the workspace's `exclude`.
