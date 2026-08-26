//! Invariant: BOTH providers are judged by ONE statement of the contract (the `plugins/ledger`
//! precedent). The suite is parameterised by `seals`, so the stub is held to "seals nothing,
//! refuses honestly, appends no step" and the summarizer to "seals once and indexes what it
//! sealed" — never to two different specs written twice.
//!
//! The suite runs in two halves. The FIRST half judges the provider over an empty trajectory —
//! what both providers must answer identically. The suite then PREPARES the trajectory with real
//! steps and judges the second half over it, because "seals nothing" is not a contract a provider
//! can satisfy by being handed nothing: a case that passes because there was nothing to seal
//! proves nothing about either provider. The `seals` flag is the one place the two providers'
//! answers are allowed to differ.

use bough_plugin_ledger::{
    ActionId, AgentName, Append, Cite, Class, LedgerHandle, Ref, RollupQuery, StepType, TrajId,
    WakeId,
};
use chrono::{Duration, TimeZone, Utc};

use crate::request::{Attribution, DigestRequest, SealRequest, Stop, SupersedeRequest};
use crate::RollupsHandle;

/// The provider-conformance suite.
pub struct Conformance {
    /// `true` for a provider that actually seals; `false` for the truthful stub.
    pub seals: bool,
}

/// The trajectory every case runs against. Named, never prepared: see the module comment.
fn traj() -> TrajId {
    TrajId::new("conformance:rollups")
}

fn agent() -> AgentName {
    AgentName::new("conformance")
}

fn seal_request() -> SealRequest {
    SealRequest {
        agent: agent(),
        traj: traj(),
        at: Utc
            .timestamp_opt(1_700_000_000, 0)
            .single()
            .expect("a valid instant"),
        upto: None,
        max_calls: None,
        attribution: Attribution::System,
    }
}

impl Conformance {
    /// Run every case against a mounted provider. `Err(detail)` names the behaviour that broke.
    pub async fn run(&self, handle: &RollupsHandle, ledger: &LedgerHandle) -> Result<(), String> {
        self.provider_is_named(handle)?;
        self.prompt_ver_is_empty_iff_it_seals_nothing(handle)?;
        self.plan_is_total_and_deterministic(handle, false).await?;
        self.seal_over_an_empty_trajectory_seals_nothing(handle, ledger)
            .await?;

        // ---- the second half, over a trajectory that HAS a history -------------------------
        prepare(ledger).await?;
        self.plan_is_total_and_deterministic(handle, true).await?;
        self.seal_seals_exactly_what_it_planned(handle, ledger)
            .await?;
        self.a_second_pass_over_an_unchanged_ledger_seals_nothing(handle)
            .await?;
        self.supersede_of_an_unknown_block_is_refused(handle)
            .await?;
        self.rebuild_digest_writes_no_tier_rollup(handle, ledger)
            .await?;
        Ok(())
    }

    fn provider_is_named(&self, handle: &RollupsHandle) -> Result<(), String> {
        if handle.0.provider().is_empty() {
            return Err("provider_is_named: `provider()` is empty; the swap test reads it".into());
        }
        Ok(())
    }

    /// `""` iff it seals nothing: a stamp is a promise about what produced a block, and a
    /// provider that produces none must not carry one.
    fn prompt_ver_is_empty_iff_it_seals_nothing(
        &self,
        handle: &RollupsHandle,
    ) -> Result<(), String> {
        let empty = handle.0.prompt_ver().is_empty();
        if empty == self.seals {
            return Err(format!(
                "prompt_ver_is_empty_iff_it_seals_nothing: `{}` seals={} but prompt_ver is {:?}",
                handle.0.provider(),
                self.seals,
                handle.0.prompt_ver()
            ));
        }
        Ok(())
    }

