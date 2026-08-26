//! The Phase 4 exit gate, SWAP half (§17 Phase 4): the `rollups` row's PROVIDER is replaced by a
//! patch edit while the tree is up. `rollups-summarizer` becomes `rollups-none`, which seals
//! nothing and says so; every consumer of `ctx.rollups` keeps running; the projection degrades to
//! the verbatim tail with no tiers band and no error; removing the patch brings the summarizer —
//! and the band — back. No recompile, no restart, one test process, through the launcher's own
//! recompose (`bough::watch::recompose_once`), the `ledger_swap.rs` precedent.

mod support;

use std::sync::Arc;

use bough_kernel::{FiberState, Kernel, RowSnapshot};
use bough_plugin_hello::trace;
use bough_plugin_ledger::{
    AgentName, Append, Class, Ledger, LedgerHandle, RollupKind, RollupQuery, StepType, TrajId,
    WakeId,
};
use bough_plugin_projection::{AssembleRequest, Projection};
use bough_plugin_rollups::{Attribution, Rollups, SealRequest, Stop};
use chrono::{TimeZone, Utc};
use support::{boot_real, clear_patch, fixture, recompose, row, write_patch, TempDir};

/// The whole swap: the `rollups` row changes PLUGIN. A patch layer replaces an entry's `config`
/// map wholesale, and the stub's config is empty — which is the point of `NoneConfig` being an
/// empty struct.
const STUB: &str = "\
entries:
  rollups:
    plugin: rollups-none
    config: {}
";

const AGENT: &str = "sol";
const TRAJ: &str = "lane/sol";

fn at(secs: i64) -> chrono::DateTime<Utc> {
    Utc.timestamp_opt(1_700_000_000 + secs, 0)
        .single()
        .expect("a valid instant")
}

fn ledger(kernel: &Kernel) -> Arc<LedgerHandle> {
    kernel
        .root()
        .peek_live::<Ledger>()
        .expect("`ledger` is bound")
}

fn rollups(kernel: &Kernel) -> Arc<bough_plugin_rollups::RollupsHandle> {
    kernel
        .root()
        .peek_live::<Rollups>()
        .expect("`rollups` is bound")
}

fn seal_request() -> SealRequest {
    SealRequest {
        agent: AgentName::new(AGENT),
        traj: TrajId::new(TRAJ),
        at: at(10_000),
        upto: None,
        max_calls: None,
        attribution: Attribution::System,
    }
}

/// A day's worth of raw trajectory, appended directly: the swap gate is about the PROVIDER, not
/// about how the steps got there, and a scripted wake would put a model in the middle of it.
///
/// One minute between steps, so the episode cut (`gap_minutes: 45`) never fires inside the seed
/// and the windowing is the `max_window_steps` arithmetic alone.
async fn seed(ledger: &LedgerHandle, n: usize) {
    for i in 0..n {
        ledger
            .0
            .append(Append {
                traj: TrajId::new(TRAJ),
                wake: WakeId::new("wake:seed"),
                kind: StepType::new("thought/text"),
                class: Class::Thought,
                body: serde_json::json!({ "text": format!("seeded thought {i}"), "step_index": i }),
                cites: vec![],
                at: at(i as i64 * 60),
                id: None,
            })
            .await
            .expect("the seed appends");
    }
}

/// Every `tier` rollup on the trajectory, superseded ones included.
async fn tier_count(ledger: &LedgerHandle) -> usize {
    ledger
        .0
        .rollups(&RollupQuery {
            trajs: vec![TrajId::new(TRAJ)],
            kind: Some(RollupKind::Tier),
            include_superseded: true,
            ..Default::default()
        })
        .await
        .expect("the query answers")
        .len()
}

/// `true` iff the assembled projection carries at least one tiers band.
async fn has_tiers_band(kernel: &Kernel) -> bool {
    let projection = kernel
        .root()
        .peek_live::<Projection>()
        .expect("`projection` is bound");
    let assembled = projection
        .0
        .assemble(&AssembleRequest {
            agent: AgentName::new(AGENT),
            wake: None,
            at: at(10_000),
            budget: None,
            as_of: None,
        })
        .await
        .expect("the projection assembles");
    assembled
        .sections
        .iter()
        .any(|s| s.id.as_str().starts_with("tier-"))
}

