# bough

A very basic agent harness where **everything is a plugin** (modeled on
[DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness)).

## Architecture

The kernel (`kernel/`) is the only non-plugin code besides the launcher
(`cmd/bough/`). It provides:

- **Services**: `ctx.Provide(key, value)` / `kernel.Get[T](ctx, key)` —
  typed lookup, error if absent.
- **Events**: `ctx.On(event, fn)` / `ctx.Emit(event, payload)` —
  fire-and-forget, listener panics contained.
- **Effects**: `ctx.Effect(dispose)` — disposers run LIFO on unmount.
- **Loader**: parses `bough.yml` (a list of rows `{id, plugin, config,
  disabled}`), mounts each enabled row once its plugin's `Inject()` keys
  are provided. Row order only breaks ties within a mount pass (which is
  why the optional loop seams sit above the loop row in `bough.yml`).
  At boot, unresolvable deps fail loud, naming the row and missing key.
- **Lifecycle**: every row is in one of four states — `pending`
  (waiting on services), `active`, `failed` (Apply error; isolated, not
  retried until its spec changes), `disabled`. The kernel tracks not
  just `Inject()` deps but every service a row `Get`s during Apply, so
  when a service lands, changes, or is withdrawn, its dependents remount
  automatically — add a `history` row to a running session and the loop
  picks it up. `./bough rows` prints the live state table.

Plugins register via `kernel.Register(name, factory)` in their `init()`
and are wired together only through service keys:

| key        | provided by        | consumed by     |
|------------|--------------------|-----------------|
| `llm`      | plugins/llm        | loop            |
| `codemode` | plugins/codemode   | tools, loop     |
| `runner`   | plugins/loop       | (internal)      |
| `inputs`   | plugins/loop       | ui              |
| `ui-mode`  | launcher           | ui              |
| `hooks`    | plugins/hooks      | loop (optional) |
| `skills`   | plugins/skills     | loop (optional) |
| `context-md` | plugins/contextmd | loop (optional) |
| `history`  | plugins/history    | loop, ui (optional) |
| `todo`     | plugins/todo       | (tools.todo, /todo) |
| `ask-answers` | plugins/ask     | ui              |
| `cognition` | plugins/initjs, plugins/todo (chained) | loop (optional) |
| `projection` | plugins/initjs   | loop (optional) |
| `theme`    | plugins/initjs     | ui (optional)   |
| `keymap`   | plugins/initjs     | ui (optional)   |

The optional seams are resolved at mount time and no-op cleanly when
their rows are absent; thanks to Get-tracking they also hot-attach when
a provider row appears later.

## Running

```sh
go build ./cmd/bough

./bough                      # native TUI (bubbletea)
./bough --web 127.0.0.1:7681 # browser UI (sip)
./bough --headless           # stdin/stdout
./bough --set llm.model=claude-haiku-4-5   # override any row config
./bough --set llm.plugin=llm-echo          # swap a row's plugin
./bough rows                 # print the row state table and exit
./bough log                  # pretty-print the latest session's history
./bough log <file> --raw     # a specific history file, raw JSONL
./bough sessions             # list sessions, newest first
./bough mcp list             # configured MCP servers (plugin command)
./bough mcp tools [server]   # their tools (all, or one server); mcp search <q> finds one
./bough mcp status           # does each server answer; mcp call <srv/tool> [args] runs one
./bough -c                   # resume the most recent session
./bough -r [id]              # resume by id, or pick from a list
./bough update               # git pull + rebuild + bounce the web session
./bough restart              # bounce the running --web session
./bough --version            # print "bough <version>"
./bough --help               # flags, subcommands, config locations
```

Flags take `--long` or `-long`; `-c`/`--continue` and `-r`/`--resume`
are the short pairs. Config comes from `./bough.yml`, else
`~/.bough/bough.yml`, else an embedded default (`--config <path>` to
be explicit); `~/.bough/init.js` is the startup script.

