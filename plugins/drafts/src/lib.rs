//! Invariant: a draft is a STEP and a pane row, and NOTHING ELSE HAPPENS. There is no code path
//! from this crate to a network, to `ctx.actions`, or to any outward surface — the absence is the
//! feature, and `src/invariant.rs` checks the ledger for it rather than trusting the reading.
//!
//! P6-D4: a draft is a THOUGHT, not evidence. It is the agent's own composition; making it
//! evidence would force a citation the agent may not have and would let a draft launder an
//! assertion into the record. Its refs ride on the body so the pane and Phase 5's router can index
//! it.

pub mod invariant;
pub mod tool;
pub mod vocabulary;

use std::collections::BTreeSet;
use std::sync::Arc;

use bough_kernel::{ConfigError, Context, InvariantSpec, Plugin, PluginError, ServiceKey};
use bough_plugin_ledger::{
    AgentName, Append, Class, ClassRule, Ledger, LedgerHandle, Order, Ref, Step, StepId, StepQuery,
    StepType, StepTypeDef, TrajId, WakeId,
};
use chrono::{DateTime, Utc};

pub use vocabulary::{DraftMessage, DraftTicket, DRAFT_MESSAGE, DRAFT_TICKET};

/// The catalog name of the Definition row.
pub const PLUGIN_NAME: &str = "drafts";
/// The catalog name of the tool row.
pub const TOOL_PLUGIN_NAME: &str = "tool-drafts";

/// The `drafts` service key.
pub struct Drafts;

impl ServiceKey for Drafts {
    type Value = DraftsHandle;
    const NAME: &'static str = "drafts";
}

bough_util::brand_id!(
    /// One draft.
    pub struct DraftId;
);

/// A branded id is a plain string in a body schema. `brand_id!` lives in `bough-util`, which has no
/// `schemars` dependency (§0.1), so the impl is written here — the `plugins/ledger/src/id.rs` shape.
impl schemars::JsonSchema for DraftId {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("DraftId")
    }
    fn json_schema(_g: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({ "type": "string" })
    }
}

/// Which of the two things a draft is.
#[derive(
    Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum DraftKind {
    Message,
    Ticket,
}

/// A draft to write.
#[derive(Clone, Debug, PartialEq)]
pub struct NewDraft {
    pub kind: DraftKind,
    pub agent: AgentName,
    pub wake: WakeId,
    /// Where it WOULD go: `"slack:#eng"`, `"linear:TEAM"`, `"email:someone"`. Free text on
    /// purpose: the harness never resolves it, because it never sends it.
    pub audience: String,
    pub subject: String,
    pub body: String,
    pub refs: BTreeSet<Ref>,
    pub at: DateTime<Utc>,
}

/// A draft as read back.
#[derive(Clone, Debug, PartialEq)]
pub struct DraftRow {
    pub id: DraftId,
    pub step: StepId,
    pub kind: DraftKind,
    pub agent: AgentName,
    pub audience: String,
    pub subject: String,
    pub body: String,
    pub refs: Vec<Ref>,
    pub at: DateTime<Utc>,
}

/// What to list.
#[derive(Clone, Debug, Default)]
pub struct DraftQuery {
    pub agents: Vec<AgentName>,
    pub kind: Option<DraftKind>,
    pub limit: Option<usize>,
}

/// The concrete handle the key's value is (Decision D5).
#[derive(Clone)]
pub struct DraftsHandle(pub Arc<DraftsInner>);

/// The seam's state: the ledger it appends into, and the retention the pane reads with.
pub struct DraftsInner {
    ledger: LedgerHandle,
    retain: usize,
}

impl DraftsHandle {
    /// A handle over one ledger.
    pub fn new(ledger: LedgerHandle, retain: usize) -> DraftsHandle {
        DraftsHandle(Arc::new(DraftsInner { ledger, retain }))
    }

    /// The default read-back size, from the row's config.
    pub fn retain(&self) -> usize {
        self.0.retain
    }

    /// Append a `draft/message` or `draft/ticket` step and return it. NOTHING ELSE HAPPENS: the
    /// only call this function makes is `LedgerStore::append`.
    pub async fn draft(&self, d: NewDraft) -> Result<DraftRow, DraftError> {
        if d.audience.trim().is_empty() {
            return Err(DraftError::NoAudience);
        }
        if d.body.trim().is_empty() {
            return Err(DraftError::Empty);
        }
        // A draft belongs to an agent's own chain: without an `agents` row there is no
        // trajectory to append to, and inventing one would file the draft where nobody looks.
        let traj = self.traj_of(&d.agent).await?;
        let id = DraftId::new(uuid::Uuid::now_v7().to_string());
        let refs: Vec<Ref> = d.refs.iter().cloned().collect();
        let (kind, body) = match d.kind {
            DraftKind::Message => (
                DRAFT_MESSAGE,
                serde_json::to_value(DraftMessage {
                    draft: id.clone(),
                    audience: d.audience.clone(),
                    subject: d.subject.clone(),
                    body: d.body.clone(),
                    refs: refs.clone(),
                }),
            ),
            DraftKind::Ticket => (
                DRAFT_TICKET,
                serde_json::to_value(DraftTicket {
                    draft: id.clone(),
                    audience: d.audience.clone(),
                    title: d.subject.clone(),
                    body: d.body.clone(),
                    refs: refs.clone(),
                }),
            ),
        };
        let body = body.map_err(|e| DraftError::Body(e.to_string()))?;
        let step = self
            .0
            .ledger
            .0
            .append(Append {
                traj,
                wake: d.wake.clone(),
                kind: StepType::new(kind),
                class: Class::Thought,
                body,
                // P6-D4: a draft is a THOUGHT. Its refs ride on the body, so a citation the agent
                // may not have is never forced on it.
                cites: Vec::new(),
                at: d.at,
                id: None,
            })
            .await?;
        Ok(DraftRow {
            id,
            step: step.id,
            kind: d.kind,
            agent: d.agent,
            audience: d.audience,
            subject: d.subject,
            body: d.body,
            refs,
            at: d.at,
        })
    }

