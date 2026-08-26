# Track B — what this track wants from the crates it may not edit

Track B (`rebuild-b`, Phase 6) adds NEW CRATES AND NEW ROWS ONLY. The crates listed as off-limits in
`docs/phase-6-plan.md` §0 are not edited by any work package. Where a design wanted a hook that does
not exist on one of them, the crate here builds against the public API that DOES exist and the want
is recorded below, with file, signature and reason, for the merge agent.

These are WANTS, not blockers: everything in `docs/phase-6-plan.md` is implementable without them.

---

## 1. `plugins/worker-spawn/src/boundary.rs` — compose the block instead of restating it

```rust
pub const WRITE_BOUNDARY: &str = concat!(
    WORKER_PREAMBLE,
    bough_plugin_boundary_instructions::BOUNDARY_BLOCK,
    REPORT_INSTRUCTIONS,
);
```

**Why.** V3 asks that the resident's projection and the spawned worker's request carry the same
bytes from the same source. That half IS proven on this branch, on the requests the adapter
actually received, by
`crates/bough/tests/boundary_injection.rs::the_boundary_block_reaches_the_adapter_on_both_paths_with_identical_bytes`:
the `boundary` row's section is `SectionScope::Global`, so BOTH a resident wake's request and a
spawned worker's request carry `BOUNDARY_BLOCK` byte for byte in the system prefix.

What is NOT folded is `worker-spawn`'s own `WRITE_BOUNDARY`, a SECOND, worker-framed block the
spawner prepends to the task message on top of the projection (P6-D3). `plugins/worker-spawn` is
off-limits to track B, so this concat is the merge agent's to make.

**Keep the narrowing when you fold.** The two texts are not interchangeable and the difference is
deliberate: `BOUNDARY_BLOCK` SANCTIONS the four outward acts for an agent, while `WRITE_BOUNDARY`
REFUSES all four to a worker ("You may NOT act outward … they belong to the agent that started
you"). A naive concat that drops `WORKER_PREAMBLE` would silently widen every worker's authority.
Three tests in `plugins/boundary-instructions/src/lib.rs` guard the fold in both directions and
must stay green through it:

- `both_statements_of_the_boundary_name_all_four_sanctioned_acts` — reads the
  `SANCTIONED_ACTS` table, which carries each act's spelling in EACH text (they word the same act
  differently: "push to a pull request" vs "updating a pull request", so one shared substring
  would have pinned nothing).
- `the_spawner_block_refuses_to_a_worker_what_the_boundary_sanctions_for_an_agent` — the narrowing.
- `both_statements_demand_a_citation`.

## 2. `plugins/actions/src/lib.rs` — `find_marker` on `ActionProvider`

```rust
#[async_trait::async_trait]
pub trait ActionProvider: Send + Sync + 'static {
    // …
    async fn find_marker(
        &self,
        kind: ActionKind,
        canonical_target: &str,
        marker: &str,
    ) -> Result<Option<ActionArtifact>, ActionError> { Ok(None) }
}

impl ActionsHandle {
    pub async fn reconcile(&self, now: DateTime<Utc>) -> Result<ReconcileReport, ActionError>;
}
```

**Why.** §7 makes reconciliation a lookup against the world through the provider. `plugins/actions`
is off-limits here, so `plugins/actions-reconcile` declares a SECOND trait (`ArtifactLookup`) and
owns its own registry under the `action_lookup` key (P6-D12). With the method on `ActionProvider`,
`actions-reconcile` folds away and the second registry disappears.

## 3. `plugins/actions/src/lib.rs` — `execute_by_name`

```rust
impl ActionsHandle {
    pub async fn execute_by_name(&self, kind: &str, req: ActionRequest) -> Result<ActionArtifact, ActionError>;
}
```

**Why.** V10 asks for an unspellable action kind to be refused BY THE EXECUTOR. Today
`runtime_actions::parse_kind` refuses it one step earlier, and only a spellable-but-unprovided kind
reaches `ActionError::NoProvider`. Both refusals are tested; only the second is literally "by the
actions executor".

## 4. `plugins/actions/src/error.rs` — `ActionError::BadPayload`

```rust
#[error("`{kind}` payload is not what §7 sanctions: {detail}")]
BadPayload { kind: &'static str, detail: String },
```

