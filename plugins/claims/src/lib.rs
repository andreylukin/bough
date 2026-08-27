//! Invariant (§16, P5-D16): ACCEPTANCE IS ANDREY'S ACT. [`ClaimsHandle::decide`] takes the actor
//! as a parameter and refuses any call reached while an agent's ambient initiator scope is set —
//! which is exactly the condition "this call is inside a wake". A claim is how an agent says
//! something it cannot make true on its own; if a wake could accept its own claim, the whole
//! propose/accept boundary would be decoration.
//!
//! P5-D15: one crate holding a Definition, a Provider and one Consumer (the global
//! `propose_claim` tool). There is one conceivable claims Provider, and the global propose tool is
//! three dozen lines that would otherwise be a crate with one file.

pub mod command;
pub mod decide;
pub mod error;
pub mod invariant;
pub mod kind;
pub mod pin;
pub mod query;
pub mod rate;
pub mod tool;

use std::collections::BTreeSet;
use std::sync::Arc;

use bough_kernel::{Context, EmitEvent, Inject, InvariantSpec, Plugin, PluginError, ServiceKey};
use bough_plugin_agents::Agents;
use bough_plugin_graph_ops::{Graph, OpOutcome};
use bough_plugin_ledger::{
    AgentName, Append, Cite, Class, Ledger, Ref, Seq, StepId, StepQuery, StepType, TrajId, WakeId,
};
use chrono::{DateTime, Utc};

pub use decide::lane_traj;
pub use error::ClaimsError;
pub use kind::{parse, ClaimKind};
pub use query::ClaimQuery;
pub use rate::{rejection_rate, Rate};
pub use tool::TOOL_NAME;

bough_util::brand_id!(
    /// The id a `claim/proposed` step mints and every later decision names.
    pub struct ClaimId;
);

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "claims";

/// The `claims` service key.
pub struct Claims;

impl ServiceKey for Claims {
    type Value = ClaimsHandle;
    const NAME: &'static str = "claims";
}

/// The concrete handle newtype the key's value is (Decision D5).
#[derive(Clone)]
pub struct ClaimsHandle(pub Arc<ClaimsInner>);

/// The seam's bound dependencies. No live state: "open" is derived from the ledger every read.
pub struct ClaimsInner {
    pub(crate) ctx: Context,
    pub(crate) ledger: bough_plugin_ledger::LedgerHandle,
    pub(crate) agents: bough_plugin_agents::AgentsHandle,
    pub(crate) graph: bough_plugin_graph_ops::GraphHandle,
    pub(crate) cfg: Arc<ClaimsConfig>,
}

/// A structural proposal: what a split WOULD be, before Andrey has seen it.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct SplitProposal {
    pub parent: AgentName,
    pub at_seq: Option<Seq>,
    pub children: Vec<ProposedChild>,
}

/// One child of a proposed split or bud.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ProposedChild {
    pub agent: Option<AgentName>,
    pub routing_refs: BTreeSet<Ref>,
    pub wake_classes: BTreeSet<String>,
}

/// A proposed merge. `survivor` may be `None`: that absence is a leader question, not a default.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct MergeProposal {
    pub survivor: Option<AgentName>,
    pub absorbed: AgentName,
}

/// A proposed bud from a past point.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct BudProposal {
    pub parent: AgentName,
    pub at_seq: Seq,
    pub child: ProposedChild,
}

/// One claim awaiting Andrey.
#[derive(Clone, Debug, PartialEq)]
pub struct OpenClaim {
    pub claim: ClaimId,
    pub proposal: StepId,
    pub traj: TrajId,
    pub by: AgentName,
    pub kind: ClaimKind,
    pub title: String,
    pub body: String,
    pub at: DateTime<Utc>,
    pub cites: Vec<Cite>,
}

/// The decision. `Accept` and `Edit` are ANDREY'S ACTS.
#[derive(Clone, Debug)]
pub enum Decision {
    Accept,
    Edit { title: String, body: String },
    Reject { reason: String },
}

/// Who is deciding. §16 makes acceptance Andrey's act, so the seam takes it as a parameter and
/// refuses anything else — including a call made while an agent's ambient initiator is set.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Actor {
    Andrey,
}

/// A decision, as a caller asks for it.
#[derive(Clone, Debug)]
pub struct DecideRequest {
    pub claim: ClaimId,
    pub decision: Decision,
    pub actor: Actor,
    pub at: DateTime<Utc>,
}

/// A proposal, as an agent or a plugin asks for it.
#[derive(Clone, Debug)]
pub struct ProposeRequest {
    pub by: AgentName,
    pub traj: TrajId,
    /// The wake the proposal is a step of. `None` ⇒ a synthetic `claim:<id>` wake, for a proposal
    /// made outside any wake (§0.2: the default is explicit, at the boundary).
    pub wake: Option<WakeId>,
    pub kind: ClaimKind,
    pub title: String,
    pub body: String,
    pub cites: Vec<Cite>,
    pub at: DateTime<Utc>,
}

/// What one decision did.
#[derive(Clone, Debug, PartialEq)]
pub struct DecideOutcome {
    pub claim: ClaimId,
    /// `claim/accepted { edited }` or `claim/rejected { reason }`.
    pub step: StepId,
    /// Set when the acceptance produced a pin (a `Requirement`).
    pub pin: Option<StepId>,
    /// Set when the acceptance produced structure (a `Lane` / `Split` / `Merge` / `Bud`).
    pub graph: Option<OpOutcome>,
    /// Set when the acceptance BORE a lane: the row and the resident that now holds it.
    pub born: Option<AgentName>,
}

