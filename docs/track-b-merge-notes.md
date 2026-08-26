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
bytes from the same source. Today they carry two differently-worded statements of the same four
refusals (P6-D3), pinned together by
`plugins/boundary-instructions/src/lib.rs::tests::the_spawner_block_states_the_same_refusals`. The
concat makes the identity structural instead of pinned.

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

## 11. `plugins/hooks-exec` — two harness points are declared and not wired

`HARNESS_POINTS` names `boot`, `schedule/fired` and `power/changed` (P6-D13). `boot` is fired by
`HooksExecPlugin::apply`; the other two are not, because their events belong to `plugins/schedule`
and `plugins/power`, which WP-1 and WP-8 own. Wiring them is two `ctx.on::<…>` listeners calling
`HooksHost::fire` with the point's name — no new API on either crate, just the two subscriptions,
once both event types exist on the merged branch.

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
