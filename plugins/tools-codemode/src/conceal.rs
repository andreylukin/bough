//! Invariant: concealment is VISIBILITY, never authority. Every tool the agent could call before
//! the row mounted is still callable from inside a program, through the SAME pipeline; all that
//! changes is which names the request's tool list carries.

use std::collections::{BTreeMap, BTreeSet};

use bough_kernel::{Context, EffectHandle, PluginError};
use bough_plugin_ledger::AgentName;
use bough_plugin_tools::{Restrict, ToolName, ToolScope, ToolSpec, ToolsHandle};

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
    /// The registrations that built it, disposed when the program ends.
    effects: Vec<EffectHandle>,
}

impl Mirror {
    /// Unwind the mirror's registrations. Called when the program ends, so a long session does
    /// not accumulate one dead registry per round.
    pub async fn dispose(self) {
        for e in self.effects {
            e.dispose().await;
        }
    }
}

/// The row's per-agent concealment, and the lock the snapshot window is taken under.
pub struct Concealment {
    mode: ConcealMode,
    /// The live `Restrict { allow: {run} }` effects, one per agent. The mutex is the SNAPSHOT
    /// LOCK too: taking it is what keeps two programs from lifting each other's restriction.
    live: tokio::sync::Mutex<BTreeMap<AgentName, EffectHandle>>,
    /// The last UNRESTRICTED view of each agent's tools, taken at install and refreshed at every
    /// snapshot. The surface section reads it: under `Mirror` the real handle answers `run` and
    /// nothing else, so a synchronous read there would document an empty surface.
    cached: parking_lot::Mutex<BTreeMap<AgentName, Vec<ToolSpec>>>,
}

impl Concealment {
    pub fn new(mode: ConcealMode) -> Concealment {
        Concealment {
            mode,
            live: tokio::sync::Mutex::new(BTreeMap::new()),
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
        if let Some(handle) = &lifted {
            handle.dispose().await;
        }
        let specs = visible_specs(tools, agent);
        self.cache(agent, &specs);
        if lifted.is_some() {
            live.insert(agent.clone(), tools.restrict(ctx, agent, only_run()).await?);
        }
        drop(live);
        mirror_of(ctx, specs, deadline_ms).await
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
) -> Result<Mirror, PluginError> {
    let tools = ToolsHandle::with_limits(1, deadline_ms);
    let mut effects = Vec::new();
    for spec in &specs {
        effects.push(tools.register(ctx, spec.clone()).await?);
    }
    Ok(Mirror {
        specs,
        tools,
        effects,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
