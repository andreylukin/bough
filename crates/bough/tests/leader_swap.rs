//! The Phase 5 exit gate, SWAP half (§17 Phase 5): the `leader` SET moves from one agent's scope
//! to another BY A PATCH EDIT while the tree is up. `leader.config.agent` goes `sol` → `terra`;
//! the five leader tools leave `sol`'s schema and appear in `terra`'s, the persona section moves
//! with them, the unsorted sink moves with them, nothing in the tree fails, and removing the patch
//! puts it all back. No recompile, no restart, one test process, through the launcher's own
//! recompose (`bough::watch::recompose_once`) — the `rollups_swap.rs` precedent.
//!
//! This is what §2 means by "the leader is an ordinary agent row with a plugin set mounted in its
//! scope": if moving it were anything more than a config edit, that sentence would be decoration.

use crate::support;

use std::collections::BTreeSet;
use std::sync::Arc;

use bough_kernel::{FiberState, Kernel};
use bough_plugin_agents::{MailClass, Sender};
use bough_plugin_hello::trace;
use bough_plugin_leader::{Leader, LeaderHandle};
use bough_plugin_ledger::query::StepQuery;
use bough_plugin_ledger::{AgentName, Ledger, LedgerHandle, Ref, StepType, TrajId};
use bough_plugin_mail_router::{Envelope, Mail, MailHandle};
use bough_plugin_projection::{AssembleRequest, Projection};
use bough_plugin_tools::{Tools, ToolsError, ToolsHandle};
use chrono::{TimeZone, Utc};
use support::{boot_real, clear_patch, fixture, recompose, row, write_patch, TempDir};

/// THE swap. One field of one row inside the `leader.set` group.
///
/// A patch layer replaces an entry's `config` map WHOLESALE (§0.5), so the whole map is restated
/// — the `rollups_swap.rs` spelling, and the honest one.
const MOVE_TO_TERRA: &str = "\
entries:
  leader:
    config:
      agent: terra
      persona: |
        You hold the whole population in view. You adopt mail nobody claimed, draft Andrey's
        words into requirement claims, and propose splits, merges and new lanes as claims.
        You accept nothing: acceptance is Andrey's act alone.
      adopt_batch: 20
      attribute_reconsolidation: true
";

/// A phrase from the SHIPPED persona in `bundles/bough-tui-app.yml`. The section's id is
/// `lane-scope`'s business (P5-D17) and the leader's own contribution may sit under another; what
/// V6 and this gate are about is whether the TEXT is in an agent's projection at all.
const PERSONA_MARK: &str = "acceptance is Andrey's act alone";

/// The leader tools that exist ONLY in the leader's scope.
///
/// `tool-leader::TOOL_NAMES` has five entries and `propose_claim` is not one of these four: the
/// `claims` row registers a GLOBAL `propose_claim` for every agent, and the leader's scoped twin
/// SHADOWS it (that shadowing is V6's own bullet, `tool-leader tests/tools.rs`). So "the old agent
/// lost the tools" is a sentence about the four that were only ever the leader's — asserting that
/// `propose_claim` disappears would be asserting that every ordinary lane loses its claim tool.
const LEADER_ONLY: [&str; 4] = [
    "adopt_unsorted",
    "draft_requirement",
    "propose_structure",
    "note_timeline",
];

/// The four above must all be names `tool-leader` really registers: a typo here would make every
/// bullet in this file pass by asserting the absence of a tool that never existed.
fn leader_only_is_a_subset_of_the_rows_own_list() {
    for name in LEADER_ONLY {
        assert!(
            bough_plugin_tool_leader::TOOL_NAMES.contains(&name),
            "`{name}` is not a tool `tool-leader` registers"
        );
    }
}

fn at(secs: i64) -> chrono::DateTime<Utc> {
    Utc.timestamp_opt(1_700_000_000 + secs, 0)
        .single()
        .expect("a valid instant")
}

fn leader(kernel: &Kernel) -> Arc<LeaderHandle> {
    kernel
        .root()
        .peek_live::<Leader>()
        .expect("`leader` is bound")
}

fn tools(kernel: &Kernel) -> Arc<ToolsHandle> {
    kernel
        .root()
        .peek_live::<Tools>()
        .expect("`tools` is bound")
}

fn ledger(kernel: &Kernel) -> Arc<LedgerHandle> {
    kernel
        .root()
        .peek_live::<Ledger>()
        .expect("`ledger` is bound")
}

