# Extending bough

Four surfaces, in order of how much they are allowed to do — and one directory that
bundles three of them.

## Plugins

A plugin is **one directory** under `~/.bough/plugins`:

```
~/.bough/plugins/acme/
  hooks/guard.lua        -> the hook id is acme/guard.lua
  skills/review/SKILL.md -> /review
  extensions/gh.js
```

Any of the three may be absent. The **directory name is the identity**, which is what
makes two plugins able to ship the same `guard.lua`.

Plugin hooks are **off** until you turn them on (`^x`), the same as a cloned repo's and
for the same reason: a plugin is the unit you get from someone else, and one keystroke
turning it on beats no way to un-run it. Skills and extensions have no such switch —
they do nothing until something names them.

Precedence: a plugin's skills rank below `~/.bough/skills` and above every foreign
directory; its extensions bind first, so a loose file of yours can shadow one of its
names.

The loose drop-boxes below are unchanged and still first-class. They are the files you
wrote for yourself, and a ten-line hook should not need a directory.

## Project rules

One file **per directory**, from the git root down: `AGENTS.md` if it exists, else
`CLAUDE.md`. Re-read every turn.

The fallback is per directory, never a second file: a directory with both is read exactly
as its author meant, and adding an `AGENTS.md` beside an existing `CLAUDE.md` is how a
project moves over one directory at a time. `~/.bough/AGENTS.md` applies everywhere.

## Skills

A skill is a folder with a `SKILL.md` the model can pull in on demand, and `/` in the
composer lists them. Four ship bundled: `history`, `wayfinder`, `domain-modeling`,
`grilling`.

Resolution runs bundled → yours → the ones other tools already put on disk:

| Tier | Where |
|---|---|
| bundled | shipped in the binary |
| user | `~/.bough/skills` |
| project | `.agents/skills`, `.claude/skills` — nearest directory wins, from the git root down |
| plugin | `~/.bough/plugins/<name>/skills` |
| foreign user | `~/.claude/skills`, `~/.agents/skills` |
| foreign plugin | installed Claude Code plugins, then Codex marketplaces |

`.agents/` is the open standard Codex documents; `.claude/` is the vendor-specific one.
Both are read, so a repo already set up for another harness needs no porting and no
symlinks. `^k` lists what resolved and from where.

## Extensions

JavaScript bound into **every program's scope**, alongside the eighteen host functions.

Drop files in `~/.bough/extensions`, a plugin's `extensions/`, or `.agents/extensions` —
`*.js`, `*.mjs`, `*.cjs`,
`*.ts` at the top level, plus `<sub>/index.*` one level down so an extension with helper
modules can be a directory. Binding order is sorted, so it is stable across runs.

**An extension is not a tool.** The model's tool list never changes, because a
per-session entry in it would split the provider's prompt cache. It is one more name in
the program's scope, documented in one more prompt section.

The functions never cross the wire: the sidecar `require()`s the file and binds its
exports directly. Nothing reaches Rust. The consequence is deliberate — an extension has
no handle to the session (no db, no recorder, no artifacts, no `ask`). What it *does*
have is the bridged host functions, in scope exactly as they are for the program, so an
extension composes `bash()` rather than reimplementing it — and a shell run that way
still lands in the tag history.

## Hooks

Lua that runs inside bough's own lifecycle and is allowed to **change what happens
next**.

Five events, plus any a plugin defines with `exec_autocmds`:

`TurnStart` · `TurnEnd` · `TurnError` · `PreTool` · `PostTool`

Two kinds of change, and the line between them is the design:

- **Returned** — the callback's value decides the thing happening right now: deny a
  command, rewrite its input, replace its output, stop the turn. Synchronous; the caller
  is blocked on the answer.
- **Effected** — `bough.session.prompt(...)`, `bough.session.set_title(...)` and friends
  act on the session rather than the call in flight.

That second kind is the point. A hook that can only return a patch is a filter; these are
first-class enough to *start* work, not only veto it. The cost is running foreign code
in-process, which is accepted deliberately — the alternative (shelling out and reading a
JSON patch off stdout) means every new power needs a new field in a schema somebody owns.

The event surface is small on purpose. Claude Code fires about thirty events; four or
five is the better number, because every event is a compatibility promise and the ones
that earn it are the turn boundaries and the tool boundary.

```bash
bough hooks          # what is installed, what is on, how many listeners each has
```

Bundled: `claude-code.lua` and `codex.lua` ship **on**, running the hooks those harnesses
already have configured in a project — including a Claude Code plugin's own hooks,
whether it keeps them at `hooks/hooks.json` or points at them from its `plugin.json`.
`guard-destructive.lua` and `redact-secrets.lua` ship **off**. Yours go in
`~/.bough/hooks`, a plugin's in `~/.bough/plugins/<name>/hooks`. `^x` toggles them live.

## MCP

There is no MCP host function. A granted server's tool is called through the shell:

```js
await bash(`bough mcp call SERVER TOOL '{"arg":"value"}'`, "mcp:call:thing")
```

The turn's prompt carries the catalog of what is connected, so the model knows what
exists without a tool-list entry per server — the same prompt-cache argument as
extensions.

Registering, granting and authorizing stay the human's job. `bough mcp` on its own
reports every server's state; `^p` is the panel. `bough sync-mcp` adopts Claude Code's
configured servers.