Output contract: `--headless` prints `[assistant]`, `[done]` and the
other event lines on **stdout** and `[error]` lines on **stderr**, and
exits 1 if any turn errored (0 otherwise; `/quit` and ctrl+c in the TUI
exit 0). An unknown subcommand exits 2. Kernel/MCP/config diagnostics
(row reloads, `N tools bound`, which config file loaded) are quiet
unless `--verbose` or `BOUGH_VERBOSE=1`; failures and warnings always
print on stderr.

The default `llm-anthropic` provider needs `ANTHROPIC_API_KEY` set (and
a `model` in config). No key handy? Smoke-test with the echo provider:

```sh
printf "say CODE! please\n" | ./bough --headless --set llm.plugin=llm-echo
```

Swap the LLM permanently by editing the `llm` row in `bough.yml`
(`llm-echo` instead of `llm-anthropic`).

## Updating

`bough update` finds the checkout (walking up from the binary, then
`$BOUGH_ROOT`, then `~/repos/bough`), runs `git pull --ff-only`,
rebuilds the binary in place (an installed copy, e.g.
`~/.local/bin/bough`, is replaced atomically), and bounces the web
session if one is running. A `--web` session records itself in
`~/.bough/web.pid`; `bough restart` alone SIGINTs that recorded pid,
waits for it to exit, and relaunches `--web` on the same addr detached
(output appends to `~/.bough/web.log`). With no running web session
both just note that sessions pick up the new binary on next launch.

## The codemode loop

The loop plugin waits for human input, then asks the LLM for a response.
The LLM writes JavaScript; the codemode plugin runs it in a goja runtime
where each registered tool is a JS function. Tool output feeds back to
the LLM until it's done, and every step is emitted as a `loop/event`
(`assistant`, `code`, `result`, `error`, `done`) that any UI renders.

## History

Conversation state lives in an append-only entry log, not in the loop.
Every turn appends `input`, `assistant`, `code`, `result`, `error`,
`done` entries; each step's model messages are projected from the log
(`projection` service, or the built-in default). With the `history` row
mounted the log is durable JSONL in `~/.bough/history/<ts>-<pid>.jsonl`
— inspect it with `./bough log`, or Ctrl+O in the TUI — and the
conversation survives loop remounts (e.g. swapping the llm row live).
Without the row the loop keeps an in-memory log and everything still
works.

## Sessions

Every run writes one JSONL file under `~/.bough/history/`; resuming
reopens that same file for append (entries load into memory, `seq`
continues from max+1 — append-only is never violated) and the UI
replays the transcript as the same blocks a live session renders.
Because model context is projected from history every step, a resumed
session picks up the full prior conversation with no extra machinery.

```sh
bough sessions        # id · local time · entry count · first-input title
bough -c              # --continue: most recent session (by file mtime)
bough -r <id>         # --resume: that session (id with or without .jsonl)
bough -r              # tui/web: session picker before the chat view
                      # headless: prints the session list, exit 2
```

The picker (Claude Code-style) lists sessions newest first — local
time, entry count, first input truncated to 60 columns — ↑/↓ select,
enter resumes, esc starts a fresh session. The status bar always shows
the live session file.

Long-term storage is TBD; the `history` row (its `file` config is how
resume works today) is the swap point for a future store.

## init.js

User configuration is JavaScript, run in the shared codemode VM:
`~/.bough/init.js` then `./.bough/init.js` (both optional). A global
`bough` API registers things Maki-style — typos fail the row loud,
naming the key:

```js
bough.setup({
  ui: {theme: {accent: "#7aa2f7"}, keymap: {quit: "ctrl+q"}},
  provider: {default: "parrot"},
  system: {append: "reply tersely"},
});
bough.tool("shout", (s) => s.toUpperCase());          // a codemode tool
bough.provider("parrot", (sys, msgs) => "...");        // a full LLM provider
bough.cognition((base) => base + "\nmore");            // system prompt transform
bough.project((entries) => [{role: "user", content: "..."}]); // history -> messages
```

