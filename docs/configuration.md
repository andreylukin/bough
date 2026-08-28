# Configuration

How the shipped tree is composed, and the switches Andrey is expected to reach for. The mechanism
is REQUIREMENTS §0.5: `bundles/` are patch documents, `profiles/` are ordered bundle lists, and a
`--patch` layer configures rows that some bundle already created — it never creates one.

```
bundles → profile patch → user patch ($BOUGH_HOME/bough.patch.yml) → --patch layers
```

Later layers win, per field. `bough --profile <name> --dump-config` prints the composed document,
the layer list and the warnings; it is the fastest way to answer "what is actually on?".

## Tool surface: code mode (default) and typed tools

The tree ships TWO consumers of the `tools` seam.

**Code mode is the default (Andrey, 2026-08-28).** The model is shown ONE tool, `run(program)`, and
writes JavaScript that calls the tools as functions inside an embedded QuickJS sandbox. Every call a
program makes goes through the same pipeline as a typed call, lands as a ledgered sub-step, and is
subject to scope shadowing and `restrict` exactly as a typed call is.

It is on with no flags:

```sh
bough                      # --profile tui
bough exec "…"             # --profile headless
```

`bundles/bough-base.yml` declares `js`, `js.quickjs` and `tools.codemode` ENABLED, and
`profiles/tui.yml` / `profiles/headless.yml` compose `bough-codemode` LAST so the tool surface is
readable at the profile level too.

**Nothing is removed.** `tools`, `tools.baseline`, `tools.operator` and every other typed row stay
mounted and executable under code mode; what changes is what the model is SHOWN. That is why the
switch back costs one field and no reconfiguration.

### The fallback: the typed tool set

`bundles/bough-typed.yml` is the whole of it:

```yaml
entries:
  tools.codemode: { disabled: true }
```

Use it as a layer over any profile — the exact patch to switch back:

```sh
bough --patch bundles/bough-typed.yml
bough exec --patch bundles/bough-typed.yml "…"
```

…or make it permanent, either by appending `bough-typed` to a profile's bundle list:

```yaml
# profiles/tui.yml
bundles: [bough-base, bough-tui-app, bough-codemode, bough-typed]
```

…or by writing the same two lines into `$BOUGH_HOME/bough.patch.yml`, which the launcher watches
and applies live — the row swap happens in the running process, and the next wake uses the other
surface (`crates/bough/tests/codemode_swap.rs`, `scripts/tui/32-codemode-swap.sh`).

`js` and `js.quickjs` stay up under the fallback on purpose: with no program to run they cost
nothing, and leaving them means switching back and forth is one field either way.

### Which one am I running?

```sh
bough --profile tui --dump-config | grep -A2 '^- id: tools.codemode'
```

A `disabled: true` inside that row's block is the typed surface; its absence is code mode. In the
TUI, a turn under code mode draws ONE program row (the JS, the console output, the inner calls
nested under it); a turn under the typed tools draws a plain tool row per call.

### Why the rows live in `bough-base`

A patch layer configures rows and never creates them, so rows that exist only in a bundle no
profile lists cannot be reached by `--patch` at all. Declared in the base bundle, the consumer is
one field away from any layer — which is what makes both the switch and the fallback one line.

### The evidence behind the default

`docs/phase-codemode-plan.md` §8: over the 15-task bank, live haiku, code mode 13/15 against the
typed set's 14/15, and 15/15 for both arms offline. The GO rests on that capability tie and NOT on
the cost ordering, which did not replicate between two runs of the same commit (§8 carries the DOES
NOT REPLICATE box). `BUILD.md`'s `Default consumer: code mode` row is the ledger entry.

- `bough --profile typed` is the same fallback reachable by name (`profiles/typed.yml`: headless +
  `bough-typed`). It is what the bench composes for its control arm (`bench/tools/src/run.rs`).
