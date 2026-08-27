//! WP-4: the two facts a mounted fault row must have — a FAILED fiber from an `Apply` fault, and a
//! section fault that fails the SECTION rather than `assemble` itself.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use bough_kernel::{
    Catalog, Composer, Composition, Context, ExprEnv, FiberState, Kernel, KernelCore,
    KernelOptions, LayerId, Patch, RowSnapshot,
};
use bough_plugin_fault_inject as fault;
use bough_plugin_ledger::{AgentName, AgentRow, LedgerHandle, TrajId};
use bough_plugin_ledger_memory::store::MemoryStore;
use bough_plugin_projection::{
    AssembleRequest, DropPriority, Place, Position, ProjectionError, Projector, SectionId,
    SectionScope, SectionSpec, Slot,
};
use bough_plugin_projection_assembler::{Assembler, AssemblerConfig};
use chrono::{TimeZone, Utc};

// ---------------------------------------------------------------------------
// harness (the `hello/tests/lifecycle.rs` shape)
// ---------------------------------------------------------------------------

fn compose(catalog: &Catalog, yaml: &str) -> Composition {
    let patch: Patch = serde_yaml::from_str(yaml).expect("the test patch parses");
    let mut composer = Composer::new(catalog, ExprEnv::new("test"));
    composer.layer(LayerId::new("test"), patch);
    composer.compose().expect("the test patch composes")
}

async fn boot(yaml: &str) -> Arc<Kernel> {
    let catalog = Catalog::from_inventory().expect("the linked catalog has no duplicate names");
    let composition = compose(&catalog, yaml);
    let kernel = Kernel::new(
        catalog,
        KernelOptions {
            profile: "test".into(),
            invariants: true,
        },
    );
    kernel.load(composition).await.expect("the tree mounts");
    kernel.quiesce().await;
    kernel
}

fn row(kernel: &Kernel, id: &str) -> RowSnapshot {
    kernel
        .snapshot()
        .rows
        .iter()
        .find(|r| r.id.as_str() == id)
        .cloned()
        .unwrap_or_else(|| panic!("no row `{id}` in the tree"))
}

// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_apply_fault_leaves_the_fiber_failed_and_apply_ran_once() {
    let _guard = fault::test_lock();
    let kernel = boot(
        "\
- id: test.fault
  plugin: fault-inject
  config: { at: apply, how: error, after: 1, times: 0, agent: null }
",
    )
    .await;

    let r = row(&kernel, "test.fault");
    assert_eq!(r.state, FiberState::Failed, "an `Err` from apply is FAILED");
    // Reported, never retried into a loop (§7): apply ran exactly once, and the site was hit once.
    assert_eq!(fault::applies(), 1, "apply is not retried");
    assert_eq!(fault::hits(fault::FaultSite::Apply), 1);

    // And the failure says what it was, so a FAILED row is a report.
    let detail = format!("{r:?}");
    assert!(
        detail.contains("injected failure at `apply`"),
        "the row carries the injected failure; got {detail}"
    );
}

#[tokio::test]
async fn a_projection_section_fault_returns_err_from_the_section_and_not_from_assemble_itself() {
    let _guard = fault::test_lock();

    // A real assembler over a real ledger: the fault has to travel the assembly path.
    let ctx = Context::root(KernelCore::new());
    let ledger = LedgerHandle(MemoryStore::new(ctx.clone()) as Arc<_>);
    ledger
        .0
        .put_agent(AgentRow {
            name: AgentName::new("sol"),
            traj: TrajId::new("t-sol"),
            routing_refs: BTreeSet::new(),
            wake_classes: BTreeSet::new(),
            model_override: None,
            tick_floor: None,
            digest_rollup: None,
        })
        .await
        .expect("the agent row goes in");

    let assembler = Assembler::new(
        Arc::new(AssemblerConfig {
            budget_tokens: 4_000,
            headroom: 1.0,
            tail_steps: 12,
            tail_floor_steps: 3,
            mail_newest_n: 2,
            max_tiers: 3,
            file_view_dir: PathBuf::from("/unused-by-this-test"),
        }),
        ledger.clone(),
        ctx.clone(),
    );

    // Assembly without the fault section succeeds: the baseline that makes the next assertion
    // about the SECTION rather than about the assembler.
    let req = AssembleRequest {
        agent: AgentName::new("sol"),
        wake: None,
        at: Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).unwrap(),
        budget: None,
        as_of: None,
    };
    assembler
        .assemble(&req)
        .await
        .expect("an answer wake must always be buildable");

    let cfg = Arc::new(fault::FaultConfig {
        at: fault::FaultSite::ProjectionSection,
        how: fault::FaultKind::Error,
        after: 1,
        times: 0,
        agent: None,
    });
    let token = assembler
        .section(SectionSpec {
            id: SectionId::new(fault::FAULT_SECTION),
            position: Position {
                slot: Slot::Tail,
                place: Place::After,
            },
            scope: SectionScope::Global,
            agent: None,
            priority: DropPriority::Never,
            render: Arc::new(fault::test_section(cfg)),
        })
        .expect("a fresh section registers");

    let err = assembler
        .assemble(&req)
        .await
        .expect_err("the section fault must surface");
    match err {
        // The error names the SECTION. `assemble` itself did not fail: it failed *on* a section,
        // and says which one — which is what makes a plugin fiber's mid-wake failure attributable.
        ProjectionError::SectionRender { id, detail } => {
            assert_eq!(id.as_str(), fault::FAULT_SECTION);
            assert!(
                detail.contains("injected failure at `projection_section`"),
                "the detail is the injected one; got {detail}"
            );
        }
        other => panic!("expected a SectionRender error naming the section; got {other:?}"),
    }
    assert_eq!(fault::hits(fault::FaultSite::ProjectionSection), 1);

    // With the section gone, assembly is buildable again — the fault was the section's, and
    // removing it leaves the assembler as it was.
    token.remove();
    assembler
        .assemble(&req)
        .await
        .expect("the assembler is unharmed once the faulty section is removed");
}
