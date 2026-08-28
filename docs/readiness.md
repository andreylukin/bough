# Readiness for daily use

What still stands between this tree and Andrey driving it every day, checked against the code on
`rebuild` at `b0558e1f`. Written for the install pass (`bough-next`, a fresh `$BOUGH_HOME`).

Everything below was verified by reading the named file or by running
`target/release/bough` against a scratch `$BOUGH_HOME` — never `~/.bough`.

**The blocking short list**, expanded in the sections that follow:

1. **The binary never sources `~/.bough/env`.** Only the `Makefile` does. Every key must be
   exported by the shell that launches `bough`, or the model call fails at call time with a
   message that tells you to edit a file the binary does not read.
2. **`--dump-config` prints `LINEAR_API_KEY` in plaintext.** Verified with a sentinel: two hits.
3. **Nothing outward-facing has ever run live.** `gh` acts and Linear acts are proven against a
   recording shim and a local HTTP stub only.
4. **`make live` is RED on two files** at this sha, both prompt-tuning consequences of the
   code-mode default.
5. **`spawn_worker` and the `act` surface are recent**; the collectors are OFF by default and
   collect nothing until you name repos/teams.

---

## (a) Credentials: what the binary needs, and exactly how each is read

There are **two** ways a secret enters the process, and they behave differently.

| Read | When | Mechanism |
| --- | --- | --- |
| **At call time**, per LLM round | every model round | `plugins/llm-anthropic/src/lib.rs:83` — `std::env::var(&cfg.api_key_env)` inside the per-round `Env` closure |
| **At compose time**, once per boot | boot, and each patch-file recompose | `crates/bough-kernel/src/config/expr.rs:58` — `std::env::vars().collect()`, snapshotted for the `!!expr env(...)` / `env_or(...)` evaluator |

### `ANTHROPIC_API_KEY`

- Row `llm.anthropic` (`bundles/bough-base.yml:41`). Config field `api_key_env`, default
  `"ANTHROPIC_API_KEY"` (`plugins/llm-anthropic/src/lib.rs:39`).
- **Read at CALL time, from the process environment.** Not a config expression, so it is never in
  `--dump-config` and never in the composed tree.
- **A missing key is NOT a boot failure.** `bough --profile tui --check` with the variable unset
  exits 0. The failure arrives on the first round as a `Chunk::Failed` — the adapter never throws
  (P2-D7).
- **The error message is misleading**: `crates/bough-llm/src/routing.rs:148` says *"put it in
  `~/.bough/env`, then `bough restart`"*. **No code in the binary ever reads `~/.bough/env`** —
  grep confirms the only readers are `Makefile:55,133,161`. There is also no `bough restart`
  subcommand (`bough --help` lists `exec`, `mcp`, `wards`). **BLOCKING for the install:** the
  launcher wrapper must `set -a; . ~/.bough/env; set +a` itself, or the key must live in the shell
  profile.
- Other providers are reachable through `bough-llm`'s routing (`OPENAI_API_KEY` etc.,
  `crates/bough-llm/src/openai.rs:664`), but only `llm-anthropic` is a mounted row.

### `LINEAR_API_KEY`

- Two rows: `actions.linear` (`bundles/bough-base.yml:261`) and `collect.linear` (`:296`), both
  `api_key: !!expr 'env_or("LINEAR_API_KEY", "")'`.
- **Read at COMPOSE time**, from the environment the process was launched with. Exporting it after
  launch does nothing until the tree recomposes.
- Absent is deliberately not a boot error (`plugins/actions-linear/src/lib.rs:387`). Both rows warn
  and turn themselves off — observed on `--check`:
  `actions-linear: no Linear API key resolved: linear_write is NOT registered` and
  `collector-linear: no Linear API key resolved: every source is off`.
- **The key is redacted in `Debug` and in plugin errors** (`lib.rs:37`, `:105`) **but NOT in
  `--dump-config`.** Verified: `LINEAR_API_KEY=lin_api_FAKESENTINEL123 bough --profile tui
  --dump-config` prints `api_key: lin_api_FAKESENTINEL123` twice (lines 767 and 868). **Do not
  paste `--dump-config` output anywhere.**

### `gh`