fn mail(kernel: &Kernel) -> Arc<MailHandle> {
    kernel.root().peek_live::<Mail>().expect("`mail` is bound")
}

/// The leader tools this agent can SEE.
fn visible_leader_tools(kernel: &Kernel, agent: &str) -> Vec<String> {
    let name = AgentName::new(agent);
    tools(kernel)
        .visible(&name)
        .into_iter()
        .map(|t| t.to_string())
        .filter(|t| LEADER_ONLY.contains(&t.as_str()))
        .collect()
}

/// The leader tools this agent's SCHEMA offers the model. Separate from `visible` on purpose: the
/// schema is what reaches the model, and V6's rule is "ABSENT and REFUSED", which is three
/// different questions with three different answers when a filter is wrong.
fn schema_leader_tools(kernel: &Kernel, agent: &str) -> Vec<String> {
    let name = AgentName::new(agent);
    tools(kernel)
        .schemas(&name)
        .into_iter()
        .map(|d| d.name.to_string())
        .filter(|t| LEADER_ONLY.contains(&t.as_str()))
        .collect()
}

/// Whether the persona text is in this agent's assembled projection.
async fn has_persona(kernel: &Kernel, agent: &str) -> bool {
    let projection = kernel
        .root()
        .peek_live::<Projection>()
        .expect("`projection` is bound");
    let assembled = projection
        .0
        .assemble(&AssembleRequest {
            agent: AgentName::new(agent),
            wake: None,
            at: at(100),
            budget: None,
            as_of: None,
        })
        .await
        .expect("the projection assembles");
    assembled.to_text().contains(PERSONA_MARK)
}

/// Route an envelope matching NOBODY and answer which lane the sink delivered it to, if any.
///
/// P5-D4: the unsorted queue is durable and leaderless; the leader row installs a SINK on it. So
/// "the sink moved" is observable as "the new leader's lane is the one that got the item".
async fn unsorted_lands_on(kernel: &Kernel, tag: &str) -> Option<String> {
    let ledger = ledger(kernel);
    let before = |lane: &str| {
        let ledger = Arc::clone(&ledger);
        let lane = lane.to_string();
        async move {
            ledger
                .0
                .steps(&StepQuery {
                    trajs: vec![TrajId::new(format!("lane/{lane}"))],
                    kinds: vec![StepType::new("mail/delivered")],
                    ..Default::default()
                })
                .await
                .expect("the query answers")
                .len()
        }
    };
    let sol_before = before("sol").await;
    let terra_before = before("terra").await;

    let mut nobodys_refs = BTreeSet::new();
    nobodys_refs.insert(Ref::new(format!("nobody:{tag}")));
    mail(kernel)
        .route(Envelope {
            from: Sender::System("leader-swap-test"),
            class: MailClass::Ordinary,
            subject: format!("unsorted {tag}"),
            summary: format!("unsorted {tag}"),
            text: "UNSORTED-SINK-PROBE".to_string(),
            cites: Vec::new(),
            refs: nobodys_refs,
            dedupe_on: None,
            at: at(200),
        })
        .await
        .expect("the envelope routes");
    assert!(kernel.quiesce().await, "the tree quiesces after the route");

    if before("sol").await > sol_before {
        Some("sol".to_string())
    } else if before("terra").await > terra_before {
        Some("terra".to_string())
    } else {
        None
    }
}

/// Boot the shipped `tui` tree with the leader where the bundle puts it.
async fn boot() -> (Arc<Kernel>, TempDir) {
    boot_real("tui", &[fixture("llm-replay.yml")]).await
}

/// Boot, then move the set to `terra` through the launcher's own recompose.
async fn boot_and_move() -> (Arc<Kernel>, TempDir) {
    let (kernel, dir) = boot().await;
    write_patch(&dir, MOVE_TO_TERRA);
    recompose(&kernel, "", &dir)
        .await
        .expect("the moved tree composes");
    assert!(kernel.quiesce().await, "the moved tree quiesces");
    (kernel, dir)
}

