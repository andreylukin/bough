# Code-mode merge notes

Seams this phase needed and could not add, because `plugins/tools`, `plugins/ledger*`,
`plugins/agents`, `plugins/agent-loop`, `plugins/workers`, `plugins/actions` and
`crates/bough-kernel` are owned by the parallel track-B merge on `rebuild`. Each entry is the exact
hook wanted, where it belongs, and why. Nothing here was changed; every consumer in this phase was
built (or is scaffolded) against the public API that exists today.

## 1. `ToolsHandle::visible_specs` is private

**File** `plugins/tools/src/lib.rs`
**Wanted** `pub fn visible_specs(&self, agent: &AgentName) -> Vec<ToolSpec>`

`tools-codemode`'s `conceal::snapshot` must mirror the registry into the sandbox: one injected async
function per visible `ToolSpec`. The public API gives only `visible()` (names), `schemas()` (name +
description + JSON schema) and `render_intent()`, so the snapshot has to *rebuild* each `ToolSpec`
from three calls and invent `scope: ToolScope::Global` for the mirror. That reconstruction is a place
the mirror can silently drift from the registry — the property "the sandbox sees exactly the agent's
scope" would be pinned by construction if the specs were readable.

Same gap hits `tools-codemode`'s surface section (WP-5): it cannot read the registry directly and has
to go through a `SurfaceSource` indirection whose production impl is `conceal::snapshot` +
`bind::bindings`.

## 2. A tool cannot learn its caller's trajectory

**File** `plugins/tools/src/lib.rs` (`ToolCall` / `ToolCx`)
**Wanted** `ToolCx { …, traj: TrajId }`, or `LedgerStore::agent(&AgentName) -> AgentRow` made public
and documented as the sanctioned mapping.

`ToolCall` carries an `AgentName`. Every ledger read the operator surface needs is keyed by `TrajId`:
`LedgerStore::unconsumed_mail(traj)`, `StepQuery { traj, .. }`, `tail(traj, n)`. So `inbox()`,
`ledger.steps()`, `ledger.tail()` and `schedule()` all have to resolve name → trajectory themselves
at call time. One field on the call context removes a lookup and a failure mode from four tools.

## 3. `ctx.schedule` does not exist

**File** `crates/bough-kernel`
**Wanted** the §5 "own scheduled intents" primitive.

`schedule(at, intent)` is implemented in `tools-operator` as a ledger step (`schedule/intent`) plus a
due-watcher fiber that appends `schedule/fired` and posts a `Message::waking(Sender::System(
"schedule"), Target::NextWake)`. The invariant — an intent fires exactly once, across a restart
replay — is why the intent is a ledger step and not a timer in memory. When the kernel primitive
lands, the watcher half is deleted and the tool registers a cron entry instead; the two step types
and the invariant stay.

**Exact hook wanted** (`crates/bough-kernel`, alongside `Context::effect_spawn`):

```rust
impl Context {
    /// Register a due-time callback owned by this entry: fires once at `at`, is cancelled by the
    /// row's disposal, and is re-registered from the ledger on boot by whoever owns the seam.
    pub async fn schedule(
        &self,
        at: chrono::DateTime<chrono::Utc>,
        f: impl Fn(EffectCtx) -> BoxFuture<'static, Result<(), PluginError>> + Send + Sync + 'static,
    ) -> Result<ScheduleHandle, PluginError>;
}
```

Until it exists there is no `schedule` key to inject and no `schedule-cron` Provider in the tree, so
`tools-operator` polls: `schedule::watch` is an `effect_spawn` loop that sleeps in 100 ms slices (the
slice is not the tick — `EffectHandle::dispose` awaits the body, so a full-tick sleep would make a
SIGINT look like a hang) and folds `intent`/`fired` off the ledger once per `schedule_tick_ms`. This
is the third kind of background job §9 says should not exist; it is deliberate and temporary, and it
runs in every profile that mounts the row (`bundles/bough-base.yml`) because the `schedule` TOOL is
mounted there too — gating the watcher to code mode would let an intent be recorded in the default
profile and never fire. Deleting the loop is a one-file change once the hook above lands.

## 4. `program/*` step-kind literals cross a crate boundary by name