**Why.** `actions-linear` refuses a `linear_write` payload naming a title, a team or a new issue —
that is how "ticket creation stays Andrey's" is enforced in the provider as well as by the absent
kind. With no `BadPayload` variant, that refusal has to surface as
`ActionError::Provider { source: LinearActionError::BadPayload }`, which reads as a provider
malfunction rather than a refusal.

## 5. `plugins/actions/src/journal.rs` — an `idem_key` filter on the journal lookup

**Why.** `row_with_idem_key` currently scans every row; its own comment says so. Phase 6 adds a
reconciliation pass that calls it once per pending row.

## 6. `plugins/tools/src/tool.rs` — carry a `StepId` on `ToolCall`

**Why.** Named as a Phase 2 deviation already. Every Phase 6 tool that reaches `ctx.actions`
inherits the synthesised `"{wake}#{step_index}"` triggering step, and with it the consequence that
two calls to one target inside one step collide as `Duplicate`.

## 7. `plugins/agents/src/mail.rs` — `Sender::Ward(String)` and `Sender::Hook(String)`

**Why.** Runtime code posts into a lane's own chat as `Sender::System("ward:<name>")`, which interns
a leaked `&'static str` per distinct ward name.
`bough_plugin_runtime_actions::RuntimeSource::sender_label` is where the spelling lives today.

## 8. `plugins/projection/src/section.rs` — `SectionScope::Kind(AgentKind)`

**Why.** Not needed for V3 (`SectionScope::Global` reaches residents and workers alike, which is
exactly what makes V3 provable). Wanted for anything that should differ between them.

## 9. `plugins/tui-shell/src/pane.rs` — the deferred-work outcome named in `docs/phase-3-plan.md` §6.2

**Why.** The drafts pane reads the ledger in `handle` and inherits the same event-loop blocking.

---

## Deviations this track took, for the merge agent to know about

- **`RuntimeAction::Act`'s `kind` field is spelled `action_kind` ON THE WIRE.** The enum is
  `#[serde(tag = "kind")]` and serde refuses a variant field that shadows its own internal tag. The
  Rust field name is still `kind`.
- **`plugins/actions-reconcile` owns an `action_lookup` service key.** It disappears with merge
  note 2.
- **WP-3: `plugins/actions/src/error.rs` — `ActionError::BadPayload { kind, detail }`.** A
  Provider's payload refusal ("both `status` and `comment`", "no commits to push") is a BAD PAYLOAD,
  not a provider malfunction, and today it can only surface as `ActionError::Provider { source }`
  wrapping `GhActionError` / `LinearActionError`. The wrapping is what the WP-3 tests assert on;
  with the variant they would read the kind directly.
- **WP-3: `actions-github` and `actions-linear` call `gh` / Linear through their own narrow
  transport traits** (`GhRunner` in `plugins/actions-github/src/runner.rs`, `LinearApi` in
  `plugins/actions-linear/src/lib.rs`), with `GhCli` / `LinearHttp` as the one production
  implementation each. That is what lets the tests inject a recording fake with no `PATH` shim and
  no HTTP stub; nothing outward-facing is reachable from a test.
- **WP-3: `push_to_pr` is a fast-forward of the PR's head ref through `gh api`
  (`PATCH repos/{o}/{r}/git/refs/heads/{head}`), and it REFUSES a push whose head commit does not
  already carry the `Bough-Action:` trailer.** §2.5 puts `push_to_pr`'s marker in the commit
  message, which only the process that MADE the commit can do; the Provider therefore verifies the
  trailer by reading the commit before it moves anything, rather than pushing an artifact that
  reconciliation could never find. `marker::commit_trailer` is the pure helper the committing side
  uses.
- **WP-3: `bot_thread_op` always leaves a comment, then resolves/closes.** A resolve with no
  comment would leave no marker in the world and could not be reconciled after a crash.
- **WP-3: `actions-reconcile` reaches drafts through its own `Drafting` trait**, implemented for
  `DraftsHandle`. It exists so the reconciliation pass is testable without the `drafts` row; it
  should collapse onto `DraftsHandle` at merge.
- **WP-3: `GithubActions::open` does not resolve `me` at activation** (the scaffold said it did).
  `gh api user` is resolved lazily and cached on first use, so a row cannot make boot fail over a
  network that nothing has asked it to reach yet. `GithubActions::check_bot_thread` also takes the
  repo as a parameter: a review comment id is only addressable under its repo.

## 10. `crates/bough-kernel/src/context.rs` — dispose ONE child a plugin mounted