    /// `plan` reads and never writes, so two calls with one request answer the same, and every
    /// candidate is either planned or skipped — never dropped.
    async fn plan_is_total_and_deterministic(
        &self,
        handle: &RollupsHandle,
        prepared: bool,
    ) -> Result<(), String> {
        let req = seal_request();
        let first = handle
            .0
            .plan(&req)
            .await
            .map_err(|e| format!("plan_is_total_and_deterministic: plan failed: {e}"))?;
        let again = handle
            .0
            .plan(&req)
            .await
            .map_err(|e| format!("plan_is_total_and_deterministic: second plan failed: {e}"))?;
        if first != again {
            return Err("plan_is_total_and_deterministic: two plans over one ledger differ".into());
        }
        if !prepared && !first.blocks.is_empty() {
            return Err(format!(
                "plan_is_total_and_deterministic: {} blocks planned over a trajectory with no \
                 steps",
                first.blocks.len()
            ));
        }
        if first.upto.0 > first.head.0 {
            return Err("plan_is_total_and_deterministic: `upto` is above the head".into());
        }
        if prepared {
            if first.head.0 < PREPARED_STEPS as u64 {
                return Err(format!(
                    "plan_is_total_and_deterministic: the head is {} over a trajectory of {} \
                     prepared steps; the plan is not reading the run",
                    first.head.0, PREPARED_STEPS
                ));
            }
            // TOTALITY, over a run that actually has episodes in it: every window the cut
            // produced is either planned or skipped WITH A REASON. A window that is silently
            // dropped is material nothing will ever summarize.
            if self.seals && first.blocks.is_empty() {
                return Err(
                    "plan_is_total_and_deterministic: a sealing provider planned nothing over a \
                     prepared trajectory; the second half of this suite would be vacuous"
                        .into(),
                );
            }
            let covered = first.blocks.len() + first.skipped.len();
            if self.seals && covered < PREPARED_EPISODES {
                return Err(format!(
                    "plan_is_total_and_deterministic: {PREPARED_EPISODES} episodes were cut but \
                     only {covered} ranges are planned or skipped; the rest were dropped"
                ));
            }
        }
        Ok(())
    }

    /// A pass seals exactly the ranges it planned, and the rows it seals are in the store when it
    /// returns. The half of the contract an empty trajectory cannot ask about.
    async fn seal_seals_exactly_what_it_planned(
        &self,
        handle: &RollupsHandle,
        ledger: &LedgerHandle,
    ) -> Result<(), String> {
        let planned = handle
            .0
            .plan(&seal_request())
            .await
            .map_err(|e| format!("seal_seals_exactly_what_it_planned: plan failed: {e}"))?;
        let report = handle
            .0
            .seal(&seal_request())
            .await
            .map_err(|e| format!("seal_seals_exactly_what_it_planned: seal failed: {e}"))?;
        if !self.seals {
            if !report.sealed.is_empty() || report.calls != 0 {
                return Err(format!(
                    "seal_seals_exactly_what_it_planned: a non-sealing provider sealed {} rows in \
                     {} calls",
                    report.sealed.len(),
                    report.calls
                ));
            }
            return Ok(());
        }
        if report.sealed.len() != planned.blocks.len() {
            return Err(format!(
                "seal_seals_exactly_what_it_planned: planned {} blocks, sealed {}",
                planned.blocks.len(),
                report.sealed.len()
            ));
        }
        let rows = ledger
            .0
            .rollups(&RollupQuery {
                trajs: vec![traj()],
                include_superseded: true,
                ..Default::default()
            })
            .await
            .map_err(|e| format!("seal_seals_exactly_what_it_planned: {e}"))?;
        for id in &report.sealed {
            if !rows.iter().any(|r| &r.id == id) {
                return Err(format!(
                    "seal_seals_exactly_what_it_planned: `{id}` is in the report but not in the \
                     store"
                ));
            }
        }
        Ok(())
    }

    async fn seal_over_an_empty_trajectory_seals_nothing(
        &self,
        handle: &RollupsHandle,
        ledger: &LedgerHandle,
    ) -> Result<(), String> {
        let before = rollup_count(ledger).await?;
        let report = handle
            .0
            .seal(&seal_request())
            .await
            .map_err(|e| format!("seal_over_an_empty_trajectory_seals_nothing: {e}"))?;
        if report.stop != Stop::NothingToDo || !report.sealed.is_empty() || report.calls != 0 {
            return Err(format!(
                "seal_over_an_empty_trajectory_seals_nothing: expected NothingToDo with no calls, \
                 got {:?} / {} sealed / {} calls",
                report.stop,
                report.sealed.len(),
                report.calls
            ));
        }
        let after = rollup_count(ledger).await?;
        if after != before {
            return Err(format!(
                "seal_over_an_empty_trajectory_seals_nothing: rollup count moved {before} -> \
                 {after}"
            ));
        }
        Ok(())
    }

    /// Idempotence, the trait's own statement: a second pass over an unchanged ledger seals
    /// nothing, whatever the first one did.
    async fn a_second_pass_over_an_unchanged_ledger_seals_nothing(
        &self,
        handle: &RollupsHandle,
    ) -> Result<(), String> {
        handle
            .0
            .seal(&seal_request())
            .await
            .map_err(|e| format!("a_second_pass_over_an_unchanged_ledger_seals_nothing: {e}"))?;
        let second =
            handle.0.seal(&seal_request()).await.map_err(|e| {
                format!("a_second_pass_over_an_unchanged_ledger_seals_nothing: {e}")
            })?;
        if !second.sealed.is_empty() {
            return Err(format!(
                "a_second_pass_over_an_unchanged_ledger_seals_nothing: the second pass sealed {}",
                second.sealed.len()
            ));
        }
        Ok(())
    }

