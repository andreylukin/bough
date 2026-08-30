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

## Collectors over Claude Code's MCP grants (Linear, Slack)

The Linear and Slack collectors can sweep through MCP servers instead of holding credentials.
`bundles/bough-mcp-claude.yml` is the shipped patch layer:

    bough --patch bundles/bough-mcp-claude.yml

It mounts two `mcp.rmcp` server rows (`linear-server` at mcp.linear.app, `slack` at
mcp.slack.com) whose `Authorization` headers are `${keychain:...}` REFERENCES into the login
keychain item Claude Code maintains ("Claude Code-credentials"), points `collect.linear` at the
`linear-server` row (`mcp_server: linear-server`, no `LINEAR_API_KEY` anywhere), and gives
`collect.slack` its scope (`queries: { mentions: "to:me" }`).

Three properties, all held by `plugins/mcp-rmcp/src/keychain.rs`:

- **Reference, never secret.** The config (and therefore `--dump-config`) holds the reference
  text; the token is read at connect time via `security` run as argv and exists only in the one
  request header, which is marked sensitive.
- **Expired is reported, not refreshed.** The grant belongs to Claude Code; the fix is running
  `claude` once, and the error says so.
- **Machine-specific grant keys.** `mcpOAuth.<server>|<hash>` is per machine; list yours with
  `security find-generic-password -s "Claude Code-credentials" -w | jq '.mcpOAuth | keys'` and
  restate the two header values in the patch.

`collect.slack` ships in `bough-base` with `queries: {}` (loud, collects nothing) exactly like
`collect.github`'s `repos: []`; the query is the scope AND the truth of the mention class, so it
is a deployment's own statement. `collect.linear` keeps its GraphQL transport when `mcp_server`
is empty; note the scope value differs per transport (MCP takes the team NAME, e.g. `FOMS`;
GraphQL takes the key, e.g. `NME`).

## Shipped default skills (`assets/skills/`)

Three trigger-gated skills earned by the terminal-bench tuning arcs (`docs/tbench.md`), written
general and benchmark-free. They are FILES, not rows: copy them into a home to enable them, and
edit them live (the `skills` row watches the directory).

    cp assets/skills/*.md "$BOUGH_HOME/skills/"

- `operate-the-machine` — configure/server/install tasks: the machine you are on is the target;
  converge live state; daemons must outlive you.
- `finish-state` — the described end state is a contract; verification debris goes, asked-for
  state stays.
- `prove-the-criteria` — every stated behavior is an executable test at its most demanding
  plausible shape; sabotage the artifact once to prove the test can fail. (The probe-strength
  half is load-bearing: discipline text without it measurably made agents confidently wrong.)

None of this belongs in the always-on identity: trigger gating keeps the standing prompt small
and byte-stable (the §12 cache tiers), and the lane persona's draft-and-defer character stays
untouched. The `bg` lifetime lesson from the same arcs lives in the `bg` TOOL DESCRIPTION
(`tools-operator`), because a harness contract belongs where the model decides to use the tool.
