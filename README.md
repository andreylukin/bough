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
  are provided. Row order carries no semantics. Unresolvable deps fail
  loud, naming the row and missing key.

Plugins register via `kernel.Register(name, factory)` in their `init()`
and are wired together only through service keys:

| key        | provided by        | consumed by     |
|------------|--------------------|-----------------|
| `llm`      | plugins/llm        | loop            |
| `codemode` | plugins/codemode   | tools, loop     |
| `runner`   | plugins/loop       | (internal)      |
| `inputs`   | plugins/loop       | ui              |
| `ui-mode`  | launcher           | ui              |

## Running

```sh
go build ./cmd/bough

./bough                      # native TUI (bubbletea)
./bough --web 127.0.0.1:7681 # browser UI (sip)
./bough --headless           # stdin/stdout
./bough --set llm.model=claude-haiku-4-5   # override any row config
./bough --set llm.plugin=llm-echo          # swap a row's plugin
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
