# bough

A sandboxed coding agent with branchable history. Written in **Gleam** (BEAM/OTP),
sandboxed by **[nono](https://nono.sh)**, structured like **opencode** (a headless
server with thin clients), with **closedshell**-style live network visibility.

> A *bough* is a branch: history is a tree you can fork at any point — and the
> filesystem forks with it. And it's safe to leave growing: every agent runs
> under a kernel-enforced nono sandbox (network allowlist + atomic filesystem
> snapshots + tamper-evident audit), so you can detach, walk away, and reattach.

See **[SPEC.md](SPEC.md)** for the full design and the v1 milestone.

## Layout

Gleam has no native workspace, so this is a set of packages wired with `path`
dependencies:

| Package | Role |
|---------|------|
| [`packages/bough_core`](packages/bough_core) | Shared, side-effect-free types & logic: session tree, provider interface, tools, nono contract. |
| [`packages/bough_server`](packages/bough_server) | Headless server: agent loop, session supervision, nono bridge, HTTP+SSE API. Depends on `bough_core`. |
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
- TUI client connects to the server.
- nono bridge launches/stops real sandboxes (`nono_bridge`), with a pure,
  unit-tested args builder driven from a capability `Profile`.
- nono proxy audit log parsed into `AuditEvent`s (the network side-pane data),
  plus `rollback restore` plumbing. Per-write-turn snapshot capture is deferred
  (nono snapshots at session boundaries — SPEC.md §11).
- Anthropic agent loop with tool use (`POST /session/:id/message`): `bash` runs
  in a nono sandbox (network blocked, workspace-scoped); `read`/`write`/`edit`
  manage files. User + assistant turns persist to the session tree.

Next: render the chat + live network side pane in the TUI; sandbox the file
tools (currently in-process); reconcile per-call vs. session-long sandboxing.
