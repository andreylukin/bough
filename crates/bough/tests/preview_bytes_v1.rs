//! V1, the strong half: the bytes the preview pane renders EQUAL the bytes the loop sent.
//!
//! `crates/bough/tests/preview_bytes.rs` proves the pane is `assemble()` — it compares the pane
//! against the seam. This file closes the loop the plan's verification map asks for: it captures
//! BOTH sides of the real thing — the `LlmRequest` the loop actually handed the adapter
//! (`agent_loop::invariant::seen()`, recorded at the call site in `wake.rs`) and the pane's own
//! `snapshot()` at that wake's `request/header.as_of` — and asserts the system prefix is byte
//! identical.
use crate::support;

use bough_plugin_agents::{AgentKind, Agents, CreateAgent, MailClass, Message, MessageId, Sender};
use bough_plugin_hello::trace;
use bough_plugin_ledger::query::{Order, StepQuery};
use bough_plugin_ledger::{AgentName, Ledger, Seq, TrajId};
use bough_plugin_projection::{Projection, ProjectionHandle};
use bough_plugin_tui_preview::{snapshot, PreviewAt};
use support::{boot_real, fixture, row_ctx};

#[tokio::test]
async fn the_preview_bytes_equal_the_system_prefix_the_loop_sent() {
    let _guard = trace::test_lock();
    bough_plugin_agent_loop::invariant::seen(); // touch, so the symbol is real
    let (kernel, _dir) = boot_real("headless", &[fixture("llm-replay-slow.yml")]).await;
    let ctx = row_ctx(&kernel, "exec");
    let agents = ctx.get::<Agents>().expect("the agents key is bound");
    let ledger = ctx.get::<Ledger>().expect("the ledger key is bound");
    let projection = row_ctx(&kernel, "agent.loop")
        .get::<Projection>()
        .expect("the projection key is bound");
    let traj = TrajId::new("lane/sol");
    let name = AgentName::new("sol");

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
    agent
        .followup(Message {
            id: MessageId::new("msg-preview-v1"),
            from: Sender::Andrey,
            class: MailClass::Wake,
            text: "say something".to_string(),
            subject: "say something".to_string(),
            cites: Vec::new(),
            refs: Default::default(),
            mail_seq: None,
            at: chrono::Utc::now(),
        })
        .await
        .expect("mail lands");
    // The window: the fixture delays the first chunk, so between "the request was handed to the
    // adapter" and "the answer lands" the ledger still holds exactly what the request was built
    // from. Sample the pane THERE — that is what "if it woke now" means, and it is the only
    // instant at which a byte comparison against the sent prefix is a fair one.
    let sent = loop {
        if let Some(s) = bough_plugin_agent_loop::invariant::seen()
            .into_iter()
            .next_back()
        {
            break s;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    };
    let sent_system = sent.request.system.clone().unwrap_or_default();

    let steps = ledger
        .0
        .steps(&StepQuery {
            trajs: vec![traj.clone()],
            order: Order::SeqAsc,
            ..Default::default()
        })
        .await
        .expect("the chain reads back");
    let header = steps
        .iter()
        .rfind(|s| s.kind.as_str() == "request/header" && s.wake == sent.wake)
        .expect("the wake appended a request/header before sending");
    let as_of = Seq(header
        .body
        .get("as_of")
        .and_then(|v| v.as_u64())
        .expect("the header records its high-water"));

    // SIDE B: what the pane renders, at that anchor, right now.
    let snap = snapshot(
        &ProjectionHandle(projection.0.clone()),
        &bough_plugin_ledger::LedgerHandle(ledger.0.clone()),
        &name,
        PreviewAt::Seq(as_of),
        header.at,
    )
    .await
    .expect("the preview is takeable");

    if snap.text != sent_system {
        let a: Vec<&str> = sent_system.lines().collect();
        let b: Vec<&str> = snap.text.lines().collect();
        let first = a
            .iter()
            .zip(b.iter())
            .position(|(x, y)| x != y)
            .unwrap_or(a.len().min(b.len()));
        panic!(
            "the preview is not the sent prefix.\n  sent {} lines, preview {} lines; first \
             divergence at line {first}\n  sent:    {:?}\n  preview: {:?}",
            a.len(),
            b.len(),
            a.get(first),
            b.get(first),
        );
    }
    assert!(!snap.text.is_empty(), "an empty prefix proves nothing");
    // Not a vacuous comparison: the prefix under test is the real, multi-section one.
    assert!(
        snap.text.contains("## Identity")
            && snap.text.contains("## Recent steps")
            && snap.text.contains("## Unconsumed mail"),
        "the compared prefix is not the substantive one: {:?}",
        snap.text
    );
    assert_eq!(
        snap.as_of, as_of,
        "the preview assembled at the header's high-water"
    );

    tokio::time::timeout(std::time::Duration::from_secs(20), agent.when_idle())
        .await
        .expect("the wake finished");

    drop(agent);
    disposer.dispose().await;
    kernel.shutdown().await;
}
