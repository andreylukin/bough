# bough-next on DeepSWE — and DeepSWE as the projection's tuning bench

Andrey, 2026-08-28: "figure out how we would set up bough next to run and work on deep-swe
benchmark tasks … I want to use DeepSWE as a tool to tune the projection for context."

## 1. What DeepSWE is

Not Agentica's RL-trained "DeepSWE" model. This is **datacurve-ai/deep-swe** (arXiv 2607.07946,
June 2026): 113 original, long-horizon engineering tasks across 91 active repos in TypeScript, Go,
Python, JavaScript and Rust. Tasks are written from scratch and never merged upstream (no
pretraining contamination); each is graded by a hand-written functional verifier that accepts any
correct implementation. Prompts are half the length of SWE-Bench Pro's; reference solutions touch
5.5× more code. Frontier agents score 0.2 %–19 % pass@1. Long-horizon is the point — and long
horizon is where bough's projection (tiers, pins, digest, tail) either carries the work or loses it.

Facts that shape the setup (read from the repo):

| | |
|---|---|
| task format | Harbor: `task.toml`, `instruction.md`, `environment/Dockerfile`, `tests/test.sh` + `grader.py`, `solution/` |
| runner | **Pier** (Harbor fork by datacurve; `uv tool install datacurve-pier`), `pier run -p tasks/<id> --agent … --env docker\|modal` |
| agent network | `network_mode = "no-network"` — the agent runs INSIDE the sandbox; Pier grants a per-agent **network allowlist** (`api.anthropic.com`) |
| agent timeout | `[agent] timeout_sec = 10800` (3 h) per task; `override_timeout_sec` in a job config |
| images | prebuilt **amd64** on `public.ecr.aws/d3j8x8q7/swe-bench-202605:…`; `force_build: true` rebuilds natively from `environment/Dockerfile` (arm64 works — the DGX Spark path) |
| grading | verifier runs after the agent; collects `git diff <base> HEAD` from `/app` as `model.patch` — the agent only has to leave the edit in the working tree |
| resources | 2 cpus, 8 GB, 20 GB per trial |
| trajectories | Pier's ATIF v1.7 (`peak_context_tokens`, `summarization_count`, `llm_call_count`), `pier view`, `pier critique run` |

## 2. What bough already has, and what is missing

Have:
- `bough exec "<task>"` — the headless profile, **in-process** (the rebuild dropped the server the
  old adapter had to boot). `exec` "resumes-or-creates the agent" on the ledger under
  `$BOUGH_HOME`: a second `bough exec` in the same home CONTINUES the trajectory, with its tiers,
  pins and digest — continuity the old Terminal-Bench adapter never had.
- Code mode as the default consumer (`bough-codemode`): `run` over bash/read/write/edit.
- Every projection knob is ROW CONFIG, so a tuning arm is a `--patch` file (the `bench/tools/arms/`
  precedent: `codemode.yml` vs `typed.yml` over one task bank).
- `request.recorder`: every request verbatim under `$BOUGH_HOME/requests/<projection_digest>.md`
  — the literal context per round, the thing being tuned.
- The ledger (`ledger.db`): `request/header` (projection digest, tokens, budget), `usage/round`
  (tokens, cost), rollups (tiers, digest), pins, `wake/end.reason`.
- The old adapter at `~/repos/bough/bench/harbor/` (`bough_agent.py`, `build-linux-binary.sh`):
  the timeout plan and the static-OpenSSL bullseye build carry over.

Missing:
1. `bench/pier/bough_agent.py` — a Pier `BaseInstalledAgent` (out-of-tree, `--agent-import-path`).
2. `bench/pier/build-linux-binary.sh` — the rebuild workspace on `rust:1-bullseye`, OpenSSL static
   (reqwest pulls `native-tls`; rusqlite is `bundled`). x86_64 for leaderboard images, aarch64 for
   local `force_build` runs on the Mac.
