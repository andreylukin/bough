//! The four Phase 1 runtime invariants, seen through the LAUNCHER: a scripted session under the
//! `dev` profile reports nothing, and each planted violation is reported by name. The runner
//! REPORTS and never acts — the violating row keeps running (§0.2).
//!
//! Every Phase 1 invariant is `Cadence::OnQuiesce` (P1-D14), so a planted violation is asserted
//! after an explicit `quiesce()`, never after a sleep.

mod support;

use bough_kernel::FiberState;
use bough_plugin_hello::trace;
use bough_plugin_ledger::invariant::Obs;
use bough_plugin_ledger::{Seq, StepType, TrajId, WakeId};
use bough_plugin_projection::{AssembleRequest, Projection};
use support::{boot_with_profile, recompose, row, write_patch};

/// The invariant runner is dispatched by the kernel's LOAD/UPDATE path, not by `quiesce()` alone
/// (Phase 0 left `Cadence::Interval`/`OnEvent` undispatched, P1-D14). So a violation planted after
/// boot is collected by making the tree work once: an immaterial patch edit, recomposed through
/// the launcher's own live path. Nothing about the planted stream changes.
const NUDGE: &str = "\
entries:
  probe:
    config:
      traj: t1
      agent: a1
      steps: 4
";

const P1: &str = "\
- id: ledger
  plugin: ledger-sqlite
  config:
    path: !!expr 'bough_path(\"ledger.db\")'
    busy_timeout_ms: 5000
- id: projection
  plugin: projection-assembler
  config:
    budget_tokens: 160000
    headroom: 0.6
    tail_steps: 60
    tail_floor_steps: 10
    mail_newest_n: 5
    max_tiers: 3
    file_view_dir: !!expr 'bough_path(\"views\")'
- id: probe
  plugin: projection-probe
  config:
    traj: t1
    agent: a1
    steps: 3
";

/// The same tree with the projection violation planted in the fixture row.
const P1_MISSING_CITE: &str = "\
- id: ledger
  plugin: ledger-sqlite
  config:
    path: !!expr 'bough_path(\"ledger.db\")'
    busy_timeout_ms: 5000
- id: projection
  plugin: projection-assembler
  config:
    budget_tokens: 160000
    headroom: 0.6
    tail_steps: 60
    tail_floor_steps: 10
    mail_newest_n: 5
    max_tiers: 3
    file_view_dir: !!expr 'bough_path(\"views\")'
- id: probe
  plugin: projection-probe
  config:
    traj: t1
    agent: a1
    steps: 3
    plant_missing_cite: true
";

/// Find one violation by invariant name, or say what was reported instead.
fn violation<'a>(
    vs: &'a [bough_kernel::InvariantViolation],
    name: &str,
) -> &'a bough_kernel::InvariantViolation {
    vs.iter()
        .find(|v| v.invariant == name)
        .unwrap_or_else(|| panic!("`{name}` was not reported; reported: {vs:?}"))
}

#[tokio::test]
async fn a_scripted_session_reports_no_ledger_violation() {
    let _guard = trace::test_lock();
    bough_plugin_projection_probe::clear();
    // The two invariant records are process-global; a leftover from a sibling test would be
    // reported against THIS test's kernel. The trace lock serialises us; this starts us clean.
    bough_plugin_ledger::invariant::clear();
    bough_plugin_projection::invariant::clear();
    assert!(
        support::profile_runs_invariants("dev"),
        "profiles/dev.yml must turn the runner on, or this test proves nothing"
    );
    let (kernel, _dir) = boot_with_profile(P1, "dev").await;

    // Precondition: the probe really did append, so a green report is not the report of an empty
    // stream.
    let observed = bough_plugin_ledger::invariant::observed();
    assert!(
        observed >= 4,
        "the probe must have appended a scripted trajectory: {observed} observations"
    );
    assert_eq!(bough_plugin_ledger::invariant::seq_violation(), None);
    assert_eq!(bough_plugin_ledger::invariant::enclosure_violation(), None);

    assert!(
        kernel.violations().is_empty(),
        "a clean scripted session reported: {:?}",
        kernel.violations()
    );
    assert_eq!(row(&kernel, "ledger").state, FiberState::Active);

    kernel.shutdown().await;
}

