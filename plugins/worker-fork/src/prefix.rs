//! Invariant (P5-D12): the child's request prefix is PINNED, and the pin is LEDGERED as
//! `fork/prefix`. §10 asks for the parent's request prefix byte-identical, and a child's own
//! projection cannot be that — its identity band names the CHILD and its verbatim tail carries the
//! `fork/end-seed` marker. The pin is an effect on the child's setup, so it unwinds with the child
//! and nothing global remembers it; the step is what keeps §0.2's "the sent request reconstructs
//! from the ledger" true THROUGH a pin: re-assembling `of_agent` at `as_of` reproduces it.

use bough_kernel::{Context, EffectHandle, PluginError};
use bough_plugin_ledger::{AgentName, Append, Class, Seq, StepType, TrajId, WakeId};
use bough_plugin_projection::{Assembled, PrefixSource, ProjectionHandle};
use chrono::{DateTime, Utc};

use crate::vocabulary::{ForkPrefix, FORK_PREFIX};

/// Pin `prefix` for `child` as an effect of `ctx`.
///
/// DEVIATION from the plan's one-function shape (`pin` also appended the step): the append needs
/// a trajectory and a wake, which the caller has and an `AgentName` does not, and splitting the
/// two lets the pinning half be exercised without a ledger. The provider's setup calls both, in
/// this order — pin, then record.
///
/// MERGE (track B -> Phase 5): `projection` is PASSED, not read off `ctx`. `ctx` here is the CHILD
/// AGENT's context — the pin has to unwind with the child (P5-D12) — and that context belongs to
/// the `agents` row, which does not declare `projection` in its `inject`. Resolving the key
/// through it made every fork in the SHIPPED tree die in setup with "plugin `agents` (row
/// `agents`) read service `projection` without declaring it", which no test saw until V3's fork
/// arm booted the real bundle. The handle the fork row already holds (it assembled the prefix with
/// it) is the honest source, and the effect still belongs to the child.
pub async fn pin(
    ctx: &Context,
    projection: &ProjectionHandle,
    child: &AgentName,
    prefix: Assembled,
    source: PrefixSource,
) -> Result<EffectHandle, PluginError> {
    projection
        .pin_prefix(ctx, child.clone(), prefix, source)
        .await
}