3. Job configs per arm under `bench/pier/jobs/`.
4. An artifact collector: `ledger.db` + `requests/` + the exec envelope out of `/logs/agent/`.
5. `bench/pier/report.py`: pass@k, cost, rounds, peak context, degraded rounds per arm.

## 3. Setup

```sh
# once
brew install colima docker uv            # Colima shares only $HOME — keep jobs/ under it
uv tool install "datacurve-pier @ git+https://github.com/datacurve-ai/pier"   # ≥0.2.1 is git-only
git clone https://github.com/datacurve-ai/deep-swe ~/repos/deep-swe

# the binary (Docker build, never a host cross-compile: native-tls + sqlite)
bench/pier/build-linux-binary.sh                 # → bench/pier/dist/bough-linux-x86_64
ARCH=aarch64 bench/pier/build-linux-binary.sh    # for force_build runs on Apple Silicon

# smoke: the harness without bough, no API spend
pier run -p ~/repos/deep-swe/tasks/abs-stepped-slices --agent oracle --env docker
```

Apple Silicon: the prebuilt images are amd64 and run emulated — slow and flaky over a 3-hour task.
Locally use `force_build: true` (native arm64 rebuild from the Dockerfile) with the aarch64 binary;
leaderboard-comparable numbers need amd64 — `--env modal` (Pier supports it; parallel sandboxes)
or a Linux x86 box.

## 4. The agent adapter (`bench/pier/bough_agent.py`)

```python
class Bough(BaseInstalledAgent):
    name = "bough"
    def install_spec(self):            # apt: git ripgrep curl; mkdir /installed-agent/home
    async def install(self, env):      # + env.upload_file(binary, "/installed-agent/bough"); chmod; `bough --check`
    def network_allowlist(self):       # NetworkAllowlist(domains=["api.anthropic.com"])
    async def run(self, instruction, environment, context):
        # 1. write /installed-agent/bough.patch.yml from kwargs["patch"] (the ARM) + model.policy
        # 2. loop attempts under the task cap (old adapter's plan: budget = cap − 60 s;
        #    one turn per attempt; a turn that ended `done` is not retried):
        #      cd /app && BOUGH_HOME=/installed-agent/home ANTHROPIC_API_KEY=… \
        #        timeout <turn> bough exec --patch /installed-agent/bough.patch.yml \
        #          --print json "<instruction + continuation note>"  > /logs/agent/exec-N.json
        #    continuity is the LEDGER: attempt 2 reads the same home and the same lane/sol,
        #    so the projection carries the tiers and pins of attempt 1.
        # 3. collect: download_dir(/installed-agent/home/requests, …), download_file(ledger.db)
        # 4. populate_context_post_run: tokens/cost from the envelope (+ ATIF later)
```

Model: `model.policy` maps `sol` to haiku in `bough-base`; the arm patch sets it
(`claude-sonnet-5` / `claude-opus-5`) with prices, so the cost chip and the ledger's
`usage/round` are right. Only Anthropic is mounted today (`llm.anthropic`, `api_key_env`); an
OpenAI row would extend the allowlist by one domain.

Run it:

```sh
export PYTHONPATH=$PWD/bench/pier
pier run -p ~/repos/deep-swe/tasks/abs-stepped-slices \
  --agent-import-path bough_agent:Bough --env docker \
  --agent-kwarg binary=$PWD/bench/pier/dist/bough-linux-x86_64 \
  --agent-kwarg patch=$PWD/bench/pier/arms/baseline.yml \
  --agent-env ANTHROPIC_API_KEY=$ANTHROPIC_API_KEY
```

Then a job config (`bench/pier/jobs/subset-baseline.yaml`): `datasets: [{path, n_tasks: 15,
sample_seed: 0}]`, `agents: [{import_path, kwargs: {binary, patch}, env}]`, `n_attempts: 2`,
`n_concurrent_trials: 4`, `override_timeout_sec: 5400` for iteration (the full cap is 3 h).

## 5. DeepSWE as the projection's tuning bench

The projection is assembled per request by `projection-assembler` from the ledger, in six bands:
identity · pins · digest · tier summaries · recent steps (tail) · mail. Everything that decides
what the model sees is row config:

