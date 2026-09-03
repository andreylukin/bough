# bough on Terminal-Bench (Harbor)

`bough_go_agent.py` is a Harbor `BaseInstalledAgent` that runs bough headless inside each trial's
container, out-of-tree (`--agent-import-path`). `tb4.sh` wraps the three steps for Terminal-Bench
4.0 on Modal; `summarize.py` turns a jobs directory into a per-trial table with pass rates.

```sh
uv tool install harbor

bench/harbor/tb4.sh build                       # linux/amd64 binary into bench/harbor/dist
bench/harbor/tb4.sh run <job> <k> task [task…]  # k trials per task
bench/harbor/tb4.sh sum <job> [<job>…]          # per-trial table + pass rates
```

Env for `run`: `MODEL` (default `openrouter/openai/gpt-5.6-luna`), `TIMEOUT` (agent seconds,
default 2400), `CONC` (default 4), `CONFIG` (an arm: a `bough.yml` instead of the adapter's
default), `BIN` (an arm binary). Keys come from `~/.bough/env`.

By hand, the same thing:

```sh
export PYTHONPATH=$PWD/bench/harbor
harbor run -d terminal-bench/terminal-bench@4.0.0 --env modal \
  --agent bough_go_agent:BoughGo --model openrouter/openai/gpt-5.6-luna \
  --ak binary=$PWD/bench/harbor/dist/bough-go-linux-amd64 --ak timeout=2400 \
  -i html-js-filter -k 3 --n-concurrent 3 --jobs-dir ~/.cache/bough-tbench/jobs
```

One `bough --headless` process per trial: the task brief goes in as one JSON prompt line, the
loop's events come out as `[kind] text` lines, and the cost row prints `[usage] {...}` after the
turn. The agent phase is capped by `timeout` (TB 4.0's own limit is 8 h), one attempt, with `-c`
continuation available if a second attempt is ever wanted.

Traps: task names are namespaced (`-i` takes `terminal-bench/<task>`, which `tb4.sh` adds for
you); keep `--jobs-dir` under `$HOME` when running locally under Colima, which shares nothing else.
