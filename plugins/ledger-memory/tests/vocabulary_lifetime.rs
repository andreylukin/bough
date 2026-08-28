//! A plugin's step-type VOCABULARY outlives the row that declared it.
//!
//! This is the one documented exception to §0.2's "registrations are effects; unload leaves no
//! trace" (AGENTS.md states the rule, `LedgerHandle::declare_step_types` carries the argument).
//! Everything else a row contributes is a fact about the RUNNING tree. A step type is not: it
//! describes BYTES THAT ARE ALREADY ON DISK, and those outlive the row.
//!
//! MERGE: it used to unwind, and that cost two bugs. `plugins/graph-ops` (D-WP8-5) had to filter
//! a chain read to the wake vocabulary because disabling any row could unregister a type on the
//! chain, and phase codemode's own swap gate found the sharp end of it
//! (`docs/codemode-merge-notes.md` §10, `scripts/tui/32-codemode-swap.sh`): disable the consumer,
//! run a program, disable it again, and the NEXT WAKE DIED with `step ... has type
//! `program/console`, unknown to this binary and not ignorable`.
//!
//! Nothing here is code-mode specific. The property belongs to every plugin that writes steps —
//! drafts, claims, wards, collectors, rollups — so it is asserted here, on the Definition.

use std::sync::Arc;

use bough_kernel::{Context, KernelCore};
use bough_plugin_ledger::{
    Append, Class, ClassRule, LedgerHandle, LedgerStore, Order, StepId, StepQuery, StepType,
    StepTypeDef, TrajId, WakeId,
};
use bough_plugin_ledger_memory::store::MemoryStore;
use chrono::{TimeZone, Utc};

const KIND: &str = "probe/line";

fn ctx() -> Context {
    Context::root(KernelCore::new())
}

/// The body of `probe/line`. A plain struct, exactly as every plugin's vocabulary module writes
/// one — the point is that this test's row is an ORDINARY row.
#[derive(serde::Serialize, schemars::JsonSchema)]
struct ProbeLine {
    text: String,
}

fn defs() -> Vec<StepTypeDef> {
    vec![StepTypeDef::of::<ProbeLine>(KIND, "probe").class_rule(ClassRule::Thought)]
}

fn append(traj: &TrajId, at: i64) -> Append {
    Append {
        traj: traj.clone(),
        wake: WakeId::new("w1"),
        kind: StepType::new(KIND),
        class: Class::Thought,
        body: serde_json::json!({ "text": "written by a row that is about to go away" }),
        cites: Vec::new(),
        at: Utc.timestamp_opt(at, 0).unwrap(),
        id: Some(StepId::new(format!("s{at}"))),
    }
}

/// The whole rule in one case: a row declares a type, writes a step of it, and goes away. The
/// step is still READABLE, and a later reader — the wake that rebuilds the trajectory — does not
/// die on it.
#[tokio::test]
async fn a_step_written_by_a_row_that_is_later_disabled_is_still_readable() {
    let ctx = ctx();
    let ledger = LedgerHandle(MemoryStore::new(ctx.clone()) as Arc<dyn LedgerStore>);
    let traj = TrajId::new("t-probe");

    // The row mounts and declares its vocabulary.
    let effect = ledger
        .declare_step_types(&ctx, defs())
        .await
        .expect("the vocabulary is declared");
    assert!(
        ledger
            .0
            .step_types()
            .iter()
            .any(|d| d.name.as_str() == KIND),
        "the type is known while the row is up"
    );

    ledger
        .0
        .append(append(&traj, 100))
        .await
        .expect("the row writes a step of its own type");

    // The row is DISABLED: its effect unwinds, exactly as a patch layer would make it.
    effect.dispose().await;

    // The vocabulary stays. This is the assertion the old behaviour failed.
    assert!(
        ledger
            .0
            .step_types()
            .iter()
            .any(|d| d.name.as_str() == KIND),
        "the vocabulary unwound with the row: every chain that used it is now unreadable"
    );

    // …and it is not a claim about a map alone: the step reads back.
    let steps = ledger
        .0
        .steps(&StepQuery {
            trajs: vec![traj.clone()],
            order: Order::SeqAsc,
            ..Default::default()
        })
        .await
        .expect("the chain reads after the row is gone");
    assert_eq!(steps.len(), 1, "{steps:?}");
    assert_eq!(steps[0].kind.as_str(), KIND);

    // And a row that comes back declares the same type again without a duplicate error, which is
    // what makes a swap two-way rather than one-way.
    let again = ledger
        .declare_step_types(&ctx, defs())
        .await
        .expect("a byte-identical redeclaration is a reference, not a duplicate");
    again.dispose().await;
    assert!(ledger
        .0
        .step_types()
        .iter()
        .any(|d| d.name.as_str() == KIND));
}

/// The declaration is still ALL-OR-NOTHING: a clash leaves the map exactly as it was. Permanence
/// is not an excuse for a half-registered vocabulary.
#[tokio::test]
async fn a_clashing_declaration_registers_nothing_at_all() {
    let ctx = ctx();
    let ledger = LedgerHandle(MemoryStore::new(ctx.clone()) as Arc<dyn LedgerStore>);

    let mine = StepTypeDef::of::<ProbeLine>("probe/first", "probe").class_rule(ClassRule::Thought);
    // A DIFFERENT definition under a name the builtins already own.
    let clash = StepTypeDef::of::<ProbeLine>("wake/start", "probe").class_rule(ClassRule::Thought);
    let e = match ledger.declare_step_types(&ctx, vec![mine, clash]).await {
        Ok(_) => panic!("a clashing definition must be refused"),
        Err(e) => e,
    };
    assert!(
        e.to_string().contains("wake/start"),
        "the error must name the clash: {e}"
    );
    assert!(
        !ledger
            .0
            .step_types()
            .iter()
            .any(|d| d.name.as_str() == "probe/first"),
        "the type declared before the clash must have been rolled back"
    );
}
