//! Invariant: concealment is VISIBILITY, never authority. Every tool the agent could call before
//! the row mounted is still callable from inside a program, through the SAME pipeline; all that
//! changes is which names the request's tool list carries.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use bough_kernel::{Context, EffectHandle, PluginError};
use bough_plugin_ledger::AgentName;
use bough_plugin_tools::{ApprovalHandle, Restrict, ToolName, ToolScope, ToolSpec, ToolsHandle};

/// How the row hides everything but `run` from the request's tool list.
#[derive(
    Clone,
    Copy,
    PartialEq,
    Eq,
    Debug,
    Default,
    serde::Serialize,
    serde::Deserialize,
    schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ConcealMode {
    /// This branch's interim (§0.1): `Restrict { allow: {run} }` on the real handle, plus a
    /// MIRROR `ToolsHandle` holding the snapshot, which is what inner calls execute against.
    #[default]
    Mirror,
    /// Post-merge: `ToolsHandle::conceal`, one handle, no mirror.
    Seam,
    /// No concealment: `run` sits alongside the typed tools. Test and bench-control arm only.
    None,
}

/// The snapshot a program executes against.
pub struct Mirror {
    /// The specs visible to the agent at the moment `run` was called.
    pub specs: Vec<ToolSpec>,
    /// The handle the inner calls go through — the same pipeline, a private registry.
    pub tools: ToolsHandle,
    /// The registrations that built it, disposed when the program ends. `None` once they have
    /// been taken — by [`Mirror::dispose`] on the happy path, or by [`Drop`] on every other.
    effects: Option<Vec<EffectHandle>>,
}

impl Mirror {
    /// Unwind the mirror's registrations. Called when the program ends, so a long session does
    /// not accumulate one dead registry per round.
    pub async fn dispose(mut self) {
        for e in self.effects.take().unwrap_or_default() {
            e.dispose().await;
        }
    }
}

/// The disposer runs even when `dispose()` never does.
///
/// `Run::call` returns EARLY on a binding error, and its future is DROPPED when the seam's
/// deadline wrap or an interrupt cancels the call — on both paths the explicit `dispose()` at
/// the end of the happy path is never reached. `EffectHandle` has no `Drop` of its own and the
/// mirror's effects are registered against the PLUGIN's context, so without this they would
/// accumulate on the row's fiber for the life of the tree: one dead registration per visible
/// tool per abandoned program.
impl Drop for Mirror {
    fn drop(&mut self) {
        let Some(effects) = self.effects.take() else {
            return;
        };
        if effects.is_empty() {
            return;
        }
        // Disposal is async; the drop is not. Hand it to the runtime this call was running on.
        // Outside a runtime (a test that builds a mirror and drops it on a bare thread) there is
        // nothing to hand it to, and the whole registry dies with the process anyway.
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                for e in effects {
                    e.dispose().await;
                }
            });
        }
    }
}

/// The row's per-agent concealment, and the lock the snapshot window is taken under.
pub struct Concealment {
    mode: ConcealMode,
    /// The live `Restrict { allow: {run} }` effects, one per agent. The mutex is the SNAPSHOT
    /// LOCK too: taking it is what keeps two programs from lifting each other's restriction.
    live: Arc<tokio::sync::Mutex<BTreeMap<AgentName, EffectHandle>>>,
    /// The last UNRESTRICTED view of each agent's tools, taken at install and refreshed at every
    /// snapshot. The surface section reads it: under `Mirror` the real handle answers `run` and
    /// nothing else, so a synchronous read there would document an empty surface.
    cached: parking_lot::Mutex<BTreeMap<AgentName, Vec<ToolSpec>>>,
}

impl Concealment {
    pub fn new(mode: ConcealMode) -> Concealment {
        Concealment {
            mode,
            live: Arc::new(tokio::sync::Mutex::new(BTreeMap::new())),
            cached: parking_lot::Mutex::new(BTreeMap::new()),
        }
    }

    pub fn mode(&self) -> ConcealMode {
        self.mode
    }

