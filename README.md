<p align="center">
  <img src="assets/logo.svg" alt="bough" width="120" height="130">
</p>

<h1 align="center">bough</h1>

<p align="center">
  <b>A coding agent that acts by writing programs.</b><br>
  One JavaScript program per round — loops, branching, composition — run against your real checkout.
</p>

Most harnesses let the model emit one tool call and wait. bough gives it a single tool that takes a
program: the model writes JavaScript with real control flow, and a harness executes it on your
machine. A headless server owns all state and execution; the terminal UI is a view over it. History
is a tree — any turn can be branched — and delegation is first-class: a program can spawn subagents
or launch a detached workflow that fans work across many of them.

bough is an alternative harness **design**, not a better coding agent. That distinction is the point
of the project, and this README tries not to blur it.

## Read this before you run it

**There is no isolation boundary.** Programs run as you, with your full authority — filesystem,
network, subprocesses, `npm:` imports. There is no sandbox, no egress proxy, no credential gating,
no confinement of any kind. Host functions are convenience and session integration, never a wall.

This is a deliberate choice, not an unfinished one ([spec §2](docs/spec.md)). The harness edits your
real files because reviewing `git diff` and pushing with your own git is the delivery mechanism.

Run it only on a machine where you would be comfortable running the code it writes, because that is
exactly what happens.

## The idea

- **One program per round.** The model's only action is `run_steps(code)`. Control flow lives in the
  program, not in a chain of round-trips.
- **In place.** The agent edits your own checkout — no copy, no overlay. The Changes rail is
  `git diff` against the sha the session started from; you deliver with `git commit` / `git push`.
- **History is a tree.** Fork any turn, compact a span onto a new branch, lift messages into a fresh
  root. Nothing is ever destructively rewritten — every operation produces a new branch.
- **The server is the system.** State, execution, and orchestration are server-side. A client can
  crash or detach without affecting a running turn.
- **Delegation is core.** Subagents and workflows are primary capabilities with real persistence,
  lifecycle control, and observability.

## Setup

macOS or Linux. The service manager is the only platform-specific piece: launchd on macOS,
a systemd **user** unit on Linux, and a plain background process where there is no user
systemd (containers, WSL1). Everything else — the server, the TUI, `exec` — is the same
code on both.

```bash
/bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/andreylukin/bough/main/install.sh)"
```

That clones into `~/bough` (override with `BOUGH_DIR`) and hands off to `scripts/bough setup`, which
installs the toolchain (`bun` ≥ 1.3, `node`, `ripgrep`, `uv`) with the platform's package
manager — Homebrew, `apt-get`, `dnf` or `pacman` — installs dependencies,
links `bough` into `~/.local/bin`, and writes an env template to `~/.bough/env`. Already have a
clone? Run `scripts/setup.sh` directly. Then:

```bash
$EDITOR ~/.bough/env      # ANTHROPIC_API_KEY=…  (OPENAI_/OPENROUTER_ keys optional)
bough start               # background service: starts at login, restarts on crash
bough                     # the TUI (auto-starts the server if it is down)
```