See `docs/INIT.md` for the full surface.

## Commands

Typing `/` at the start of the composer opens an fzf-style palette
over the `commands` service (the `commands` plugin's registry). The
filter is pure and keyed on the name alone — prefix matches, then
substring, then subsequence (`mnr` finds `monarch`), each tier
alphabetical — so a growing query only ever removes rows and the
selection is never swapped out under the typist. Up/Down move the
selection (wrapping), Tab completes the draft to `/name ` and keeps
the palette open, Enter dispatches, Esc closes; everything else falls
through to the composer, which re-filters.

A submitted `/` line never reaches the LLM: the UI dispatches it
through the registry and renders the output as a dim `system` block
(headless prints `[system] <output>`). Unknown names answer
`unknown command: /x (try /help)`. Built-ins: `/help`, `/sessions`,
`/model`, `/clear` (visible transcript only; history untouched),
`/collapse`, `/expand`, `/quit`. UI-owned effects come back to the UI
as a typed `UIAction` sentinel on the error channel — the registry
itself never touches the UI.

**`/model`** shows or live-swaps the LLM provider row. Bare `/model`
prints the current row (`plugin · model`), the registered `llm-*`
providers, and usage. `/model <provider> [model]` swaps the row's
plugin (and optionally model); `/model <model>` keeps the plugin and
changes only the model. The swap is a real runtime reconcile — the llm
row remounts and the loop follows — and it survives a config hot
reload. It needs the bough launcher (`bough`, `--web`, `--headless`);
in bare embeddings without the `config-set` service the swap errors
and only showing works.

**`!` bash mode.** A composer line starting with `!` runs the rest
directly as `sh -c` (60s timeout, the process cwd) and never reaches
the LLM. The output lands as a collapsible result block labeled
`! <cmd>` (expanded by default — you asked for it), with a loud
trailing `! exit status N` / `! timeout` line on failure and
`(no output)` for silence. Headless prints `[system] <output>`. Both
halves are recorded to history as `command`/`system` entries, which the
default projection hides from the model.

Register your own from init.js:

```js
bough.command("shout", "<text>", "uppercase the args", (args) => args.toUpperCase());
```

## Claude-Code parity: subagents, todo, ask

Three plugins close the day-to-day gap with Claude Code. Each is one
`bough.yml` row — delete or `disabled: true` the row to turn it off.

**Subagents (`workers` row).** The model calls `tools.spawn(task)` from
a code block: a bounded child agent (its own step loop, same LLM and
tools, depth 1 — a child cannot spawn) runs the task and its final
plain-text reply is the tool's return value. The child's activity
streams into the transcript and history as `sub:assistant`, `sub:code`,
`sub:result`, `sub:error`, `sub:done` entries tagged with a worker
number. Config: `max_spawns` (per parent turn, default 4), `max_steps`
(child steps, default 6).

**TODO list (`todo` row).** Three surfaces over one list: the `/todo`
command (`/todo`, `/todo add <text>`, `/todo done <id>`, `/todo
clear`), `tools.todo` for the model (`add(text) -> id`, `done(id)`,
`list()`), and a live "Current TODO list:" section appended to the
system prompt each step (`inject_prompt: false` turns that off). State
is derived from history entries (`todo/add`, `todo/done`, `todo/clear`)
so the list survives session resume; ids are never reused. Rendered
lines are Claude-style checkboxes: `[ ] 1 buy milk`, `[x] 2 done
thing`.

**Interactive questions (`ask` row).** The model calls
`tools.ask(question, options...)`: the turn blocks, the UI shows the
question with numbered options, and your next composer submission is
the answer — a bare number picks that option, anything else is
freeform, clicking an option row answers with it, esc declines. The
answer returns as the tool's output and both halves are history entries
(`ask`, `ask/answer`). Headless: `[ask] question` + numbered lines
print, the next stdin line answers. Unanswered asks expire (config
`timeout_minutes`, default 10) and render as `(expired)`.

## TUI

Semantic transcript (themed prompt, markdown-rendered assistant, status
bar with row/entry counts and a spinner while a turn runs). Code and
result blocks render behind a one-line disclosure header — `▸ code js
(N lines): preview` — and all start collapsed by default; the ui row's
`collapse` config in `bough.yml` picks the mode (`all` default,
`large` collapses only bodies over 3 lines, `none` starts everything
expanded). Click anywhere in a block to toggle it (mouse works in the
native TUI and through the web terminal alike), and click a history
inspector row to expand its entry as inline JSON. Default keys, all
remappable via the `keymap` service: Ctrl+C quit, Ctrl+O history
inspector, Tab/Shift+Tab move the block cursor (styled by the `focus`
theme token), Enter toggles the focused block (submits input
otherwise), Ctrl+L clear input, arrows/PgUp/PgDn/mouse wheel scroll;
`collapse_all`/`expand_all` are available unbound. Colors come from the
`theme` service (`"fg[:bg][:bold|italic|faint]"`, hex or ANSI-256).

## Hooks (hooks-js)

Hooks are plain `.js` files run in the **shared codemode VM** — the same
runtime model code executes in. This is deliberate: hooks see the
persistent globals model code created and can call `tools.*` themselves.
It also means a hook can clobber those globals; there is no isolation.

A hook file's text is the **body** of `function(event){...}`. It may
`return` an object, or nothing:

```js
// ~/.bough/hooks/user-prompt-submit/tag.js
return {input: event.input + "\n(reply tersely)"};
```

Discovery: `~/.bough/hooks/<event>/*.js` plus `./.bough/hooks/<event>/*.js`;
a project file shadows a global one with the same file name. Files run in
base-name order and are **re-read on every fire** — edit them live, no
restart. Results merge in file order (later keys win); a `block`/`deny`
key short-circuits remaining files. A file that fails to read or run is
logged to stderr and skipped, never fatal.

Events and honored result keys:

| event                | payload             | result keys                       |
|----------------------|---------------------|-----------------------------------|
| `session-start`      | `{}`                | `context` → appended to system prompt |
| `user-prompt-submit` | `{input}`           | `block` → refuse turn; `input` → rewrite |
| `pre-code-exec`      | `{code}` per block  | `deny` → skip, model sees `[hook denied: ...]`; `code` → rewrite |
| `post-result`        | `{code, result}`    | `result` → rewrite                |
| `stop`               | `{}`                | —                                 |
| `session-end`        | `{}`                | — (fires at unmount, not from the loop) |

## Memory (graphiti)

Long-term memory on [getzep/graphiti](https://github.com/getzep/graphiti),
self-hosted with no Docker: `bough graphiti install` clones Graphiti's
`mcp_server`, builds its venv (python 3.12, `uv`) with `falkordblite` (an
embedded FalkorDB: one file, `~/.bough/graphiti/graph.db`), and runs the
stock MCP server under launchd (`com.bough.graphiti`) on
`http://127.0.0.1:8621/mcp/`. One server, every bough talks to it over
http — the embedded database is single-process, so nothing spawns its own.

The loop is two hook files the install writes:

| hook                 | does                                                        |
|----------------------|-------------------------------------------------------------|
| `user-prompt-submit` | `search_memory_facts` for the prompt, appended as a `[memory]` block |
| `stop`               | `add_memory` of `{input, reply}`, backgrounded (`nohup … &`) |

Both go through `bough mcp call graphiti/…`; a server that is down is
silence, never a blocked turn. The `graphiti` row only adds a prompt
section naming the memory. Extraction and embeddings use OpenAI by default
(`OPENAI_API_KEY` from `~/.bough/env`; model `gpt-5-mini`, embedder
`text-embedding-3-small`); `llm: openrouter` routes both through OpenRouter
on `OPENROUTER_API_KEY` instead. Row config
`{port, llm: openai|openrouter, model, embedder}` overrides, then
`bough graphiti install` again rewrites the plist. Attribute-free entity
types are on purpose: the typed built-ins fail validation on small models.

`bough update` runs `graphiti install` after the rebuild (skipped, with a
hint, when `uv` is not on PATH), so a new machine is `brew install uv libomp`,
the llm's key in `~/.bough/env`, then `bough update`. The
install also adds the `graphiti` row to `~/.bough/bough.yml` after its `mcp`
row when that file exists. `bough graphiti status | logs | start | stop |
uninstall` (uninstall keeps the checkout and the graph).

## Skills

Mention-triggered injection. Pools: `~/.claude/skills` and
`./.claude/skills` (project shadows global), layout
`<pool>/<name>/SKILL.md`. A skill whose directory name appears as a
case-insensitive whole word in the human input gets its SKILL.md
appended to that turn's user message as `[skill: <name>]\n<body>`,
capped at 3 per turn.

## Context files

At session start, whichever exist of `./AGENTS.md`, `./CLAUDE.md`,
`~/.claude/CLAUDE.md`, `~/.bough/BOUGH.md` are prepended to the system
prompt, each labeled with its path.

## MCP

The `mcp` row spawns stdio MCP servers and binds their tools into
codemode as `tools.mcp_<server>_<tool>` (names sanitized to
`[A-Za-z0-9_]`). Sources merged by server name, highest precedence
first: row `config.servers` > `./.mcp.json` `mcpServers` >
`~/.claude.json` `mcpServers`; `config.disable: [names]` removes
entries after the merge. Non-stdio (url/http) entries are skipped with
a log line; a server that fails to connect is skipped, never fails the
mount.

## Hot reload

The launcher watches the `--config` file (fsnotify, 300ms debounce) and
reconciles per row: changed/removed rows are unmounted (plus their
dependent closure, transitively — swapping the `llm` row remounts the
loop, but with the `history` row mounted the conversation survives:
projection replays the log), added rows are mounted. Rows whose deps
aren't satisfied stay `pending` (visible in `./bough rows`) instead of
tearing the tree down; a row that fails Apply is isolated as `failed`
until its spec changes. A config that fails to parse or validate keeps
the last good tree and logs loudly. `--set` overrides are re-applied on
every reload.

## Testing

Three e2e layers on top of the per-package unit tests, all designed to
run fully parallel: every test gets its own temp HOME, temp cwd, config
copy, and (where one is needed) its own bough process and port. The LLM
is always deterministic — `llm-echo` or a JS parrot provider from
`init.js` — never a real API.

**1. teatest model tests** (`plugins/ui`, `internal/uitest`, and the
`*_tui_test.go` files in each plugin): the real bubbletea model driven
in-process — typed keys, loop events, rendered frames, golden files
(`go test ./plugins/ui -run Golden -update` to regenerate).

```sh
go test -race ./plugins/... ./internal/... ./kernel/
```

**2. Headless + PTY binary e2e** (`e2e/`): `TestMain` builds the binary
once per run (or reuses `$BOUGH_BIN`), then each test execs it —
`--headless` with stdin lines in and `[kind] text` events out, the CLI
subcommands (`bough log`, `bough rows`), config hot-reload, and 3
native-TTY cases on a real PTY (status bar renders, echo roundtrip,
quit restores the terminal). PTY cases skip on Windows/no-PTY.

```sh
go test -race ./e2e/
```

**3. Playwright web e2e** (`tests/web/`): real `bough --web` processes,
a real browser reading the sip/WebTerm buffer.

```sh
cd tests/web && npm ci && npx playwright install chromium && npm test
```

Parallelism knobs: `go test -parallel N` (Go layers; every test calls
`t.Parallel()`), `npx playwright test --workers=N` or `--shard=k/n`
(web layer; defaults to one worker per CPU).

CI runs all three on every push/PR to `go-rewrite` — see
`.github/workflows/ci.yml` (one `unit` job for both Go layers, a
4-shard `web-e2e` matrix; traces uploaded as artifacts on failure).