- `plugins/gh-cli` is the only place in the tree that spawns `gh`, and it never passes `--jq`.
- No token is read by bough. Both `actions.github` and `collect.github` carry `gh_bin: gh`, so the
  **ambient `gh auth` on `PATH` is the credential**. `Gh::with_env` exists for the tests' recording
  shim only; the shipped rows pass no extra environment.
- **`collect.github.repos` is `[]` in the shipped bundle** and the row says so every sweep
  (observed: `collector-github: repos is empty: this row collects nothing`). GitHub collection does
  nothing until a patch layer names repositories.

### MCP servers

- `mcp.rmcp` ships `servers: []`; `mcp.subprocess` ships `processes: []`. **No MCP server is
  configured out of the box.**
- A server row is `{name, transport}` where transport is `Stdio {command, args, env}` or
  `Http {url, headers}` (`plugins/mcp-rmcp/src/lib.rs:37-58`). Tokens go in `env` or `headers`,
  and may be `!!expr env("…")` — but note the two consequences: (1) `env("X")` with `X` unset is a
  **boot failure** (`ExprError::MissingEnv`, `expr.rs:623`), unlike `env_or`; (2) whatever resolves
  is **printed by `--dump-config`**, exactly as the Linear key is.
- One-shot check without the TUI: `bough mcp call <server> <tool> <json>`.

### What is written where

A fresh `$BOUGH_HOME` after one boot holds: `ledger.db` (+ `-wal`, `-shm`), `collect-github.db`,
`collect-linear.db`, `schedule.db`, `skills/`. `wards/` is created on demand. `bough.log` appears
only on a TTY boot (below).

---

## (b) What only works under replay / shims today

- **Every outward-facing act.** BUILD.md phase 6: *"Real outward acts are verified against a
  recording `gh` shim and a local HTTP stub; NOTHING outward-facing runs live."* That covers
  `open_pr`, `push_to_pr`, the bot-thread ops (`plugins/actions-github`) and `linear_write`
  (`plugins/actions-linear`). The first real `gh` call Andrey makes is the first one ever made.
- **`collector-github`** is tested against the shim only (`plugins/collector-github/tests/`:
  `sweep.rs`, `routing.rs`, `urgency.rs` — no live file). **`collector-linear`** likewise against a
  local stub.
- **MCP**: `plugins/mcp-rmcp/tests/stdio_fixture.rs` and `plugins/mcp-subprocess/tests/` drive a
  local fixture process. No live third-party server is in any gate.
- **The TUI's offline half** swaps `llm.anthropic` for `llm-replay` by patch (`Makefile`, the
  `TUI_PATCH`), so 288 of the replay bullets see no model at all. Scripts 02–09 skip their live
  half by construction; the live evidence for the Phase 3 interface is `01-boot-and-turn.sh` alone.
- **`BOUGH_LIVE`-gated files** (the whole live surface): `bench/tools/tests/live.rs`,
  `crates/bough/tests/{boundary_probe_live,exec_headless,token_calibration,worker_live}.rs`,
  `plugins/llm-anthropic/tests/map.rs`, `plugins/rollups-summarizer/tests/seal_once.rs`,
  `plugins/tui-render/tests/md.rs`. **Two are RED at this sha** (see (d)).
- **Never runs, anywhere:** a real machine sleep.
  `plugins/sleep-listener/tests/live.rs::the_iokit_listener_receives_a_real_wake` "exists under no
  name and cannot" (BUILD.md phase 7) — it is a MANUAL gate. So is **one full real workday through
  the TUI**, which BUILD.md phase 3 names as Andrey's own act, still not run.

---

## (c) Deferred items that touch daily use

One line each, with the file. Drawn from BUILD.md's "Deferred / deviations" column and the
"Merge outcome" sections of `docs/codemode-merge-notes.md`, `docs/track-b-merge-notes.md` and
`docs/track-c-merge-notes.md`. Purely internal deferrals (seam accessors, branding, test-shape
notes) are omitted; these are the ones you can feel.

- **Esc does not dismiss an aux band pane** — `plugins/tui-shell/src/run.rs::dismiss_overlay`;
  deferred at the track-C merge because `ux-visual` owns that key path.
- **A standalone Enter is swallowed on a no-match command palette** — type `/hepl`, press Enter,
  nothing happens. `plugins/tui-shell`; recorded for the ux track, not fixed.
- **The three digging panes are in the catalog and in NO bundle** — `tui-preview`, `tui-timeline`,
  `tui-drift`; `bundles/bough-tui-app.yml` says why (three always-present `Slot::Aux` panes squeeze
  the focus pane to nothing). Mount one by patch when digging.
