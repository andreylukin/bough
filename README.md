# bough

A sandboxed coding agent with branchable history. Written in **Gleam** (BEAM/OTP),
sandboxed by **[nono](https://nono.sh)**, structured like **opencode** (a headless
server with thin clients), with **closedshell**-style live network visibility — and
a **ReDACT-style supervisor-worker** agent loop (as in **tent**): a frontier model
plans, a deterministic harness is the only thing that executes, a local model
patches trivial breakage.

> A *bough* is a branch: history is a tree you can fork at any point — and the
> filesystem forks with it. And it's safe to leave growing: every agent runs
> under a kernel-enforced nono sandbox (network allowlist + atomic filesystem
> snapshots + tamper-evident audit), so you can detach, walk away, and reattach.

See **[SPEC.md](SPEC.md)** for the full design and the v1 milestone.

## Install

One line on any Mac — installs the toolchain (Gleam/Erlang, the `nono` sandbox,
`llama.cpp` for the worker), downloads the default worker model
(Qwen2.5-Coder-7B, ~4.7 GB, to `~/.bough/models/`), clones bough, and builds it:

```bash
curl -fsSL https://raw.githubusercontent.com/andreylukin/bough/main/install.sh | bash
```

Already cloned? Run `./install.sh` from the checkout instead. The script is
idempotent — re-running updates, rebuilds, and resumes a partial model download.
Knobs: `BOUGH_HOME` (clone target, default `~/repos/bough`), `BOUGH_NO_LLAMA=1`
(skip `llama.cpp`), `BOUGH_NO_MODEL=1` (skip the model download), and
`BOUGH_MODEL_URL` (use a different GGUF). The Qwen2.5-Coder worker is always on:
bough starts a local `llama-server` on first run and uses the pre-installed
weights, so there's nothing to configure. Point `BOUGH_WORKER_URL` at a remote
endpoint to override.

Then set your key and start it:

```bash
export ANTHROPIC_API_KEY=sk-ant-...   # add to ~/.zshrc to persist
scripts/bough                         # starts the server, waits for health, runs the TUI
```

`scripts/bough` is a convenience launcher; you can also run the two processes by
hand (see [Develop](#develop)).

## Layout

Gleam has no native workspace, so this is a set of packages wired with `path`
dependencies:

| Package | Role |
|---------|------|
| [`packages/bough_core`](packages/bough_core) | Shared, side-effect-free types & logic: session tree, provider interface, artifact grammar, nono contract. |
| [`packages/bough_server`](packages/bough_server) | Headless server: supervisor-worker loop, session supervision, nono bridge, HTTP+SSE API. Depends on `bough_core`. |
| [`packages/bough_tui`](packages/bough_tui) | Terminal client: chat pane + live network side pane + tree overlay. Depends on `bough_core`. |

## Develop

Requires Gleam (`brew install gleam`, which pulls in Erlang/OTP).

```bash
make check    # type-check all packages
make test     # run all tests
make build    # compile all packages
make serve    # run the server (placeholder)
```

Or per package: `cd packages/<name> && gleam check`.

## Status

Early slices of the v1 vertical slice (SPEC.md §10) are working:

- Server boots (wisp/mist) with `/`, `/health`, `/doc`.
- Session CRUD with JSONL persistence to `~/.bough/sessions/`
  (`POST /session`, `GET /session/:id`, `POST /session/:id/entry`).
- TUI client (shore) with a conversation pane + network side pane + input
  line; sends prompts and shows replies. Network calls run as async effects so
  the UI stays responsive. Run it in a real terminal (`gleam run` in
  `packages/bough_tui`); set `BOUGH_SERVER` to override the default.
- nono bridge launches/stops real sandboxes (`nono_bridge`), with a pure,
  unit-tested args builder driven from a capability `Profile`.
- nono proxy audit log parsed into `AuditEvent`s (the network side-pane data),
  plus `rollback restore` plumbing. Per-write-turn snapshot capture is deferred
  (nono snapshots at session boundaries — SPEC.md §11).
- **Supervisor-worker engine** (`engine`, SPEC §5), driving `POST
  /session/:id/message` and the streaming `…/run`: the Anthropic supervisor
  plans via plain-text `STEP`/`RUN`/`WRITE`/`EDIT`/`READ`/`GREP` + `### CHECK`
  artifacts (parsed by `bough_core/artifact`, unit-tested against tent's suite);
  the harness executes each step in a nono sandbox (network blocked,
  workspace-scoped), digests output to the blackboard, gates `DONE` on the CHECK
  passing plus an adversarial review, tracks file integrity, and caps the turn
  with round/step budgets. Earlier provider tool-use code (`agent`, `tools`)
  remains but is no longer wired in.
- **Worker runtime** (`worker_runtime`, SPEC §5.6): always on, fixed to
  Qwen2.5-Coder. bough supervises a local `llama-server` against the
  pre-installed GGUF (`~/.bough/models/`), handing the engine a localhost
  OpenAI-compatible endpoint; a failed step gets one worker fix command. Set
  `BOUGH_WORKER_URL` to use a running/remote endpoint instead.

Next: stream live egress + step activity into the network pane and add rule
editing (needs server SSE + a rules endpoint); a session-tree overlay; sandbox
the file tools (WRITE/EDIT currently run in-process); per-write-turn snapshots
so a failed review can fork back (SPEC §5.4).
