# Working in this repo

## Layout

```
src/          the whole system — server, turn runner, harness, host functions,
              subagents, workflows, history ops, MCP, LSP, prompt, TUI
docs/         spec.md (what the system is) · implementation-plan.md (how it is built)
skills/       the one bundled skill
assets/       logo and screenshots
```

Everything builds from the root `deno.json`. There is no build step and no web UI — the server and
the Ink TUI both run from source.

```
deno task check    # typecheck — must pass before every commit
deno task test     # unit + integration, offline and hermetic
deno task dev      # server with --watch
deno task tui      # the TUI against the local server
```

`docs/spec.md` is authoritative for behavior; `docs/implementation-plan.md` carries the module
boundary rules and the invariants that are not rediscoverable from the spec (worker wind-down
ordering, same-millisecond message ordering, replay determinism, and the rest). Read the invariants
section before changing anything in `turn/`, `harness/`, or `workflow/`.

Conventions that reviews enforce: every module opens with a comment stating the invariant it holds;
dependencies (db, clock, LLM client) are injected rather than reached for; parsing and other core
logic stay pure with `now` passed in; Zod validates at the boundary; tests sit next to the module
they cover.

## Version control: plain git

This repo is a normal git checkout on `main`, and bough works in it **in place** — no shadow store,
no per-session worktree, no overlay. Edits land in the real files immediately, so git is the only
record of what a session changed. Each session's starting sha is recorded in the database and drives
the Changes rail.

- Ship work with ordinary commits on `main`; branch first for anything you'd want reviewed as a PR.
- There is no snapshot store to salvage from — uncommitted work lives only in the working tree.

## Running against this tree

The live server builds from this working tree and is respawned by launchd when killed. Point a
development instance somewhere else before exercising it: `BOUGH_HOME` relocates the data root and
`BOUGH_PORT` moves the listener.

Never point a bough session's workspace at this repository itself while testing server endpoints
that mutate a workspace. Use a scratch directory.