    /// A supersession names a block; a name that resolves to nothing is REFUSED, never invented.
    async fn supersede_of_an_unknown_block_is_refused(
        &self,
        handle: &RollupsHandle,
    ) -> Result<(), String> {
        let req = SupersedeRequest {
            block: bough_plugin_ledger::RollupId::new("tier:conformance:1:1-4"),
            reason: "the conformance suite asks".into(),
            at: seal_request().at,
            attribution: Attribution::System,
        };
        match handle.0.supersede(&req).await {
            Err(_) => Ok(()),
            Ok(report) => Err(format!(
                "supersede_of_an_unknown_block_is_refused: minted `{}` over a block that does not \
                 exist",
                report.new
            )),
        }
    }

    /// §8: a digest rebuild READS sealed tiers and seals none. Whether the rebuild succeeds over
    /// an empty trajectory is the provider's business; that it wrote no tier row is not.
    async fn rebuild_digest_writes_no_tier_rollup(
        &self,
        handle: &RollupsHandle,
        ledger: &LedgerHandle,
    ) -> Result<(), String> {
        let before = tier_count(ledger).await?;
        let outcome = handle
            .0
            .rebuild_digest(&DigestRequest {
                agent: agent(),
                traj: traj(),
                at: seal_request().at,
                attribution: Attribution::System,
                from_raw: true,
                parents: Vec::new(),
            })
            .await;
        // The OUTCOME is part of the contract, not something to discard: a provider that seals
        // must be able to rebuild a digest over a trajectory that has evidence in it, and one
        // that seals nothing must refuse rather than invent a block.
        match (&outcome, self.seals) {
            (Err(e), true) => {
                return Err(format!(
                    "rebuild_digest_writes_no_tier_rollup: a sealing provider refused a rebuild \
                     over a prepared trajectory: {e}"
                ))
            }
            (Ok(report), false) => {
                return Err(format!(
                    "rebuild_digest_writes_no_tier_rollup: a non-sealing provider minted `{}`",
                    report.digest
                ))
            }
            _ => {}
        }
        if let Ok(report) = &outcome {
            if report.calls == 0 {
                return Err(
                    "rebuild_digest_writes_no_tier_rollup: a rebuild that made no model call \
                     still reported a digest"
                        .into(),
                );
            }
        }
        let after = tier_count(ledger).await?;
        if after != before {
            return Err(format!(
                "rebuild_digest_writes_no_tier_rollup: tier rows moved {before} -> {after}; a \
                 rebuild reads sealed tiers and re-seals none"
            ));
        }
        Ok(())
    }
}

/// How many steps [`prepare`] writes, and over how many episodes.
pub const PREPARED_STEPS: usize = 24;
pub const PREPARED_EPISODES: usize = 3;

/// Give the trajectory a history: three episodes of eight steps, ten hours apart, so the episode
/// cut lands on the episode boundary the way a real day does. Only BUILTIN step types, so the
/// suite needs nothing declared beyond what a ledger provider ships with.
async fn prepare(ledger: &LedgerHandle) -> Result<(), String> {
    let base = seal_request().at;
    for w in 0..PREPARED_EPISODES {
        for i in 0..(PREPARED_STEPS / PREPARED_EPISODES) {
            let at = base + Duration::minutes((w as i64) * 600 + i as i64);
            ledger
                .0
                .append(Append {
                    traj: traj(),
                    wake: WakeId::new(format!("conformance-w{w}")),
                    kind: StepType::new("action/done"),
                    class: Class::Evidence,
                    body: serde_json::json!({
                        "action": ActionId::new(format!("a{w}-{i}")),
                        "status": "done",
                        "artifact": null
                    }),
                    cites: vec![Cite {
                        r#ref: Ref::new(format!("gh:o/r#{w}")),
                        url: None,
                    }],
                    at,
                    id: None,
                })
                .await
                .map_err(|e| format!("the conformance suite could not prepare a history: {e}"))?;
        }
    }
    Ok(())
}

async fn rollup_count(ledger: &LedgerHandle) -> Result<usize, String> {
    ledger
        .0
        .rollups(&RollupQuery {
            trajs: vec![traj()],
            include_superseded: true,
            ..Default::default()
        })
        .await
        .map(|r| r.len())
        .map_err(|e| format!("the conformance suite could not read the ledger: {e}"))
}

async fn tier_count(ledger: &LedgerHandle) -> Result<usize, String> {
    ledger
        .0
        .rollups(&RollupQuery {
            trajs: vec![traj()],
            kind: Some(bough_plugin_ledger::RollupKind::Tier),
            include_superseded: true,
            ..Default::default()
        })
        .await
        .map(|r| r.len())
        .map_err(|e| format!("the conformance suite could not read the ledger: {e}"))
}