    /// The last unrestricted view of `agent`'s tools, if one was ever taken.
    pub fn cached_specs(&self, agent: &AgentName) -> Option<Vec<ToolSpec>> {
        self.cached.lock().get(agent).cloned()
    }

    fn cache(&self, agent: &AgentName, specs: &[ToolSpec]) {
        self.cached.lock().insert(agent.clone(), specs.to_vec());
    }

    /// Is `run` still visible to `agent`? Under `Mirror` that is the whole request tool list.
    pub fn sees_run(&self, tools: &ToolsHandle, agent: &AgentName) -> bool {
        tools
            .visible(agent)
            .iter()
            .any(|n| n.as_str() == crate::RUN_TOOL)
    }

    /// Hide everything but `run` from `agent`'s request tool list. Idempotent per agent.
    pub async fn install(
        &self,
        ctx: &Context,
        tools: &ToolsHandle,
        agent: &AgentName,
    ) -> Result<(), PluginError> {
        match self.mode {
            ConcealMode::None => Ok(()),
            ConcealMode::Seam => Err(PluginError::new(
                ctx.entry_id().clone(),
                anyhow::anyhow!(
                    "conceal mode `seam` needs `ToolsHandle::conceal`, which does not exist on \
                     this branch (see docs/codemode-merge-notes.md); use `mirror` until it lands"
                ),
            )),
            ConcealMode::Mirror => {
                let mut live = self.live.lock().await;
                if live.contains_key(agent) {
                    return Ok(());
                }
                // Read the surface BEFORE hiding it: this is the roster the section documents.
                self.cache(agent, &visible_specs(tools, agent));
                let handle = tools.restrict(ctx, agent, only_run()).await?;
                live.insert(agent.clone(), handle);
                Ok(())
            }
        }
    }

    /// Snapshot `agent`'s visible tools and build the mirror handle they execute against.
    ///
    /// In `Mirror` mode the row's OWN restriction is lifted for the width of the read and put
    /// back before the lock is released; no other row's restriction is ever touched, so a lane's
    /// `deny: [bash]` composes exactly as it did.
    pub async fn snapshot(
        &self,
        ctx: &Context,
        tools: &ToolsHandle,
        agent: &AgentName,
        deadline_ms: u64,
    ) -> Result<Mirror, PluginError> {
        let mut live = self.live.lock().await;
        let lifted = match self.mode {
            ConcealMode::Mirror => live.remove(agent),
            _ => None,
        };
        // ARMED for the whole window in which the agent is un-concealed. Concealment must never
        // fail OPEN: if `restrict` errors, or this future is DROPPED between the dispose and the
        // re-install (the seam's deadline wrap, an interrupt), nothing else would ever put the
        // restriction back — `install` runs only at `apply` and on `AgentCreated`, and it
        // early-returns for an agent already in `live`, which this one no longer is — and the
        // next request would show the model the whole typed tool list under `--profile codemode`.
        let mut relift = lifted.is_some().then(|| Relift {
            live: self.live.clone(),
            ctx: ctx.clone(),
            tools: tools.clone(),
            agent: agent.clone(),
            armed: true,
        });
        if let Some(handle) = &lifted {
            handle.dispose().await;
        }
        let specs = visible_specs(tools, agent);
        self.cache(agent, &specs);
        if lifted.is_some() {
            let handle = tools.restrict(ctx, agent, only_run()).await?;
            live.insert(agent.clone(), handle);
            if let Some(r) = relift.as_mut() {
                r.armed = false;
            }
        }
        drop(relift);
        drop(live);
        mirror_of(ctx, specs, deadline_ms, tools.approval()).await
    }

    /// Is `agent` concealed right now? `None` while a snapshot holds the lock.
    pub fn is_concealed(&self, agent: &AgentName) -> Option<bool> {
        self.live.try_lock().ok().map(|l| l.contains_key(agent))
    }
}

