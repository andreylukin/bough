# specs — per-subsystem behavioral contracts

Seventeen files pinning the invariants you cannot rediscover by reading the code: worker
wind-down ordering, same-millisecond message ordering, replay determinism, the exact SSE
framing, and the rest. Read the relevant one before changing anything in `turn/`,
`harness/` or `workflow/`.

## Read this first

These were written against the TypeScript implementation this tree replaced, so they are
titled "Port spec" and still name `src/*.ts` modules in places. That tree is gone.

**The behavior they pin is current and binding. The file names are a map, not a path** —
each contract now lives in `crates/`, per the table below. Where a spec and the code
disagree on behavior, the spec wins and the code is a bug; where a spec names a file
that no longer exists, the spec is describing where the contract *came from*.

`docs/spec.md` is authoritative for product behavior overall, and `specs/docs.md` is the
system-level invariant list — on conflict between it and a per-subsystem file here, it
wins. Anything under `docs/history/` is finished history and describes nothing current.

## The files

| Spec | Pins the contract for |
|---|---|
| [docs.md](docs.md) | The system-level invariant list — every invariant a change must preserve |
| [root.md](root.md) | Event bus, error taxonomy, `~/.bough` layout, schedule ticker, scratch dirs, shared ports |
| [db.md](db.md) | SQLite persistence, the `Db` port, migrations, the wire schemas |
| [server.md](server.md) | The HTTP surface — routes, SSE framing, app wiring. **This file IS the API contract** |
| [turn.md](turn.md) | The turn runner, queue, replay, state machine |
| [harness.md](harness.md) | Code-mode VM workers and the sidecar protocol |
| [hostfn.md](hostfn.md) | The host functions programs call |
| [llm.md](llm.md) | The provider boundary — routing, retry, pricing, prefix caching |
| [history.md](history.md) | Command-tag memory and the history tree operations |
| [agents.md](agents.md) | Subagents-as-branches — caps, launch, notes |
| [mind.md](mind.md) | Persistent agency — the wake driver, steps, backoff, rollups. Written for the Rust tree, not a port spec |
| [workflow.md](workflow.md) | The workflow engine — journal, replay, control |
| [mcp.md](mcp.md) | MCP config, clients, grants, OAuth |
| [cli.md](cli.md) | The headless subcommands |
| [tui-core.md](tui-core.md) | TUI state, reduce, selectors, event loop |
| [tui-components.md](tui-components.md) | The rendered components |
| [small.md](small.md) | Five small subsystems: `logs/`, `worker/`, `vcs/`, `skills/`, `prompt/` |

## Where each named module now lives

The specs' `src/<x>.ts` is `crates/bough-core/src/<x>.rs` unless listed here.

| Named in the specs | Now |
|---|---|
| `src/types.ts`, `src/bus.ts`, `src/errors.ts`, `src/paths.ts`, `src/scratch.ts`, `src/schedules.ts` | `bough-core/src/` — same names, `.rs` |
| `src/schema/{parts,events,requests}.ts` | `bough-core/src/schema/` |
| `src/db/db.ts` | `bough-core/src/db/sqlite_db.rs` (the `Db` trait is declared in `bough-core/src/types.rs`) |
| `src/history/{record,stats,embed}.ts` | `bough-core/src/history/tags/` |
| `src/prompt/assemble.ts`, `src/prompt/*.md` | `bough-core/src/prompt/assemble.rs`, `bough-core/src/prompt/sections/*.md` |
| `src/harness/{vm_worker,wf_worker}.ts` | `bough-core/src/harness/js/` — still JavaScript, run in a sidecar |
| `src/harness/protocol.ts` | `bough-core/src/harness/protocol.rs` |
| `src/server/*.ts` | `bough-server/src/` |
| `src/tui/*.ts` | `bough-tui/src/` |
| `src/cli/*.ts` | `bough/src/` |
| `src/skills/*/SKILL.md` | `bough-core/skills/` |

Two naming shifts run throughout: identifiers are `snake_case` in Rust
(`normalizeTags` → `normalize_tags`), and the zod schemas the specs describe are serde
types in `bough-core/src/schema/`. Wire field names are unchanged — they are the
parity anchor.
