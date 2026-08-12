# How it works

```
┌──────────────┐  HTTP + SSE   ┌──────────────────────────────────────┐
│ TUI (ratatui)│ ─────────────▶│  server  (Rust, 127.0.0.1:4321)      │
│  CLI (exec)  │ ◀─────────────│  ├─ turn runner                      │
│  ACP client  │   events      │  ├─ program sidecar                  │
└──────────────┘               │  ├─ workflow sidecar                 │
                               │  ├─ SQLite  ~/.bough/bough.db        │
                               │  └─ artifacts  ~/.bough/artifacts/   │
                               └──────────────────────────────────────┘
```

**The server is the system.** State, execution and orchestration are server-side; a
client can crash or detach without affecting a running turn. One Rust process: JSON API,
SSE event stream, static artifact hosting. Loopback only, no auth layer.

Four crates, one workspace:

| | |
|---|---|
| `bough-core` | The system — turn runner, harness, host functions, subagents, workflows, history ops, MCP, db, llm, prompt, skills, hooks, extensions |
| `bough-server` | The loopback HTTP + SSE server |
| `bough-tui` | The ratatui terminal UI |
| `bough` | The binary and its subcommands |

Crate boundaries, shared types and the concurrency model are in
[architecture.md](architecture.md).

## The turn loop

Everything after a user message lands is `turn/runner.rs`, and it holds one invariant:
**a turn always ends, always ends visibly, and always ends exactly once.** Three distinct
failures hide in that sentence, and each shaped the code:

1. **A turn never ends implicitly.** The model calls `stop` after its final text, in the
   same response. A response that just trails off is a model that forgot, so it is
   nudged — with a bounded count, so a stop-incapable model cannot loop the API forever.
   Nudges and the `stop` call are loop control, never content: they live only in the
   in-memory exchange and are never persisted, so the thread and every future replay stay
   clean.
2. **Every turn must produce user-visible text.** A turn of nothing but tool calls shows
   a stack of collapsed cards and no answer — the agent looks mute. Narration counts for
   nothing: a turn that says "let me implement the changes:" and ends on a raw `rg` dump
   has said less than one that said nothing. So the check asks only about text *after the
   last tool call*, and a turn about to end mute is asked once for a closing report, then
   forced into a text-only round.
3. **The pending message is closed on every path** — success, failure, interrupt, or a
   crash in the loop. A message left pending is a session the UI shows as busy forever
   and a queue that never drains.

There is no acceptance gate. `done: true` is a report, not a promise: nothing re-runs a
check and nothing verifies it. The model does the work, says what it did, and the user
verifies.

## The sidecars

**Program sidecar** — a fresh JS process per round, inheriting the server's full
authority. Host functions bridge over a line protocol on its pipes. It exists to give the
program a clean global scope and a cancellable lifetime, **not to contain it**.

**Workflow sidecar** — a JS process running one orchestration script. A scripting
surface, not a sandbox: bound to `agent` / `phase` / `log` / `parallel` / `pipeline`, and
starved of ambient nondeterminism (`Date.now`, `Math.random`) so journal replay stays
sound.

The distinction matters. The program sidecar is deliberately uncontained — that is the
project's central design decision, and the README's warning is the thesis, not an
unfinished edge. The workflow sidecar *is* restricted, because determinism is a
correctness requirement for replay rather than a security posture.

## History is a tree

Sessions form a forest by `parent_id`; a session's thread is its ancestors' messages plus
its own. Fork, compact and extract are therefore all the same cheap operation — a branch
parented at the target's parent inherits shared ancestors for free and only seeds the
rest.

**Nothing is destructively rewritten.** Every history operation produces a new branch, so
the previous line always survives.

## Working in place

The agent edits your own checkout — no copy, no overlay, no worktree. The Changes rail is
`git diff` against the sha the session started from, and you deliver with your own
`git commit` and `git push`.

That choice is what makes review the delivery mechanism, and it is why subagents share
one checkout rather than merging: there is nothing to merge. The safeguard against two
agents clobbering each other is `patch`'s version tag, which turns a collision into a
reported conflict — see [programs.md](programs.md).

## Where to read next

[`spec.md`](spec.md) is authoritative for product behavior. [`../specs/`](../specs) pins
per-subsystem contracts module by module, including invariants that are not
rediscoverable from the code: worker wind-down ordering, same-millisecond message
ordering, replay determinism. Read the relevant one before changing `turn/`, `harness/`
or `workflow/`.
