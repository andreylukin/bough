//! §7/§10, V3: the boundary block reaches EVERY agent's assembled context — a resident and a
//! worker alike — carries the same bytes as the const, and survives every rung of §5's
//! degradation ladder. Assembled through the real provider, not asserted against a registry.

use std::collections::BTreeSet;
use std::sync::Arc;

use bough_kernel::{Context, KernelCore};
use bough_plugin_boundary_instructions::{section_spec, BOUNDARY_BLOCK, SECTION_TITLE};
use bough_plugin_ledger::{AgentName, AgentRow, LedgerHandle, TrajId};
use bough_plugin_ledger_memory::store::MemoryStore;
use bough_plugin_projection::{AssembleRequest, Assembled, Flag, Projector};
use bough_plugin_projection_assembler::{Assembler, AssemblerConfig};
use chrono::{TimeZone, Utc};

/// A resident and a worker: two `agents` rows, which is all the difference there is at the
/// ledger. `SectionScope::Global` is what makes one registration reach both.
const RESIDENT: &str = "sol";
const WORKER: &str = "worker:w1";

async fn fixture() -> (Arc<Assembler>, LedgerHandle) {
    let ctx = Context::root(KernelCore::new());
    let ledger = LedgerHandle(MemoryStore::new(ctx.clone()));
    for (name, traj) in [(RESIDENT, "t-sol"), (WORKER, "t-w1")] {
        ledger
            .0
            .put_agent(AgentRow {
                name: AgentName::new(name),
                traj: TrajId::new(traj),
                routing_refs: BTreeSet::new(),
                wake_classes: BTreeSet::new(),
                model_override: None,
                tick_floor: None,
                digest_rollup: None,
            })
            .await
            .expect("agents is mutable config");
    }
    let cfg = AssemblerConfig {
        budget_tokens: 1_000,
        headroom: 1.0,
        tail_steps: 20,
        tail_floor_steps: 5,
        mail_newest_n: 3,
        max_tiers: 3,
        file_view_dir: std::path::PathBuf::from("/nonexistent-unless-a-test-writes"),
    };
    let assembler = Assembler::new(Arc::new(cfg), ledger.clone(), ctx);
    (assembler, ledger)
}

async fn assemble(assembler: &Arc<Assembler>, agent: &str, budget: Option<usize>) -> Assembled {
    assembler
        .assemble(&AssembleRequest {
            agent: AgentName::new(agent),
            wake: None,
            at: Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).unwrap(),
            budget,
            as_of: None,
        })
        .await
        .expect("the projection assembles")
}

fn boundary(out: &Assembled) -> &bough_plugin_projection::RenderedSection {
    out.sections
        .iter()
        .find(|s| s.id.as_str() == "boundary")
        .unwrap_or_else(|| {
            panic!(
                "no boundary section for `{}`; got: {}",
                out.agent,
                out.sections
                    .iter()
                    .map(|s| s.id.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
}

/// One source of the text: what the section renders is the const, byte for byte.
#[tokio::test]
async fn the_section_text_is_byte_identical_to_the_const() {
    let (assembler, _l) = fixture().await;
    assembler.section(section_spec()).expect("it registers");
    let out = assemble(&assembler, RESIDENT, None).await;
    let s = boundary(&out);
    assert_eq!(s.body, BOUNDARY_BLOCK, "the section is not the const");
    assert_eq!(s.title, SECTION_TITLE);
}

/// V3: one global registration, both kinds of agent.
#[tokio::test]
async fn the_section_reaches_a_resident_and_a_worker() {
    let (assembler, _l) = fixture().await;
    assembler.section(section_spec()).expect("it registers");
    for agent in [RESIDENT, WORKER] {
        let out = assemble(&assembler, agent, None).await;
        assert_eq!(boundary(&out).body, BOUNDARY_BLOCK, "agent `{agent}`");
    }
}

/// `DropPriority::Never`: at a budget of one token every rung of §5's ladder has run, the
/// projection is flagged over-budget, and the boundary is STILL there. A buildable wake without
/// the boundary is worse than no wake.
#[tokio::test]
async fn the_section_survives_every_degradation_rung() {
    let (assembler, _l) = fixture().await;
    assembler.section(section_spec()).expect("it registers");
    for agent in [RESIDENT, WORKER] {
        let out = assemble(&assembler, agent, Some(1)).await;
        assert!(
            out.flags.contains(&Flag::OverBudget),
            "a 1-token budget must have run the whole ladder for `{agent}`: {:?}",
            out.flags
        );
        assert_eq!(boundary(&out).body, BOUNDARY_BLOCK, "agent `{agent}`");
        assert!(
            boundary(&out).degraded.is_none(),
            "a `Never` section is not shortened either"
        );
    }
}

/// The row's own runtime invariant, run against a real assembly rather than a hand-built one.
#[tokio::test]
async fn the_invariant_passes_on_a_real_assembly_and_fails_without_the_row() {
    let (assembler, _l) = fixture().await;
    let without = assemble(&assembler, RESIDENT, None).await;
    assert!(
        bough_plugin_boundary_instructions::invariant::check(&without).is_err(),
        "with no boundary row mounted the check must fail: that is what makes it a check"
    );
    assembler.section(section_spec()).expect("it registers");
    let with = assemble(&assembler, RESIDENT, Some(1)).await;
    bough_plugin_boundary_instructions::invariant::check(&with).expect("the boundary is there");
}
