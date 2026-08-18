# Extending bough

Four surfaces, in order of how much they are allowed to do, plus one directory that
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

Precedence: a plugin's skills rank below `~/.bough/skills` and above every foreign
directory; its extensions bind first, so a loose file of yours can shadow one of its
names.

The loose drop-boxes below are unchanged and still first-class. They are the files you
wrote for yourself, and a ten-line hook should not need a directory.

### Switching pieces of one off

A unit you install in one move has to come apart, or the only way to stop one piece is
to delete a file the plugin puts back on its next update. Every plugin, and everything
in one, has a switch:

```
bough plugins                            # what is installed, and what is on
bough plugins disable acme/extensions/gh.js
bough plugins enable acme/guard.lua
```

`⌥p` is the same switchboard in the panel; `⏎` toggles the row under the cursor.

An id is a plugin (`acme`) or one of its items: `acme/guard.lua`,
`acme/skills/review`, `acme/extensions/gh.js`. **A plugin that is off contributes
nothing, whatever its items say** — its hooks stop being a source, its skills are not
listed and cannot be loaded by name, its extensions are not bound. Its items keep their
own switches while it is off, so turning it back on restores the picture you left.

Switching a skill off hands its name back: the rung below wins, which is the difference
between "use the other `review`" and "break `review`".

Defaults are unchanged by there being a switch. A plugin's **hooks are off** until you
turn them on, the same as a cloned repo's and for the same reason: a plugin is the unit
you get from someone else, its Lua runs in-process on the next turn, and one keystroke
turning it on beats no way to un-run it. Its **skills and extensions are on** — a skill
does nothing until something names it, and an extension that stopped binding the day the
switch shipped would be a working setup broken by an upgrade. For those two the switch is
an opt-*out*.

The switches live in `~/.bough/plugins-state.json`, except a hook's, which stays in
`~/.bough/hooks-state.json` where it has always been — turning a hook off is a reload of
the interpreter, not a flag, and two files both claiming to know whether a hook is on is
the bug the split avoids. Which file holds which is an implementation detail; one id
namespace covers all of it.

MCP servers are not part of this: a bough plugin ships hooks, skills and extensions, and
a server is granted per session (`^p`). A Claude Code or Codex plugin's skills are not
either — those are turned off in the harness that installed them.

## Project rules

One file **per directory**, from the git root down: `AGENTS.md` if it exists, else
`CLAUDE.md`. Re-read every turn.

The fallback is per directory, never a second file: a directory with both is read exactly
as its author meant, and adding an `AGENTS.md` beside an existing `CLAUDE.md` is how a
project moves over one directory at a time. `~/.bough/AGENTS.md` applies everywhere.

## Skills

A skill is a folder with a `SKILL.md` the model can pull in on demand, and `/` in the
composer lists them. Seven ship bundled, compiled into the binary: `history`,
`wayfinder`, `domain-modeling`, `grilling`, `analyze-logs`, `flint` and
`prepopulate-tags`.

Resolution runs bundled → this repo → yours → the ones other tools already put on disk:

| Tier | Where |
|---|---|
| bundled | shipped in the binary |
| project | `.agents/skills`, `.claude/skills`; nearest directory wins, from the git root down |
| user | `~/.bough/skills` |
| plugin | `~/.bough/plugins/<name>/skills` |
| foreign user | `~/.claude/skills`, `~/.agents/skills` |
| foreign plugin | installed Claude Code plugins, then Codex marketplaces |

`.agents/` is the open standard Codex documents; `.claude/` is the vendor-specific one.
Both are read, so a repo already set up for another harness needs no porting and no
symlinks. `^k` lists what resolved and from where.

## Extensions

JavaScript bound into **every program's scope**, alongside the nineteen host functions.