**Files** `plugins/tui-focus/src/program.rs`, `plugins/tools-codemode/src/lib.rs`

`tui-focus` folds a program row by reading `RUN_TOOL = "run"` and the `program/*` step kinds *by
name* (the existing P3-D11 convention this crate already uses for `claim/*` and `about/line`), so the
two crates build in either order and `tui-focus` gains no dependency on `tools-codemode`. When
`tools-codemode` lands its bodies it must add a test pinning
`tools_codemode::RUN_TOOL == tui_focus::RUN_TOOL`, the way `andrey_ref_is_the_spelling_agents_writes`
pins the other cross-crate literal. Related: `tui-focus` deserializes `program/result` straight into
`ToolResultBody`; adding `deny_unknown_fields` there breaks the fold.

## 5. `tui-search`'s `Row` match is exhaustive

**File** `plugins/tui-search/src/index.rs`

Adding `Row::Program` to `tui-focus` broke the release build of `tui-search`. One arm was added,
indexing a program by its source, its console output and its folded sub-calls' args/output. Whoever
owns `tui-search` should confirm that is the indexing they want.

## 6. `plugins/tools-baseline::fs` containment is not reusable

**File** `plugins/tools-baseline/src/fs.rs`
**Wanted** the workspace-containment check exported (or lifted to `bough-plugin-tools`).

`tools-operator`'s `view` / `patch` / `write` need exactly the same "path resolves inside the
workspace root" rule. `tools-baseline` is off-limits to this phase and is not a dependency, so
`plugins/tools-operator/src/files/mod.rs` carries its own copy of `contain`. Two copies of a
containment rule is one more than is safe.

## 7. `spawn_worker` cannot execute: `agent-loop` reads `workers` without declaring it

**File** `plugins/agent-loop/src/lib.rs` (`fn inject`)
**Wanted** `Inject::required(["agents", "ledger", "projection", "llm", "tools"])` gains an
**optional** `"workers"` — or `tool-workers` executes against its OWN registration context rather
than the caller's.

Found by the bench (WP-8), on the shipped `headless` tree, with no code-mode row anywhere near it:

```
$ bough --root . --patch bench/tools/fixtures/typed/08-spawn-a-worker.yml exec "…"
tool/result  spawn_worker  outcome=error
  "workers seam unavailable: plugin `agent-loop` (row `agent.loop`) read service `workers`
   without declaring it in inject"
```

`tool-spawn_worker` resolves `Workers` from the context the tool is *executed* under, and the
agent loop is what executes tools, so the resolve is attributed to `agent.loop`, whose `inject()`
does not name `workers`. The declared-key rule then refuses it — correctly; the bug is the
declaration. The consequence is that **no agent can spawn a worker through the tool surface today**,
under EITHER consumer: `bench/tools/bank/08-spawn-a-worker.yml` is red for the typed arm, and code
mode's `agent()` will hit the same wall the moment WP-2's bodies land. Nothing in this phase can fix
it (`plugins/agent-loop` and `plugins/workers` belong to the track-B merge), and it is not a code-mode
finding — it is a Phase 5 gap the bench happened to be the first thing to exercise end to end.

## 8. A patch layer cannot create a row, and `bough exec` forces `headless`

**Files** `crates/bough-kernel/src/config/patch.rs`, `crates/bough/src/exec.rs::force_profile`
**Wanted** nothing changed — recorded so the next reader does not repeat the experiment.