#[tokio::test]
async fn a_planted_seq_gap_is_reported() {
    let _guard = trace::test_lock();
    bough_plugin_projection_probe::clear();
    // The two invariant records are process-global; a leftover from a sibling test would be
    // reported against THIS test's kernel. The trace lock serialises us; this starts us clean.
    bough_plugin_ledger::invariant::clear();
    bough_plugin_projection::invariant::clear();
    let (kernel, dir) = boot_with_profile(P1, "dev").await;
    let fiber = row(&kernel, "ledger").uid.expect("uid");

    // A real append cannot produce a gap — the single writer allocates seq inside the commit — so
    // the violation is planted on the OBSERVED stream, which is exactly what the invariant is a
    // statement about.
    let last = bough_plugin_ledger::invariant::last_seq(&TrajId::new("t1"))
        .expect("the probe appended to t1")
        .0;
    bough_plugin_ledger::invariant::record(Obs {
        fiber,
        traj: TrajId::new("t1"),
        seq: Seq(last + 2),
        wake: WakeId::new("t1-w1"),
        kind: StepType::new("probe/note"),
    });
    assert!(
        bough_plugin_ledger::invariant::seq_violation().is_some(),
        "the planted stream must itself violate the invariant"
    );

    write_patch(&dir, NUDGE);
    recompose(&kernel, P1, &dir)
        .await
        .expect("the nudge composes");
    let vs = kernel.violations();
    let v = violation(&vs, "seq_strictly_grows_per_trajectory");
    assert_eq!(v.plugin, "ledger-sqlite");
    assert_eq!(v.entry.as_str(), "ledger");

    // A report, never an unload.
    assert_eq!(row(&kernel, "ledger").state, FiberState::Active);
    kernel.shutdown().await;
}

#[tokio::test]
async fn a_planted_unenclosed_step_pair_is_reported() {
    let _guard = trace::test_lock();
    bough_plugin_projection_probe::clear();
    // The two invariant records are process-global; a leftover from a sibling test would be
    // reported against THIS test's kernel. The trace lock serialises us; this starts us clean.
    bough_plugin_ledger::invariant::clear();
    bough_plugin_projection::invariant::clear();
    let (kernel, dir) = boot_with_profile(P1, "dev").await;
    let fiber = row(&kernel, "ledger").uid.expect("uid");

    // A `step/start`..`step/end` pair under a wake that was never opened.
    let traj = TrajId::new("t1");
    let wake = WakeId::new("t1-never-opened");
    let last = bough_plugin_ledger::invariant::last_seq(&traj)
        .expect("the probe appended to t1")
        .0;
    for (n, kind) in ["step/start", "step/end"].into_iter().enumerate() {
        bough_plugin_ledger::invariant::record(Obs {
            fiber,
            traj: traj.clone(),
            seq: Seq(last + 1 + n as u64),
            wake: wake.clone(),
            kind: StepType::new(kind),
        });
    }
    assert!(
        bough_plugin_ledger::invariant::enclosure_violation().is_some(),
        "the planted stream must itself violate the invariant"
    );

    write_patch(&dir, NUDGE);
    recompose(&kernel, P1, &dir)
        .await
        .expect("the nudge composes");
    let vs = kernel.violations();
    let v = violation(&vs, "wake_step_enclosure");
    assert_eq!(v.plugin, "ledger-sqlite");
    assert_eq!(v.entry.as_str(), "ledger");

    assert_eq!(row(&kernel, "ledger").state, FiberState::Active);
    kernel.shutdown().await;
}

