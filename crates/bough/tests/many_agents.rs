//! Phase 5's population, seen through the LAUNCHER. Everything the unit suites prove on a fixture
//! is proved here once more on a tree that BOOTED: three lanes from one `residents.bootstrap`,
//! one envelope reaching two of them and not the third, a lane that is asleep costing exactly
//! nothing over a whole boot, and every invariant the phase's rows declare running at the only
//! cadence the kernel dispatches.
//!
//! The `memory_invariants.rs` precedent: the point of these is that the ROWS Andrey ships are the
//! ones under test, so they boot `profiles/` + `bundles/` off disk through `boot_real`.

mod support;

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use bough_kernel::{Catalog, FiberState, Kernel};
use bough_plugin_agents::{Agents, MailClass, Sender, Target};
use bough_plugin_dormancy::{Dormancy, DormancyHandle, SleepRequest};
use bough_plugin_hello::trace;
use bough_plugin_ledger::{AgentName, Ledger, LedgerHandle, Ref, StepQuery, StepType, TrajId};
use bough_plugin_mail_router::{Envelope, Mail, MailHandle};
use bough_plugin_rollups::Attribution;
use chrono::{TimeZone, Utc};
use support::{boot_real, fixture, row};

/// The three lanes this file boots. `luna` is the one that goes to sleep.
const LANES: [&str; 3] = ["sol", "terra", "luna"];

fn at(secs: i64) -> chrono::DateTime<Utc> {
    Utc.timestamp_opt(1_700_000_000 + secs, 0)
        .single()
        .expect("a valid instant")
}

/// A `--patch` layer widening `residents.bootstrap` to the three lanes.
///
/// Written to a per-process temp file rather than checked in: it is one config field, and the
/// checked-in fixtures are for trees a SCRIPT boots.
fn three_lane_patch() -> PathBuf {
    let path = std::env::temp_dir().join(format!("bough-many-agents-{}.yml", std::process::id()));
    std::fs::write(
        &path,
        "\
entries:
  residents:
    config:
      bootstrap: [sol, terra, luna]
      traj_prefix: \"lane/\"
      resume_all: true
      catch_up: true
",
    )
    .expect("the patch file is writable");
    path
}