```rust
impl Context {
    /// Mount `entry` as a child of this fiber, returning a handle that DISPOSES that child.
    pub async fn mount(&self, entry: Entry) -> Result<FiberHandle, KernelError>;
}

impl FiberHandle {
    pub async fn dispose(&self);
}
```

**Why.** Both file-watching hosts reload by disposing exactly one child and remounting it
(`plugins/skills`, `plugins/wards-rhai`). `Context::mount` returns a `FiberHandle` with no disposal,
and the cascade lives on the fiber's parent uid rather than on the parent's effect accumulator, so
the only way in is `ctx.kernel().unwrap().runtime().dispose(uid)` —
`plugins/skills/src/lib.rs::reconcile` does exactly that. It works and it is public, but it reaches
through `Kernel` and `FiberRuntime` for something a plugin should be able to say on its own handle.

## 11. `plugins/hooks-exec` — two harness points are declared and not wired ✅ CLOSED

**Closed by the review-fix pass.** Both events are defined by track-B crates on this branch
(`bough_plugin_schedule::ScheduleFired`, `bough_plugin_power::PowerChanged`), so the two listeners
are wired here and no merge action is needed. Their `hook/fired` rows land on the `system`
trajectory with a synthetic trigger, because a job fire and a power change belong to no agent's
trajectory. `HooksConfig` also gained a point-name check in two halves: `is_point_shaped` at load
(self-contained) and the `every_configured_point_is_a_point_that_exists` invariant at quiesce (the
step-type vocabulary is not complete until the tree is up). Proven by
`crates/bough/tests/hooks_journal.rs::{the_power_changed_harness_point_fires_on_a_real_power_event,
the_schedule_fired_harness_point_fires_on_a_real_job_run}`.

## 12. `plugins/agents/src/agent.rs` — a way to know when a requested wake FINISHED

```rust
impl Agent {
    /// The wake `request_wake` opened, awaited to completion.
    pub async fn when_wake_done(&self, wake: &WakeId);
}
```

**Why.** `catch-up-on-wake` must drop a second `DidWake` that arrives while the first catch-up is
still running (§2.14). "Still running" is the window between `WakeRequest::Started(id)` and that
wake ending, and the seam exposes no such await: the row uses `Agent::when_idle()` on a spawned
task instead, which is the right answer for an agent doing nothing else and a slightly early one
for an agent that was already mid-wake when the machine woke. The `in_flight` set is the row's own
(`CatchUpOnWake::finish`), so the tests are deterministic either way.

---

## Deviations WP-8 took, for the merge agent to know about

- **`plugins/power` exposes `dispatch(&Context, PowerEvent)` and every Provider goes through it.**
  The Definition's invariant is "the mounted source's `last()` is the last payload that went
  through the seam", and a Provider that dispatched on its own could not be held to it. The
  Definition therefore declares `Inject::optional(["power"])` — a Definition reading the key it
  defines, for the invariant and nothing else.
- **`sleep-listener` owns a `Gate`**, the platform-independent half of a source: it remembers the
  `WillSleep`, measures the wake, drops a wake under `min_sleep_ms`, and writes `last` BEFORE
  dispatching. A dropped wake never becomes `last()`, which is what keeps the seam's invariant
  true. Both macOS hooks and the no-op source are wrapped in one `GatedSource`.
- **`sleep-listener` DEGRADES rather than refuses when `source: auto` and IOKit gives no port**:
  IOKit → NSWorkspace → `noop`, with a warning at each step. An EXPLICIT `iokit`/`nsworkspace` off
  macOS is refused loudly by `choose`, and an explicit `nsworkspace` that will not start on macOS
  fails the row. §13 makes TUI-launch catch-up the reliable baseline, so a laptop that refuses to
  boot bough over a power notification is the worse outcome.
- **The NSWorkspace fallback is hand-rolled through the Objective-C runtime** — `objc_getClass`,
  `objc_allocateClassPair`, `class_addMethod`, and `objc_msgSend` CAST TO ITS EXACT SIGNATURE at
  every call site (the variadic declaration is wrong on aarch64). No `objc2`/`cocoa` dependency was
  added. `NsWorkspaceSource::post` exists so the observer can be exercised by posting into the
  workspace's own notification center, which is how
  `plugins/sleep-listener/tests/macos_ffi.rs::the_nsworkspace_observer_receives_a_posted_sleep_and_wake`
  proves the path without a real sleep.
