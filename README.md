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
| `cognition` | plugins/initjs    | loop (optional) |
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
./bough -c                   # resume the most recent session
./bough -r [id]              # resume by id, or pick from a list
```

The default `llm-anthropic` provider needs `ANTHROPIC_API_KEY` set (and
a `model` in config). No key handy? Smoke-test with the echo provider:

```sh
printf "say CODE! please\n" | ./bough --headless --set llm.plugin=llm-echo
```

Swap the LLM permanently by editing the `llm` row in `bough.yml`
(`llm-echo` instead of `llm-anthropic`).

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
