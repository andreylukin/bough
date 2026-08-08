# bough documentation

Two audiences, in order.

**Using it**

| | |
|---|---|
| [install.md](install.md) | Requirements, install, models and keys, update, uninstall |
| [tui.md](tui.md) | The terminal UI — the one panel, every chord, how a session runs |
| [cli.md](cli.md) | Every subcommand: `exec`, `acp`, `mcp`, `tags`, `patterns`, `hooks` |
| [configuration.md](configuration.md) | Environment variables and what lives under `~/.bough` |
| [troubleshooting.md](troubleshooting.md) | When it does not start, does not answer, or answers wrong |

**Understanding it**

| | |
|---|---|
| [programs.md](programs.md) | The program environment and all eighteen host functions |
| [delegation.md](delegation.md) | Subagents and workflows |
| [extending.md](extending.md) | Skills, hooks, extensions, MCP, project rules |
| [tags.md](tags.md) | Command memory: how tagging makes past work recallable |
| [how-it-works.md](how-it-works.md) | Server, sidecars, the turn loop, the data model |

**Contributing**

| | |
|---|---|
| [architecture.md](architecture.md) | Crate boundaries, shared types, concurrency model |
| [spec.md](spec.md) | What the system is. Authoritative for product behavior |
| [../specs/](../specs) | Per-subsystem behavioral contracts — the invariants, module by module |
| [../.github/CONTRIBUTING.md](../.github/CONTRIBUTING.md) | Setup, the bar for a pull request |
| [../AGENTS.md](../AGENTS.md) | Conventions this repo's reviews enforce |
| [history/](history) | Finished and unmaintained — the old build order and the Rust port plan |

## Which file is right

`spec.md` and `specs/` are **normative**: when a page here disagrees with them, they
win and the page is a bug. They are also written for people changing bough, not people
using it — start above the line.

`how-it-works.md` and `architecture.md` are not the same thing. The first is how the
running system behaves — server, sidecars, the turn loop — and is for anyone who wants
to understand bough. The second is the contract between crates, and matters when you are
editing them.

Nothing in [history/](history) describes bough as it is today.
