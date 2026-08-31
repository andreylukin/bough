//! Invariant (P5-D11 again, for an ordinary lane): every registration is made from THIS ROW's ctx
//! and scoped to its lane by spec, so a patch that drops a lane from the list unwinds exactly that
//! lane's section and restriction and nothing else.
//!
//! P5-D17: the GLOBAL persona section is this row's too. Shadowing is only demonstrable against a
//! twin, and no row contributed a persona section before this phase — without one,
//! "most-specific-wins" has nothing to win against and V6 could only assert that a section
//! appeared. Keeping both halves in one row keeps the pair honest: same `SectionId`, one place to
//! read the rule.
//!
//! A lane named by config with no live agent is a WARNING at apply and a RETRY on `agent/created`,
//! never a boot failure: lanes are born at runtime, and a config that names tomorrow's lane must
//! not stop today's boot.

pub mod invariant;

use std::collections::BTreeSet;
use std::sync::Arc;

use bough_kernel::{Context, EffectHandle, Inject, InvariantSpec, Plugin, PluginError};
use bough_plugin_agents::{Agents, AgentsHandle};
use bough_plugin_ledger::AgentName;
use bough_plugin_projection::{
    DropPriority, Place, Position, Projection, ProjectionError, ProjectionHandle, SectionBody,
    SectionCites, SectionId, SectionRender, SectionRequest, SectionScope, SectionSpec, Slot,
};
use bough_plugin_tools::{Restrict, ToolName, Tools, ToolsHandle};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "lane-scope";

/// The `SectionId` both the global persona and every lane's persona are contributed under. One id
/// is what makes the lane's section SHADOW the global one rather than sit beside it.
pub const PERSONA_SECTION_ID: &str = "persona";

/// The persona rides the identity band, right after it: who the agent is, then how it behaves.
pub const PERSONA_POSITION: Position = Position {
    slot: Slot::Identity,
    place: Place::After,
};

/// The section title, so a shadowed and a shadowing section are indistinguishable in shape and
/// differ only in text.
pub const PERSONA_TITLE: &str = "Persona";

/// The row's config.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LaneScopeConfig {
    /// The GLOBAL persona section. `None` ⇒ no global section, and a lane's persona is then
    /// simply additive (P5-D17).
    pub default_persona: Option<String>,
    /// One entry per lane that wants a scoped world.
    pub lanes: Vec<LaneSpec>,
}

/// One lane's scoped world.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LaneSpec {
    pub agent: String,
    /// Replaces the global persona section FOR THIS AGENT (same `SectionId`, agent scope).
    pub persona: Option<String>,
    /// §5's intersection filter. `None` ⇒ everything the deny list admits.
    pub allow: Option<Vec<String>>,
    #[serde(default)]
    pub deny: Vec<String>,
}

/// The one [`SectionId`] both halves are contributed under.
pub fn persona_section_id() -> SectionId {
    SectionId::new(PERSONA_SECTION_ID)
}

/// A section whose body is a fixed string from config.
///
/// It carries NO cites, and that is not an oversight: it renders config, not ledger rows, so there
/// is no row for it to name — the bundle patch is its durable record.
pub struct StaticSection(pub String);

#[async_trait::async_trait]
impl SectionRender for StaticSection {
    async fn render(&self, _req: &SectionRequest) -> Result<Option<SectionBody>, ProjectionError> {
        // An empty persona contributes NOTHING rather than an empty band (the `about-line`
        // precedent): a band with no text is noise in every projection that carries it.
        if self.0.trim().is_empty() {
            return Ok(None);
        }
        Ok(Some(SectionBody {
            title: PERSONA_TITLE.to_string(),
            body: self.0.clone(),
            cites: SectionCites::default(),
        }))
    }
}

/// The global persona spec.
pub fn global_section(text: &str) -> SectionSpec {
    SectionSpec {
        id: persona_section_id(),
        position: PERSONA_POSITION,
        scope: SectionScope::Global,
        agent: None,
        // Identity is never dropped (§5): an answer wake must always know who it is.
        priority: DropPriority::Never,
        render: Arc::new(StaticSection(text.to_string())),
    }
}