/// A replay transcript long enough for a whole seal pass, written next to this test's home: the
/// shipped `llm-replay.yml` fixture has four rounds, and a pass over a seeded day makes more calls
/// than that.
fn replay_patch(dir: &TempDir) -> std::path::PathBuf {
    let mut rounds = String::new();
    for i in 0..40 {
        rounds.push_str(&format!(
            "        - chunks:\n            - {{ type: text, text: \"recap {i}: the seeded thoughts, summarised\" }}\n            - {{ type: end, stop: end_turn }}\n"
        ));
    }
    let path = dir.path().join("replay-many.yml");
    std::fs::write(
        &path,
        format!(
            "entries:\n  llm.anthropic:\n    plugin: llm-replay\n    config:\n      strict: true\n      models: \"*\"\n      rounds:\n{rounds}"
        ),
    )
    .expect("the replay patch is writable");
    path
}

/// Boot the shipped `headless` tree with no live model, create the resident whose trajectory the
/// seeded day belongs to, and seed it.
///
/// The agent row is CREATED here rather than assumed: `headless` carries no `residents` row, and a
/// projection assembled for an agent with no trajectory renders an identity band and nothing else
/// — which would make the tail assertion below vacuous.
async fn boot_seeded() -> (Arc<Kernel>, TempDir, bough_plugin_agents::AgentDisposer) {
    let (kernel, dir) = boot_real("headless", &[fixture("llm-replay.yml")]).await;
    let agents = kernel
        .root()
        .peek_live::<bough_plugin_agents::Agents>()
        .expect("`agents` is bound");
    let (_agent, disposer) = agents
        .create(bough_plugin_agents::CreateAgent {
            name: AgentName::new(AGENT),
            traj: TrajId::new(TRAJ),
            kind: bough_plugin_agents::AgentKind::Resident,
            scope: None,
            setup: None,
            seed: Vec::new(),
            at: at(0),
        })
        .await
        .expect("the creation transaction commits");
    seed(&ledger(&kernel), 60).await;
    (kernel, dir, disposer)
}

#[tokio::test]
async fn the_stub_provider_seals_nothing() {
    let _guard = trace::test_lock();
    let (kernel, dir, _disposer) = boot_seeded().await;
    assert_eq!(
        rollups(&kernel).0.provider(),
        "rollups-summarizer",
        "the shipped row binds the summarizer"
    );

    write_patch(&dir, STUB);
    recompose(&kernel, "", &dir)
        .await
        .expect("the swap composes");

    let r = row(&kernel, "rollups");
    assert_eq!(r.plugin.as_deref(), Some("rollups-none"));
    assert_eq!(r.state, FiberState::Active);

    let handle = rollups(&kernel);
    assert_eq!(handle.0.provider(), "rollups-none");
    assert_eq!(
        handle.0.prompt_ver(),
        "",
        "a provider that seals nothing stamps nothing"
    );

    let before = tier_count(&ledger(&kernel)).await;
    let report = handle
        .0
        .seal(&seal_request())
        .await
        .expect("the stub's pass answers");
    assert_eq!(report.stop, Stop::NothingToDo);
    assert!(report.sealed.is_empty());
    assert_eq!(report.calls, 0, "the stub makes no model call");
    assert_eq!(
        tier_count(&ledger(&kernel)).await,
        before,
        "the stub sealed a block"
    );

    clear_patch(&dir);
    kernel.shutdown().await;
}

#[tokio::test]
async fn the_projection_degrades_to_the_verbatim_tail_with_the_stub() {
    let _guard = trace::test_lock();
    let (kernel, dir, _disposer) = boot_seeded().await;

    write_patch(&dir, STUB);
    recompose(&kernel, "", &dir)
        .await
        .expect("the swap composes");
    rollups(&kernel)
        .0
        .seal(&seal_request())
        .await
        .expect("the stub's pass answers");

    let projection = kernel
        .root()
        .peek_live::<Projection>()
        .expect("`projection` is bound");
    let assembled = projection
        .0
        .assemble(&AssembleRequest {
            agent: AgentName::new(AGENT),
            wake: None,
            at: at(10_000),
            budget: None,
            as_of: None,
        })
        .await
        .expect("the projection assembles with no rollups at all");

    assert!(
        !assembled
            .sections
            .iter()
            .any(|s| s.id.as_str().starts_with("tier-")),
        "no tiers band can exist when nothing was ever sealed: {:?}",
        assembled
            .sections
            .iter()
            .map(|s| s.id.to_string())
            .collect::<Vec<_>>()
    );
    // …and the verbatim tail is what carries the day instead. The seeded steps are IN it.
    let text = assembled.to_text();
    assert!(
        text.contains("seeded thought 59"),
        "the newest raw step must ride the tail verbatim:\n{text}"
    );

    clear_patch(&dir);
    kernel.shutdown().await;
}