- **`env("LINEAR_API_KEY")` in the two Linear rows became `env_or("LINEAR_API_KEY", "")`.** As
  written in `docs/phase-6-plan.md` §2.18 it made EVERY boot on a machine without the key fail
  composition with `MissingEnv` — `--dump-config`, `--check`, `make tui-test` and the audit
  included. Both rows already treat an empty key as "this source is off" and say so in a warning.
- **`mcp.call` and `wards.test` are in `bundles/bough-headless.yml` only**, not in
  `bough-base.yml`. §2.18 lists them in the base block and then says headless gains them; headless
  includes base, both subcommands force the headless profile, and putting them in base would mount
  two inert CLI rows in the TUI as well.
- **The subcommand's profile is read, not written.** `Cli::effective_profile()` returns the
  subcommand's profile (headless for all three) and `compose::plan_layers` resolves through it, so
  `--dump-config` and boot agree without either having to normalize first. `Cli::normalize()`
  (called from `main`) only sets `no_watch`. `exec::force_profile` is untouched.

### What WP-8 leaves for the merge

- `plugins/hooks-exec`'s `power/changed` harness point (merge note 11) can now be wired: the event
  is `bough_plugin_power::PowerChanged`, a `ParallelEvent` with payload
  `bough_plugin_power::PowerEvent`, subscribed with `ctx.on_parallel::<PowerChanged, _, _>`.
- The `wards` and `skills` fibers do not settle within the kernel's 5s teardown budget on
  `--check` (`fiber did not settle within 5s; disposing it anyway`). The tree still exits 0 and no
  invariant fires, but the file watchers those two rows hold are not being stopped by their own
  disposers. WP-6/WP-7's crates own it.

## 15. Integration pass (track B, all eight work packages together)

Four things only showed up once every WP's crates were in one tree and `make gates` ran end to end.
All four are fixed on this branch; they are recorded because three of them are notes an earlier
section got wrong, and the fourth is a shape the merge should keep.

- **`actions-github`, `actions-linear` and `actions-reconcile` now declare `ledger` in `inject`.**
  Each one's runtime invariant folds its own `ActionRow`s off `ctx.get::<Ledger>()`, and the
  kernel's undeclared-read check reported all three on every scripted session
  (`crates/bough/tests/agent_invariants.rs::every_invariant_reports_clean_over_a_scripted_session`).
  `required`, not `optional`: the `actions` seam these Providers already require itself requires
  `ledger`, so the declaration cannot widen a tree that would otherwise boot.

- **WP-8's closing note — "the `wards` and `skills` fibers do not settle within the kernel's 5s
  teardown budget" — was a real bug in those two crates, not a watcher-disposal gap, and it is
  fixed.** Both hot-reload tasks sat in `while rx.recv().await.is_some()`, which never reaches a
  halt checkpoint; `EffectInner` awaits a spawned body before running its inverses, so the fiber
  could not settle and the kernel timed it out three times over (`wards`, `skills`, and `commands`
  waiting on them as their provider). Both now poll the channel on a 100ms `RELOAD_POLL` timeout
  and read `EffectCtx::is_halted` between polls. `bough --profile tui` SIGINTs to a clean exit in
  under a second with no "did not settle" line. **Any other long-lived `effect_spawn` body added on
  the merge branch owes the same shape**: an awaited receive with no checkpoint is a teardown hang,
  not a warning.

- **`disabled: true` on the old-feed row retires two Phase 3 surfaces, so those tests re-enable
  exactly that row by patch.** `crates/bough/tests/old_feed_surface.rs` (via the new fixture
  `crates/bough/tests/fixtures/old-feed-on.yml`) and `scripts/tui/07-old-feed.sh` are about the
  ADAPTER, not about whether the shipped tree still boots it — and flipping `disabled` back is
  itself the live proof that the one-week revert path works. `tui_swap.rs` instead asserts the row
  is now `Inactive` in the shipped tree.

- **`tui_swap.rs`'s reflow assertion was racing the next frame.** `TuiHandle::rect_of` answers from
  the LAST DRAW's rectangles while unregistering a pane drops its rect immediately, so membership
  updates synchronously and geometry only catches up on the following frame. Adding `tui.drafts` to
  the bundle changed the timing enough to expose it. The assertion now waits (bounded, 5s) for the
  reflow, and asks whether SOME surviving pane grew rather than naming `tui.focus` — which pane
  absorbs the freed rows depends on who else is mounted in the slot.