/// One lane's persona spec, which shadows [`global_section`] for that agent alone.
pub fn lane_section(agent: &AgentName, text: &str) -> SectionSpec {
    SectionSpec {
        id: persona_section_id(),
        position: PERSONA_POSITION,
        scope: SectionScope::Agent,
        agent: Some(agent.clone()),
        priority: DropPriority::Never,
        render: Arc::new(StaticSection(text.to_string())),
    }
}

/// PURE: the [`Restrict`] one [`LaneSpec`] asks for. Branding happens HERE, at the boundary,
/// because config carries strings and the registry carries [`ToolName`]s (AGENTS.md).
pub fn restrict_of(spec: &LaneSpec) -> Restrict {
    Restrict {
        allow: spec
            .allow
            .as_ref()
            .map(|names| names.iter().map(ToolName::new).collect()),
        deny: spec.deny.iter().map(ToolName::new).collect(),
    }
}

/// Whether a [`LaneSpec`] asks for any restriction at all. An entry with `allow: None` and an
/// empty deny list is a persona-only lane, and registering an open [`Restrict`] for it would put a
/// no-op row in the registry that reads, at a glance, like a filter.
pub fn restricts(spec: &LaneSpec) -> bool {
    spec.allow.is_some() || !spec.deny.is_empty()
}

/// What one apply pass mounted, and what it could not.
///
/// DEVIATION from plan §2.6, which describes the warning-then-retry rule in prose only: the
/// pending set is RETURNED rather than merely logged, so `a_lane_named_by_config_that_does_not_
/// exist_yet_is_a_warning_then_a_retry` asserts the rule against a value instead of against a log
/// line. `apply` keeps the same set to drive the retry.
#[derive(Default)]
pub struct Mounted {
    /// The effects, in registration order. Dropping them does nothing; disposing them unwinds.
    pub effects: Vec<EffectHandle>,
    /// Lanes whose agent was live and whose registrations are in force.
    pub mounted: Vec<AgentName>,
    /// Lanes named by config with no live agent. Warned about, and retried on `agent/created`.
    pub pending: Vec<AgentName>,
}

/// Register one lane's section and restriction from `ctx`. Total: a lane with neither a persona
/// nor a restriction registers nothing and is still reported as mounted.
pub async fn mount_lane(
    ctx: &Context,
    projection: &ProjectionHandle,
    tools: &ToolsHandle,
    spec: &LaneSpec,
) -> Result<Vec<EffectHandle>, PluginError> {
    let agent = AgentName::new(&spec.agent);
    let mut effects = Vec::new();
    if let Some(text) = &spec.persona {
        effects.push(projection.section(ctx, lane_section(&agent, text)).await?);
    }
    if restricts(spec) {
        effects.push(tools.restrict(ctx, &agent, restrict_of(spec)).await?);
    }
    Ok(effects)
}

/// The whole of the row's registration, minus the retry listener: the global section, then every
/// lane whose agent is live.
pub async fn mount(
    ctx: &Context,
    projection: &ProjectionHandle,
    tools: &ToolsHandle,
    agents: &AgentsHandle,
    cfg: &LaneScopeConfig,
) -> Result<Mounted, PluginError> {
    let mut out = Mounted::default();
    if let Some(text) = &cfg.default_persona {
        out.effects
            .push(projection.section(ctx, global_section(text)).await?);
    }
    for lane in &cfg.lanes {
        let agent = AgentName::new(&lane.agent);
        if agents.by_name(&agent).is_none() {
            // A config that names tomorrow's lane must not stop today's boot (§2.6).
            tracing::warn!(
                agent = %agent,
                "lane-scope: no live agent yet; the lane's scope will mount on agent/created"
            );
            out.pending.push(agent);
            continue;
        }
        out.effects
            .extend(mount_lane(ctx, projection, tools, lane).await?);
        out.mounted.push(agent);
    }
    Ok(out)
}

/// The `lane.scope` row.
pub struct LaneScopePlugin;