| row | knob | what it moves |
|---|---|---|
| `projection` | `budget_tokens` (160k), `headroom` (0.6) | when the degradation ladder starts dropping fine tiers → coarse tiers → shrinking the tail |
| `projection` | `tail_steps` (60), `tail_floor_steps` (10) | how much recent work is verbatim vs summarised |
| `projection` | `max_tiers` (3), `mail_newest_n` | how deep the recap goes |
| `rollups` | `prompt_ver` (r4.1), `max_window_steps`, `seal_lag_steps`, `max_block_chars`, `map/reduce_max_tokens` | what a tier summary keeps; how soon a window seals |
| `reconsolidation` | `batch_steps`, `expirable_kinds` | which old evidence expires from the ledger's view |
| `claims` → pins | what becomes a standing pin | the durable memory across a 3-hour task |
| `model.policy` | model per agent | fixed across arms — the bench is a fixed-model A/B |

An ARM is one patch file over `bough-base` (e.g. `arms/long-tail.yml`: `tail_steps: 120`;
`arms/early-seal.yml`: `seal_lag_steps: 5, max_window_steps: 6`; `arms/r5-prompt.yml`: a new
summarizer prompt). The bench's rule from `bench/tools`: every pass predicate is DATA — the
verifier's reward, the ledger's rows — never a model judgement.

Metrics per (task, arm, attempt), all from artifacts already written:

- **reward** (verifier), pass@1 over the subset, pass@2 across attempts;
- **cost** and **tokens per solved task** (`usage/round`);
- **rounds** to done, and `wake/end.reason` (done / timeout / context failure);
- **peak context** and **degraded rounds** — `request/header.tokens` against `budget`, and the
  `> DEGRADED: …` line in the recorded request (`requests/<digest>.md`);
- **recap quality**, offline: for a failed task, the round where the model first acted on stale or
  missing state — read straight from the recorded context, the way the TUI's context view shows
  it live.

Protocol (the fixed-model A/B bank rule; k=1 is noise):

1. **Smoke** — 1 task, `override_timeout_sec: 1800`, baseline arm. Proves the binary, the
   allowlist, the patch, the collector.
2. **Subset** — 15 tasks, `sample_seed: 0`, 2 attempts, baseline. This is the control; keep the
   job dir.
3. **Arms** — the same 15 × 2 under each arm; one knob per arm. Compare with `report.py`.
4. **Long tail** — the tasks that timed out or died on context under baseline are the ones the
   projection is for; rerun only those under the arms that helped.
5. **Prompt loop** — feed the recorded contexts of failed rounds to the AHE prompt-evolution loop
   (`~/repos/bough` bench/tune) for `rollups.prompt_ver`; every candidate is an arm, judged by
   step 3, never by the loop's own critic.

Cost: a frontier model on a DeepSWE task is $2–10 and up to 3 h; 15 tasks × 2 attempts × 3 arms
≈ 90 trials ≈ $300–600 and a weekend on 4 local workers (hours on Modal). The full 113 × 4 for a
leaderboard number is a separate, later decision.

## 6. Traps carried over

- Colima shares only `$HOME`: a `--jobs-dir` outside it makes EVERY trial `RewardFileNotFound`
  (oracle included). If oracle scores 0, suspect the mount.
- Match the container's arch, not the host's: an aarch64 binary in an amd64 image dies with
  "cannot execute: required file not found" (the ELF interpreter).
- Build on bullseye with OpenSSL static: a bookworm build wants glibc 2.32+ and libssl.so.3.
- Three clocks: Pier's cap kills the phase and loses the envelope; keep 60 s back; one long turn
  beats three stunted ones.
- `bough exec` is one wake; the ledger is the continuity, and the continuation prompt must say
  so or the model re-reads the tree from scratch.
- Same home, same lane across attempts means the SECOND attempt's projection is what the arms
  differ on — that is the measurement, not a confound; keep attempts in the same trial.
