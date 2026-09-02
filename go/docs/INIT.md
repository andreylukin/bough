# init.js — programmable configuration

The `init-js` plugin runs your init files in the shared codemode VM at
boot, with a global `bough` API. Files, in order:

1. `~/.bough/init.js` (global)
2. `./.bough/init.js` (project — overrides global)

Both are optional. Errors are loud: a JS exception, an unknown setup
key, or a bad style string fails the plugin's mount with a message
naming the problem. The config surface is sealed after the init files
run — calling `bough.setup` / `bough.provider` / `bough.cognition` /
`bough.project` later throws. `bough.tool` stays live (tools may be
registered at runtime, even by the model).

Because init files run in the same VM as the agent's code, they can use
anything already registered there (`tools.bash(...)`, etc.).

## API

### bough.setup(config)

Callable once per file. Unknown keys at any level are boot errors
naming the key. Full shape:

```js
bough.setup({
  ui: {
    theme:  { token: "style", ... },   // see Theme
    keymap: { action: "key", ... },    // see Keymap
  },
  provider: {
    default: "name",                   // a bough.provider() name; provides "llm"
  },
  system: {
    append: "text",                    // appended to the system prompt
  },
})
```

Maps merge: project-file entries override global-file entries;
untouched entries survive.

#### Theme

Provides the `"theme"` service (token → style), merged over the UI's Go
defaults. Tokens:

`user`, `assistant`, `code`, `result`, `error`, `accent`, `dim`,
`border`, `status`, `focus` (the block-cursor highlight)

Style syntax: `fg[:bg][:bold|italic|faint]`, colors as `#rrggbb` hex or
ANSI-256 numbers (`0`–`255`). Examples: `"#ff5f87"`, `"213:236:bold"`,
`"#c0c0c0:italic"`.

One special entry: `markdown: "dark"` or `"light"` pins the assistant
markdown (glamour) style. Without it the UI follows the detected
terminal background.

#### Keymap

Provides the `"keymap"` service (action → key), merged over Go
defaults. Actions:

`quit`, `scroll_up`, `scroll_down`, `page_up`, `page_down`,
`history_inspect`, `block_next` (tab), `block_prev` (shift+tab),
`collapse_toggle` (enter, on the focused block), `collapse_all`,
`expand_all`, `clear_input`

Keys use bubbletea names: `"ctrl+c"`, `"pgup"`, `"q"`, `"shift+tab"`.
`collapse_all` and `expand_all` ship unbound — they only fire when the
keymap service binds them.

#### Collapse

Whether code/result blocks *start* collapsed is not an init.js
setting: it is the ui row's config in `bough.yml` — `collapse: all`
(default: every block starts collapsed), `large` (only bodies over 3
lines), or `none` (everything starts expanded). Any other value is a
boot error.

#### provider.default

Names a provider registered with `bough.provider`. Naming an
unregistered provider is a boot error. When set, `init-js` provides the
`"llm"` service backed by that JS function — deliberately shadowing the
yml `llm` row (the kernel logs its last-write-wins warning; that is the
designed path).

#### system.append

Shorthand cognition: provides a `"cognition"` service that appends this
text to the built system prompt. Ignored (with a loud warning) when a
`bough.cognition` function is also registered.

### bough.tool(name, fn)

Registers `fn` as `tools.<name>` in the codemode VM. Arguments pass
through as-is; the return value is the tool's result.

```js
bough.tool("shout", function (s) { return s.toUpperCase() + "!" })
// model code: tools.shout("hi") -> "HI!"
```

### bough.command(name, usage, summary, fn)

Registers a slash command in the `"commands"` registry: it appears in
the composer's `/` palette and in `/help`, and dispatches when you
submit `/name args`. `fn(args)` receives everything after the name as
one trimmed string and must return a string (the system output); the
call runs in the codemode VM under its mutex, so `tools.*` is
available. A thrown JS exception (or a non-string return) becomes the
command's error output. An empty return echoes `/name` as a notice —
every command shows something.

```js
bough.command("branch", "", "current git branch", function () {
  return tools.bash("git branch --show-current").trim()
})
// composer: /branch  ->  dim system block with the branch name
```

Duplicate names (including the built-ins `/help`, `/sessions`,
`/clear`, `/collapse`, `/expand`, `/quit`) are boot errors. Like
`bough.tool`, `bough.command` stays live after init. Dispatched
command output is recorded in history as a `system` entry; a `/` line
never reaches the LLM.

### bough.provider(name, fn)

Registers a JS LLM provider: `fn(system, messages) -> string`, where
`messages` is `[{role, content}, ...]`. Only used when
`setup.provider.default` names it. Calls run under the VM mutex with
the codemode timeout; a JS exception surfaces as a completion error.

### bough.cognition(fn)

Provides the `"cognition"` service: `fn(baseSystem) -> string`
transforms or replaces the system prompt each step. A JS error is
logged and the base prompt is used unchanged.

### bough.project(fn)

Provides the `"projection"` service: `fn(entries) -> [{role, content}]`
derives the model messages from the append-only history each step.
Entries arrive as plain objects:

```js
{ seq: 3, at: "2026-09-01T12:00:00.000000000Z", kind: "result",
  data: { text: "...", code: "..." } }
```

Kinds: `input`, `assistant`, `code`, `result`, `error`, `done`, plus
`system` (dispatched slash-command output; carries no model-visible
text in the built-in projection). A JS
error or bad return shape is logged and the built-in projection
(input→user, assistant→assistant, result→user `[tool output]`) is used
for that step.

## Worked example

`~/.bough/init.js`:

```js
bough.setup({
  ui: {
    theme: {
      user: "#87d7ff:bold",     // my lines, cyan bold
      error: "#ff5f5f",
    },
    keymap: {
      quit: "ctrl+q",
      history_inspect: "ctrl+h",
    },
  },
  system: {
    append: "Prefer ripgrep over grep. Never push to git remotes.",
  },
})

// a tiny local tool
bough.tool("today", function () {
  return tools.bash("date +%F").trim()
})
```

Boot output on a typo (`theem`, `scrolll_up`, `"redish"`) names the bad
key or value and refuses to mount — fix the file and the config
hot-reload remounts the row.