#[async_trait::async_trait]
impl Plugin for LaneScopePlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = LaneScopeConfig;

    fn inject() -> Inject {
        Inject::required(["agents", "tools", "projection"])
    }

    fn validate(cfg: &Self::Config) -> Result<(), bough_kernel::ConfigError> {
        // Two entries for one lane is a config typo whose effect — two restrictions intersecting
        // and two sections tied on one `SectionId` — is exactly the kind of thing nobody would
        // read back out of the registry.
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        for lane in &cfg.lanes {
            if !seen.insert(lane.agent.as_str()) {
                return Err(bough_kernel::ConfigError::Rejected {
                    detail: format!("lane `{}` is named twice", lane.agent),
                });
            }
        }
        Ok(())
    }

    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let entry = ctx.entry_id().clone();
        let err = |e: bough_kernel::KernelError| PluginError::new(entry.clone(), e);
        let projection = ctx.get::<Projection>().map_err(err)?;
        let tools = ctx.get::<Tools>().map_err(err)?;
        let agents = ctx.get::<Agents>().map_err(err)?;

        let mounted = mount(&ctx, &projection, &tools, &agents, &cfg).await?;
        let pending: Arc<parking_lot::Mutex<BTreeSet<AgentName>>> = Arc::new(
            parking_lot::Mutex::new(mounted.pending.into_iter().collect()),
        );

        // The retry. It registers from THIS row's ctx, so a lane born at runtime gets a scope
        // owned by the row rather than by the agent that happened to trigger it.
        let ctx2 = ctx.clone();
        let cfg2 = cfg.clone();
        let projection2 = projection.clone();
        let tools2 = tools.clone();
        ctx.on::<bough_plugin_agents::AgentCreated, _, _>(move |agent| {
            let ctx = ctx2.clone();
            let cfg = cfg2.clone();
            let projection = projection2.clone();
            let tools = tools2.clone();
            let pending = pending.clone();
            async move {
                let name = agent.name().clone();
                if !pending.lock().remove(&name) {
                    return;
                }
                let Some(lane) = cfg.lanes.iter().find(|l| l.agent == name.as_str()) else {
                    return;
                };
                if let Err(e) = mount_lane(&ctx, &projection, &tools, lane).await {
                    tracing::error!(agent = %name, error = %e, "lane-scope: the retry failed");
                }
            }
        })
        .await?;
        Ok(())
    }

    fn invariants() -> Vec<InvariantSpec> {
        // See `invariant.rs`: no runtime invariant, and why.
        Vec::new()
    }
}

bough_kernel::register_plugin!(LaneScopePlugin);

#[cfg(test)]
mod tests {
    use super::*;

    fn lane(agent: &str, allow: Option<&[&str]>, deny: &[&str]) -> LaneSpec {
        LaneSpec {
            agent: agent.to_string(),
            persona: None,
            allow: allow.map(|a| a.iter().map(|s| s.to_string()).collect()),
            deny: deny.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn restrict_of_brands_at_the_boundary() {
        let r = restrict_of(&lane("terra", Some(&["bash", "read_file"]), &["bash"]));
        assert_eq!(
            r.allow,
            Some(["bash", "read_file"].iter().map(ToolName::new).collect())
        );
        assert!(!r.admits(&ToolName::new("bash")), "a denial wins");
        assert!(r.admits(&ToolName::new("read_file")));
        assert!(!r.admits(&ToolName::new("grep")), "outside the allow list");
    }

    #[test]
    fn a_persona_only_lane_registers_no_restriction() {
        assert!(!restricts(&lane("terra", None, &[])));
        assert!(restricts(&lane("terra", None, &["bash"])));
        assert!(restricts(&lane("terra", Some(&[]), &[])));
    }

    #[test]
    fn a_lane_named_twice_is_refused_at_compose() {
        let cfg = LaneScopeConfig {
            default_persona: None,
            lanes: vec![lane("terra", None, &["bash"]), lane("terra", None, &[])],
        };
        assert!(LaneScopePlugin::validate(&cfg).is_err());
    }
}
