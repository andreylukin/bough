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

## The first iteration arc (configure-git-webserver, hard, Luna, n=3 per round)

| round | prompt change | resolved |
| --- | --- | --- |
| baseline | none | **0/3** — wrote a setup script and a README, declared "run this as root on the target server", not registering that it IS root on the target server; treated missing git as a blocker instead of `apt-get install`-ing |
| iter 1 | `skills/operate-the-machine.md`: the machine you are on IS the target; missing tools are installable; deliver live state, not documents; run the task's own acceptance commands | **1/3** — it now acted and even curl-verified, but all three served port 8080 from the `bg` tool, whose jobs die with the bough process; the checker found HTTP 000 |
| iter 2 | appended the lifetime rule: `bg` jobs die with you; anything that must outlive you is a real daemon (`nginx` via service, or `setsid nohup … &`); prove survival (parent is init), not just the response | **3/3** — installs nginx, configures the root, proves the listener |

Two generalizations worth keeping:

- The baseline failure is bough's DEFAULT persona showing through: a lane that drafts and defers.
  For operate-the-machine work the skill corrects it; if tbench-style work becomes a real use
  case, the same text belongs nearer the identity for that agent kind.
- The iter-1 failure is a HARNESS truth the model could not know: `bg` is process-scoped. The
  skill now says so, but the `bg` tool's own description should say it too (and a `detach:`
  option would make the honest path easy). Filed as the next tool-description iteration.

## Second bed: polyglot-c-py (medium, Luna, n=3 per round)

| round | prompt change | resolved |
| --- | --- | --- |
| baseline | operate-the-machine only | **0/4** (scout + n=3), all four IDENTICAL: the polyglot itself was correct, but the agent verified with the instruction's own `gcc … -o /app/polyglot/cmain` example and left the binary; the checker asserts the directory holds ONLY `main.py.c` before it tests anything |
| iter 1 | `skills/finish-state.md`: the described end state is a contract ("a single file in DIR" constrains the directory); verify hard but in scratch space or clean up after; the line is "state the task asks for stays, state your verification created goes"; `ls -la` the target locations last | **3/3** |

Cross-bed regression (both skills active, one attempt each): configure-git-webserver,
analyze-access-logs, polyglot-c-py — 3/3. The finishing doctrine's two halves do not fight:
daemons the task asked for stay up, verification debris goes.

Scout notes for the next bed (`~/tbench-runs/scout-1`): `cancel-async-tasks` (hard) fails on
asyncio cancellation semantics ("Cleaned up." 0 of 2) — a reasoning failure, try a
run-the-acceptance-criteria-as-tests discipline; `broken-networking` died with a harness
`parse_error` worth understanding before trusting it as a bed; `gpt2-codegolf` is
capability-shaped, not prompt-shaped; `sqlite-db-truncate` and `fix-code-vulnerability` pass
already.

## Third bed: cancel-async-tasks (hard, Luna, n=3 per round)

| round | prompt change | resolved |
| --- | --- | --- |
| baseline | two prior skills | **1/3** — the failures never simulated the interrupt at all; the pass never did either (semantic luck) |
| iter 1 | `skills/prove-the-criteria.md`: every stated behavior is a test you write and run; "sometimes I cancel…" sentences are requirements; signals need a real subprocess + `send_signal` | **0/3** — WORSE. The rule produced self-tests, but WEAK ones: one trial's probe used a synchronous cleanup (`cleaned.append`) where the checker's cleanup itself awaits inside the `finally`, so the probe green-lit a broken runner. A discipline rule without probe strength manufactures false confidence |
| iter 2 | appended probe-strength rules: give behaviors their most demanding realistic shape (async, slow cleanup); COMBINE stated edges (interrupt while more work is queued than running); sabotage your artifact once and confirm the test goes red | **3/3** |

Cross-bed regression, all three skills active, one attempt each: configure-git-webserver,
analyze-access-logs, polyglot-c-py, cancel-async-tasks — **4/4**.

The doctrine after three beds, one theme per bed:
1. **operate-the-machine** — the machine is the target; converge live state; missing tools are
   installable; daemons must outlive you (never `bg`), prove survival.
2. **finish-state** — the described end state is a contract; verification debris goes, asked-for
   state stays; `ls` the targets last.
3. **prove-the-criteria** — every stated behavior is an executable test, at its most demanding
   plausible shape, with combined edges, validated by sabotage.

The iter-1 dip is the finding worth keeping: telling an agent to self-test WITHOUT telling it
what a strong test is produces weak probes and confident wrong ships — measurably worse than no
rule. Test-strength language must travel with test-discipline language.

## Fourth bed: intrusion-detection (medium, Luna) — and the prompt ceiling

The failure, byte-identical across every baseline run: the task names the incident report by
bare filename, the checker lists `/app` non-recursively, and the agent files it under an
invented `/app/reports/` for tidiness. An unrequested improvement indistinguishable from a
missing deliverable.

| round | prompt change | resolved |
| --- | --- | --- |
| baseline | three skills | 0/3 (0/4 with the scout) |
| iter 1 | finish-state gains "where the task is silent, be literal; artifacts named without a path land beside the deliverables" | 0/3 — injected in every request, lost to the tidiness prior |
| iter 2 | sharpened to a prohibition: NEVER create a directory the task did not name; per-artifact literal-path check | 3/3 |
| iter 3 | location became a TESTABLE criterion in prove-the-criteria (run scripts from a foreign cwd, assert the literal absolute path) | 3/5 |

Aggregate after iter 2's text: 6/8 versus 0/7 before. The two residual failures are the same
`reports/` move — on this model the prior survives ~40% of runs against three layers of
instruction. That is the honest ceiling of prompt-only iteration for this behavior on Luna;
the escalations past it are a stronger model, or a harness affordance (artifact-path
validation at the boundary), or in real work simply review.

Mechanism note worth keeping: the skill did NOT flap mid-wake — finish-state appears in all
eight requests of a failing run, so the skills seam's per-request trigger scan held through a
long wake. The residual is model compliance, not injection.

Also from scout-2:
- `broken-networking` is STRUCTURALLY out of scope for an installed agent: the task blackholes
  DNS (the agent cannot reach its own model until the thing it must fix is fixed) and pins
  `platform: linux/amd64` (the arm64 binary cannot run). Tasks of this class need the external
  terminus-style driver, not an installed bough.
- `bank-trans-filter` (easy) is a flaky 2/3: the "same name OR same account" rule is a
  transitive closure the agent sometimes single-pass filters. Parked as a possible fifth bed;
  its theme (relational rules are fixpoints) is real but the flakiness makes iteration noisy.