#[tokio::test]
async fn a_projection_citing_a_missing_step_is_reported() {
    let _guard = trace::test_lock();
    bough_plugin_projection_probe::clear();
    // The two invariant records are process-global; a leftover from a sibling test would be
    // reported against THIS test's kernel. The trace lock serialises us; this starts us clean.
    bough_plugin_ledger::invariant::clear();
    bough_plugin_projection::invariant::clear();
    let (kernel, dir) = boot_with_profile(P1_MISSING_CITE, "dev").await;

    // The violation only exists once a projection has actually been assembled: the invariant is a
    // statement about what the model was shown.
    let handle = kernel
        .root()
        .peek_live::<Projection>()
        .expect("projection is bound");
    handle
        .0
        .assemble(&AssembleRequest {
            as_of: None,
            agent: bough_plugin_ledger::AgentName::new("a1"),
            wake: None,
            at: chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
                .unwrap()
                .into(),
            budget: None,
        })
        .await
        .expect("the projection assembles");
    assert!(
        !bough_plugin_projection::invariant::seen().is_empty(),
        "the assembler must have recorded its sections' cites"
    );

    write_patch(&dir, NUDGE);
    recompose(&kernel, P1_MISSING_CITE, &dir)
        .await
        .expect("the nudge composes");
    let vs = kernel.violations();
    let v = violation(&vs, "model_visible_is_ledgered");
    assert_eq!(v.entry.as_str(), "projection");
    // The violation names the PROVIDER that owns the spec, not the Definition crate the rule
    // lives in: the check reads `ctx.plugin_name()`, exactly as the ledger's four specs do.
    assert_eq!(v.plugin, "projection-assembler");
    assert!(
        v.detail.contains("step-that-was-never-appended"),
        "the report must name the missing id: {}",
        v.detail
    );

    assert_eq!(row(&kernel, "projection").state, FiberState::Active);
    kernel.shutdown().await;
}

/// `seal_once` is a statement about REAL supersessions, so this test drives one through the live
/// provider first — which is what proves `supersede_rollup` records the transition at all — and
/// only then plants a second transition for the same rollup, which no API can produce.
#[tokio::test]
async fn a_planted_second_supersession_is_reported() {
    let _guard = trace::test_lock();
    bough_plugin_projection_probe::clear();
    bough_plugin_ledger::invariant::clear();
    bough_plugin_projection::invariant::clear();
    let (kernel, dir) = boot_with_profile(P1, "dev").await;
    let fiber = row(&kernel, "ledger").uid.expect("uid");
    let ledger = kernel
        .root()
        .peek_live::<bough_plugin_ledger::Ledger>()
        .expect("ledger is bound")
        .as_ref()
        .clone();

    let at = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
        .unwrap()
        .into();
    let mut sealed = Vec::new();
    for id in ["r1", "r2", "r3"] {
        sealed.push(
            ledger
                .0
                .seal_rollup(bough_plugin_ledger::NewRollup {
                    id: Some(bough_plugin_ledger::RollupId::new(id)),
                    traj: TrajId::new("t1"),
                    kind: bough_plugin_ledger::RollupKind::Tier,
                    tier: 1,
                    from_seq: bough_plugin_ledger::Seq(1),
                    to_seq: bough_plugin_ledger::Seq(2),
                    src_trajs: vec![TrajId::new("t1")],
                    body: serde_json::json!({ "text": id }),
                    notable_refs: Default::default(),
                    prompt_ver: "v1".into(),
                    sealed_at: at,
                })
                .await
                .unwrap_or_else(|e| panic!("seal {id}: {e}"))
                .id,
        );
    }
    ledger
        .0
        .supersede_rollup(&sealed[0], &sealed[1])
        .await
        .expect("the first supersession is the permitted one");
    // THE wiring this invariant depends on: the provider recorded the transition.
    assert_eq!(
        bough_plugin_ledger::invariant::supersessions(),
        vec![(sealed[0].clone(), sealed[1].clone())],
        "supersede_rollup must record the transition, or `seal_once` can never fire"
    );
    // A second one is refused by the API and by the trigger, so it is PLANTED on the record.
    assert!(ledger
        .0
        .supersede_rollup(&sealed[0], &sealed[2])
        .await
        .is_err());
    bough_plugin_ledger::invariant::record_supersession(fiber, &sealed[0], &sealed[2]);

    write_patch(&dir, NUDGE);
    recompose(&kernel, P1, &dir)
        .await
        .expect("the nudge composes");
    let vs = kernel.violations();
    let v = violation(&vs, "seal_once");
    assert_eq!(v.plugin, "ledger-sqlite");
    assert_eq!(v.entry.as_str(), "ledger");
    assert!(v.detail.contains("set once"), "unhelpful: {}", v.detail);

    assert_eq!(row(&kernel, "ledger").state, FiberState::Active);
    bough_plugin_ledger::invariant::clear();
    kernel.shutdown().await;
}
