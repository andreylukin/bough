# bough on Terminal-Bench

Terminal-Bench 2.x runs on [Harbor](https://harborframework.com). This directory
is the adapter that lets Harbor drive bough:
[`bough_agent.py`](bough_agent.py) is a `BaseInstalledAgent`, out-of-tree — Harbor
is not forked or patched.

## Once, on the host

```bash
uv tool install harbor                    # or: pip install harbor
bench/harbor/build-linux-binary.sh        # x86_64; ARCH=aarch64 if you know why
```

The build step is not optional. bough publishes no binaries and a normal install
compiles the workspace, which is minutes — times 89 tasks times 5 trials, inside
throwaway containers. So build one Linux binary here and upload it into each
container instead. It lands in `dist/`, which is gitignored.

**Build for amd64 even on an Apple Silicon Mac.** TB 2.0 tasks pin prebuilt
`alexgshaw/…` images in `task.toml`, and those are amd64-only, so the containers
run emulated and want an x86_64 binary regardless of the host. Getting this
wrong fails at install with `cannot execute: required file not found` — the ELF
interpreter, not the file. Emulated trials are also slow, and at least one task
(`gpt2-codegolf`) fails here even for the `oracle` agent, so a local suite total
on this machine is not comparable to a published x86 score.

**Keep `--jobs-dir` on a path your Docker VM shares with the host.** Harbor bind-
mounts the trial's `verifier/` directory into the container and reads
`reward.txt` back out of it. Under Colima only a whitelist of host paths is
shared (`$HOME` by default), and a mount outside it silently succeeds while
writing into the VM — the tests run, the reward is written, the host never sees
it, and *every* trial fails with `RewardFileNotFoundError`, the `oracle` agent
included. If oracle scores 0, suspect the mount before suspecting the tasks.

## Run

```bash
export ANTHROPIC_API_KEY=…
export PYTHONPATH=$PWD                    # Harbor imports the agent by module path

harbor run \
  --dataset terminal-bench@2.0 \
  --agent bench.harbor.bough_agent:Bough \
  --model claude-opus-4-1 \
  --ak binary=$PWD/bench/harbor/dist/bough-linux-x86_64 \
  --jobs-dir ~/.cache/bough-tbench/jobs \
  --n-concurrent 4 \
  -k 5
```

Sanity-check the harness itself first, with no API spend and no bough involved:

```bash
harbor run --dataset terminal-bench@2.0 --agent oracle --n-concurrent 4
```

For real concurrency, `--n-concurrent 100 --env daytona` (needs `DAYTONA_API_KEY`);
local Docker tops out around 4–8.

Leaderboard-valid runs need `-k 5` and `timeout_multiplier=1.0` with no resource
overrides. TB 2.0 (`harbor`) and the legacy TB-Core 0.1.1 (the `tb` CLI, a
different package and a different task set) are not comparable — never mix the
numbers.

## `--ak` options

| key | default | what it does |
| --- | --- | --- |
| `binary` | — | host path to the Linux `bough` to upload — must match the CONTAINER arch. Required unless `source=1`. |
| `source` | `0` | build bough from source inside every container instead. Slow; use for HEAD. |
| `ref` | `main` | git ref for the source build. |
| `port` | `4321` | loopback port for the in-container server. |
| `timeout` | `900` | wall clock for ONE turn, in seconds. |
| `attempts` | `3` | how many turns to spend on a task that keeps getting cut off. |
| `budget` | `attempts × timeout` | wall clock for all attempts together, in seconds. |

### Timeouts, and why there are three of them

A task that times out is not a task the agent got wrong — it is a task it never
finished. In the first suite run half the non-solves were timeouts, so the
numbers said more about the clock than about the model.

Three clocks have to be ordered, outermost first:

1. **Harbor's agent phase** — the task's own `timeout_sec` (900s for most TB 2.0
   tasks, but 1800s and 3600s appear) times `--agent-timeout-multiplier`. When
   this expires Harbor kills the phase and the trial errors with
   `AgentTimeoutError`, losing every attempt's envelope.
2. **`budget`** — all attempts together. Must sit under (1) with room to spare.
   The loop refuses to start an attempt it cannot finish inside the budget.
3. **`timeout`** — one turn. bough stops its own turn when this expires, which
   is the graceful ending: the container keeps the partial work.

So `--ak attempts=3 --ak timeout=900` (budget 2700) wants
`--agent-timeout-multiplier 3.5`, which gives 3150s on a 900s task.

### Retrying a stuck agent

Attempt 2+ only happens when the previous turn did **not** end `done` — a
timeout, an error, an interrupt. A turn that finished on its own terms is left
alone: with the tests hidden, a retry would let the model second-guess correct
work with nothing to tell the two apart.

`bough exec` has no session resume, so each attempt is a fresh conversation in
the same container — the filesystem, not the transcript, is the continuity. The
continuation prompt says so explicitly, or the model starts from scratch and
spends the new budget rediscovering what it already wrote.

## Model ids

Harbor writes `provider/model`; bough routes on the id alone — a bare `claude-…`
is Anthropic, anything containing `/` is OpenRouter, `openai:…` is OpenAI proper.
The adapter strips a leading `anthropic/` so Harbor's usual spelling reaches
Anthropic. Everything else passes through, so `openai/gpt-5` goes to OpenRouter;
say `--model openai:gpt-5` if you meant the Responses API.

## How a trial actually runs

1. `install()` — apt-gets `curl git ripgrep nodejs`, uploads the binary to
   `/installed-agent/bough`, symlinks it onto `PATH`.
2. `run()` starts `bough start` detached and polls `GET /sessions` until it
   answers. `bough exec` is a client and does **not** start a server — the
   auto-start lived in the old bash wrapper and the Rust binary dropped it.
   Getting this wrong makes every trial fail at setup with a connection error.
3. `bough exec --json --port … --timeout … -- "<instruction>"` in the task's
   working directory, with the provider keys in the environment.
4. The one-line envelope becomes Harbor's `AgentContext` — `treeUsage` when
   present, so subagent and workflow tokens are counted — and is written whole
   to `<trial>/agent/bough-exec.json`.

Exit code 1 (the turn errored, was interrupted, or timed out) is a failed task,
not a broken harness, so it does not raise. Only exit 2 — usage error, or the
server unreachable — is reported as an agent failure.

## Two things to watch on a first run

- **bough is not confined.** It runs programs as the container user with full
  authority, which is what you want inside a disposable container and worth
  knowing before pointing it anywhere else.
- **`bough exec` is not interactive.** A question from the agent is auto-declined
  with `[declined a question — bough exec is not interactive: …]` on stderr. If
  tasks fail with that line in the log, the prompt is asking rather than acting;
  that is a bough problem, not an adapter one.
