//! Invariant (§2): the leader is an ORDINARY AGENT ROW with a plugin set mounted in its scope.
//! Nothing here gives it authority: it adopts unsorted mail, drafts requirements as claims, and
//! proposes structure — every one of which Andrey then accepts or rejects. What makes it "the
//! leader" is a `config.agent` field and four scoped registrations, which is why moving the set to
//! another agent is a patch and not a rewrite.
//!
//! Every registration is an EFFECT owned by THIS ROW's fiber and scoped to the target by SPEC
//! (P5-D11). Editing `leader.config.agent` is a material config diff, so the row reloads: the
//! section leaves the old agent's scope, the sink is replaced by the null sink, `ctx.leader` is
//! withdrawn, and `tool-leader` — which injects `leader` — unloads and reloads against the new
//! binding. No compile, no restart.

pub mod adopt;
pub mod draft;
pub mod error;
pub mod invariant;
pub mod persona;
pub mod timeline;
pub mod vocabulary;

use std::sync::Arc;

use bough_kernel::{Context, Inject, InvariantSpec, Plugin, PluginError, ServiceKey};
use bough_plugin_claims::{Claims, ClaimsHandle, OpenClaim};
use bough_plugin_graph_ops::{Graph, GraphHandle};
use bough_plugin_ledger::{
    AgentName, Append, Class, Ledger, LedgerHandle, Order, StepId, StepQuery, StepType, TrajId,
};
use bough_plugin_mail_router::{Mail, MailHandle, UnsortedSink};
use bough_plugin_projection::Projection;

pub use adopt::{AdoptReport, AdoptRequest};
pub use draft::DraftRequest;
pub use error::LeaderError;
pub use timeline::{TimelineEntry, TimelineQuery, TimelineRow};
pub use vocabulary::{TimelineEntryBody, OWNER};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "leader";

/// The `leader` service key.
pub struct Leader;

impl ServiceKey for Leader {
    type Value = LeaderHandle;
    const NAME: &'static str = "leader";
}

/// The concrete handle newtype the key's value is (Decision D5).
#[derive(Clone)]
pub struct LeaderHandle(pub Arc<LeaderInner>);

/// The seam's bound dependencies and the one name that defines the set.
pub struct LeaderInner {
    target: AgentName,
    ledger: LedgerHandle,
    mail: MailHandle,
    claims: ClaimsHandle,
    #[allow(dead_code)]
    graph: GraphHandle,
    cfg: Arc<LeaderConfig>,
}

/// The unsorted sink the row installs: it names the TARGET, so moving the set moves the sink.
struct LeaderSink(AgentName);

#[async_trait::async_trait]
impl UnsortedSink for LeaderSink {
    fn agent(&self) -> AgentName {
        self.0.clone()
    }
}

impl LeaderHandle {
    /// A seam over the bound keys. `apply` builds the live one; tests build small ones.
    pub fn new(
        target: AgentName,
        ledger: LedgerHandle,
        mail: MailHandle,
        claims: ClaimsHandle,
        graph: GraphHandle,
        cfg: Arc<LeaderConfig>,
    ) -> LeaderHandle {
        LeaderHandle(Arc::new(LeaderInner {
            target,
            ledger,
            mail,
            claims,
            graph,
            cfg,
        }))
    }

    /// The agent this set is mounted for. `tool-leader` reads it; nothing else needs it (P5-D10).
    pub fn target(&self) -> &AgentName {
        &self.0.target
    }

    /// The target's own trajectory, which is where the leader's own steps land.
    pub async fn traj(&self) -> Result<TrajId, LeaderError> {
        self.0
            .ledger
            .0
            .agent(&self.0.target)
            .await?
            .map(|row| row.traj)
            .ok_or_else(|| LeaderError::NoTarget(self.0.target.clone()))
    }

    /// Unsorted adoption (§2): read the queue, route each item to a lane, or hold it.
    ///
    /// Adoption ROUTES; it never decides. Placement is the caller's (the leader's own reading of
    /// the item), and an item with no placement stays on the queue.
    pub async fn adopt(&self, req: AdoptRequest) -> Result<AdoptReport, LeaderError> {
        let candidates: Vec<StepId> = match &req.steps {
            Some(steps) => steps.clone(),
            None => self
                .0
                .mail
                .unsorted(self.0.cfg.adopt_batch)
                .await?
                .into_iter()
                .map(|s| s.id)
                .collect(),
        };
        let (adopted, held) = adopt::plan(&candidates, &req.placements);
        for (step, agent) in &adopted {
            // One call per item, so a lane that cannot take it leaves the REST of the pass alone.
            self.0
                .mail
                .adopt(agent, std::slice::from_ref(step), req.at)
                .await?;
        }
        Ok(AdoptReport { adopted, held })
    }

