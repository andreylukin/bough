//! Invariant: this provider seals NOTHING, ever, and says so. `plan` reports every candidate as
//! [`SkipReason::Refused`], `seal` reports [`Stop::NothingToDo`], and `supersede` /
//! `rebuild_digest` return [`RollupsError::Refused`]. It appends no step and makes no model call,
//! which is what makes it a truthful stub rather than a slow one — the swap test reads exactly
//! that difference.

pub mod command;
pub mod invariant;

use std::sync::Arc;

use bough_kernel::{Context, Inject, InvariantSpec, Plugin, PluginError};
use bough_plugin_ledger::Seq;
use bough_plugin_rollups::{
    DigestReport, DigestRequest, PassId, Rollups, RollupsError, RollupsHandle, SealPlan,
    SealReport, SealRequest, Skip, SkipReason, Stop, Summarizer, SupersedeReport, SupersedeRequest,
};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "rollups-none";

/// The stub's config: nothing to tune, so the swap patch is `config: {}`.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NoneConfig {}

/// The stub summarizer.
#[derive(Clone)]
pub struct NoneSummarizer {
    pub ledger: Arc<bough_plugin_ledger::LedgerHandle>,
}

#[async_trait::async_trait]
impl Summarizer for NoneSummarizer {
    fn provider(&self) -> &'static str {
        PLUGIN_NAME
    }

    /// `""`: this provider stamps nothing because it seals nothing.
    fn prompt_ver(&self) -> &str {
        ""
    }

    /// Reads the ledger — and NOTHING else. The plan is total: the whole candidate range is one
    /// [`SkipReason::Refused`] skip, so a caller rendering the plan is told the truth rather than
    /// shown an empty list it could read as "already sealed".
    async fn plan(&self, req: &SealRequest) -> Result<SealPlan, RollupsError> {
        let head = self.ledger.0.head_seq(&req.traj).await?.unwrap_or(Seq(0));
        let upto = req.upto.unwrap_or(head).min(head);
        let skipped = if upto.0 == 0 {
            Vec::new()
        } else {
            vec![Skip {
                tier: 1,
                from_seq: Seq(1),
                to_seq: upto,
                why: SkipReason::Refused,
            }]
        };
        Ok(SealPlan {
            traj: req.traj.clone(),
            head,
            upto,
            blocks: Vec::new(),
            skipped,
        })
    }

    /// No model call, no appended step, no rollup row: [`Stop::NothingToDo`], every time.
    async fn seal(&self, req: &SealRequest) -> Result<SealReport, RollupsError> {
        let plan = self.plan(req).await?;
        Ok(SealReport {
            // The stub runs no pass, so the id is a CONSTANT naming exactly that rather than a
            // fresh uuid pretending a pass happened.
            pass: PassId::new("pass:none"),
            planned: 0,
            sealed: Vec::new(),
            skipped: plan.skipped,
            calls: 0,
            tokens_in: 0,
            tokens_out: 0,
            stop: Stop::NothingToDo,
        })
    }

    async fn supersede(&self, req: &SupersedeRequest) -> Result<SupersedeReport, RollupsError> {
        Err(RollupsError::Refused(format!(
            "`{}` seals nothing, so it cannot supersede `{}`",
            PLUGIN_NAME, req.block
        )))
    }

    /// P5-D13's `reconcile` changes what a SEALING provider writes; it does not make this one
    /// start writing. A merge against `rollups-none` refuses here, exactly as every other rebuild
    /// does, and `graph-ops` reports the refusal rather than inventing a reconciliation.
    async fn rebuild_digest(&self, req: &DigestRequest) -> Result<DigestReport, RollupsError> {
        Err(RollupsError::Refused(format!(
            "`{}` seals nothing, so it cannot rebuild `{}`'s digest",
            PLUGIN_NAME, req.agent
        )))
    }
}

/// The stub provider row.
pub struct RollupsNonePlugin;

#[async_trait::async_trait]
impl Plugin for RollupsNonePlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = NoneConfig;

    fn inject() -> Inject {
        // `ledger` only — it needs the store to answer `plan` honestly — so the swap changes no
        // other row's satisfaction.
        Inject::required(["ledger"]).union(&Inject::optional(["commands"]))
    }

    async fn apply(ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let entry = ctx.entry_id().clone();
        let ledger = ctx
            .get::<bough_plugin_ledger::Ledger>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;
        let stub = NoneSummarizer { ledger };
        ctx.provide::<Rollups>(RollupsHandle(Arc::new(stub.clone())))
            .await
            .map_err(|e| PluginError::new(entry, e))?;
        // §16: the stub is REACHABLE. `/seal` must not vanish with the summarizer — under the
        // stub it reports, truthfully, that nothing will ever be sealed.
        crate::command::register(&ctx, &stub).await?;
        Ok(())
    }

    fn invariants() -> Vec<InvariantSpec> {
        vec![
            bough_plugin_rollups::invariant::seal_once(),
            bough_plugin_rollups::invariant::tiers_are_an_index(),
        ]
    }
}

bough_kernel::register_plugin!(RollupsNonePlugin);