impl ClaimsHandle {
    /// A seam over the three bound keys.
    ///
    /// DEVIATION from the plan's `new(ledger, agents, graph, cfg)`: the context is a parameter
    /// too, because `claim/decided` is an EMIT event and a handle that cannot emit would make the
    /// event a promise rather than a contract.
    pub fn new(
        ctx: Context,
        ledger: bough_plugin_ledger::LedgerHandle,
        agents: bough_plugin_agents::AgentsHandle,
        graph: bough_plugin_graph_ops::GraphHandle,
        cfg: Arc<ClaimsConfig>,
    ) -> ClaimsHandle {
        ClaimsHandle(Arc::new(ClaimsInner {
            ctx,
            ledger,
            agents,
            graph,
            cfg,
        }))
    }

    /// Open claims, newest first.
    pub async fn open(&self, q: &ClaimQuery) -> Result<Vec<OpenClaim>, ClaimsError> {
        let steps = self
            .0
            .ledger
            .0
            .steps(&StepQuery {
                trajs: q.traj.clone().into_iter().collect(),
                kinds: query::kinds(),
                ..Default::default()
            })
            .await?;
        Ok(query::open(&steps, q, self.0.cfg.open_limit))
    }

    /// One claim, decided or not.
    pub async fn get(&self, claim: &ClaimId) -> Result<Option<OpenClaim>, ClaimsError> {
        match decide::load(&self.0, claim).await {
            Ok((c, _)) => Ok(Some(c)),
            Err(ClaimsError::NoSuchClaim(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Whether a claim has been decided.
    pub async fn is_decided(&self, claim: &ClaimId) -> Result<bool, ClaimsError> {
        Ok(decide::load(&self.0, claim).await?.1)
    }

    /// The only writer of `claim/accepted` and `claim/rejected`. See [`crate::decide`].
    pub async fn decide(&self, req: DecideRequest) -> Result<DecideOutcome, ClaimsError> {
        let outcome = decide::run(&self.0, req).await?;
        self.0.ctx.emit::<ClaimDecided>(outcome.clone());
        Ok(outcome)
    }

    /// A PROPOSAL: appends `claim/proposed`. Agents and plugins may call it; nothing else.
    pub async fn propose(&self, req: ProposeRequest) -> Result<OpenClaim, ClaimsError> {
        let claim = ClaimId::new(uuid::Uuid::now_v7().to_string());
        let body = serde_json::json!({
            "claim": claim.as_str(),
            "kind": req.kind.as_str(),
            "title": req.title,
            "body": req.body,
            kind::DETAIL_KEY: req.kind.detail(),
            query::BY_KEY: req.by.as_str(),
        });
        let step = self
            .0
            .ledger
            .0
            .append(Append {
                traj: req.traj.clone(),
                // A proposal made outside a wake (a command, a test) still needs a wake id; a
                // proposal made inside one is a step of that wake and the caller passes it.
                wake: req
                    .wake
                    .clone()
                    .unwrap_or_else(|| WakeId::new(format!("claim:{claim}"))),
                kind: StepType::new("claim/proposed"),
                class: Class::Thought,
                body,
                cites: req.cites.clone(),
                at: req.at,
                id: None,
            })
            .await?;
        query::as_claim(&step).ok_or_else(|| {
            ClaimsError::Other(anyhow::anyhow!("a claim/proposed step that is not a claim"))
        })
    }

    /// Rejection rate over a window, for drift-watch (§8). PURE over the steps it is handed.
    pub fn rejection_rate(steps: &[bough_plugin_ledger::Step]) -> Option<Rate> {
        rate::rejection_rate(steps)
    }
}

/// `claim/decided` — EMIT. The focus pane and drift-watch listen.
pub struct ClaimDecided;
impl EmitEvent for ClaimDecided {
    const NAME: &'static str = "claim/decided";
    type Payload = DecideOutcome;
}

/// The row's config.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ClaimsConfig {
    /// How many open claims one [`ClaimsHandle::open`] read returns at most.
    pub open_limit: usize,
}

/// The `claims` row.
pub struct ClaimsPlugin;

#[async_trait::async_trait]
impl Plugin for ClaimsPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = ClaimsConfig;

    fn inject() -> Inject {
        Inject::required(["ledger", "agents", "graph"])
            .union(&Inject::optional(["tools", "commands"]))
    }

    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let entry = ctx.entry_id().clone();
        let fail = |e: bough_kernel::KernelError| PluginError::new(entry.clone(), e);

        let ledger = (*ctx.get::<Ledger>().map_err(fail)?).clone();
        let agents = (*ctx.get::<Agents>().map_err(fail)?).clone();
        let graph = (*ctx.get::<Graph>().map_err(fail)?).clone();

        let handle = ClaimsHandle::new(ctx.clone(), ledger, agents, graph, cfg);
        ctx.provide::<Claims>(handle.clone())
            .await
            .map_err(|e| PluginError::new(entry.clone(), e))?;

        // The recorded stream this row's invariants read is per fiber LIFE: a reload starts
        // clean, or a previous instance's decisions would be judged against this one's store.
        ctx.effect(move |e| async move {
            e.defer_sync(invariant::reset);
            Ok(())
        })
        .await?;

        tool::register(&ctx, &handle).await?;
        command::register(&ctx, &handle).await?;
        Ok(())
    }

    fn invariants() -> Vec<InvariantSpec> {
        vec![
            invariant::decided_once(),
            invariant::accepted_requirement_has_a_pin(),
        ]
    }
}

bough_kernel::register_plugin!(ClaimsPlugin);
