//! V1 (§17 Phase 2): a scripted multi-wake conversation appends every durable step, in §5's order.
//!
//! It runs against `agent-loop-scripted` and `llm-replay`, so it is hermetic; what it asserts is
//! the LEDGER PROTOCOL, not the transcript. A replacement loop is held to exactly this sequence,
//! which is why the assertion is a kind sequence per wake and not a golden file.

mod support;

use bough_kernel::EntryId;
use bough_plugin_agents::{AgentKind, Agents, CreateAgent, MailClass, Message, MessageId, Sender};
use bough_plugin_hello::trace;
use bough_plugin_ledger::query::{Order, StepQuery};
use bough_plugin_ledger::{AgentName, Ledger, Step, TrajId, WakeId};
use support::{boot_real, fixture, row_ctx};

/// The kinds of one wake, in seq order.
fn kinds_of(steps: &[Step], wake: &WakeId) -> Vec<String> {
    steps
        .iter()
        .filter(|s| &s.wake == wake)
        .map(|s| s.kind.to_string())
        .collect()
}

/// The wakes that actually OPENED, in order.
///
/// A splice appended between wakes carries a placeholder wake id (nothing was awake to own it),
/// so "how many wakes ran" is "how many `wake/start` steps there are" — not how many distinct
/// wake ids appear.
fn wakes_of(steps: &[Step]) -> Vec<WakeId> {
    steps
        .iter()
        .filter(|s| s.kind.as_str() == "wake/start")
        .map(|s| s.wake.clone())
        .collect()
}

fn andrey(text: &str) -> Message {
    Message {
        id: MessageId::new(uuid_like(text)),
        from: Sender::Andrey,
        class: MailClass::Wake,
        text: text.to_string(),
        subject: text.to_string(),
        cites: Vec::new(),
        refs: Default::default(),
        mail_seq: None,
        at: chrono::Utc::now(),
    }
}

/// A stable id per message text: the tests assert on ORDER, so a deterministic id keeps a failure
/// readable without dragging a uuid dependency into the test target.
fn uuid_like(text: &str) -> String {
    format!("msg-{}", text.replace(' ', "-"))
}

/// The two loop Providers this gate is parameterised over (P1-D10's lesson: one named case per
/// driver, never a single red test that could mean either).
fn patches(driver: &str) -> Vec<std::path::PathBuf> {
    match driver {
        "agent-loop" => vec![fixture("llm-replay.yml")],
        "agent-loop-scripted" => vec![fixture("loop-scripted.yml"), fixture("llm-replay.yml")],
        other => panic!("no such driver `{other}`"),
    }
}

/// Boot the shipped tree under one driver, run a two-wake conversation, return its steps.
async fn conversation(
    driver: &str,
) -> (
    std::sync::Arc<bough_kernel::Kernel>,
    Vec<Step>,
    support::TempDir,
) {
    let (kernel, dir) = boot_real("headless", &patches(driver)).await;

    let ctx = row_ctx(&kernel, "exec");
    let agents = ctx.get::<Agents>().expect("the agents key is bound");
    let ledger = ctx.get::<Ledger>().expect("the ledger key is bound");

    let name = AgentName::new("sol");
    let traj = TrajId::new("lane/sol");
    let (agent, disposer) = agents
        .create(CreateAgent {
            name: name.clone(),
            traj: traj.clone(),
            kind: AgentKind::Resident,
            scope: None,
            setup: None,
            seed: Vec::new(),
            at: chrono::Utc::now(),
        })
        .await
        .expect("the creation transaction commits");

    for text in ["first", "second"] {
        agent.followup(andrey(text)).await.expect("mail lands");
        // A hang is a FAILURE, not a hung suite: `when_idle` that never resolves is exactly the
        // bug this gate exists to catch, and a test that waits forever reports nothing.
        tokio::time::timeout(std::time::Duration::from_secs(20), agent.when_idle())
            .await
            .unwrap_or_else(|_| panic!("the agent never went idle after `{text}`"));
    }

    let steps = ledger
        .0
        .steps(&StepQuery {
            trajs: vec![traj],
            order: Order::SeqAsc,
            ..Default::default()
        })
        .await
        .expect("the chain reads back");

    disposer.dispose().await;
    (kernel, steps, dir)
}