    /// Read drafts back out of the ledger, newest first. A pure query: this reads what `draft`
    /// wrote and nothing else.
    pub async fn list(&self, q: &DraftQuery) -> Result<Vec<DraftRow>, DraftError> {
        let rows = self.0.ledger.0.agents().await?;
        let wanted: Vec<&bough_plugin_ledger::AgentRow> = rows
            .iter()
            .filter(|a| q.agents.is_empty() || q.agents.contains(&a.name))
            .collect();
        // An agent filter naming nobody must return nothing, NOT everything: an empty `trajs` is
        // "no filter" at the ledger, which would leak another agent's drafts into the answer.
        if wanted.is_empty() {
            return Ok(Vec::new());
        }
        let kinds = match q.kind {
            None => vec![StepType::new(DRAFT_MESSAGE), StepType::new(DRAFT_TICKET)],
            Some(DraftKind::Message) => vec![StepType::new(DRAFT_MESSAGE)],
            Some(DraftKind::Ticket) => vec![StepType::new(DRAFT_TICKET)],
        };
        let steps = self
            .0
            .ledger
            .0
            .steps(&StepQuery {
                trajs: wanted.iter().map(|a| a.traj.clone()).collect(),
                kinds,
                order: Order::SeqDesc,
                limit: Some(q.limit.unwrap_or(self.0.retain)),
                ..Default::default()
            })
            .await?;
        Ok(steps
            .iter()
            .filter_map(|s| {
                let agent = wanted.iter().find(|a| a.traj == s.traj)?;
                row_of(s, &agent.name)
            })
            .collect())
    }

    async fn traj_of(&self, agent: &AgentName) -> Result<TrajId, DraftError> {
        self.0
            .ledger
            .0
            .agent(agent)
            .await?
            .map(|a| a.traj)
            .ok_or_else(|| DraftError::UnknownAgent(agent.clone()))
    }
}

/// PURE: one committed step read back as a draft. A row whose body does not parse is SKIPPED
/// rather than guessed at — a half-read draft shown to Andrey would be a lie about what an agent
/// wrote.
pub fn row_of(step: &Step, agent: &AgentName) -> Option<DraftRow> {
    let kind = match step.kind.as_str() {
        DRAFT_MESSAGE => DraftKind::Message,
        DRAFT_TICKET => DraftKind::Ticket,
        _ => return None,
    };
    let (id, audience, subject, body, refs) = match kind {
        DraftKind::Message => {
            let m: DraftMessage = serde_json::from_value((*step.body).clone()).ok()?;
            (m.draft, m.audience, m.subject, m.body, m.refs)
        }
        DraftKind::Ticket => {
            let t: DraftTicket = serde_json::from_value((*step.body).clone()).ok()?;
            (t.draft, t.audience, t.title, t.body, t.refs)
        }
    };
    Some(DraftRow {
        id,
        step: step.id.clone(),
        kind,
        agent: agent.clone(),
        audience,
        subject,
        body,
        refs,
        at: step.at,
    })
}

/// The two step types this crate owns (P6-D4: `Thought`, never ignorable).
pub fn step_types() -> Vec<StepTypeDef> {
    vec![
        StepTypeDef::of::<DraftMessage>(DRAFT_MESSAGE, PLUGIN_NAME)
            .class_rule(ClassRule::Thought)
            .ignorable(false),
        StepTypeDef::of::<DraftTicket>(DRAFT_TICKET, PLUGIN_NAME)
            .class_rule(ClassRule::Thought)
            .ignorable(false),
    ]
}

/// What a draft refuses.
#[derive(Debug, thiserror::Error)]
pub enum DraftError {
    #[error("a draft needs an audience: say where it would go (`slack:#eng`, `linear:TEAM`, …)")]
    NoAudience,
    #[error("a draft needs a body")]
    Empty,
    #[error("`{0}` is not a live agent")]
    UnknownAgent(AgentName),
    #[error("a draft body would not serialise: {0}")]
    Body(String),
    #[error(transparent)]
    Ledger(#[from] bough_plugin_ledger::LedgerError),
}

/// The Definition row's config.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DraftsConfig {
    /// How many drafts the pane and `list()` read back by default.
    pub retain: usize,
}

/// The Definition row.
pub struct DraftsPlugin;

#[async_trait::async_trait]
impl Plugin for DraftsPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = DraftsConfig;

    fn inject() -> bough_kernel::Inject {
        bough_kernel::Inject::required(["ledger"])
    }

    fn validate(cfg: &Self::Config) -> Result<(), ConfigError> {
        if cfg.retain == 0 {
            // A pane that reads back nothing shows Andrey no draft, silently — which is the one
            // failure mode a draft surface cannot have.
            return Err(ConfigError::Rejected {
                detail: "retain must be greater than zero".to_string(),
            });
        }
        Ok(())
    }

    /// Declare the two step types as an effect, then provide `drafts`.
    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let entry = ctx.entry_id().clone();
        let ledger = ctx
            .get::<Ledger>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;
        ledger.declare_step_types(&ctx, step_types()).await?;
        ctx.provide::<Drafts>(DraftsHandle::new(
            LedgerHandle(ledger.0.clone()),
            cfg.retain,
        ))
        .await
        .map_err(|e| PluginError::new(entry, e))?;
        Ok(())
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

bough_kernel::register_plugin!(DraftsPlugin);
bough_kernel::register_plugin!(tool::DraftToolsPlugin);
