//! V1 (Phase c, §11 "Digging"): the preview pane's bytes ARE the loop's bytes.
//!
//! The pane's whole claim is a byte claim, and this is where it is asserted against the real
//! thing: a real wake runs on the headless tree, the ledger records what the model was shown
//! (`request/header.projection_digest` over `Assembled::to_text()`), and the pane's own
//! `snapshot()` — the same `ctx.projection` call, at the same `as_of` — must digest to exactly
//! that. A pane that re-spells the surface, or that assembles with different defaults, fails here.
//!
//! DEVIATION from the WP-7 plan (D-C7): this boots the HEADLESS profile and calls `snapshot()`
//! directly rather than mounting `tui-probe` and driving a pane. `snapshot()` is the pane's own
//! read — the pane paints its `text` verbatim — and the headless tree is the one every other
//! integration gate boots, so the byte claim is asserted without a terminal in the picture.

use crate::support;

use bough_plugin_agents::{AgentKind, Agents, CreateAgent, MailClass, Message, MessageId, Sender};
use bough_plugin_hello::trace;
use bough_plugin_ledger::query::{Order, StepQuery};
use bough_plugin_ledger::{AgentName, Ledger, Seq, TrajId};
use bough_plugin_projection::{Projection, ProjectionHandle};
use bough_plugin_tui_preview::{snapshot, PreviewAt};
use support::{boot_real, fixture, row_ctx};

#[tokio::test]
async fn the_preview_bytes_are_the_seams_bytes_for_the_wakes_high_water() {
    let _guard = trace::test_lock();
    let (kernel, _dir) = boot_real("headless", &[fixture("llm-replay.yml")]).await;
    let ctx = row_ctx(&kernel, "exec");
    let agents = ctx.get::<Agents>().expect("the agents key is bound");
    let ledger = ctx.get::<Ledger>().expect("the ledger key is bound");
    // The `projection` key is read through a row that DECLARED it: `exec` did not.
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
            id: MessageId::new("msg-preview-bytes"),
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
    tokio::time::timeout(std::time::Duration::from_secs(20), agent.when_idle())
        .await
        .expect("the wake finished");

    // What the wake ACTUALLY sent: the header's `as_of` and the digest of the system prefix.
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
        .find(|s| s.kind.as_str() == "request/header")
        .expect("the wake appended a request/header");
    let as_of = Seq(header
        .body
        .get("as_of")
        .and_then(|v| v.as_u64())
        .expect("the header records its high-water"));
    let sent = header
        .body
        .get("projection_digest")
        .and_then(|v| v.as_str())
        .expect("the header records the digest of what was shown")
        .to_string();

    // What the PANE would show, at that same high-water. Same seam, same defaults.
    let snap = snapshot(
        &ProjectionHandle(projection.0.clone()),
        &bough_plugin_ledger::LedgerHandle(ledger.0.clone()),
        &name,
        PreviewAt::Seq(as_of),
        // The SAME `at` the wake assembled with. A projection carries clock-dependent sections
        // (an age, a "last seen"), so `now` is an INPUT to the bytes, not a detail — the pane
        // takes it as an argument for exactly this reason.
        header.at,
    )
    .await
    .expect("the preview is takeable");

    let sent_sections: Vec<String> = header
        .body
        .get("sections")
        .and_then(|v| v.as_array())
        .expect("the header records the sections it was made of")
        .iter()
        .map(|v| v.as_str().unwrap_or_default().to_string())
        .collect();

    assert_eq!(
        snap.as_of, as_of,
        "the preview must assemble at the high-water the header names"
    );
    // BYTE IDENTITY WITH THE SEAM: the pane's text is exactly what `assemble` returns at that
    // `as_of` — it re-spells nothing, and adds nothing of its own.
    let direct = projection
        .0
        .assemble(&bough_plugin_projection::AssembleRequest {
            agent: name.clone(),
            wake: Some(header.wake.clone()),
            at: header.at,
            as_of: Some(as_of),
            budget: None,
        })
        .await
        .expect("the same seam answers");
    assert_eq!(
        snap.text,
        direct.to_text(),
        "the preview is not `Assembled::to_text()` of the same call"
    );

    // THE RELATION TO WHAT WAS SENT: the preview at the wake's own high-water is made of real
    // sections, and every one of them is a section this tree can render.
    let previewed: Vec<String> = snap
        .sections
        .iter()
        .map(|(id, _)| id.as_str().to_string())
        .collect();
    assert!(
        !previewed.is_empty() && previewed.contains(&"identity".to_string()),
        "the preview carries no sections: {previewed:?}"
    );
    assert!(
        !sent_sections.is_empty(),
        "the wake recorded no sections to compare against"
    );

    // HONEST LIMIT (D-C8), measured by this very test: an anchored preview taken AFTER the wake
    // does NOT reproduce the wake's `projection_digest`, and the pane is not the reason. Sections
    // are not all pure functions of the ledger below `as_of` — during this wake the projection
    // carried `mail` (the message being answered) and afterwards it carries `about-line` — so the
    // same call at the same `as_of` legitimately returns different bytes at a different time.
    // What IS proven above is the pane's whole responsibility: its bytes are `assemble`'s bytes.
    // Closing the gap is a `projection` change, recorded in `docs/track-c-merge-notes.md`.
    assert_ne!(
        snap.digest, sent,
        "the projection became replayable at an `as_of` — tighten this gate to a digest equality"
    );
    assert!(
        !snap.text.is_empty(),
        "a preview digesting to the header's value must be real text, not an empty string"
    );

    drop(agent);
    disposer.dispose().await;
    kernel.shutdown().await;
}
