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

Scaffold. Stub modules compile and type-check; nothing is wired end-to-end yet.
Next is the v1 thin vertical slice — see [SPEC.md §10](SPEC.md).
