# Writing a plugin

Everything in bough except the kernel and the launcher is a plugin: the
LLM providers, the loop, the tools, the TUI, memory, MCP, hooks, skills.
Adding behaviour means adding a row, not changing the loop.

This walks through [`plugins/example/example.go`](../plugins/example/example.go),
a working plugin that is compiled into the binary and tested in CI. It
is about 90 lines and does four things you will almost always want:
read and validate config, provide a service, use optional services when
they happen to be mounted, and clean up after itself.

Try it first. Add the row to your `bough.yml`:

```yaml
- id: wordcount
  plugin: example-wordcount
  config:
    min_length: 4
```

Then in a session, `/wordcount the quick brown fox`, or from a code
block:

```js
console.log(tools.wordcount("the quick brown fox jumps over the lazy dog"))
// { quick: 1, brown: 1, jumps: 1, over: 1, lazy: 1 }
// "the", "fox" and "dog" are under min_length and dropped.
```

`bough rows` shows it as `active`.

## The interface

```go
type Plugin interface {
	Name() string
	Inject() []string
	Apply(ctx *Context, cfg map[string]any) error
}
```

Register a factory from `init()` under the name rows will use:

```go
func init() {
	kernel.Register("example-wordcount", func() kernel.Plugin { return plugin{} })
}
```

`init()` runs because `cmd/bough` imports every plugin package for its
side effects. **A new plugin package needs its blank import added
there**, or nothing will know it exists.

## Inject: hard dependencies

`Inject()` lists service keys that must exist *before* `Apply` runs.
The kernel holds the row as `pending` until they do, so anything named
here can be fetched in `Apply` without checking for absence.

List only what the plugin genuinely cannot work without. At boot, a
dependency nothing provides fails loudly and names the row and the
missing key.

## Apply: mounting

`Apply` runs again whenever a service it read changes, so it must be
safe to call more than once.

**Validate config and return an error.** The kernel marks that row
`failed`, reports it, and leaves the rest of the tree running — one bad
row never takes the session down.

```go
minLength := 1
if v, ok := cfg["min_length"]; ok {
	n, ok := v.(int)
	if !ok || n < 1 {
		return fmt.Errorf("example-wordcount: min_length must be a positive integer, got %v", v)
	}
	minLength = n
}
```

**Provide a service** for other rows to consume. Service keys are the
whole wiring story: no plugin imports another plugin's package to reach
it.

```go
ctx.Provide("wordcount", counter)
```

**Use optional services** by fetching them and carrying on if they are
absent. The kernel tracks every service read during `Apply`, so a row
that mounts without `codemode` today remounts by itself when a codemode
row appears later.

```go
if cm, err := kernel.Get[toolRegistry](ctx, "codemode"); err == nil {
	cm.RegisterTool("wordcount", counter.Count)
	ctx.Effect(func() { cm.RegisterTool("wordcount", nil) })
}
```

**Clean up with `ctx.Effect`.** Disposers run LIFO on unmount — on hot
reload, on remount, and at exit. Whatever you register, unregister.

## Ask for the method set, not the package

Declare the slice of a service you actually use:

```go
type toolRegistry interface{ RegisterTool(name string, fn any) }
```

Every plugin here does this. It keeps plugins from depending on each
other's packages, and it documents exactly how much of a service you
rely on.

## Adding a tool the model can call

`RegisterTool(name, fn)` puts `tools.<name>` in the code-mode VM. The
return value is what the model sees, so keep it JSON-shaped — maps,
slices, strings, numbers. A `map[string]any` of results beats a
pre-formatted string: the model can index it.

The VM is [goja](https://github.com/dop251/goja), not Node. There is no
event loop, so a tool function is an ordinary synchronous Go function
and `async`/`await` are syntax errors on the JS side.

## Adding a slash command

```go
info := commands.CommandInfo{Name: "wordcount", Usage: "<text>", Summary: "count the words in some text"}
reg.Register(info, func(args string) (string, error) { ... })
ctx.Effect(func() { reg.Unregister("wordcount") })
```

The summary shows in the `/` palette, so write it as the one line
someone scanning a list needs.

## Adding to the system prompt

The loop publishes `prompt-sections`. A section is named, so setting it
again replaces it, and empty text removes it:

```go
type sections interface{ Set(name, text string) }

if s, err := kernel.Get[sections](ctx, "prompt-sections"); err == nil {
	s.Set("wordcount", "tools.wordcount(text) counts words.")
	ctx.Effect(func() { s.Set("wordcount", "") })
}
```

Sections are live: change one mid-session and the next model call sees
it. Spend them carefully — every section is in the context on every
call, and `/context` shows a user exactly what you added.

## Row order

Mount order comes from dependencies, not from the file. Row order only
breaks ties within a mount pass, which is why the optional loop seams
sit above the `loop` row in the shipped `bough.yml` and why `graph` and
`scratchpad` sit below it.

## Testing

Mount the plugin against a bare `kernel.NewContext()`, provide fakes for
the services it consumes, and assert on the service it provides:

```go
ctx := kernel.NewContext()
ctx.Provide("codemode", &fakeTools{fns: map[string]any{}})
if err := (plugin{}).Apply(ctx, map[string]any{"min_length": 4}); err != nil {
	t.Fatal(err)
}
```

Cover the three cases that break in production: a **bad config value**
fails the row, the plugin **mounts with its optional services absent**,
and `ctx.Unmount()` **removes everything it registered**.
[`example_test.go`](../plugins/example/example_test.go) is those tests.

Tests must be offline and hermetic — a deterministic LLM (`llm-echo`,
or a JS provider from `init.js`), its own temp HOME, no network. See the
Testing section of [the Go README](../README.md).