The plan's §1 says the codemode rows can be mounted "by `--patch` at runtime for the SWAP test".
They cannot: a patch layer configures rows and never creates them (`patch.rs`: "a patch naming an
absent row id is a `ComposeWarning`, never an error"), so `--patch` naming `js` / `js.quickjs` /
`tools.codemode` prints three warnings and boots the typed tree. Row *creation* is a bundle's job,
and the bundle list comes from a profile document. `bough exec` in turn forces `--profile headless`
and says so on stderr.

So `bench/tools/src/run.rs` reaches the consumer the only way that exists: it builds a scratch
`--root` holding the repo's `bundles/` and `profiles/` verbatim with `profiles/headless.yml`
replaced by `profiles/codemode.yml` renamed — `--root` is searched before `$BOUGH_HOME`
(`bough::profile::search_roots`, so the home is *not* an override path, whatever
`crates/bough/tests/support/mod.rs`'s comment says). `bench/tools/tests/bank.rs::
the_codemode_arm_comes_from_the_shipped_profile_and_bundle` pins that copy to the shipped document.
Related, and also recorded rather than fixed: a `--patch` layer REPLACES a row's whole config, so
every bench arm restates `tools.operator` in full
(`a_restated_row_names_every_field_the_shipped_bundle_sets`).

## 9. The code-mode SHELL surface is unsatisfiable: no registered tool carries `tags`

**Files** `plugins/tools-operator/src/lib.rs`, `plugins/tools-baseline/src/lib.rs`,
`plugins/tools-codemode/src/{bind.rs,surface/shell.md}`
**Wanted** a `bash`/`sh` Provider whose input schema has a `tags` property.

`bind.rs:382` refuses a `bash`/`sh` call carrying fewer than 3 or more than 5 `tags` when
`tags_required` is on (plan D-6, and `arms/*.yml` keeps it on deliberately). No registered tool has
a `tags` property:

- `tools-baseline`'s `bash` is `{"command": …, "cwd": …}` — no `tags`, and the first property is
  `command`, not `cmd`. Positional binding therefore maps `bash("echo hi", ["a","b","c"])`'s second
  argument to `cwd`, `tags_of` finds nothing, and the call is refused.
- `tools-operator` registers `bg`, `ledger_read`, `inbox`, `schedule`, `view`, `patch`, `write`.
  There is no `bash` and no `sh` anywhere in the tree.
- `surface/shell.md` documents `tags` as a COLON-SEPARATED STRING (`"git:push:main"`) while
  `bind.rs` counts an ARRAY of 3–5 strings. Whichever is right, they are not both.

Observed on the real binary under `--profile codemode`:

```
program/error {"kind":"thrown",
  "message":"`bash` needs 3–5 tags naming what this command is about; it carried 0"}
```

Consequences the bench recorded rather than hid: every shell task in `bench/tools/bank/` was red
for the code-mode arm by construction, and the brief's structural claim ("`bash()` is the escape
hatch, so *every command goes through bash*") read as "no command goes through bash".

**FIXED at integration, 2026-08-27, inside `plugins/tools-codemode/src/bind.rs` — no seam change
and no edit to `tools-baseline`.** The premise was wrong: the tags on a shell command are a
HARNESS fact (they index the command in the cross-session tag history), not an argument of the
tool that runs it, so requiring the tool to declare them was the bug. `bind.rs` now takes the tag
argument off a `bash`/`sh` call BEFORE binding, whenever the tool declares no `tags` property of
its own:

- `takes_a_tag_argument(spec)` — `bash`/`sh` with no `tags` property. `arity_of` reports one more
  argument than the schema declares, so the injected signature is the documented `bash(cmd, tags)`.
- `shell_tags(spec, &mut args)` — removes argument 1 (the position `surface/shell.md` documents)
  when it is a string or an array, and returns its tags; `sh([{cmd, tag}, …])` collects them per
  leg from its one argument. The tags land on `program/call.tags` and never reach the tool.
- `parse_tags` accepts BOTH spellings, closing the third half of the mismatch: the colon-separated
  string `"git:push:main"` that `surface/shell.md` teaches (main's own spelling), and the array a
  tool that declares `tags` would take.

A shell Provider that DOES declare `tags` is unaffected and keeps binding it positionally.

Gates: `bind.rs`'s `a_tag_argument_is_taken_off_a_bash_that_does_not_declare_tags`,
`an_array_of_tags_is_taken_as_written_and_a_missing_one_yields_none`, `sh_carries_its_tags_per_leg`,
`a_bash_that_declares_tags_still_binds_them_positionally`,
`a_colon_separated_tag_string_on_a_declared_property_parses_too`; and end to end through the
pipeline, `tests/pipeline.rs::a_tagged_bash_call_reaches_a_tool_that_declares_no_tags` (against a
`{command, cwd}` bash, `tags_required` on) with
`an_untagged_bash_call_is_still_refused_and_lands_no_step` holding the other side.

**Still open, and NOT fixed here:** the tree registers no `sh` tool at all (`tools-baseline` has
`bash` only, `tools-operator` registers `bg` and the file tools), so `surface/shell.md`'s `sh`
paragraphs document a function the sandbox does not inject. The generated function table is built
from the live registry and therefore does not list it; the prose still teaches it. A `sh` Provider
belongs in `tools-operator`. And the live bench numbers in `docs/phase-codemode-plan.md` §8 were
measured BEFORE this fix, with every shell call refused — they are recorded as superseded there.

## 10. Disabling the consumer makes an already-written chain UNREADABLE

**Files** `plugins/tools-codemode/src/lib.rs` (the `declare_step_types` effect),
`plugins/ledger*` (the unknown-step-type rule)
**Wanted** the `program/*` vocabulary declared for the life of the BINARY rather than the life of
the row — or an unknown step type in a trajectory that is being REBUILT to be skippable rather than
fatal.

Found by the phase's own screen-level SWAP gate (`scripts/tui/32-codemode-swap.sh`), and it is the
one thing the in-process gate cannot see. Sequence, one running process:

1. `tools.codemode` disabled → the model calls `bash`, a plain tool row. Fine.
2. patch removed → the model calls `run`; `program/call`, `program/result` and `program/console`
   are appended to `lane/sol`. Fine.
3. `tools.codemode` disabled again → the NEXT wake dies at its first step:

```
wake/end {"cause":"step `01a0…` in trajectory `lane/sol` has type `program/console`,
          unknown to this binary and not ignorable"}
```

`tools-codemode` declares its step types through `ledger.declare_step_types(&ctx, …)` inside
`apply`, so the declaration is an effect that unwinds with the row — correct by the effects rule,
and wrong for a *vocabulary*, which describes bytes that are already on disk and outlive the row
that wrote them. `crates/bough/tests/codemode_swap.rs` passes because it swaps a tree that has
never run a program.

**FIXED at integration, 2026-08-27, inside `tools-codemode` — no seam change was needed.** The row
no longer calls `ledger.declare_step_types`; it calls `ledger.0.register_step_type(def)` once per
type and DROPS the token, which spends it without ever unregistering (what `StepTypeMap::
with_builtins` does for the ledger's own sixteen). `StepTypeMap::register` takes a reference on a
byte-identical redeclaration, so a remount is not a duplicate and two rows declaring the same type
still compose. The exception to the effects rule is stated at the call site: a step type is a
statement about bytes that are already on disk and it outlives the row that wrote them.

Gates: `scripts/tui/32-codemode-swap.sh` is GREEN in full (8 ok, `typed_rows_again_after_revert`
included), and `crates/bough/tests/codemode_swap.rs::the_program_vocabulary_survives_disabling_the_row`
holds the same property in process — the three types are still in `step_types()` after the
disabling patch recomposes.

## 11. A boot race in the KERNEL makes the whole suite intermittently red (not code mode's)

**Files** `crates/bough-kernel/src/{context.rs,fiber.rs}`, seen through `plugins/about-line`
**Wanted** a required key read in `apply` that is guaranteed to be in the reader's committed view,
or a fiber that re-Pends instead of Failing when `Context::get` on a REQUIRED key finds nothing.

Observed while integrating, under a loaded machine (three tracks building at once), in roughly one
full `make test` run in three. Two different binaries, one cause:

```
bough: 1 enabled row(s) never activated:
  about.line (plugin `about-line`) is Failed; unmet: -
      error: row `about.line`: plugin `about-line` (row `about.line`) read optional service
             `projection`, which no active fiber provides
```

`about-line` declares `projection` as **required** (`Inject::required(["agents","ledger",
"projection"])`) and reads it with `ctx.get::<Projection>()`. `Context::get` resolves against the
fiber's COMMITTED view, so when `about.line`'s `apply` runs before the view has committed the
`projection` row's provision, a required key comes back absent and the row Fails — where the
declared dependency should have kept it Pending. The row then never activates and the launcher
exits 1 with "enabled row(s) never activated", which is what the tests see. (The error's wording,
"read optional service", is a second, cosmetic bug: `ServiceUnavailable` is raised for required
reads too.)

It is NOT specific to code mode: `bough-base` carries both rows, so `--profile headless` composes
the same pair. It surfaced in this phase only because the phase added test binaries that drive the
real launcher, which raised the parallelism of `cargo test --workspace`. Seen in
`crates/bough/tests/exec_headless.rs` and `crates/bough/tests/codemode_wake.rs`; both pass in
isolation, repeatedly (10 consecutive clean runs of each), and three consecutive full `make test`
runs were clean before the failure recurred.

`crates/bough-kernel` is outside this run's file list, so nothing here changes it.
`codemode_wake.rs` now asserts the ordering it relied on rather than slicing on it, so the failure
reports the boot error instead of "slice index starts at 12 but ends at 5".

## 12. Two reds this run inherited rather than caused

**Files** `scripts/tui/24-honesty.sh`, `Makefile` (`tui-test-replay`)
**Wanted** nothing from another branch: both are FIXED here, and are recorded so the merge does not
re-introduce them from the other side.

**`scripts/tui/24-honesty.sh::a_good_patch_says_reloaded`** — RED on branch HEAD, deterministically
(3 of 3 runs), before any edit in this phase. `bf4a9ad3` ("phase ux1: review fixes") removed
`gutter` from `StripConfig` — the "one column, one knob" note in `plugins/tui-strip/src/lib.rs` —
but the script's GOOD patch still set `tui.strip.config.gutter: 1`, so the patch the script calls
good was rejected and the screen never said "reloaded". Fixed here by deleting that one line from
the script; the ux1 track has been told, since the same line is on `rebuild`.

**`make tui-test-replay` did not run the code-mode arm of the consumer-parameterised scripts.** The
loop over `scripts/tui/[0-9]*.sh` leaves `BOUGH_CONSUMER` unset, which is the TYPED control arm, so
`31-program.sh`'s program-row bullets were only ever asserted ABSENT in the gates. The `Makefile`
now runs a second, explicit pass over `31-program.sh` with `BOUGH_CONSUMER=codemode`.

## §12 — two `tools` accessors the mirror needs (review, phase codemode)

**File** `plugins/tools/src/lib.rs`
**Wanted** `ToolsHandle::default_deadline_ms()` and `ToolsHandle::max_parallel()`

Both are read-only getters on `ToolsHandle`; neither changes the seam's behaviour.

* `pub fn default_deadline_ms(&self) -> u64` — `plugins/tools/src/lib.rs`, beside `approval()`.
  The mirror a program executes against is built with `ToolsHandle::with_limits(1, deadline)`, and
  the Consumer has no way to read the seam's own `tools.default_deadline_ms`. Until it lands,
  `tools-codemode`'s `inner_deadline_ms` config field carries the value and `bundles/bough-codemode.yml`
  keeps it equal to `bough-base`'s `tools.default_deadline_ms` by hand — a duplication a getter deletes.
* `pub fn max_parallel(&self) -> usize` — same place. `tools.max_parallel` cannot apply under code
  mode (each host call issues its own single-call `execute_under`), so the Consumer enforces its own
  `max_parallel_calls` with a semaphore. With the getter the two knobs become one.

Approval no longer needs a hook: the mirror now calls the existing public `mount_approval` with
whatever `tools.approval()` answers, so an `ask` inside a program reaches the same approver a typed
call would.

## Merge outcome (rebuild-codemode → rebuild, 2026-08-28)

**Files** touched by this section's fixes: `plugins/tools-baseline/src/lib.rs`,
`plugins/tools-operator/src/{lib.rs,sh.rs}`, `plugins/tools-codemode/src/{lib.rs,bind.rs,
surface/shell.md}`, `plugins/agent-loop/src/lib.rs`, `plugins/agent-loop-scripted/src/lib.rs`,
`plugins/ledger/src/{lib.rs,types.rs}`, `plugins/exec-headless/src/lib.rs`,
`crates/bough-kernel/src/{error.rs,kernel.rs,fiber.rs}`, `bundles/bough-base.yml`,
`bundles/bough-codemode.yml`, `scripts/tui/24-honesty.sh`, `AGENTS.md`.

One line per numbered entry above: IMPLEMENTED, or DEFERRED with the reason.

| # | Hook | Outcome |
| --- | --- | --- |
| 1 | `ToolsHandle::visible_specs` is private | **DEFERRED.** The mirror still rebuilds each `ToolSpec` from `visible()` + `schemas()` + `render_intent()`. Nothing in the merge made the reconstruction wrong, and the accessor is a `plugins/tools` API change that wants its own review; `crates/bough/tests/codemode_swap.rs::every_visible_spec_is_a_function_in_the_sandbox_and_a_restricted_one_is_not` is what holds the property in the meantime. |
| 2 | A tool cannot learn its caller's trajectory | **DEFERRED.** `ToolCx` gains no `traj`; the four operator tools still resolve name → trajectory themselves. Same reason as 1 — a seam field, not a merge fix — and the merge added no new caller. |
| 3 | `ctx.schedule` does not exist | **DEFERRED**, as the note itself expects. `tools-operator::schedule::watch` keeps its 100 ms polling fold; deleting it is a one-file change once the kernel primitive lands. |
| 4 | `program/*` step-kind literals cross a crate boundary by name | **IMPLEMENTED already, and kept.** `plugins/tools-codemode/tests/pins.rs::run_tool_name_is_pinned_to_the_focus_pane_fold` is green in the merged tree; `tui-focus` still reads the kinds by name and takes no dependency on the consumer. |
| 5 | `tui-search`'s `Row` match is exhaustive | **IMPLEMENTED (union kept).** `tui-search` carries BOTH sides: codemode's `index.rs` `Row::Program` arm (source, console, folded sub-calls) and `ux-visual`'s render + Esc arm in `lib.rs`. Auto-merged, both behaviours asserted by the crate's own tests. |
| 6 | `tools-baseline::fs` containment is not reusable | **DEFERRED.** `tools-operator/src/files/mod.rs` keeps its copy of `contain`. Lifting it to `bough-plugin-tools` is the right move and it changes a shared crate every tool depends on; it belongs to a pass that can re-gate all of them. Recorded as still open. |
| 7 | `spawn_worker` cannot execute: `agent-loop` reads `workers` without declaring it | **IMPLEMENTED.** `plugins/agent-loop` and `plugins/agent-loop-scripted` declare `workers` as an OPTIONAL inject key. `crates/bough/tests/workers_seam.rs` proves a worker spawns under the typed surface AND from inside a program, and its third case greps the tree so a NEW tool that reaches a seam through `ToolCx.ctx` fails the gate instead of shipping dead. Track C's H-C5 is the same finding and is closed by the same change. |
| 8 | A patch layer cannot create a row, and `bough exec` forces `headless` | **IMPLEMENTED, differently.** The three code-mode rows are now DECLARED in `bundles/bough-base.yml` with `disabled: true`, and `bundles/bough-codemode.yml` is the three-line `disabled: false` switch. A `--patch` can therefore reach the consumer, because the rows exist. The default consumer was UNCHANGED at the merge; on 2026-08-28 Andrey took §7.A's GO and code mode became the DEFAULT — the rows ship ENABLED, `profiles/{tui,headless}.yml` compose `bough-codemode` last, and `crates/bough/tests/docs.rs::every_shipped_profile_boots_the_codemode_consumer` asserts that (plus the `bundles/bough-typed.yml` fallback) instead. |
| 9 | The code-mode SHELL surface is unsatisfiable: no registered tool carries `tags` | **IMPLEMENTED, both halves.** (a) `tools-baseline`'s `bash` DECLARES `tags` — an array of 3–5 strings, in `required` before `cwd` so `positional_order` makes the injected signature the documented `bash(cmd, tags)`. (b) `sh` EXISTS: `plugins/tools-operator/src/sh.rs` registers the concurrent shell the prose had always taught, with per-leg `tags`, `[{code, out}, …]` in leg order, a non-zero exit as data, and four new config fields (`sh_max_legs`, `sh_timeout_ms`, `sh_tags_min`, `sh_tags_max`). `surface/shell.md` and `bind.rs` now agree on the ARRAY form; the colon-separated string still parses. `bind.rs`'s tag-count rule became PER LEG (`tag_counts`) — checking the union refused a correctly tagged two-leg `sh` for "carrying 6". Gates: `crates/bough/tests/codemode_shell.rs` (4, end to end on the real binary), `plugins/tools-operator/tests/sh.rs` (7). Cost, recorded deliberately: the surface section grew 3 846 → 4 218 tokens, because the `needs: sh` half of `shell.md` had never been assembled. |
| 10 | Disabling the consumer makes an already-written chain UNREADABLE | **IMPLEMENTED at the root, as the brief asked.** The special case inside `tools-codemode` is GONE; `LedgerHandle::declare_step_types` itself now registers for the life of the BINARY (it spends every token with `StepTypeToken::forget`), so the rule holds for EVERY plugin that writes steps — drafts, claims, wards, collectors, rollups. It is still all-or-nothing: a clash spends the tokens taken so far as inverses and leaves the map untouched. Recorded in `AGENTS.md` as the ONE exception to "registrations are effects". Gates: `plugins/ledger-memory/tests/vocabulary_lifetime.rs` (2), `crates/bough/tests/codemode_swap.rs::the_program_vocabulary_survives_disabling_the_row`, and `scripts/tui/32-codemode-swap.sh`'s `typed_rows_again_after_revert`. |
| 11 | A boot race in the KERNEL makes the whole suite intermittently red | **IMPLEMENTED, and it was TWO bugs.** (a) `PluginBody::load` captured its committed view a second time, after the fiber had already checked `unmet` against the live store; a provider reloading in between took a REQUIRED key away and the row FAILED where the declared dependency should have kept it PENDING. `KernelError::NotReady` now says so and `fiber::drive` puts the row back to Pending, where it loads when the key returns. (b) `exec-headless`'s task is an effect that started the moment `apply` returned — its own comment claimed it ran "AFTER boot quiesces" and it did not — so a wake could begin before a row that registers a TOOL had activated: `tool/result run outcome=error "no tool named `run` is available to agent `sol`"`, with a clean exit 0 and no boot error. It waits for `Kernel::quiesce` first now. Measured: 2 misses in 40 runs under a loaded machine before, 0 in 80 after, and five consecutive clean `cargo nextest run --workspace` (2291 tests). The cosmetic half is fixed too — `ServiceUnavailable` no longer calls a required read "optional". Track C's two "race-shaped flakes" (`exec_headless`, `05-commands.sh`) are the same shape and the same fix. |
| 12 | Two reds this run inherited rather than caused | **IMPLEMENTED / kept.** `24-honesty.sh`'s `gutter` line is gone on both sides. The `Makefile`'s second, explicit code-mode pass survives the merge, renumbered: `scripts/tui/31-program.sh` (the phase's `30-program.sh`; `30-` was taken by track B's `30-swap-wards.sh`) and `scripts/tui/32-codemode-swap.sh`. |
| §12 | Two `tools` accessors the mirror needs | **DEFERRED**, with 1 and 2. `inner_deadline_ms` and `max_parallel_calls` stay config fields kept equal to the seam's by hand in `bundles/bough-base.yml`, and the bundle says so. |

### Two more the merge itself fixed

**`scripts/tui/24-honesty.sh`'s phantom list** (the merge brief's item v). The live half asserted
only `merge_pr` and `deploy_to_production` while the replay half asserted three; `open_pr` and
`push_to_pr` are genuinely registered by `actions-github` + `tool-actions` in the `tui` profile, so
naming either would be dishonest. Both halves now assert the SAME list —
`deploy_to_production`, `send_email`, `merge_pr` — none of which any row registers.

**`plugins/tui-focus/tests/program.rs::the_collapsed_line_is_one_line_of_stable_width`** asserted
the program row is EXACTLY the pane's width. `ux-visual` pass A's visual-audit F7 moved the outcome
glyph from the far edge to just after the arguments, so no tool header pads any more. The program
row is built through `tool_header` and inherits the convention: the case now asserts one line,
never wider than the pane. **Nothing was removed from `plugins/tui-focus/src/program.rs`** — the
boundary the merge brief drew was already respected. It renders the PROGRAM BODY only (the header
through `tool_header`, the JS block, the console output, the nested tool rows) and carries no
speaker labelling and no turn-open/close chrome for the `ux-visual` track to fight.
