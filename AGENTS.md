# Working in this repo

## Layout

```
crates/bough-llm/    the provider layer: LlmClient + types, routing, retries,
                     pricing, tracing; standalone (no bough-core dep)
crates/bough-core/   the system: turn runner, harness, host functions, subagents,
                     workflows, history ops, MCP, db, prompt, skills
crates/bough-server/ the loopback HTTP + SSE server
crates/bough-tui/    the ratatui terminal UI
crates/bough/        the binary: start | restart | tui | exec | acp | hooks | mcp |
                     sync-mcp | tags | notes | patterns
specs/               per-subsystem behavioral contracts (16 files), authoritative;
                     README.md indexes them and maps the old TS names to crates/
docs/                README.md is the map. spec.md (what the system is) ·
                     architecture.md (crate boundaries, shared types, concurrency) ·
                     tags.md (the command-memory tag system) · the user-facing set
                     (install · tui · cli · programs · delegation · extending ·
                     configuration · troubleshooting · how-it-works)
docs/history/        finished and unmaintained: port-plan.md, implementation-plan.md
scripts/             setup.sh (fresh machine) · bough (the service manager) ·
                     smoke.sh and tui-test.sh (the PTY acceptance suites)
Formula/             the Homebrew formula. A release tags the commit, then bumps
                     this file's url + sha256 to that TAG (never a commit sha, because
                     it goes stale on the next push); the tap repo copies it
.github/             CI, issue and PR templates, CONTRIBUTING · SECURITY · CODE_OF_CONDUCT
assets/              the logo (logo.svg is the source; the PNGs are exports)
```

One cargo workspace at the root. `make help` lists the targets.

```
cargo check --workspace    # must pass before every commit
cargo test --workspace     # unit + integration, offline and hermetic
make release               # what `scripts/bough` runs: target/release/bough
make server                # the server on a scratch BOUGH_HOME
make tui                   # the TUI against the local server
```

`docs/spec.md` is authoritative for product behavior; `specs/*.md` pin per-subsystem
contracts module by module, including the invariants that are not rediscoverable from
the spec (worker wind-down ordering, same-millisecond message ordering, replay
determinism, and the rest). Read the relevant spec before changing anything in `turn/`,
`harness/`, or `workflow/`. `docs/history/implementation-plan.md` is historical: it describes
a build order two rewrites old, and is worth reading only for its reasoning.

`specs/` and `docs/architecture.md` were written against the TypeScript implementation this
tree replaced, and still name `src/*.ts` modules in places. The *behavior* they pin is
current and binding; the file names are a map to where each contract now lives in
`crates/`. `docs/history/port-plan.md` is finished history, not a to-do list.

The only non-Rust runtime dependency is a JS runtime for the code-mode sidecar
(`crates/bough-core/src/harness/js/`): `bun` if it is on PATH, else `node`. It needs no
`node_modules`; the sidecar uses `node:*` builtins only.

Conventions that reviews enforce: every module opens with a comment stating the
invariant it holds; dependencies (db, clock, LLM client) are injected rather than
reached for; parsing and other core logic stay pure with `now` passed in; validation
happens at the boundary; tests sit next to the module they cover.

## Version control: plain git

This repo is a normal git checkout on `main`, and bough works in it **in place**, with no shadow store,
no per-session worktree, no overlay. Edits land in the real files immediately, so git is the only
record of what a session changed. Each session's starting sha is recorded in the database and drives
the Changes rail.

- Ship work with ordinary commits on `main`; branch first for anything you'd want reviewed as a PR.
- There is no snapshot store to salvage from; uncommitted work lives only in the working tree.

## Running against this tree

The live server runs the binary built from this working tree and is respawned by launchd when
killed, so a `make release` here replaces what the next restart runs. Point a development instance
somewhere else before exercising it: `BOUGH_HOME` relocates the data root and `BOUGH_PORT` moves the
listener.

Never point a bough session's workspace at this repository itself while testing server endpoints
that mutate a workspace. Use a scratch directory.