/// PURE: the `fork/prefix` row that records where a pin came from. Re-assembling `of_agent` at
/// `as_of` reproduces the pinned bytes, and this row is the only thing that says so.
pub fn prefix_append(
    traj: &TrajId,
    wake: &WakeId,
    of_agent: &AgentName,
    as_of: Seq,
    at: DateTime<Utc>,
) -> Append {
    Append {
        traj: traj.clone(),
        wake: wake.clone(),
        kind: StepType::new(FORK_PREFIX),
        class: Class::Thought,
        body: serde_json::to_value(ForkPrefix {
            of_agent: of_agent.clone(),
            as_of,
        })
        .expect("ForkPrefix serialises"),
        cites: Vec::new(),
        at,
        id: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use bough_kernel::KernelCore;
    use bough_plugin_ledger::LedgerHandle;
    use bough_plugin_ledger_memory::store::MemoryStore;
    use bough_plugin_projection::{AssembleRequest, Projection};
    use bough_plugin_projection_assembler::{Assembler, AssemblerConfig};
    use chrono::TimeZone;

    fn at() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).unwrap()
    }

    fn cfg() -> AssemblerConfig {
        AssemblerConfig {
            budget_tokens: 100_000,
            headroom: 1.0,
            tail_steps: 12,
            tail_floor_steps: 3,
            mail_newest_n: 2,
            max_tiers: 3,
            file_view_dir: std::path::PathBuf::from("/unused-by-these-tests"),
        }
    }

    /// The handle a caller of [`pin`] holds: the fork row resolves it from its OWN context.
    fn handle_of(ctx: &Context) -> ProjectionHandle {
        (*ctx.get::<Projection>().expect("a projection is mounted")).clone()
    }

    /// A root context with a real projection over an empty in-memory ledger.
    async fn mounted() -> Context {
        let ctx = Context::root(KernelCore::new());
        let ledger = LedgerHandle(MemoryStore::new(ctx.clone()));
        let assembler = Assembler::new(Arc::new(cfg()), ledger, ctx.clone());
        ctx.provide::<Projection>(ProjectionHandle(assembler))
            .await
            .expect("the slot is free");
        ctx
    }

    fn request(agent: &str, budget: Option<usize>) -> AssembleRequest {
        AssembleRequest {
            agent: AgentName::new(agent),
            wake: None,
            at: at(),
            budget,
            as_of: None,
        }
    }

    /// A prefix that could never be assembled for the child: proof the bytes are replayed.
    fn parents_prefix() -> Assembled {
        use bough_plugin_projection::{
            Place, Position, RenderedSection, SectionCites, SectionId, Slot,
        };
        Assembled {
            agent: AgentName::new("sol"),
            sections: vec![RenderedSection {
                id: SectionId::new("identity"),
                position: Position {
                    slot: Slot::Identity,
                    place: Place::Band,
                },
                title: "Identity".into(),
                body: "sol / lane/sol".into(),
                cites: SectionCites::default(),
                tokens: 4,
                degraded: None,
            }],
            flags: Default::default(),
            tokens: 4,
            budget: 100,
            cites: SectionCites::default(),
        }
    }

    fn source() -> PrefixSource {
        PrefixSource {
            of_agent: AgentName::new("sol"),
            as_of: Seq(7),
        }
    }

    async fn assemble(ctx: &Context, agent: &str, budget: Option<usize>) -> Assembled {
        ctx.get::<Projection>()
            .expect("a projection is mounted")
            .0
            .assemble(&request(agent, budget))
            .await
            .expect("an answer wake must always be buildable")
    }

    #[tokio::test]
    async fn a_pin_is_returned_verbatim_whatever_the_budget() {
        let ctx = mounted().await;
        let child = AgentName::new("sol/worker-fork-1");
        let _h = pin(&ctx, &handle_of(&ctx), &child, parents_prefix(), source())
            .await
            .expect("pinning is an effect");
        for budget in [Some(1usize), Some(1_000_000), None] {
            let got = assemble(&ctx, child.as_str(), budget).await;
            assert_eq!(
                got,
                parents_prefix(),
                "the pin was re-assembled rather than replayed at budget {budget:?}"
            );
        }
    }

    #[tokio::test]
    async fn a_pin_for_one_agent_does_not_leak_to_another() {
        let ctx = mounted().await;
        let _h = pin(
            &ctx,
            &handle_of(&ctx),
            &AgentName::new("sol/worker-fork-1"),
            parents_prefix(),
            source(),
        )
        .await
        .expect("pinning is an effect");
        let other = assemble(&ctx, "terra", None).await;
        assert_ne!(other, parents_prefix(), "another agent read a foreign pin");
        assert_eq!(other.agent.as_str(), "terra");
    }

    #[tokio::test]
    async fn disposing_the_token_restores_normal_assembly() {
        let ctx = mounted().await;
        let child = AgentName::new("sol/worker-fork-1");
        let h = pin(&ctx, &handle_of(&ctx), &child, parents_prefix(), source())
            .await
            .expect("pinning is an effect");
        assert_eq!(assemble(&ctx, child.as_str(), None).await, parents_prefix());
        h.dispose().await;
        let after = assemble(&ctx, child.as_str(), None).await;
        assert_ne!(
            after,
            parents_prefix(),
            "the pin outlived the effect that held it"
        );
        assert_eq!(
            after.agent, child,
            "and the child assembles as itself again"
        );
    }

    #[test]
    fn the_recorded_step_names_the_parent_and_the_seq() {
        let a = prefix_append(
            &TrajId::new("child"),
            &WakeId::new("w1"),
            &AgentName::new("sol"),
            Seq(7),
            at(),
        );
        assert_eq!(a.kind.as_str(), FORK_PREFIX);
        assert_eq!(a.class, Class::Thought);
        assert_eq!(a.body["of_agent"], "sol");
        assert_eq!(a.body["as_of"], 7);
    }
}