## 16. The first message after a cold boot can be swallowed (found by V4's probe)

`scripts/tui/10-drafts.sh` was intermittently failing (~1 run in 4, always the first run of a batch,
always on a cold `$BOUGH_HOME`): after `tui_start` returned and the screen already showed the agent
row, the drafts pane and the composer prompt, the first `shell-use submit` left NO trace at all —
no `user/message` step, no `wake/start`, an empty `ledger.db`. A second submit in the same session
always works. The message is dropped between the composer and the ledger, silently.

The crates that own that path (`tui-shell`, `agents`, `residents`) are ones this track may not edit,
so the script now asserts the echo and retries up to three times
(`the_composer_takes_the_message`), and the drop is recorded here and in `docs/phase-6-plan.md` §6.
**What the merge should do:** find where a composer submit becomes mail, and make the pre-ready case
either queue or report — a submit that reaches no ledger row and no error is indistinguishable, from
the outside, from a boundary refusal, which is exactly the confusion it caused here.

## 17. The ward host bounds its firing loop by RATE; provenance is the better fix

Found by the verification pass (P6-D17 in `docs/phase-6-plan.md`). A ward whose actions cause an
agent to write a step the ward triggers on feeds itself through that agent and fires forever. The
host now bounds it with `WardHostConfig::max_firings_per_minute` (validated `1..=600`, `60` in
`bundles/bough-base.yml`), because a rate catches every loop shape and needs one field.

**What the merge agent should consider instead.** The exact fix is provenance: a ward does not fire
on a step its own actions caused. That needs a cause carried from `runtime-actions`' `RuntimeCx`
(which already knows `RuntimeSource::Ward(name)` and the triggering `Trigger`) through the mail a
`hint` sends, onto the wake it opens, and onto the steps written in that wake — touching
`plugins/agents` and `plugins/workers`, both off-limits to this track. With provenance in place the
rate bound stays as the backstop for loops that close through something else (two wards triggering
each other, a hook, a schedule), so this is an ADDITION, not a replacement.

Merge note 7 (`Sender::Ward(String)`) is the first half of that plumbing and should land first.


---

## 18. What the review-fix pass changed that the merge should know about

- **`plugins/actions-github`'s `bot_thread_op` payload changed shape.** `thread: String` is now
  `comment_id: ReviewCommentId` (a branded u64 — the REST review-comment database id). The
  GraphQL ids the resolve and close mutations need are LOOKED UP (`GithubActions::thread_node_id`)
  and never spelled by the caller. `close` is now `minimizeComment`, a different mutation from
  `resolve`'s `resolveReviewThread`. Anything on the merge branch that constructs a
  `bot_thread_op` payload must be updated.
- **`bundles/bough-base.yml`: `skills.dir` and `wards.dir` are `bough_path(...)`, not
  `home_path(".bough/...")`.** Same location for a real user; hermetic for any test that isolates
  only `$BOUGH_HOME`. Any other row whose default should follow bough's home rather than the
  user's home wants the same treatment — `old-feed`'s `bough_db`/`jungler_db` are deliberately
  `home_path`, because they name the OLD installation's files.
- **`schedule-cron`'s `tick_ms` config field is GONE**, replaced by
  `bough_plugin_schedule_cron::SCHEDULER_TICK_MS`. A bundle on the merge branch that still sets it
  will be refused by `deny_unknown_fields`.
- **`mcp-subprocess`'s `ProcessRow` gained `call_timeout_ms` and `boot_timeout_ms`**, both
  validated as non-zero. A `processes:` entry written before this pass will not deserialize.
- **`plugins/wards-rhai` publishes a `wards.config` service key** (the mounted host row's
  `Arc<WardHostConfig>`) so `bough wards test` dry-fires under the deployment's own limits. The CLI
  row declares it OPTIONAL, which makes the row a dependent and therefore re-appliable — hence
  `DRY_RUN_DONE`, which keeps the dry run to once per process.
- **`plugins/collector-linear` exposes `hold_key`/`release_key`.** The row's disposer releases;
  the set is refcounted so two rows on one key do not blind each other's invariant.
- **`plugins/wards-rhai::evaluate` and `::dry_run` take a `Duration` budget.** Any caller on the
  merge branch needs the extra argument.
