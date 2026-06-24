<p align="center">
  <img src="assets/logo.svg" alt="bough" width="150" height="163">
</p>

# bough

A sandboxed coding agent with branchable history. Written in **Gleam** (BEAM/OTP),
sandboxed by **[nono](https://nono.sh)**, structured like **opencode** (a headless
server with thin clients), with **closedshell**-style live network visibility — and
a **ReDACT-style supervisor-worker** agent loop (as in **tent**): a frontier model
plans by writing Python that a deterministic harness runs in a
**[monty](https://github.com/pydantic/monty)** code-mode sandbox, and a local
model (**VibeThinker-3B**) patches trivial breakage.

> A *bough* is a branch: history is a tree you can fork at any point — and the
> filesystem forks with it. And it's safe to leave growing: every agent runs
> under a kernel-enforced nono sandbox (network allowlist + atomic filesystem
> snapshots + tamper-evident audit), so you can detach, walk away, and reattach.

See **[SPEC.md](SPEC.md)** for the full design and the v1 milestone.

## Install

One line on any Mac — installs the toolchain (Gleam/Erlang, the `nono` sandbox,
`llama.cpp` for the worker), downloads the default worker model
(VibeThinker-3B, ~1.9 GB, to `~/.bough/models/`), clones bough, and builds it:

```bash
curl -fsSL https://raw.githubusercontent.com/andreylukin/bough/main/install.sh | bash
```

Already cloned? Run `./install.sh` from the checkout instead. The script is
idempotent — re-running updates, rebuilds, and resumes a partial model download.
Knobs: `BOUGH_HOME` (clone target, default `~/repos/bough`), `BOUGH_NO_LLAMA=1`
(skip `llama.cpp`), `BOUGH_NO_MODEL=1` (skip the model download), and
`BOUGH_MODEL_URL` (use a different GGUF). The VibeThinker-3B worker is always on:
bough starts a local `llama-server` on first run and uses the pre-installed
weights, so there's nothing to configure. Point `BOUGH_WORKER_URL` at a remote
endpoint to override.

Then set your key and start it:

```bash
export ANTHROPIC_API_KEY=sk-ant-...   # add to ~/.zshrc to persist
bough                                  # starts the server, waits for health, opens the web UI
```

`bough` starts the headless server and opens the **web UI**
(`http://127.0.0.1:4096`) in your browser; the server runs in the foreground, so
Ctrl-C stops it. The UI is served by the server itself from
`packages/bough_server/priv/web` — nothing extra to install, no separate build.

The installer symlinks `bough` (the `scripts/bough` launcher) into Homebrew's
`bin`, so it's on your PATH and runnable from anywhere. You can also run the
server by hand (see [Develop](#develop)). The original terminal client
(`bough_tui`) is still buildable as a fallback — `gleam run` in
`packages/bough_tui`.

To pull the latest version and rebuild in place:

```bash
bough update     # git pull --ff-only + make build, then prints the new commit
```

## Working with bough

The web UI is a single screen with three columns: **sessions** on the left
(resume or start a new one), the **conversation** in the middle (the supervisor's
plan, each sandboxed step with its exit code, worker fixes, and the CHECK), and a
tabbed **side panel** on the right — **Tree** (branch/fork/graft), **Network**
(live egress feed), **Capabilities** (nono groups), and **Subagents**. Approval
gates (plan review, network, capability) appear inline in the conversation with
**Allow / Steer / Reject** buttons; type in the composer while a run is in flight
to steer it. Everything below is reachable from those panels — no keybindings to
memorize.

**Project instructions.** Drop an `AGENTS.md` (or `CLAUDE.md`) at the root of a
project and the supervisor treats it as authoritative — build/test commands,
conventions, what "done" means. It shapes the supervisor's plan and its `check`.

**Plan-review gate.** Toggle it with `p` (or start with `BOUGH_REVIEW=1`). When
on, every non-empty plan pauses before the harness runs it: bough shows the
batch of actions and you press `a` to allow, `e` to edit/steer (type guidance,
Enter sends), or `r` to reject. Steering and rejection feed your words back to
the supervisor to replan. A `⏸ review` marker in the status bar shows it's armed.

**Steer a run mid-flight.** While any run is in progress, just type a message and
press Enter — it's injected into that agent's conversation at its next round. No
need to wait for it to finish or stop it.

**Subagents.** The supervisor can delegate a self-contained sub-task with a
`spawn` action. Spawning is **asynchronous**: the subagent runs concurrently on
the same workspace and `spawn` returns its id, so the supervisor can `tell` it
more context while it works and `collect` it to wait for and read its result —
the main agent and its subagents stay in contact, not just hand-off-and-wait.
Press `a` to list a session's subagents and Enter to jump into one — you'll see
its live transcript and can type messages to it yourself (steering, as above);
`b` returns to the parent. Subagents can spawn their own subagents.

> Concurrency note: subagents share one workspace, so the supervisor is
> responsible for not running conflicting file edits in parallel (it can `collect`
> one before starting the next). Isolated per-subagent branches are future work.

**Checkpoints.** Every turn checkpoints the workspace. Open the tree with `t` and
press Enter on an earlier node to fork from it: the conversation **and the files**
revert to that point, so you can explore a branch and jump back if it goes wrong.
Snapshots live in a per-session shadow git repo under `~/.bough/snapshots/` and
never touch your project's own `.git`; set `BOUGH_NO_SNAPSHOTS=1` to turn them off.

**Network leash.** With `BOUGH_NET=1`, the agent's sandboxed commands get the
network on a default-deny allowlist instead of being fully blocked. When a
command is denied, the run **pauses** and asks you — `a` allow the host, `e` a
custom path-glob rule (pre-filled), or `r` deny — then adds the rule and retries
the command. Approvals persist with the session, so you're asked once per host.

Granularity is up to you. Approve a **host** (`api.foo.com`, a CONNECT tunnel),
or a **method+path glob** (`https://api.foo.com/v1/**`); nono TLS-intercepts
hosts that have a path rule and enforces at the endpoint level, so once a host is
scoped, later denials show the exact `METHOD host/path` and you can narrow
further. (Globs, not regex: `*` = one path segment, `**` = zero or more.)
Multiple path rules for the same host **union** — approving `/v1/**` then
`/v2/**` allows both. Approvals are compiled to a per-session nono profile under
`~/.bough/`.

Without `BOUGH_NET`, commands have no network (as before). The supervisor's own
model calls are made by the server, outside the sandbox, so the leash governs
only what the agent runs.

**Toolchain access.** Sandboxed `RUN`/`CHECK` commands get the workspace plus
read-only access to the language-toolchain dirs that exist under `$HOME`
(`~/.cargo/bin`, `~/go/bin`, `~/.pyenv/shims`, `~/.nvm`, `~/.local/bin`, …), and
the generated profile includes nono's `git_config` group, so `git`, `cargo`,
`go`, `node`, etc. resolve on `PATH` without granting write access outside the
workspace.

**Capability groups.** nono exposes capability *groups* — bundles of paths
and permissions (e.g. `git_config`, a language toolchain, a secrets dir) that
layer into the sandbox profile on top of the always-on base. The right-hand
capabilities panel lists the catalog for this host: the locked "always on"
groups (folded away) and the toggleable ones. Press `c` to focus it, `↑`/`↓`
(or `k`/`j`) to move, `space` (or `Enter`) to toggle a group for this session,
and `Esc` (or `c`) to return to the conversation; right-click a group to
inspect its paths and description in an overlay. Enabled groups persist with the
session and apply to every subsequent run. The catalog is also reachable over
the API (`GET /groups`, `GET /groups/:name`, `POST /session/:id/groups`).

**Credential injection.** For an authed `RUN` (e.g. `curl` to a private API),
set `BOUGH_NET_CREDENTIALS` to a comma-separated list of `name=ENV_VAR` (or a
bare `name`, whose env var defaults to its upper-cased form). Each one whose env
var is set on the server is declared in the session's nono profile as an
`env_credentials` entry, so nono injects it on egress and the raw secret never
enters the sandbox (SPEC §6.4). Off by default.

**Full plan of a turn.** Click anywhere on a bough turn (or press `f` for the
latest one) to open the full-plan overlay: the supervisor's complete plan in one
view — every action with its argument, the full `WRITE`/`EDIT` content, the
otherwise-hidden `READ`/`GREP` steps, worker fixes, and the `CHECK`. `↑↓` scroll,
`Esc` closes.

**Network pane.** A collapsible side pane (not a fixed column): it starts hidden
so the conversation gets the full width, and you toggle it with `n` (start it
open with `BOUGH_NET_PANE=1`). When open it streams the run's **live egress
feed** — each sandboxed request the agent made, `✓` allowed (green) or `✗` denied
(red), with the host and, where nono intercepted at L7, the `METHOD /path`. The
feed populates under the leash (`BOUGH_NET=1`); without it the policy is simply
"net blocked". Collapsing loses nothing critical — when the leash pauses on a
denied request, the allow/deny prompt surfaces inline in the conversation
regardless.

> The descriptions above refer to the web UI's panels and buttons. The legacy
> terminal client (`bough_tui`, still buildable) drives the same server with
> keybindings instead — `s`/`t`/`a`/`b` resume/tree/subagents/back, `f`/`p`/`n`/`c`
> full-plan/plan-review/network/capabilities, `a`/`e`/`r` to allow/edit/reject a
> paused plan.

## Layout

Gleam has no native workspace, so this is a set of packages wired with `path`
dependencies:

| Package | Role |
|---------|------|
| [`packages/bough_core`](packages/bough_core) | Shared, side-effect-free types & logic: session tree, provider interface, artifact grammar, nono contract. |
| [`packages/bough_server`](packages/bough_server) | Headless server: supervisor-worker loop, session supervision, nono bridge, HTTP API. Serves the web UI from `priv/web`. Depends on `bough_core`. |
| [`packages/bough_server/priv/web`](packages/bough_server/priv/web) | Web client (default): a no-build static SPA (HTML/CSS/JS) the server serves at `/`. Talks to the HTTP API; chat + tree/fork/graft + network dock + capabilities + subagents. |
| [`packages/bough_tui`](packages/bough_tui) | Terminal client (legacy/fallback): chat pane + live network side pane + tree overlay. Depends on `bough_core`. |
| [`sidecar`](sidecar) | `bough-monty`: a Rust binary embedding the monty interpreter — runs the supervisor's code-mode Python, exposing `bash`/`read`/`write`/`edit` as host functions (`bash` via nono). |

## Develop

Requires Gleam (`brew install gleam`, which pulls in Erlang/OTP) and, for the
code-mode sidecar, a recent stable Rust (`brew install rust`; monty needs
≥1.95).

```bash
make check    # type-check all packages
make test     # run all tests
make build    # compile all packages
make serve    # run the server (127.0.0.1:4096)

# the code-mode sidecar (built by install.sh; or by hand):
cargo build --release --manifest-path sidecar/Cargo.toml
ln -sf "$PWD/sidecar/target/release/bough-monty" ~/.bough/bin/bough-monty
```

Or per package: `cd packages/<name> && gleam check`.

## Status

Early slices of the v1 vertical slice (SPEC.md §10) are working:

- Server boots (wisp/mist) with `/`, `/health`, `/doc`.
- Session CRUD with JSONL persistence to `~/.bough/sessions/`
  (`POST /session`, `GET /session/:id`, `POST /session/:id/entry`).
- TUI client (etch) with a conversation pane + network side pane + input
  line; sends prompts and shows replies. Network calls run as async effects so
  the UI stays responsive. Run it in a real terminal (`gleam run` in
  `packages/bough_tui`); set `BOUGH_SERVER` to override the default.
- nono bridge launches/stops real sandboxes (`nono_bridge`), with a pure,
  unit-tested args builder driven from a capability `Profile`.
- nono proxy audit log parsed into `AuditEvent`s (the network side-pane data).
- **Checkpoints** (`snapshots`, SPEC §4.1): every turn checkpoints the workspace
  to a per-session shadow git repo (`~/.bough/snapshots/`, never the user's own
  `.git`), recorded as the node's `snapshot_ref`. A `fork` restores the files to
  that node — branch the history and the filesystem branches with it. Disable
  with `BOUGH_NO_SNAPSHOTS=1`.
- **Supervisor-worker engine** (`engine`, SPEC §5), driving `POST
  /session/:id/message` and the streaming `…/run`: the Anthropic supervisor acts
  in **code-mode** — each round it writes a Python `code` program that runs in a
  [monty](https://github.com/pydantic/monty) sandbox via the `bough-monty`
  sidecar (`monty_bridge`), calling host functions `bash`/`read`/`write`/`edit`
  (`bash` shells through nono); plus `spawn`/`tell`/`collect` for subagents and a
  `### CHECK`. The harness digests output to the blackboard, gates `DONE` on the
  CHECK passing plus an adversarial review, tracks file integrity, and caps the
  turn with round/step budgets. Two nested sandboxes meet at `bash`: monty
  confines the agent's Python, nono confines the processes it launches (SPEC §6).
  Earlier provider tool-use code (`agent`, `tools.execute`) remains but is no
  longer wired in.
- **Worker runtime** (`worker_runtime`, SPEC §5.6): always on, fixed to
  VibeThinker-3B (arXiv:2606.16140, a reasoning/coding SLM on Qwen2.5-Coder-3B).
  bough supervises a local `llama-server` against the
  pre-installed GGUF (`~/.bough/models/`), handing the engine a localhost
  OpenAI-compatible endpoint; a failed step gets one worker fix command. Set
  `BOUGH_WORKER_URL` to use a running/remote endpoint instead.

- **Live egress feed** (network dock): each run accumulates the sandbox's
  observed egress (allow/deny, host + intercepted method/path) in the engine and
  publishes it on the run-poll JSON (`…/run`), which the TUI renders in the
  collapsible network pane. Populated under the leash (`BOUGH_NET=1`).
- **Capability groups picker** (`nono_bridge`, SPEC §7): the TUI's capabilities
  panel lists nono's policy-group catalog for this host — toggleable groups the
  human enables per session (persisted, layered into the run profile) plus the
  locked "always on" base; right-click inspects a group. Served over `GET
  /groups`, `GET /groups/:name`, `POST /session/:id/groups`.
- **Sandboxed file tools**: READ/GREP/WRITE/EDIT now run through the nono sandbox
  (`sandboxed`/`sandboxed_write`) alongside RUN/CHECK, so a path that escapes the
  workspace — including via a symlink — is kernel-denied. The write content is
  staged in bough's own dir and granted to the sandbox read-only, so it never
  passes through argv.

Next: push run/egress updates over SSE instead of polling, and add in-pane rule
editing (a rules endpoint); context compaction for long autonomous runs; and an
optional auto-fork on diverging edits, so the human can compare two branches
side by side before committing to either.