`bough` also takes `kill`, `restart`, `update` (fast-forward `origin/main` and restart), `status`,
`logs`, `run` (foreground server), `purge`, and `sync-mcp` (adopt Claude Code's MCP servers).

There is no file watcher on the service: editing code lands only on an explicit `bough restart`.

**Scripting.** `bough exec [-w dir] [-m model] [--json] "do the thing"` creates a session, streams
the assistant's text to stdout, and exits 0 on a completed turn, 1 on an errored one, 2 on a usage
or connection problem.

**Environment.** `BOUGH_HOME` relocates the whole data root (`~/.bough` by default) and `BOUGH_PORT`
moves the listener, so a development instance never touches a live install. `BOUGH_MODEL` and
`BOUGH_CHEAP_MODEL` set the default and cheap-tier models (both also settable in the picker).

## Use it

Point a session at a repo and ask in plain language. bough writes a small program, runs it, and
answers — folded reasoning, the code that ran, live cost and context in one view.

**The program environment.** Host functions are pre-injected globals; the program also has the full
Bun runtime and may ignore all of them.

| | |
|---|---|
| Shell | `bash` (auto-backgrounds past 60s, never killed) · `sh` for concurrent commands · `bashBg` / `bashOutput` / `bashWait` / `bashKill` |
| Files | `view` → numbered lines with a version tag · `patch` → hash-anchored line edits · `write` for new files |
| Delegation | `agent` (blocking) · `spawn` (detached) · `join` · `adopt` · `workflow.*` |
| Session | `ask` a human mid-program · `state.*` durable KV · `schedule.*` · `image` · `fetch` · `artifact` · `mcp` · `lsp.*` |

**One editing idiom.** `patch` names lines instead of quoting them, so code being edited never has
to survive the model's own string escaping. The tag pins the version that was viewed: if the file
moved on but the patched lines are untouched, the edit rebases; if they *were* touched, it reports a
conflict. With subagents sharing one checkout, that is the primary safeguard against silent
clobbering.

**Fan out.** `agent()` runs a subagent to completion and returns its report; `spawn()` detaches one
that reports back as a system note. For bigger fan-outs, a **workflow** is a script that runs
detached from the turn, with `agent` / `parallel` / `pipeline` primitives and structured (schema
validated) results. Every `agent()` call is journaled before it runs, so a stopped run loses no
completed work and a relaunch replays the unchanged prefix instead of paying for it twice.

**Everything else in one panel.** Sessions, conversation tree, changes review, model picker, MCP,
skills, themes — with direct-jump keys. Models route to Anthropic, OpenAI, or OpenRouter by id
prefix, and the picker's catalog is what the server's keys can actually reach, not a compiled-in
list; a cheap tier handles titles, ghost text, and activity blurbs and fails silently when it can't.

Plus: artifact publishing at a URL with a comment layer, keyword search across transcripts, `@`
files and `/` skills in the composer, and recurring runs on a schedule. Four skills ship bundled —
`history`, `wayfinder`, `domain-modeling`, `grilling` — and `~/.bough/skills` holds your own.
Project rules come from `AGENTS.md`, read per turn from the git root down.

## How it works

```
┌──────────────┐  HTTP + SSE   ┌──────────────────────────────────────┐
│ TUI (OpenTUI)│ ─────────────▶│  server  (Bun, 127.0.0.1:4321)       │
│  CLI (exec)  │ ◀──────────────│  ├─ turn runner                      │
└──────────────┘   events      │  ├─ program worker                   │
                               │  ├─ workflow worker                  │
                               │  ├─ SQLite  ~/.bough/bough.db        │
                               │  └─ artifacts  ~/.bough/artifacts/   │
                               └──────────────────────────────────────┘
```

- **Server** — one Bun process: JSON API, SSE event stream, static artifact hosting. Loopback only,
  no auth layer.
- **Program worker** — a fresh `Worker` per round, inheriting the server's full authority. Host
  functions bridge over `postMessage`. The worker exists to give the program a clean global scope
  and a cancellable lifetime, not to contain it.
- **Workflow worker** — a `Worker` running one orchestration script. A scripting surface, not a
  sandbox: it is bound to `agent()` / `phase()` / `log()` / `parallel()` / `pipeline()`, and is
  starved of ambient nondeterminism (`Date.now`, `Math.random`) so journal replay stays sound.
- **Clients** — an OpenTUI TUI and a headless one-shot CLI.

## Develop

Bun, TypeScript, SQLite. No build step — everything runs from source.

```bash
bun run dev     # server on 127.0.0.1:4321, with --watch
bun run tui     # the terminal UI against it
bun run check   # typecheck — must pass before every commit
bun test        # unit + integration, offline and hermetic
```

## What bough is not

These are decisions, not gaps:

- No confinement of any kind, and no credential gating.
- No acceptance gate — the model reports what it did and you verify it. The harness does not re-run
  a committed command or block a turn from finishing.
- No local inference; the cheap tier is a hosted model.
- No embeddings or vector index — cross-session search is SQLite FTS over transcripts.
- No per-agent worktrees or file leases. One shared checkout.
- No remote access, no auth layer, no web UI.

## Docs

- [`docs/spec.md`](docs/spec.md) — what the system is. Authoritative.
- [`docs/implementation-plan.md`](docs/implementation-plan.md) — how it is built, the module layout,
  and the invariants worth knowing before changing anything.
- [`AGENTS.md`](AGENTS.md) — conventions this repo's reviews enforce.
- [`ahe/README.md`](ahe/README.md) — the observability-driven prompt-evolution loop and its task
  bank, kept alongside the harness it measures.