async fn every_durable_step_in_order(driver: &str) {
    let (kernel, steps, _dir) = conversation(driver).await;

    let wakes = wakes_of(&steps);
    assert_eq!(wakes.len(), 2, "one wake per Andrey message: {wakes:?}");

    for wake in &wakes {
        let kinds = kinds_of(&steps, wake);
        // DEVIATION from plan §2.8's numbering, which puts `wake/start` (2) before the claim
        // splice (3): `agent-loop` appends the claim first, so the claim is durable before
        // anything says a wake is running. Both orders satisfy §5's substantive rule — the claim
        // and the open are durable BEFORE the step that shows them to the model — so this asserts
        // the rule, not the plan's line numbers.
        assert!(
            kinds.contains(&"wake/start".to_string()),
            "every wake opens: {kinds:?}"
        );
        assert!(
            kinds
                .iter()
                .take_while(|k| *k != "step/start")
                .any(|k| k == "wake/start"),
            "the wake is open before its first step: {kinds:?}"
        );
        // `about/line` is appended by a listener ON the wake-end moment and carries the wake's
        // id, so it sits AFTER `wake/end` in the chain. The rule is that the loop appends nothing
        // of its own after the close, not that no row may.
        let loop_kinds: Vec<&String> = kinds.iter().filter(|k| *k != "about/line").collect();
        assert_eq!(
            loop_kinds.last().map(|k| k.as_str()),
            Some("wake/end"),
            "the loop's last durable step of a wake is wake/end: {kinds:?}"
        );
        // §5's order: the claim splice precedes the step, the header precedes the text, and the
        // step is bracketed.
        let at = |k: &str| kinds.iter().position(|x| x == k);
        assert!(
            at("inbox/spliced") < at("step/start"),
            "the claim is durable before the step opens: {kinds:?}"
        );
        assert!(
            at("step/start") < at("request/header"),
            "the header belongs to an open step: {kinds:?}"
        );
        assert!(
            at("request/header") < at("thought/text"),
            "the request is ledgered before what it produced: {kinds:?}"
        );
        assert!(
            at("thought/text") < at("step/end"),
            "the step closes after its output: {kinds:?}"
        );
    }

    kernel.shutdown().await;
}

async fn reason_and_consumed_set(driver: &str) {
    let (kernel, steps, _dir) = conversation(driver).await;

    let ends: Vec<&Step> = steps
        .iter()
        .filter(|s| s.kind.as_str() == "wake/end")
        .collect();
    assert_eq!(ends.len(), 2, "two wakes, two ends");
    for e in ends {
        assert_eq!(
            e.body.get("reason").and_then(|v| v.as_str()),
            Some("completed"),
            "a scripted wake that ran to the end of its script completes: {:?}",
            e.body
        );
        let consumed = e
            .body
            .get("consumed")
            .unwrap_or_else(|| panic!("wake/end must carry a consumed set: {:?}", e.body));
        assert!(
            consumed.is_array(),
            "the consumed set is a set of seq ranges: {consumed:?}"
        );
    }

    kernel.shutdown().await;
}

async fn invariants_hold(driver: &str) {
    let (kernel, _steps, _dir) = conversation(driver).await;

    kernel.run_invariants().await;
    let violations = kernel.violations();
    assert!(
        violations.is_empty(),
        "a clean scripted conversation must violate nothing: {violations:#?}"
    );
    // The gate is only meaningful if the runner actually had specs to run.
    assert!(
        kernel.row_context(&EntryId::new("ledger")).is_some(),
        "the ledger row must be live for its invariant to mean anything"
    );

    kernel.shutdown().await;
}

/// One named case per driver, so a failure names WHICH loop broke.
macro_rules! for_each_driver {
    ($body:ident, $live:ident, $scripted:ident) => {
        #[tokio::test]
        async fn $live() {
            let _guard = trace::test_lock();
            $body("agent-loop").await;
        }
        #[tokio::test]
        async fn $scripted() {
            let _guard = trace::test_lock();
            $body("agent-loop-scripted").await;
        }
    };
}

for_each_driver!(
    every_durable_step_in_order,
    a_scripted_conversation_appends_every_durable_step_in_order,
    a_scripted_conversation_appends_every_durable_step_in_order_under_the_scripted_driver
);
for_each_driver!(
    reason_and_consumed_set,
    wake_end_carries_the_reason_and_the_consumed_seq_set,
    wake_end_carries_the_reason_and_the_consumed_seq_set_under_the_scripted_driver
);
for_each_driver!(
    invariants_hold,
    the_ledger_and_agents_invariants_hold_across_the_conversation,
    the_ledger_and_agents_invariants_hold_across_the_conversation_under_the_scripted_driver
);