/// Puts the row's own restriction back if the snapshot window does not close cleanly.
///
/// Disarmed the moment the restriction is re-installed. If it drops still armed — a `restrict`
/// that errored, or a cancelled `run` call — it re-installs on the runtime, which is the only
/// place an `async` disposal can go from a `Drop`.
struct Relift {
    live: Arc<tokio::sync::Mutex<BTreeMap<AgentName, EffectHandle>>>,
    ctx: Context,
    tools: ToolsHandle,
    agent: AgentName,
    armed: bool,
}

impl Drop for Relift {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let (live, ctx, tools, agent) = (
            self.live.clone(),
            self.ctx.clone(),
            self.tools.clone(),
            self.agent.clone(),
        );
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        handle.spawn(async move {
            let mut live = live.lock().await;
            if live.contains_key(&agent) {
                // A later snapshot got there first; its restriction is the live one.
                return;
            }
            match tools.restrict(&ctx, &agent, only_run()).await {
                Ok(h) => {
                    live.insert(agent, h);
                }
                Err(e) => tracing::error!(
                    agent = %agent,
                    error = %e,
                    "code mode could not re-conceal an agent after an interrupted snapshot; the \
                     next request would show the typed tool list"
                ),
            }
        });
    }
}

/// The restriction the row installs: the model is shown `run` and nothing else.
pub fn only_run() -> Restrict {
    Restrict {
        allow: Some(BTreeSet::from([ToolName::new(crate::RUN_TOOL)])),
        deny: BTreeSet::new(),
    }
}

/// Rebuild `agent`'s visible `ToolSpec`s from the seam's public API — `visible` + `schemas` +
/// `render_intent` + `resolve`, which between them carry every field of a spec.
///
/// `run` itself is never in the snapshot: a program that could call `run` could nest a program,
/// and the ledger's "sub-steps sit between the call and its result" would stop being a fact.
pub fn visible_specs(tools: &ToolsHandle, agent: &AgentName) -> Vec<ToolSpec> {
    let schemas: BTreeMap<String, bough_plugin_tools::LlmToolDef> = tools
        .schemas(agent)
        .into_iter()
        .map(|d| (d.name.clone(), d))
        .collect();
    let mut out = Vec::new();
    for name in tools.visible(agent) {
        if name.as_str() == crate::RUN_TOOL {
            continue;
        }
        let Ok(tool) = tools.resolve(agent, &name) else {
            continue;
        };
        let (description, schema) = match schemas.get(name.as_str()) {
            Some(d) => (d.description.clone(), d.input_schema.clone()),
            None => (String::new(), serde_json::json!({"type": "object"})),
        };
        let Ok(input_schema) = schemars::Schema::try_from(schema) else {
            continue;
        };
        out.push(ToolSpec {
            name: name.clone(),
            description,
            input_schema,
            render: tools.render_intent(agent, &name),
            // The mirror is per-program and per-agent, so every spec in it is unconditionally
            // visible: the shadowing decision was already made by the snapshot.
            scope: ToolScope::Global,
            tool,
        });
    }
    out
}

