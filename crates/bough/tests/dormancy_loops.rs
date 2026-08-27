//! V4's "under BOTH loop providers" (P5-D1). `agent/wake-request` is the ONE loop change of the
//! phase, and a suppression point only one Provider dispatches is a suppression point that stops
//! working the moment the swap gate runs. So each bullet below is parameterised over the two
//! drivers, one named case each — P1-D10's lesson: never a single red test that could mean either.
//!
//! These tests do NOT go through the `dormancy` row. They stand a listener in its place, because
//! what is under test is the LOOP's obligation to dispatch and to obey, not the fold that decides.

mod support;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use bough_plugin_agents::{
    Admit, AgentKind, AgentWakeRequest, Agents, CreateAgent, MailClass, Message, MessageId, Sender,
};
use bough_plugin_hello::trace;
use bough_plugin_ledger::query::StepQuery;
use bough_plugin_ledger::{AgentName, Ledger, StepType, TrajId};
use support::{boot_real, fixture, row_ctx};

/// The two loop Providers this gate is parameterised over.
fn patches(driver: &str) -> Vec<std::path::PathBuf> {
    match driver {
        "agent-loop" => vec![fixture("llm-replay.yml")],
        "agent-loop-scripted" => vec![fixture("loop-scripted.yml"), fixture("llm-replay.yml")],
        other => panic!("no such driver `{other}`"),
    }
}

fn andrey(text: &str) -> Message {
    Message {
        id: MessageId::new(format!("msg-{}", text.replace(' ', "-"))),
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

struct Booted {
    kernel: Arc<bough_kernel::Kernel>,
    ctx: bough_kernel::Context,
    agent: bough_plugin_agents::Agent,
    disposer: bough_plugin_agents::AgentDisposer,
    ledger: Arc<bough_plugin_ledger::LedgerHandle>,
    _dir: support::TempDir,
}

/// Boot `headless` under one driver with one live lane. `headless` because the wake-admission
/// point is not a terminal feature: `bough-base` is where `dormancy` lives.
async fn boot(driver: &str) -> Booted {
    let (kernel, dir) = boot_real("headless", &patches(driver)).await;
    let ctx = row_ctx(&kernel, "exec");
    let agents = ctx.get::<Agents>().expect("the agents key is bound");
    let ledger = ctx.get::<Ledger>().expect("the ledger key is bound");
    let (agent, disposer) = agents
        .create(CreateAgent {
            name: AgentName::new("sol"),
            traj: TrajId::new("lane/sol"),
            kind: AgentKind::Resident,
            scope: None,
            setup: None,
            seed: Vec::new(),
            at: chrono::Utc::now(),
        })
        .await
        .expect("the creation transaction commits");
    Booted {
        kernel,
        ctx,
        agent,
        disposer,
        ledger,
        _dir: dir,
    }
}

impl Booted {
    /// Deliver one Andrey message and wait for the agent to go idle again. A hang is a failure.
    async fn say(&self, text: &str) {
        self.agent
            .followup(andrey(text))
            .await
            .expect("the mail lands");
        tokio::time::timeout(std::time::Duration::from_secs(20), self.agent.when_idle())
            .await
            .unwrap_or_else(|_| panic!("the agent never went idle after `{text}`"));
    }

    async fn wake_starts(&self) -> usize {
        self.ledger
            .0
            .steps(&StepQuery {
                trajs: vec![TrajId::new("lane/sol")],
                kinds: vec![StepType::new("wake/start")],
                ..Default::default()
            })
            .await
            .expect("the chain reads back")
            .len()
    }

    async fn finish(self) {
        self.disposer.dispose().await;
        self.kernel.shutdown().await;
    }
}

/// The loop dispatches `agent/wake-request` before it opens a wake — proved by a listener that
/// COUNTS and admits, so the conversation still runs and the count is not an artefact of a
/// suppressed one.
async fn admission_is_dispatched_by(driver: &str) {
    let _guard = trace::test_lock();
    let b = boot(driver).await;

    let seen = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&seen);
    let _listener = b
        .ctx
        .on_waterfall::<AgentWakeRequest, _, _>(move |v, next| {
            let counter = Arc::clone(&counter);
            async move {
                counter.fetch_add(1, Ordering::Relaxed);
                next.run(v).await
            }
        })
        .await
        .expect("the listener registers");

    let before = b.wake_starts().await;
    b.say("hello").await;
    let after = b.wake_starts().await;

    assert!(
        after > before,
        "`{driver}` opened no wake at all, so this bullet would be vacuous"
    );
    assert!(
        seen.load(Ordering::Relaxed) >= 1,
        "`{driver}` opened a wake without dispatching `agent/wake-request` (P5-D1)"
    );
    b.finish().await;
}

#[tokio::test]
async fn admission_is_dispatched_by_agent_loop() {
    admission_is_dispatched_by("agent-loop").await;
}

#[tokio::test]
async fn admission_is_dispatched_by_agent_loop_scripted() {
    admission_is_dispatched_by("agent-loop-scripted").await;
}

/// The half that matters: a `Defer` means the wake NEVER EXISTS. Not an empty wake, not a
/// `wake/start` followed by a `wake/end` — no step at all. `agent/pre-step` could not have given
/// this answer, which is the whole argument of P5-D1.
async fn a_deferred_wake_appends_no_wake_start(driver: &str) {
    let _guard = trace::test_lock();
    let b = boot(driver).await;

    // The control: without the listener, this driver opens a wake for an Andrey message.
    let before = b.wake_starts().await;
    b.say("the control").await;
    let control = b.wake_starts().await;
    assert!(
        control > before,
        "`{driver}` opened no wake for an Andrey message; the deferral below would prove nothing"
    );

    let _listener = b
        .ctx
        .on_waterfall::<AgentWakeRequest, _, _>(|mut v, _next| async move {
            v.decision = Admit::Defer {
                by: "dormancy",
                reason: "the lane is dormant".to_string(),
            };
            v
        })
        .await
        .expect("the listener registers");

    b.agent
        .followup(andrey("into the void"))
        .await
        .expect("the mail lands");
    // No wake will open, so `when_idle` is not the thing to wait for: the tree going quiet is.
    assert!(b.kernel.quiesce().await, "the tree quiesces");

    assert_eq!(
        b.wake_starts().await,
        control,
        "`{driver}` appended a `wake/start` for a wake the admission point refused"
    );
    b.finish().await;
}

#[tokio::test]
async fn a_deferred_wake_appends_no_wake_start_under_either() {
    a_deferred_wake_appends_no_wake_start("agent-loop").await;
    a_deferred_wake_appends_no_wake_start("agent-loop-scripted").await;
}