    /// Requirement drafting from Andrey's words (§2): a claim, never a pin. Acceptance is his.
    pub async fn draft_requirement(&self, req: DraftRequest) -> Result<OpenClaim, LeaderError> {
        Ok(self
            .0
            .claims
            .propose(draft::as_proposal(self.0.target.clone(), &req))
            .await?)
    }

    /// Cross-agent timeline DATA (§17: the surface is Phase 8).
    pub async fn note_timeline(&self, e: TimelineEntry) -> Result<StepId, LeaderError> {
        let traj = self.traj().await?;
        // Evidence with no cites is refused by the ledger itself, which is the rule this method
        // relies on rather than re-checks: a timeline entry nobody can check is not appendable.
        Ok(self
            .0
            .ledger
            .0
            .append(Append {
                traj,
                wake: bough_plugin_agents::mail::outside_wake(),
                kind: StepType::new(vocabulary::TIMELINE_ENTRY),
                class: Class::Evidence,
                body: serde_json::to_value(e.body()).expect("TimelineEntryBody serializes"),
                cites: e.cites.clone(),
                at: e.at,
                id: None,
            })
            .await?
            .id)
    }

    /// Read the timeline back.
    pub async fn timeline(&self, q: &TimelineQuery) -> Result<Vec<TimelineRow>, LeaderError> {
        let steps = self
            .0
            .ledger
            .0
            .steps(&StepQuery {
                trajs: vec![self.traj().await?],
                kinds: vec![StepType::new(vocabulary::TIMELINE_ENTRY)],
                order: Order::SeqAsc,
                ..Default::default()
            })
            .await?;
        Ok(timeline::select(&steps, q))
    }
}

/// The row's config.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LeaderConfig {
    /// THE field this phase's SWAP test edits. The one agent whose scope holds the set.
    pub agent: String,
    /// The persona section's text, contributed at `Slot::Identity` / `Place::After`.
    pub persona: String,
    /// How many unsorted items `adopt_unsorted` may take at once.
    pub adopt_batch: usize,
    /// Attribute reconsolidation passes to the leader (§8) when `reconsolidation` is bound.
    pub attribute_reconsolidation: bool,
}

/// The `leader` row.
pub struct LeaderPlugin;

#[async_trait::async_trait]
impl Plugin for LeaderPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = LeaderConfig;

    fn inject() -> Inject {
        Inject::required(["agents", "ledger", "mail", "graph", "claims", "projection"])
            .union(&Inject::optional(["reconsolidation"]))
    }

    fn validate(cfg: &Self::Config) -> Result<(), bough_kernel::ConfigError> {
        if cfg.agent.trim().is_empty() {
            return Err(bough_kernel::ConfigError::Rejected {
                detail: "leader.agent must name an agent: the set is mounted for exactly one"
                    .to_string(),
            });
        }
        if cfg.adopt_batch == 0 {
            return Err(bough_kernel::ConfigError::Rejected {
                detail: "adopt_batch must be at least 1: a batch of nothing adopts nothing"
                    .to_string(),
            });
        }
        Ok(())
    }

    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let entry = ctx.entry_id().clone();
        let err = |e: bough_kernel::KernelError| PluginError::new(entry.clone(), e);
        let ledger = ctx.get::<Ledger>().map_err(err)?;
        let mail = ctx.get::<Mail>().map_err(err)?;
        let claims = ctx.get::<Claims>().map_err(err)?;
        let graph = ctx.get::<Graph>().map_err(err)?;
        let projection = ctx.get::<Projection>().map_err(err)?;

        let target = AgentName::new(&cfg.agent);

        // Declaration is an effect: unloading the row leaves the step-type map untouched.
        ledger
            .declare_step_types(&ctx, vocabulary::step_types())
            .await?;

        // Scoped to the target BY SPEC, owned by THIS fiber. That pair is the whole of SWAP.
        persona::register(&ctx, &projection, &target, &cfg.persona).await?;
        mail.unsorted_sink(&ctx, Arc::new(LeaderSink(target.clone())))
            .await?;

        ctx.provide::<Leader>(LeaderHandle::new(
            target,
            (*ledger).clone(),
            (*mail).clone(),
            (*claims).clone(),
            (*graph).clone(),
            cfg,
        ))
        .await
        .map_err(err)?;
        Ok(())
    }

    fn invariants() -> Vec<InvariantSpec> {
        vec![invariant::adoption_names_its_unrouted_step()]
    }
}

bough_kernel::register_plugin!(LeaderPlugin);

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(agent: &str, batch: usize) -> LeaderConfig {
        LeaderConfig {
            agent: agent.to_string(),
            persona: "lead".to_string(),
            adopt_batch: batch,
            attribute_reconsolidation: true,
        }
    }

    #[test]
    fn a_set_with_no_target_is_refused_at_compose() {
        assert!(LeaderPlugin::validate(&cfg("  ", 4)).is_err());
        assert!(LeaderPlugin::validate(&cfg("sol", 0)).is_err());
        assert!(LeaderPlugin::validate(&cfg("sol", 1)).is_ok());
    }
}