/// Register `specs` into a private registry running the SAME pipeline on the SAME context.
///
/// The mirror's parallelism is 1 (a host call executes one call at a time; the program's own
/// `RwLock` is what lets `Promise.all` overlap) and its deadline is the program's wall clock: an
/// inner call cannot legitimately outlive the program that issued it.
pub async fn mirror_of(
    ctx: &Context,
    specs: Vec<ToolSpec>,
    deadline_ms: u64,
    approval: Option<ApprovalHandle>,
) -> Result<Mirror, PluginError> {
    let tools = ToolsHandle::with_limits(1, deadline_ms);
    let mut effects = Vec::new();
    // The mirror is a private registry, but it must answer `ask` the way the REAL one does: a
    // fresh `ToolsInner` starts with `approval: None`, so without this an `ask` decision inside a
    // program degraded to deny on a tree that has an approver mounted, and the two consumers
    // would have answered the same tool differently.
    if let Some(approval) = approval {
        effects.push(tools.mount_approval(ctx, approval).await?);
    }
    for spec in &specs {
        effects.push(tools.register(ctx, spec.clone()).await?);
    }
    Ok(Mirror {
        specs,
        tools,
        effects: Some(effects),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use bough_kernel::KernelCore;
    use bough_plugin_tools::{RenderIntent, Tool, ToolCall, ToolCx, ToolFailure, ToolOutcome};
    use std::sync::Arc;

    struct Nope;

    #[async_trait::async_trait]
    impl Tool for Nope {
        async fn call(
            &self,
            _call: Arc<ToolCall>,
            _cx: ToolCx,
        ) -> Result<ToolOutcome, ToolFailure> {
            unreachable!("these cases never execute a tool")
        }
    }

    fn a_spec(name: &str) -> ToolSpec {
        ToolSpec {
            name: ToolName::new(name),
            description: String::new(),
            input_schema: schemars::Schema::try_from(serde_json::json!({"type": "object"}))
                .unwrap(),
            render: RenderIntent::Generic,
            scope: ToolScope::Global,
            tool: Arc::new(Nope),
        }
    }

    async fn fixture() -> (Context, ToolsHandle, AgentName, Concealment) {
        let ctx = Context::root(KernelCore::new());
        let tools = ToolsHandle::with_limits(1, 1_000);
        for name in ["bash", "view", crate::RUN_TOOL] {
            tools.register(&ctx, a_spec(name)).await.unwrap();
        }
        let agent = AgentName::new("sol");
        let conceal = Concealment::new(ConcealMode::Mirror);
        conceal.install(&ctx, &tools, &agent).await.unwrap();
        (ctx, tools, agent, conceal)
    }

    /// Concealment must not fail OPEN when the snapshot window does not close.
    ///
    /// `snapshot` lifts the row's own `Restrict{allow:{run}}` to read the real tool list. Inside
    /// that window the agent is UN-CONCEALED. If the window never closes — `restrict` returns
    /// `Err` and the `?` propagates, or the `run` call is cancelled and the future is dropped —
    /// nothing else puts the restriction back: `install` runs only at `apply` and on
    /// `AgentCreated`, and it early-returns for an agent it believes is already concealed, which
    /// this one no longer is. The agent stayed un-concealed for the rest of the session and the
    /// very next request showed the model the whole typed tool list under `--profile codemode`.
    ///
    /// The window is reproduced here exactly as `snapshot` opens it — take the handle out of
    /// `live`, dispose it — and then the guard `snapshot` arms across it is dropped STILL ARMED,
    /// which is what a cancelled future or an `Err` does to it.
    #[tokio::test]
    async fn an_interrupted_snapshot_window_puts_the_concealment_back() {
        let (ctx, tools, agent, conceal) = fixture().await;
        assert_eq!(tools.visible(&agent), vec![ToolName::new(crate::RUN_TOOL)]);

        let lifted = conceal
            .live
            .lock()
            .await
            .remove(&agent)
            .expect("the agent is concealed");
        lifted.dispose().await;
        assert!(
            tools.visible(&agent).len() > 1,
            "inside the window the agent really is un-concealed"
        );

        drop(Relift {
            live: conceal.live.clone(),
            ctx: ctx.clone(),
            tools: tools.clone(),
            agent: agent.clone(),
            armed: true,
        });

        // The guard re-installs on the runtime, so give it a turn.
        for _ in 0..200 {
            tokio::task::yield_now().await;
            if tools.visible(&agent) == vec![ToolName::new(crate::RUN_TOOL)] {
                break;
            }
        }
        assert_eq!(
            tools.visible(&agent),
            vec![ToolName::new(crate::RUN_TOOL)],
            "an interrupted snapshot must leave the agent concealed, not showing the typed list"
        );
        assert_eq!(conceal.is_concealed(&agent), Some(true));
    }

    /// …and a guard that was disarmed (the window closed normally) does not install a second
    /// restriction on top of the live one.
    #[tokio::test]
    async fn a_disarmed_relift_does_nothing() {
        let (ctx, tools, agent, conceal) = fixture().await;
        drop(Relift {
            live: conceal.live.clone(),
            ctx: ctx.clone(),
            tools: tools.clone(),
            agent: agent.clone(),
            armed: false,
        });
        for _ in 0..50 {
            tokio::task::yield_now().await;
        }
        assert_eq!(tools.visible(&agent), vec![ToolName::new(crate::RUN_TOOL)]);
        assert_eq!(
            conceal.live.lock().await.len(),
            1,
            "one restriction, not two"
        );
    }

    /// The happy path still ends concealed, and the guard does not double-install.
    #[tokio::test]
    async fn a_completed_snapshot_leaves_exactly_one_restriction() {
        let (ctx, tools, agent, conceal) = fixture().await;
        let mirror = conceal.snapshot(&ctx, &tools, &agent, 1_000).await.unwrap();
        assert_eq!(mirror.specs.len(), 2, "bash and view, never `run` itself");
        for _ in 0..50 {
            tokio::task::yield_now().await;
        }
        assert_eq!(tools.visible(&agent), vec![ToolName::new(crate::RUN_TOOL)]);
        assert_eq!(conceal.is_concealed(&agent), Some(true));
        mirror.dispose().await;
    }

    /// A mirror that is DROPPED rather than disposed still unwinds its registrations.
    ///
    /// `Run::call` returns early on a binding error and is dropped outright on cancellation; on
    /// both paths `dispose()` is never reached. `EffectHandle` has no `Drop`, so before
    /// `Mirror`'s the effects leaked one per visible tool per abandoned program.
    #[tokio::test]
    async fn dropping_a_mirror_disposes_its_registrations() {
        let (ctx, tools, agent, conceal) = fixture().await;
        let mirror = conceal.snapshot(&ctx, &tools, &agent, 1_000).await.unwrap();
        let inner = mirror.tools.clone();
        assert_eq!(
            inner.visible(&agent).len(),
            2,
            "the mirror holds the snapshot"
        );

        drop(mirror);
        for _ in 0..200 {
            tokio::task::yield_now().await;
            if inner.visible(&agent).is_empty() {
                break;
            }
        }
        assert!(
            inner.visible(&agent).is_empty(),
            "an abandoned mirror leaves no registration behind"
        );
    }

    #[test]
    fn the_restriction_shows_run_and_nothing_else() {
        let r = only_run();
        assert!(r.admits(&ToolName::new("run")));
        assert!(!r.admits(&ToolName::new("bash")));
    }

    #[test]
    fn mirror_is_the_default_mode() {
        assert_eq!(ConcealMode::default(), ConcealMode::Mirror);
    }

    /// §9: `ask` is serviced by `ctx.approval` when one is mounted, and degrades to deny only
    /// when none is. The mirror is a PRIVATE registry built with `ToolsHandle::with_limits`,
    /// which mints a fresh `ToolsInner` whose `approval` is `None` — so an `ask` decision inside
    /// a program used to degrade to deny even on a tree with an approver mounted, and the two
    /// consumers would have answered the same tool differently the moment one was.
    #[tokio::test]
    async fn the_mirror_carries_the_real_handles_approver() {
        let (ctx, tools, agent, conceal) = fixture().await;

        // No approver on the real handle: the mirror has none either.
        let mirror = conceal.snapshot(&ctx, &tools, &agent, 1_000).await.unwrap();
        assert!(mirror.tools.approval().is_none());
        mirror.dispose().await;

        tools
            .mount_approval(&ctx, approver())
            .await
            .expect("an approver mounts");
        let mirror = conceal.snapshot(&ctx, &tools, &agent, 1_000).await.unwrap();
        assert!(
            mirror.tools.approval().is_some(),
            "an `ask` inside a program must reach the tree's approver"
        );
        mirror.dispose().await;
    }

    /// A stand-in approver: what it decides is irrelevant, that it is REACHABLE is the point.
    fn approver() -> bough_plugin_tools::ApprovalHandle {
        struct Yes;
        #[async_trait::async_trait]
        impl bough_plugin_tools::Approver for Yes {
            async fn ask(
                &self,
                _call: &ToolCall,
                _reason: &str,
            ) -> bough_plugin_tools::ApprovalOutcome {
                bough_plugin_tools::ApprovalOutcome::Allow
            }
        }
        bough_plugin_tools::ApprovalHandle(Arc::new(Yes))
    }
}
