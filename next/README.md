# bough (rewrite)

This is the new tree. It is built here, beside the old one, and renamed to `src/` at cutover (plan
T10.8). Until then `../src/` is read-only reference material — the running server builds from it,
and the `Port from` line in each task says which old file to read.

This file is a stub. The real README is T10.7.

## What bough is

A coding agent that acts by **writing programs**. Instead of emitting one tool call at a time and
waiting, the model writes a single JavaScript program per round — with loops, branching, and
composition — and a harness executes it against the user's machine. A headless server owns all state
and execution; clients are views over it.

bough is an alternative harness _design_, not a better coding agent. That distinction is the point
of the project and the README should not blur it.

## There is no isolation boundary

Programs run as the user, with the user's full authority: filesystem, network, subprocesses,
`npm:`/`jsr:` imports. There is no sandbox, no egress proxy, and no credential gating (spec §2,
§17). Host functions are convenience and session integration — never confinement. `confine()` in
`paths.ts` guards the _server's_ own path construction against a name in a request; it is not a
containment mechanism and does not pretend to be one.

Run bough only on a machine where you would be comfortable running the code it writes, because that
is exactly what happens.

## Layout and commands

The module layout, the milestones, and the ground rules live in
[`../docs/implementation-plan.md`](../docs/implementation-plan.md); what the system is lives in
[`../docs/spec.md`](../docs/spec.md).

```
deno task check    # typecheck — must pass before every commit
deno task test     # unit + integration, offline and hermetic
deno task dev      # server with --watch
deno task tui      # TUI against the local server
```

Run them from this directory, against `next/deno.json`.

## Running beside the live install

The old server runs on `:4321` against `~/.bough/bough.db`. The rewrite must not collide with it.
`BOUGH_HOME` relocates the whole data root and `BOUGH_PORT` moves the listener, so develop against
both and leave the live database alone (plan §2). Every path in `paths.ts` resolves through
`boughHome()`, which is what makes a single env var sufficient — and what lets tests get a hermetic
root instead of writing to the real one.