#[tokio::test]
async fn reconsolidation_and_drift_watch_stay_active_with_the_stub() {
    let _guard = trace::test_lock();
    let (kernel, dir, _disposer) = boot_seeded().await;

    write_patch(&dir, STUB);
    recompose(&kernel, "", &dir)
        .await
        .expect("the swap composes");

    for id in ["reconsolidation", "drift.watch"] {
        let r = row(&kernel, id);
        assert!(
            matches!(r.state, FiberState::Active | FiberState::Pending),
            "row `{id}` settled {:?} under the stub",
            r.state
        );
        assert_eq!(
            r.state,
            FiberState::Active,
            "row `{id}` injects `rollups`, which the stub still provides"
        );
    }

    clear_patch(&dir);
    kernel.shutdown().await;
}

#[tokio::test]
async fn nothing_in_the_tree_is_failed_after_the_swap() {
    let _guard = trace::test_lock();
    let (kernel, dir, _disposer) = boot_seeded().await;

    write_patch(&dir, STUB);
    recompose(&kernel, "", &dir)
        .await
        .expect("the swap composes");

    fn failed(rows: &[RowSnapshot], out: &mut Vec<String>) {
        for r in rows {
            if matches!(r.state, FiberState::Failed) {
                out.push(r.id.to_string());
            }
            failed(&r.children, out);
        }
    }
    let mut bad = Vec::new();
    failed(&kernel.snapshot().rows, &mut bad);
    assert!(bad.is_empty(), "rows FAILED after the swap: {bad:?}");
    assert!(
        kernel.snapshot().unresolved().is_empty(),
        "unresolved after the swap: {:#?}",
        kernel.snapshot().unresolved()
    );

    clear_patch(&dir);
    kernel.shutdown().await;
}

#[tokio::test]
async fn swapping_back_restores_the_tiers_band() {
    let _guard = trace::test_lock();
    let (kernel, dir) = boot_real("headless", &[fixture("llm-replay.yml")]).await;
    // The long transcript, layered live: a real seal pass makes more calls than the shipped
    // fixture has rounds.
    let long = replay_patch(&dir);
    std::fs::write(
        dir.patch_path(),
        format!(
            "{}\n{}",
            std::fs::read_to_string(&long).unwrap(),
            STUB.trim_start_matches("entries:\n")
        ),
    )
    .expect("the patch is writable");
    seed(&ledger(&kernel), 60).await;
    recompose(&kernel, "", &dir)
        .await
        .expect("the swap composes");
    assert_eq!(rollups(&kernel).0.provider(), "rollups-none");

    rollups(&kernel)
        .0
        .seal(&seal_request())
        .await
        .expect("the stub's pass answers");
    assert_eq!(tier_count(&ledger(&kernel)).await, 0);
    assert!(!has_tiers_band(&kernel).await);

    // Swap BACK: the same layer, minus the stub entry.
    std::fs::write(dir.patch_path(), std::fs::read_to_string(&long).unwrap())
        .expect("the patch is writable");
    recompose(&kernel, "", &dir)
        .await
        .expect("swapping back composes");
    assert_eq!(rollups(&kernel).0.provider(), "rollups-summarizer");

    let report = rollups(&kernel)
        .0
        .seal(&seal_request())
        .await
        .expect("the summarizer's pass answers");
    assert!(
        !report.sealed.is_empty(),
        "the restored summarizer sealed nothing: {report:?}"
    );
    assert!(tier_count(&ledger(&kernel)).await > 0);
    assert!(
        has_tiers_band(&kernel).await,
        "the tiers band must be back once real blocks exist"
    );

    clear_patch(&dir);
    kernel.shutdown().await;
}