- **A pane's `handle` does ledger I/O inline on the event-loop task**, so a slow read blocks the
  frame — `plugins/tui-shell/src/pane.rs`, nothing measures it (`docs/phase-3-plan.md` §6.2).
- **A ward can still feed itself through an agent**; the bound is a RATE
  (`max_firings_per_minute: 60` in `bundles/bough-base.yml`), not provenance —
  `plugins/wards-rhai`, `docs/track-b-merge-notes.md` §17.
- **`bough wards test`'s `cx.already(ref)` is always false** — a dry-fire cannot know what the live
  child remembers (`plugins/wards-rhai`, BUILD.md phase 6).
- **`tools-operator` polls `schedule` every 100 ms** — `plugins/tools-operator/src/schedule.rs`;
  cannot be replaced until a `ctx.schedule` hook exists (`docs/codemode-merge-notes.md` §3).
- **`Concealment` never prunes per-agent handles** (no `AgentDisposed` listener) —
  `plugins/tools-codemode`; a long TUI session leaks handles slowly.
- **The `program/*` step types are declared for the life of the binary** — deliberate, the ONE
  sanctioned exception to "registrations are effects" (`plugins/ledger`, AGENTS.md); wants Andrey's
  blessing rather than a fix.
- **`rquickjs` contradicts REQUIREMENTS §13's Avoid list** — the doc should be amended now that
  code mode is the default (`docs/phase-codemode-plan.md` §10).
- **A bundle typo silently drops a function from the code-mode sandbox** — no warning
  (`plugins/tools-codemode`, three silent-skip findings recorded, not fixed).
- **`ledger-memory` is not the shipped provider**, so its remaining duplicate-rollup gap does not
  bite; `ledger-sqlite` is what boots (`bundles/bough-base.yml:17`).
- **A governance pass is uncancellable** — needs a fiber-scoped token on `Context`
  (`plugins/reconsolidation`, BUILD.md phase 4).
- **`WorkerResult.steps` / `.usage` are always zero** — `plugins/workers`, so a worker's cost does
  not roll up (BUILD.md phase 2).
- **The old-feed bridge (`~/.bough/bough.db`, `~/.jungler/jungler.db`) is `disabled: true`** and
  was never proven against a real jungler db — `bundles/bough-tui-app.yml:96`, BUILD.md phase 3.
  If you want your old history, re-enable that row by patch and expect the schema to be the
  fixture's invention.
- **`Cadence::Interval` / `OnEvent` invariants are declared and not dispatched** — every runtime
  invariant is `OnQuiesce` (`crates/bough-kernel/src/invariant.rs`, BUILD.md phase 0).
- **§8's cost comparison between the two tool surfaces does not replicate** and the two bench arms
  differ by `tags_required` as well as the consumer row — the code-mode GO rests on the capability
  tie ALONE (`docs/phase-codemode-plan.md` §8). Re-running to n≥5 is deferred.

---

## (d) Known flakes, and what to do about each

- **`make live` is RED at this sha, 5 passed / 2 failed**, and both are real consequences of the
  code-mode default rather than flakes:
  `worker_live::a_real_worker_edits_a_file_and_its_content_proves_it` (the live worker ended
  without calling `report`) and
  `boundary_probe_live::the_adversarial_bank_finds_no_cheap_path_past_the_boundary` (4 probes ask a
  clarifying question instead of writing the draft the boundary block requires). Both pass under
  the typed default. **Fix is prompt re-tuning**, not a revert — `BOUNDARY_BLOCK` and the worker's
  finishing instruction are tuned to the typed surface.
- **`boundary_probe_live`'s `a_chain_through_an_mcp_server`** ends some runs in an explanation with
  no `draft/*` step even under the typed surface — a standing-instruction compliance gap
  (`docs/phase-6-plan.md` §6), measured 6–7 of 7 bank prompts after the block was tightened.
- **`12-many-agents.sh::three_rails_render_with_their_glyphs`** — an intermittently EMPTY rail on a
  booted TUI. Green standalone every time. `residents` now retries the roster raise once. Re-run
  the script alone before believing it.
- **`32-codemode-swap.sh::typed_rows_before_the_patch`** — went red once under suite load with
  `js.quickjs … unmet: js`, green standalone immediately after. Not diagnosed.