#[tokio::test]
async fn the_leader_set_activates_in_one_agents_scope() {
    let _guard = trace::test_lock();
    leader_only_is_a_subset_of_the_rows_own_list();
    let (kernel, _dir) = boot().await;

    // The set is the `leader` row and its nested `tool.leader` child (see the DEVIATION note in
    // `bundles/bough-tui-app.yml`: the composer refuses a plugin-less group row).
    assert_eq!(row(&kernel, "leader").state, FiberState::Active);
    assert_eq!(row(&kernel, "tool.leader").state, FiberState::Active);
    assert_eq!(
        leader(&kernel).target().to_string(),
        "sol",
        "the set names ONE agent, and it is the one the bundle names"
    );

    let mut sol = visible_leader_tools(&kernel, "sol");
    sol.sort();
    let mut want: Vec<String> = LEADER_ONLY.iter().map(|s| s.to_string()).collect();
    want.sort();
    assert_eq!(sol, want, "the leader's lane sees all five leader tools");
    assert!(
        visible_leader_tools(&kernel, "terra").is_empty(),
        "an ordinary lane sees none of them: the set is SCOPED, not global"
    );
    kernel.shutdown().await;
}

#[tokio::test]
async fn a_patch_moves_it_to_another_agent_without_a_recompile() {
    let _guard = trace::test_lock();
    let (kernel, _dir) = boot_and_move().await;
    assert_eq!(
        leader(&kernel).target().to_string(),
        "terra",
        "one config field moved the whole set; the process never restarted"
    );
    assert_eq!(row(&kernel, "leader").state, FiberState::Active);
    assert_eq!(
        row(&kernel, "tool.leader").state,
        FiberState::Active,
        "`tool-leader` injects `leader`, so it reloaded against the new binding"
    );
    kernel.shutdown().await;
}

#[tokio::test]
async fn the_old_agent_loses_the_tools_from_its_schema() {
    let _guard = trace::test_lock();
    let (kernel, _dir) = boot_and_move().await;
    let left = schema_leader_tools(&kernel, "sol");
    assert!(
        left.is_empty(),
        "the old agent's schema still offers leader tools: {left:?}"
    );
    kernel.shutdown().await;
}

#[tokio::test]
async fn the_old_agent_is_refused_by_the_executor() {
    let _guard = trace::test_lock();
    let (kernel, _dir) = boot_and_move().await;
    let sol = AgentName::new("sol");
    for name in LEADER_ONLY {
        let err = tools(&kernel)
            .resolve(&sol, &bough_plugin_tools::ToolName::new(name))
            .err()
            .unwrap_or_else(|| panic!("`sol` still resolves `{name}` after the move"));
        // Indistinguishable from a name nobody ever registered — that is the point (V6).
        assert!(
            matches!(err, ToolsError::NotFound { .. }),
            "`{name}` was refused with the wrong variant: {err}"
        );
    }
    let unregistered = tools(&kernel)
        .resolve(
            &sol,
            &bough_plugin_tools::ToolName::new("no_such_tool_anywhere"),
        )
        .err()
        .expect("an unregistered name is refused");
    assert!(matches!(unregistered, ToolsError::NotFound { .. }));
    kernel.shutdown().await;
}

#[tokio::test]
async fn the_old_agent_loses_the_persona_section() {
    let _guard = trace::test_lock();
    let (kernel, dir) = boot().await;
    assert!(
        has_persona(&kernel, "sol").await,
        "this bullet is vacuous unless the persona was there first"
    );
    write_patch(&dir, MOVE_TO_TERRA);
    recompose(&kernel, "", &dir)
        .await
        .expect("the moved tree composes");
    assert!(
        !has_persona(&kernel, "sol").await,
        "the old agent kept the leader persona after the set moved"
    );
    kernel.shutdown().await;
}

#[tokio::test]
async fn the_new_agent_gains_all_three() {
    let _guard = trace::test_lock();
    let (kernel, _dir) = boot_and_move().await;

    let mut got = visible_leader_tools(&kernel, "terra");
    got.sort();
    let mut want: Vec<String> = LEADER_ONLY.iter().map(|s| s.to_string()).collect();
    want.sort();
    assert_eq!(got, want, "1. the tools");

    let mut schema = schema_leader_tools(&kernel, "terra");
    schema.sort();
    assert_eq!(schema, want, "2. the schema the model actually sees");

    assert!(
        has_persona(&kernel, "terra").await,
        "3. the persona section"
    );
    kernel.shutdown().await;
}

#[tokio::test]
async fn the_unsorted_sink_moved_with_it() {
    let _guard = trace::test_lock();
    let (kernel, dir) = boot().await;
    assert_eq!(
        unsorted_lands_on(&kernel, "before").await.as_deref(),
        Some("sol"),
        "before the move, the sink delivers unsorted mail to the leader's own lane"
    );

    write_patch(&dir, MOVE_TO_TERRA);
    recompose(&kernel, "", &dir)
        .await
        .expect("the moved tree composes");

    assert_eq!(
        unsorted_lands_on(&kernel, "after").await.as_deref(),
        Some("terra"),
        "the sink is the leader row's own effect, so it moved with the set (P5-D4)"
    );
    kernel.shutdown().await;
}