async fn boot_three() -> (Arc<Kernel>, support::TempDir, PathBuf) {
    let patch = three_lane_patch();
    let (kernel, dir) = boot_real("tui", &[fixture("llm-replay.yml"), patch.clone()]).await;
    (kernel, dir, patch)
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

fn dormancy(kernel: &Kernel) -> Arc<DormancyHandle> {
    kernel
        .root()
        .peek_live::<Dormancy>()
        .expect("`dormancy` is bound")
}

fn agents(kernel: &Kernel) -> Arc<bough_plugin_agents::AgentsHandle> {
    kernel
        .root()
        .peek_live::<Agents>()
        .expect("`agents` is bound")
}

fn refs(one: &str) -> BTreeSet<Ref> {
    let mut s = BTreeSet::new();
    s.insert(Ref::new(one));
    s
}

/// How many steps of a kind stand on a trajectory.
async fn steps_of(ledger: &LedgerHandle, traj: &str, kind: &str) -> usize {
    ledger
        .0
        .steps(&StepQuery {
            trajs: vec![TrajId::new(traj)],
            kinds: vec![StepType::new(kind)],
            ..Default::default()
        })
        .await
        .expect("the query answers")
        .len()
}

#[tokio::test]
async fn three_lanes_boot_and_appear_in_the_registry() {
    let _guard = trace::test_lock();
    let (kernel, _dir, _patch) = boot_three().await;

    assert_eq!(
        row(&kernel, "residents").state,
        FiberState::Active,
        "the roster row must activate"
    );

    let rows = ledger(&kernel).0.agents().await.expect("the rows read");
    let mut names: Vec<String> = rows.iter().map(|r| r.name.to_string()).collect();
    names.sort();
    assert_eq!(
        names,
        vec!["luna".to_string(), "sol".to_string(), "terra".to_string()],
        "one bootstrap list, three `agents` rows"
    );
    for r in &rows {
        assert_eq!(
            r.traj.to_string(),
            format!("lane/{}", r.name),
            "each lane took `traj_prefix` + its own name"
        );
    }

    let live = agents(&kernel);
    for lane in LANES {
        assert!(
            live.by_name(&AgentName::new(lane)).is_some(),
            "`{lane}` is in the live registry, not only in the ledger"
        );
    }
    kernel.shutdown().await;
}

#[tokio::test]
async fn mail_fans_out_across_lanes_in_a_booted_tree() {
    let _guard = trace::test_lock();
    let (kernel, _dir, _patch) = boot_three().await;
    let mail = mail(&kernel);
    let ledger = ledger(&kernel);

    // Two lanes claim the SAME ref; the third claims nothing of the sort.
    for lane in ["sol", "terra"] {
        mail.link_ref(&AgentName::new(lane), refs("repo:bough"), at(10))
            .await
            .expect("the link lands");
    }
    mail.link_ref(&AgentName::new("luna"), refs("repo:other"), at(10))
        .await
        .expect("the link lands");

    let before: Vec<usize> = {
        let mut v = Vec::new();
        for lane in LANES {
            v.push(steps_of(&ledger, &format!("lane/{lane}"), "mail/delivered").await);
        }
        v
    };

    let report = mail
        .route(Envelope {
            from: Sender::System("many-agents-test"),
            class: MailClass::Ordinary,
            subject: "a push landed".to_string(),
            summary: "a push landed".to_string(),
            text: "FAN-OUT-PROBE".to_string(),
            cites: Vec::new(),
            refs: refs("repo:bough"),
            at: at(20),
        })
        .await
        .expect("the envelope routes");

    let mut matched: Vec<String> = report.matched.iter().map(|n| n.to_string()).collect();
    matched.sort();
    assert_eq!(
        matched,
        vec!["sol".to_string(), "terra".to_string()],
        "EVERY matching agent, not the best one (§3)"
    );
    assert!(
        report.unsorted.is_none(),
        "a matched envelope never touches the unsorted queue"
    );
    assert_eq!(
        report.delivered.len(),
        2,
        "one delivery per recipient: {:?}",
        report.delivered
    );

    // …and each recipient's delivery is its OWN step (P3-D15). The SEQ is not the thing to
    // compare: seqs are per-trajectory, so two recipients legitimately share one. The step id is
    // what says the two deliveries are two facts.
    let mut steps = BTreeSet::new();
    for (name, receipt) in &report.delivered {
        assert!(
            steps.insert(receipt.step.clone()),
            "`{name}` shares its `mail/delivered` step with another recipient"
        );
    }

    for (i, lane) in LANES.iter().enumerate() {
        let after = steps_of(&ledger, &format!("lane/{lane}"), "mail/delivered").await;
        let want = if *lane == "luna" { 0 } else { 1 };
        assert_eq!(
            after - before[i],
            want,
            "`{lane}` should have gained {want} `mail/delivered` step(s)"
        );
    }
    kernel.shutdown().await;
}

#[tokio::test]
async fn a_dormant_lane_runs_no_wake_over_a_whole_boot() {
    let _guard = trace::test_lock();
    let (kernel, _dir, _patch) = boot_three().await;
    let mail = mail(&kernel);
    let ledger = ledger(&kernel);
    let luna = AgentName::new("luna");

    mail.link_ref(&luna, refs("repo:asleep"), at(10))
        .await
        .expect("the link lands");
    dormancy(&kernel)
        .sleep(SleepRequest {
            agent: luna.clone(),
            reason: "the test puts it to sleep".to_string(),
            by: Attribution::Andrey,
            cites: Vec::new(),
            at: at(15),
        })
        .await
        .expect("the lane sleeps");
    assert!(dormancy(&kernel).is_dormant(&luna));

    let wakes_before = steps_of(&ledger, "lane/luna", "wake/start").await;

    // Ordinary mail, three times. Delivery HAPPENS — §5 queues it — and no wake does.
    for i in 0..3 {
        mail.route(Envelope {
            from: Sender::System("many-agents-test"),
            class: MailClass::Ordinary,
            subject: format!("queued {i}"),
            summary: format!("queued {i}"),
            text: "DORMANT-QUEUE-PROBE".to_string(),
            cites: Vec::new(),
            refs: refs("repo:asleep"),
            at: at(20 + i),
        })
        .await
        .expect("the envelope routes");
    }
    assert!(kernel.quiesce().await, "the tree quiesces");

    assert_eq!(
        steps_of(&ledger, "lane/luna", "mail/delivered").await,
        3,
        "ordinary mail is DELIVERED to a dormant lane and simply queues (§5)"
    );
    assert_eq!(
        steps_of(&ledger, "lane/luna", "wake/start").await,
        wakes_before,
        "…and not one wake was opened for it over the whole boot"
    );

    let agent = agents(&kernel).by_name(&luna).expect("luna is still live");
    assert!(
        !agent.inbox().pending(Target::NextWake).is_empty(),
        "the backlog is still there, unconsumed, waiting for the reactivation drain"
    );
    kernel.shutdown().await;
}

/// The `memory_invariants.rs` gate, one phase on: each Phase 5 row declares a runtime invariant,
/// every spec is `OnQuiesce` (P1-D14: the only cadence the kernel dispatches), and a clean boot of
/// the shipped tree reports none of them violated.
#[tokio::test]
async fn every_phase_five_invariant_runs_at_quiesce() {
    let _guard = trace::test_lock();

    // The rows §17 Phase 5 adds, and the plugin each is bound to. `lane-scope` and `tool-leader`
    // are deliberately absent: both carry a written `No runtime invariant:` reason (AGENTS.md
    // allows exactly that), so requiring a spec of them would be requiring a lie.
    const WITH_INVARIANTS: [(&str, &str); 6] = [
        ("mail", "mail-router"),
        ("dormancy", "dormancy"),
        ("graph", "graph-ops"),
        ("claims", "claims"),
        ("worker.fork", "worker-fork"),
        ("leader", "leader"),
    ];

    let catalog = Catalog::from_inventory().expect("the linked catalog has no duplicate names");
    let mut expected: std::collections::BTreeSet<&'static str> = Default::default();
    for (_, plugin) in WITH_INVARIANTS {
        let p = catalog
            .get(plugin)
            .unwrap_or_else(|| panic!("`{plugin}` is not in the linked catalog"));
        let specs = p.invariants();
        assert!(
            !specs.is_empty(),
            "`{plugin}` declares no runtime invariant (AGENTS.md requires one or a written reason)"
        );
        for spec in specs {
            assert_eq!(
                spec.cadence,
                bough_kernel::Cadence::OnQuiesce,
                "`{plugin}`'s `{}` would never be dispatched",
                spec.name
            );
            expected.insert(spec.name);
        }
    }

    let (kernel, _dir, _patch) = boot_three().await;
    for (id, plugin) in WITH_INVARIANTS {
        let r = row(&kernel, id);
        assert_eq!(
            r.state,
            FiberState::Active,
            "row `{id}` must be ACTIVE (§0.2: an enabled row that never activates is a boot failure)"
        );
        assert_eq!(r.plugin.as_deref(), Some(plugin));
    }
    assert!(kernel.quiesce().await, "the tree quiesces");

    let phase_five: Vec<_> = kernel
        .violations()
        .into_iter()
        .filter(|v| expected.contains(v.invariant))
        .collect();
    assert!(
        phase_five.is_empty(),
        "a clean boot reported Phase 5 violations: {phase_five:?}"
    );
    kernel.shutdown().await;
}