Drop files in `~/.bough/extensions`, a plugin's `extensions/`, or `.agents/extensions`.
`*.js`, `*.mjs`, `*.cjs`,
`*.ts` at the top level, plus `<sub>/index.*` one level down so an extension with helper
modules can be a directory. Binding order is sorted, so it is stable across runs.

**An extension is not a tool.** The model's tool list never changes, because a
per-session entry in it would split the provider's prompt cache. It is one more name in
the program's scope, documented in one more prompt section.

The functions never cross the wire: the sidecar `require()`s the file and binds its
exports directly. Nothing reaches Rust. The consequence is deliberate: an extension has
no handle to the session (no db, no recorder, no artifacts, no `ask`). What it *does*
have is the bridged host functions, in scope exactly as they are for the program, so an
extension composes `bash()` rather than reimplementing it, and a shell run that way
still lands in the tag history.

## Hooks

Lua that runs inside bough's own lifecycle and is allowed to **change what happens
next**.

Five events, plus any a plugin defines with `exec_autocmds`:

`TurnStart` · `TurnEnd` · `TurnError` · `PreTool` · `PostTool`

Two kinds of change, and the line between them is the design:

- **Returned.** The callback's value decides the thing happening right now: deny a
  command, rewrite its input, replace its output, stop the turn. Synchronous; the caller
  is blocked on the answer.
- **Effected.** `bough.session.prompt(...)`, `bough.session.set_title(...)` and friends
  act on the session rather than the call in flight.

That second kind is the point. A hook that can only return a patch is a filter; these are
first-class enough to *start* work, not only veto it. The cost is running foreign code
in-process, which is accepted deliberately: the alternative (shelling out and reading a
JSON patch off stdout) means every new power needs a new field in a schema somebody owns.

The event surface is small on purpose. Claude Code fires about thirty events; four or
five is the better number, because every event is a compatibility promise and the ones
that earn it are the turn boundaries and the tool boundary.

```bash
bough hooks          # what is installed, what is on, how many listeners each has
```

Bundled: `claude-code.lua` and `codex.lua` ship **on**, running the hooks those harnesses
already have configured. What each reads:

| Adapter | Reads |
|---|---|
| `claude-code.lua` | `~/.claude/settings.json`, the project's `settings.json` / `settings.local.json`, and an installed plugin's hooks (`hooks/hooks.json` or wherever its `plugin.json` points) |
| `codex.lua` | `~/.codex/hooks.json`, the project's `.codex/hooks.json`, and `notify` from `config.toml` |

Both fold the same contract: exit 2 blocks with stderr as the reason, exit 0 with JSON on
stdout carries `permissionDecision` / `additionalContext` / `updatedInput`.
claude-code.lua also recognizes Git AI's standard checkpoint claude shell hook. Bough has no Claude JSONL transcript, so it translates the Bash event to Git AI's documented agent-v1 input instead. The repository directory comes from bough's resolved command workspace and the Bough session id becomes Git AI's conversation id.


Not adopted, deliberately: Codex's inline `[hooks]` TOML tables (hand-parsing nested TOML
fails toward *running the wrong command*, so it warns instead), Codex plugin hooks (a
marketplace lists what is *available*, not installed, and Codex itself will not run them
untrusted), and the ~25 events with no bough counterpart.

`guard-destructive.lua` and `redact-secrets.lua` ship **off**. Yours go in
`~/.bough/hooks`, a plugin's in `~/.bough/plugins/<name>/hooks`. `^x` toggles them live,
and `⌥p` toggles a plugin's alongside the rest of what it ships.

## MCP

A granted server's tool is called directly, with the arguments as an object:

```js
await mcp.call("SERVER", "TOOL", { arg: "value" })
```

The turn's prompt carries the catalog of what is connected, so the model knows what
exists without a tool-list entry per server, the same prompt-cache argument as
extensions.

Registering, granting and authorizing stay the human's job. `bough mcp` on its own
reports every server's state; `^p` is the panel. `bough sync-mcp` adopts Claude Code's
configured servers.
