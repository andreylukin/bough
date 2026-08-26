//! Invariant: BOTH providers are judged by ONE statement of the contract (the `plugins/ledger`
//! precedent). The suite is parameterised by `seals`, so the stub is held to "seals nothing,
//! refuses honestly, appends no step" and the summarizer to "seals once and indexes what it
//! sealed" — never to two different specs written twice.
//!
//! Every case runs against a trajectory this suite names and never prepares: the provider under
//! test is judged on what it does with an EMPTY history, which is the half of the contract both
//! providers must answer identically. What a provider does with real steps is its own crate's
//! business — `rollups-summarizer` seals, `rollups-none` does not — and the `seals` flag is the
//! one place that difference is stated.

use bough_plugin_ledger::{AgentName, LedgerHandle, RollupQuery, TrajId};
use chrono::{TimeZone, Utc};

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
        self.plan_is_total_and_deterministic(handle).await?;
        self.seal_over_an_empty_trajectory_seals_nothing(handle, ledger)
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
    async fn plan_is_total_and_deterministic(&self, handle: &RollupsHandle) -> Result<(), String> {
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
        if !first.blocks.is_empty() {
            return Err(format!(
                "plan_is_total_and_deterministic: {} blocks planned over a trajectory with no \
                 steps",
                first.blocks.len()
            ));
        }
        if first.upto.0 > first.head.0 {
            return Err("plan_is_total_and_deterministic: `upto` is above the head".into());
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
        let _ = handle
            .0
            .rebuild_digest(&DigestRequest {
                agent: agent(),
                traj: traj(),
                at: seal_request().at,
                attribution: Attribution::System,
                from_raw: true,
            })
            .await;
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
