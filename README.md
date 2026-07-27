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

<p align="center">
  <img src="assets/shots/01-home.png" alt="bough home screen" width="800">
</p>

## Read this before you run it

**There is no isolation boundary.** Programs run as you, with your full authority — filesystem,
network, subprocesses, `npm:`/`jsr:` imports. There is no sandbox, no egress proxy, no credential
gating, no confinement of any kind. Host functions are convenience and session integration, never a
wall.

This is a deliberate choice, not an unfinished one ([spec §2](docs/spec.md), [§17](docs/spec.md)).
The harness edits your real files because reviewing `git diff` and pushing with your own git is the
delivery mechanism. bough states the posture plainly rather than implying safety it does not
provide.

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

## Use it

Point a session at a repo and ask in plain language. bough writes a small program, runs it, and
answers — folded reasoning, the code that ran, live cost and context in one view.

<p align="center">
  <img src="assets/shots/02-chat.png" alt="a bough conversation" width="800">
</p>

**The program environment.** Host functions are pre-injected globals; the program also has the full
Deno runtime and may ignore all of them.

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
skills, themes — with direct-jump keys. Frontier models route to Anthropic, OpenAI, or OpenRouter by
id prefix; a cheap tier handles titles, ghost text, and activity blurbs and fails silently when it
can't.

Plus: artifact publishing at a URL with a comment layer, keyword search across transcripts, `@`
files and `/` skills in the composer, and recurring runs on a schedule.

## How it works

```
┌─────────────┐   HTTP + SSE    ┌──────────────────────────────────────┐
│  TUI (Ink)  │ ───────────────▶│  server  (Deno, 127.0.0.1:4321)      │
│  CLI (exec) │ ◀─────────────── │  ├─ turn runner                      │
└─────────────┘   events         │  ├─ program worker   (inherit perms) │
                                 │  ├─ workflow worker  (no perms)      │
                                 │  ├─ SQLite  ~/.bough/bough.db        │
                                 │  └─ artifacts  ~/.bough/artifacts/   │
                                 └──────────────────────────────────────┘
```

- **Server** — one Deno process: JSON API, SSE event stream, static artifact hosting. Loopback only,
  no auth layer.
- **Program worker** — a fresh `Worker` per round with `permissions: "inherit"`. Host functions
  bridge over `postMessage`. The worker exists to give the program a clean global scope and a
  cancellable lifetime, not to contain it.
- **Workflow worker** — a `Worker` with `permissions: "none"` running one orchestration script. Its
  only bridge is `agent()` / `phase()` / `log()`, and it is deliberately starved of ambient
  nondeterminism so journal replay stays sound.
- **Clients** — an Ink TUI and a headless one-shot CLI.

## Run

Deno, TypeScript, SQLite. No build step — everything runs from source.

```bash
deno task dev     # server on 127.0.0.1:4321, with --watch
deno task tui     # the terminal UI against it
deno task check   # typecheck
deno task test    # unit + integration, offline and hermetic
```

`install.sh` bootstraps a fresh machine: it clones the repo and hands off to `scripts/bough setup`
for dependencies, the API key, and a background service.

For scripting there is a headless one-shot client — `bough exec "do the thing"` creates a session,
streams the assistant's text to stdout, and exits 0 on a completed turn, 1 on an errored one, 2 on a
usage or connection problem.

`BOUGH_HOME` relocates the whole data root (`~/.bough` by default) and `BOUGH_PORT` moves the
listener, so a development instance never touches a live install.

## What bough is not

These are decisions, not gaps:

- No confinement of any kind, and no credential gating.
- No acceptance gate — the model reports what it did and you verify it. The harness does not re-run
  a committed command or block a turn from finishing.
- No local inference; the cheap tier is a hosted model.
- No embeddings or vector index — cross-session search is SQLite FTS over transcripts.
- No per-agent worktrees or file leases. One shared checkout.
- No remote access, no auth layer, no web UI.
- No archive, deprecate, or purge — session visibility is derived from lineage.

## Docs

- [`docs/spec.md`](docs/spec.md) — what the system is. Authoritative.
- [`docs/implementation-plan.md`](docs/implementation-plan.md) — how it is built, the module layout,
  and the invariants worth knowing before changing anything.