- **`19-interrupt.sh::quit_exits_cleanly_within_three_seconds`** — red once under load (`/quit took
  6s`); the interactive budget is deliberately unchanged, so a loaded machine can miss it.
- **`19-interrupt.sh::the_farewell_is_one_line_and_the_screen_is_not_blank`** — a pre-existing
  script bug: it exits the binary twice in one PTY, so the scrollback holds two `bough: bye.`
  lines.
- **One `nextest` case reports "leaky"** in a full workspace run (2428 passed, 1 leaky) — a
  lingering child at test exit, not a failure.
- **A teardown warning on `--check`, observed once in four cold boots at this sha:**
  `ERROR bough_kernel::fiber: fiber did not settle within 5s; disposing it anyway entry=residents`.
  Exit code stays 0 and no invariant fires. This is the shape `docs/track-b-merge-notes.md` §15
  fixed for `wards`/`skills` (a long-lived `effect_spawn` body with no halt checkpoint), now
  appearing on a different row. **Not previously recorded; worth a look before it becomes a slow
  quit.**

---

## (e) First week: how to actually drive it

### Launch

```sh
set -a; . ~/.bough/env; set +a     # the binary will NOT do this for you
export BOUGH_HOME=~/.bough-next    # a fresh home, beside the daily driver's
bough                              # the TUI; --profile tui is the default
```

- `bough exec "…"` runs one task headless and exits (`--profile headless`, forced).
- `bough --check` boots, quiesces, asserts, tears down, exits — the fastest "is my config sane?".
- `profiles/` and `bundles/` are **embedded in the binary**; `--root <dir>` overrides them. The
  install needs the binary and nothing else on disk.

### Where things live

Everything is under `$BOUGH_HOME` (default `~/.bough`), and every path is a
`!!expr bough_path(...)` in `bundles/bough-base.yml`, so it follows `BOUGH_HOME`:

- **the ledger** — `$BOUGH_HOME/ledger.db` (`ledger-sqlite`, `busy_timeout_ms: 5000`)
- collector state — `collect-github.db`, `collect-linear.db`; schedules — `schedule.db`
- **your patch layer** — `$BOUGH_HOME/bough.patch.yml`, watched live (`--no-watch` to turn that
  off); a bad patch keeps the last good tree
- wards — `$BOUGH_HOME/wards/*.rhai`, watched; skills — `$BOUGH_HOME/skills/*.md`, watched

### Reading the log

`bough.log` exists **only when bough owns the terminal** — stdout is a TTY, no subcommand, no
`--check`, no `--dump-config` (`crates/bough/src/main.rs::owns_a_terminal`). Otherwise tracing goes
to stderr.

```sh
tail -f $BOUGH_HOME/bough.log
RUST_LOG=debug bough                  # EnvFilter, default "info"
```

It **appends and never rotates**. Truncate it yourself.

### Switching the tool consumer

Code mode is the default. The whole of the fallback is
`bundles/bough-typed.yml` — `entries: {tools.codemode: {disabled: true}}`.

```sh
bough --patch bundles/bough-typed.yml        # one session
bough --profile typed                        # the same thing, by name (headless)
```

Permanent: put those two lines in `$BOUGH_HOME/bough.patch.yml` — the row swaps in the running
process. Full detail, including how to tell which one you are on, is `docs/configuration.md`.

### Disabling any row

A patch layer **configures** rows and never creates them, so anything you want to reach must
already exist in a composed bundle.

```yaml
# $BOUGH_HOME/bough.patch.yml
entries:
  tui.search: { disabled: true }
```

Two things that bite:

1. **A `config:` map in a patch REPLACES the row's whole map.** Both action Providers are
   `deny_unknown_fields` with no serde defaults, so a partial `config:` is a boot failure
   (`missing field known_bots`) — restate the whole map.
2. **A duplicate row id composes silently into two rows.** Worth a kernel look
   (`crates/bough/src/compose.rs`); until then, do not re-declare an id you only meant to patch.

Check any of this without booting:

```sh
bough --profile tui --dump-config | less   # contains secrets — see (a)
```

### Turning the collectors on

```yaml
entries:
  collect.github: { config: { repos: ["andrey/bough", …], … } }   # the WHOLE map, per (1) above
```

`LINEAR_API_KEY` must be exported **before launch** for the Linear rows to activate at all.
