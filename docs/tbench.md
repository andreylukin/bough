# terminal-bench as the transcript iteration bed (2026-08-29)

Andrey's ask: pick a SPECIFIC terminal-bench task and use it to iterate on and improve the bough
transcript. The loop below is built and proven end to end (task `analyze-access-logs`, live
`openai:gpt-5.6-luna`, resolved 3/3, full transcript recovered on the host).

## The pieces

- **`scripts/tbench/bough_agent.py`** — bough as a terminal-bench "installed agent"
  (`AbstractInstalledAgent`): copies a LINUX bough binary into the task container, installs it,
  and runs `BOUGH_HOME=/agent-logs/bough-home bough exec <instruction>`. `/agent-logs` is the
  harness's host-mounted per-trial directory, so the ENTIRE transcript survives the run on the
  host: `ledger.db` (every step: program calls, results, reasoning, usage per round) and
  `requests/*.md` (every request VERBATIM, one file per projection digest, one `## Round` per
  request — the request-recorder row, which ships enabled).
- **`scripts/tbench/bough-setup.sh`** — the in-container install: `install` the copied binary,
  write the model into the run home's own `bough.patch.yml` (`$BOUGH_MODEL`, default Luna), so
  nothing in the checkout changes per run.

## One-time setup

```sh
uv tool install terminal-bench        # the `tb` CLI (bypass the ASI index: env -u UV_INDEX_URL \
                                      #   -u UV_DEFAULT_INDEX -u PIP_INDEX_URL -u UV_KEYRING_PROVIDER)
git clone --depth 1 https://github.com/laude-institute/terminal-bench ~/repos/terminal-bench
# the linux binary (task containers are debian/ubuntu; ~3 min, cached cargo volume):
docker run --rm -v "$PWD":/src -v bough-cargo-cache:/usr/local/cargo/registry -w /src \
  rust:1.90-bookworm bash -c "apt-get update -qq && apt-get install -y -qq pkg-config libssl-dev clang >/dev/null \
  && CARGO_TARGET_DIR=/src/target-linux cargo build --release -p bough"
```

## The loop

```sh
set -a; . ~/.bough/env; set +a
PYTHONPATH=$PWD/scripts/tbench tb run \
  --dataset-path ~/repos/terminal-bench/original-tasks \
  --task-id <task> \
  --agent-import-path bough_agent:BoughAgent \
  --output-path ~/tbench-runs          # MUST be under /Users: this machine's docker (Rancher)
                                       # silently drops bind mounts of /tmp paths
```

Then, per iteration:

1. **Score**: the run prints Resolved/Unresolved; `results.json` has the per-test detail.
2. **Read the transcript**: `~/tbench-runs/<run>/<task>/<trial>/agent-logs/bough-home/` —
   `requests/*.md` is what the model SAW (identity, boundary, skills, the volatile tail per
   round); `ledger.db` is what it DID (`select type, body from steps order by seq`); the trial's
   `panes/post-agent.txt` is what the terminal showed.
3. **Change ONE thing.** The knobs, all reachable without a rebuild:
   - the standing text: `boundary-instructions`, the agent's `about`/persona sections, tool
     descriptions — patch layers or bundle edits (a rebuild embeds bundle edits; a `--patch`
     needs none);
   - the model: `-k model_name=<id>` (the agent kwarg) or `BOUGH_MODEL`;
   - the surface: `bundles/bough-typed.yml` as an extra patch for the typed-tools arm;
   - config: budgets, `tools.operator` limits, effort.
     For patch-layer knobs, add the layer to `bough-setup.sh`'s written patch (it is the run
     home's user patch; config REPLACES per row, restate what you keep).
4. **Re-run and diff**: two runs' `requests/*.md` diff cleanly because the stable tier is
   byte-stable; what moved is the volatile tail and the rounds. `steps`, `usage/round` counts and
   the wake shape in the ledger are the objective deltas.
5. When a transcript is worth pinning, `llm-replay` can replay it offline — the recorded chunks
   are exactly the `Round`/`RecordedChunk` shapes the fixtures use.

## First result (the baseline to beat)

`analyze-access-logs`, Luna, code-mode arm: resolved, 5 model rounds, 5 program calls,
~16 s of agent time, transcript 2.6k lines across 5 request files. Observed knob-worthy detail:
every round re-sent the tool list and the identity block unchanged (cache-read, so cheap), and
round 1 spent a call on `ls`-style orientation the instruction already implied.

## Gotchas found while wiring it

- Rancher Desktop only shares `/Users`: a `/tmp` output path makes `/agent-logs` a black hole
  with no error anywhere. Anything mount-shaped in tb must live under `/Users`.
- `tb`'s installed-agent env vars are exported in the tmux session; the run command carries
  `BOUGH_HOME` inline anyway so the transcript's location does not depend on shell state.
- The task instruction reaches bough as one `exec` message; `run-tests.sh` runs AFTER the agent
  exits, so bough must leave the world converged rather than report intentions.