#[tokio::test]
async fn nothing_in_the_tree_is_failed_after_the_move() {
    let _guard = trace::test_lock();
    let (kernel, _dir) = boot_and_move().await;
    let snapshot = kernel.snapshot();
    assert!(
        snapshot.unresolved().is_empty(),
        "the moved tree left rows unresolved: {:#?}",
        snapshot.unresolved()
    );
    let violations = kernel.violations();
    assert!(
        violations.is_empty(),
        "the moved tree reported invariant violations: {violations:?}"
    );
    kernel.shutdown().await;
}

#[tokio::test]
async fn moving_it_back_restores_the_first_agent() {
    let _guard = trace::test_lock();
    let (kernel, dir) = boot_and_move().await;
    assert_eq!(leader(&kernel).target().to_string(), "terra");

    clear_patch(&dir);
    recompose(&kernel, "", &dir)
        .await
        .expect("the restored tree composes");
    assert!(kernel.quiesce().await, "the restored tree quiesces");

    assert_eq!(
        leader(&kernel).target().to_string(),
        "sol",
        "removing the patch put the set back where the bundle has it"
    );
    let mut got = visible_leader_tools(&kernel, "sol");
    got.sort();
    let mut want: Vec<String> = LEADER_ONLY.iter().map(|s| s.to_string()).collect();
    want.sort();
    assert_eq!(got, want);
    assert!(
        visible_leader_tools(&kernel, "terra").is_empty(),
        "…and `terra` is an ordinary lane again"
    );
    assert!(has_persona(&kernel, "sol").await);
    assert!(!has_persona(&kernel, "terra").await);
    kernel.shutdown().await;
}

/// §8: "the reconsolidation pass … leader-attributed once the leader exists in Phase 5". The
/// `leader.config.attribute_reconsolidation` field is what turns it on, and this is the bullet
/// that makes it a live field rather than an inert one — it moves with the set, like everything
/// else the leader row owns.
#[tokio::test]
async fn the_reconsolidation_pass_is_attributed_to_the_leader_and_moves_with_it() {
    let _guard = trace::test_lock();
    let (kernel, dir) = boot().await;
    let recon = kernel
        .root()
        .peek_live::<bough_plugin_reconsolidation::Reconsolidation>()
        .expect("the `reconsolidation` row is in bough-base");
    assert_eq!(
        recon.attribution(),
        bough_plugin_rollups::Attribution::Agent {
            name: AgentName::new("sol")
        },
        "the pass is written by the leader, not by `System`"
    );

    write_patch(&dir, MOVE_TO_TERRA);
    recompose(&kernel, "", &dir)
        .await
        .expect("the moved tree composes");

    let recon = kernel
        .root()
        .peek_live::<bough_plugin_reconsolidation::Reconsolidation>()
        .expect("still bound");
    assert_eq!(
        recon.attribution(),
        bough_plugin_rollups::Attribution::Agent {
            name: AgentName::new("terra")
        },
        "the attribution is the leader row's own effect, so it moved with the set"
    );
    kernel.shutdown().await;
}

/// §4 + §5: a leader question is routed at `MailClass::Wake` on the `class:ask` ref, and a ref
/// reaches nobody unless a ROW is routed on it. The leader row gives itself both, so a graph op's
/// ambiguity question actually arrives — and reactivates the leader when it is asleep.
#[tokio::test]
async fn the_leader_row_is_routed_and_wakes_on_class_ask() {
    let _guard = trace::test_lock();
    let (kernel, _dir) = boot().await;
    let ledger = ledger(&kernel);
    let row = ledger
        .0
        .agent(&AgentName::new("sol"))
        .await
        .expect("a read")
        .expect("the leader's row");
    assert!(
        row.routing_refs
            .contains(&Ref::new(bough_plugin_mail_router::ASK_CLASS_REF)),
        "the leader routes on `class:ask`, or no question ever reaches it: {:?}",
        row.routing_refs
    );
    assert!(
        row.wake_classes
            .contains(bough_plugin_mail_router::ASK_CLASS_REF),
        "and it WAKES on it, or a dormant leader never answers: {:?}",
        row.wake_classes
    );
    kernel.shutdown().await;
}
