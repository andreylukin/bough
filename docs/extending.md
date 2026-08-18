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

### Switching any of it off

A unit you install in one move has to come apart, or the only way to stop one piece is
to delete a file the plugin puts back on its next update. Every plugin, and everything
in one, has a switch:

```
bough config                            # everything installed, and what is on
bough config disable acme/extensions/gh.js
bough config enable acme/guard.lua
bough config disable bundled/skills/wayfinder
```

It is not only plugins. **One listing covers every hook, skill and extension the
harness injects**, whatever surface implemented it and wherever it came from — the ones
that ship with bough, the ones you wrote, the ones this checkout ships, the ones a
cloned repo ships, and the ones inside a plugin.

`^x` is the same switchboard in the panel (`⌥p` still lands there too). It opens on the
list of SOURCES, collapsed — one row each, with what that source ships (`acme · 1 hook ·
1 skill · 2/3 on`). `⏎` expands the one under the cursor, showing everything inside it
indented underneath, and `⏎` on one of those switches it on or off. `x` switches the row
under the cursor whichever kind it is, so turning a whole source off never needs
expanding it first, and the legend follows the cursor so the row you are on always says
what `⏎` will do to it. `esc` collapses before it closes the panel.

An id is a **source** — `bundled`, `local` (yours), `project`, `claude-code`, `codex`, a
cloned repo's slug, or a plugin's name — or one thing inside one: `acme/guard.lua`,
`local/skills/mine`, `acme/extensions/gh.js`. **A source that is off contributes nothing, whatever the things
inside it say** — its hooks stop being LOADED (so the listeners they registered are gone,
which is the only way to un-register one), its skills are not listed and cannot be
loaded by name, its extensions are not bound. Those things keep their own switches while
it is off, so turning it back on restores the picture you left.

Switching a skill off hands its name back: the rung below wins, which is the difference
between "use the other `review`" and "break `review`".

### The harness sections

Everything bough adopts from **another harness** sits under that harness's name rather
than in one "foreign" pile: `claude-code` holds the adapter that reads
`~/.claude/settings.json` and its hooks, the skills in `~/.claude/skills`, and the skills
of every installed Claude Code plugin; `codex` holds the same for `~/.agents` and Codex
marketplaces. So "stop taking anything from Claude Code" is one switch, and each thing it
brought is still switchable on its own.

The line under the list names every directory a source was read from, and the exact file
or folder a thing IS — which is how you tell the `~/.claude/skills/review` you wrote from
the one that arrived inside a plugin.

Defaults are unchanged by there being a switch. A **hook you did not write is off** until you
turn it on — bundled, cloned or a plugin's, all for the same reason: it arrived rather
than being written, its Lua runs in-process on the next turn, and one keystroke turning
it on beats no way to un-run it. **Skills and extensions are on** — a skill
does nothing until something names it, and an extension that stopped binding the day the
switch shipped would be a working setup broken by an upgrade. For those two the switch is
an opt-*out*.

Every switch lives in `~/.bough/switches.json` — one file, because there is one id
namespace and one question. The stores it replaces (`hooks-state.json`,
`plugins-state.json`, `hooks-disabled.json`) are folded in on read, so an existing
machine keeps every switch it had set. Turning a hook off is still a reload of the
interpreter rather than a flag read at dispatch: a disabled hook has to stop existing,
because the listener it registered at load does not unregister itself.

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
alongside every skill and extension.

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
