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
