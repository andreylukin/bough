//! §2's about-line: two halves, never confused, refreshed on COMPLETED wakes only.
//!
//! The whole durable path runs against a real ledger (`ledger-memory`, the behavioural twin of
//! `ledger-sqlite`), so "the state half cites the steps it summarises" is checked by the LEDGER's
//! own evidence-requires-cites rule and not only by an assertion here.

use std::sync::Arc;

use bough_kernel::{Context, KernelCore};
use bough_plugin_about_line::{
    compose, invariant, refresh, render, section, step_types, AboutConfig, AboutLine, ABOUT_LINE,
    INTENT_LABEL,
};
use bough_plugin_ledger::vocabulary::WakeEndReason;
use bough_plugin_ledger::{
    Append, Class, Connected, LedgerHandle, Order, Step, StepQuery, StepType, TrajId, WakeId,
};
use bough_plugin_ledger_memory::store::MemoryStore;
use bough_plugin_projection::order::order;
use bough_plugin_projection::{
    Place, Position, RenderedSection, SectionCites, SectionId, SectionRender, SectionRequest, Slot,
};
use chrono::{DateTime, TimeZone, Utc};

fn at() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap()
}

fn cfg() -> AboutConfig {
    AboutConfig {
        max_state_chars: 400,
        max_intent_chars: 200,
    }
}

/// A ledger with this crate's step type declared, as `apply` declares it.
fn ledger() -> LedgerHandle {
    let store = MemoryStore::new(Context::root(KernelCore::new()));
    let handle = LedgerHandle(store);
    for def in step_types() {
        // The token is dropped, not spent: a registration is undone by an EFFECT, never by a
        // `Drop` (§0.2), so dropping it leaves the type registered for the test's life.
        drop(
            handle
                .0
                .register_step_type(def)
                .expect("about/line is a fresh step type"),
        );
    }
    handle
}

async fn put(l: &LedgerHandle, wake: &str, kind: &str, body: serde_json::Value) -> Step {
    l.0.append(Append {
        traj: TrajId::new("t1"),
        wake: WakeId::new(wake),
        kind: StepType::new(kind),
        class: Class::Thought,
        body,
        cites: vec![],
        at: at(),
        id: None,
    })
    .await
    .expect("the step appends")
}

/// One wake that said something, ran a tool and ended `reason`.
async fn a_wake(l: &LedgerHandle, wake: &str, reason: WakeEndReason) -> (Vec<Step>, Step) {
    let mut body = Vec::new();
    put(
        l,
        wake,
        "wake/start",
        serde_json::json!({ "urgency": "immediate" }),
    )
    .await;
    put(l, wake, "step/start", serde_json::json!({ "index": 0 })).await;
    body.push(
        put(
            l,
            wake,
            "thought/text",
            serde_json::json!({ "text": "read the plan\nnext: write the tests", "step_index": 0 }),
        )
        .await,
    );
    body.push(
        put(
            l,
            wake,
            "tool/call",
            serde_json::json!({ "call": "c1", "name": "bash", "args": {}, "render": "generic", "step_index": 0 }),
        )
        .await,
    );
    put(
        l,
        wake,
        "step/end",
        serde_json::json!({ "index": 0, "outcome": "ok", "detail": null }),
    )
    .await;
    let end = put(
        l,
        wake,
        "wake/end",
        serde_json::json!({ "reason": reason, "cause": null, "consumed": [] }),
    )
    .await;
    (body, end)
}

async fn lines(l: &LedgerHandle) -> Vec<Step> {
    l.0.steps(&StepQuery {
        trajs: vec![TrajId::new("t1")],
        kinds: vec![StepType::new(ABOUT_LINE)],
        order: Order::SeqAsc,
        ..Default::default()
    })
    .await
    .expect("the query runs")
}

/// The thought/text and tool/call step types this test plants are `agents`' and `tools`'; the
/// ledger refuses an unknown type, so the test declares the two it writes.
fn declare_borrowed_types(l: &LedgerHandle) {
    use bough_plugin_ledger::{ClassRule, StepTypeDef};
    #[derive(schemars::JsonSchema)]
    #[allow(dead_code)]
    struct ThoughtText {
        text: String,
        step_index: u32,
    }
    #[derive(schemars::JsonSchema)]
    #[allow(dead_code)]
    struct ToolCall {
        call: String,
        name: String,
        args: serde_json::Value,
        render: String,
        step_index: u32,
    }
    drop(
        l.0.register_step_type(
            StepTypeDef::of::<ThoughtText>("thought/text", "test").class_rule(ClassRule::Thought),
        )
        .expect("fresh"),
    );
    drop(
        l.0.register_step_type(
            StepTypeDef::of::<ToolCall>("tool/call", "test").class_rule(ClassRule::Thought),
        )
        .expect("fresh"),
    );
}

#[tokio::test]
async fn a_completed_wake_refreshes_the_line_and_the_state_half_cites_the_steps_it_summarises() {
    let l = ledger();
    declare_borrowed_types(&l);
    let (body, end) = a_wake(&l, "w1", WakeEndReason::Completed).await;

    let step = refresh(
        &l,
        &cfg(),
        &WakeId::new("w1"),
        WakeEndReason::Completed,
        &end.id,
    )
    .await
    .expect("the refresh writes")
    .expect("a completed wake refreshes");

    // EVIDENCE: the ledger would have refused the append with no cites.
    assert_eq!(step.class, Class::Evidence);
    let cited: Vec<String> = step.cites.iter().map(|c| c.r#ref.to_string()).collect();
    assert_eq!(
        cited,
        body.iter()
            .map(|s| format!("step:{}", s.id))
            .collect::<Vec<_>>(),
        "the state half cites exactly the steps it summarises"
    );

    let line: AboutLine = serde_json::from_value((*step.body).clone()).expect("a readable line");
    assert_eq!(line.of_wake, WakeId::new("w1"));
    assert_eq!(line.state, "read the plan; ran `bash`");
    assert_eq!(line.intent, "next: write the tests");
    assert_eq!(lines(&l).await.len(), 1);
}

/// §5: a preempted wake refreshes nothing.
#[tokio::test]
async fn an_interrupted_wake_refreshes_nothing() {
    let l = ledger();
    declare_borrowed_types(&l);
    let (_, end) = a_wake(&l, "w1", WakeEndReason::Interrupted).await;

    for reason in [
        WakeEndReason::Interrupted,
        WakeEndReason::Aborted,
        WakeEndReason::Error,
        WakeEndReason::MaxTokens,
    ] {
        assert!(
            refresh(&l, &cfg(), &WakeId::new("w1"), reason, &end.id)
                .await
                .expect("the refresh runs")
                .is_none(),
            "{reason:?} must refresh nothing"
        );
    }
    assert!(lines(&l).await.is_empty(), "no line was written");
}

/// §2: the intent half is never presented as truth — it renders under its own explicit label.
#[test]
fn the_intent_half_renders_under_its_label_and_the_state_half_does_not() {
    let body = render(&AboutLine {
        state: "read the plan".into(),
        intent: "write the tests".into(),
        of_wake: WakeId::new("w1"),
    });
    let (state_half, intent_half) = body.split_once(INTENT_LABEL).expect("the label is present");
    assert_eq!(state_half.trim(), "read the plan");
    assert_eq!(
        intent_half.trim_start_matches(':').trim(),
        "write the tests"
    );
    assert!(
        !state_half.contains("write the tests"),
        "the halves are never confused"
    );
}

#[tokio::test]
async fn the_section_renders_the_newest_line_at_identity_after() {
    let l = ledger();
    declare_borrowed_types(&l);
    let (_, e1) = a_wake(&l, "w1", WakeEndReason::Completed).await;
    refresh(
        &l,
        &cfg(),
        &WakeId::new("w1"),
        WakeEndReason::Completed,
        &e1.id,
    )
    .await
    .unwrap()
    .unwrap();

    let rendered = section::AboutSection
        .render(&SectionRequest {
            agent: bough_plugin_ledger::AgentName::new("sol"),
            wake: Some(WakeId::new("w1")),
            at: at(),
            ledger: l.clone(),
            as_of: None,
            connected: Arc::new(Connected {
                own: TrajId::new("t1"),
                ancestry: vec![],
                ref_matches: vec![],
                refs: Default::default(),
            }),
        })
        .await
        .expect("the section renders")
        .expect("there is a line to render");
    assert_eq!(rendered.title, "About");
    assert!(rendered.body.contains(INTENT_LABEL));
    assert_eq!(
        rendered.cites.steps.len(),
        1,
        "the section names the row it read"
    );

    // Identity/After: after the identity band itself, before everything in Pins. Checked through
    // the projection's OWN ordering function, so this is the real placement and not a restatement
    // of the constant.
    assert_eq!(
        section::POSITION,
        Position {
            slot: Slot::Identity,
            place: Place::After
        }
    );
    let mut sections = vec![
        stub("pins", Position::band(Slot::Pins)),
        stub(section::section_id().as_str(), section::POSITION),
        stub("identity", Position::band(Slot::Identity)),
    ];
    order(&mut sections);
    assert_eq!(
        sections
            .iter()
            .map(|s| s.id.to_string())
            .collect::<Vec<_>>(),
        vec!["identity", "about-line", "pins"]
    );
}

fn stub(id: &str, position: Position) -> RenderedSection {
    RenderedSection {
        id: SectionId::new(id),
        position,
        title: id.into(),
        body: String::new(),
        cites: SectionCites::default(),
        tokens: 0,
        degraded: None,
    }
}

/// The invariant module, against the rows the refresh actually wrote.
#[tokio::test]
async fn the_invariant_holds_over_a_refreshed_trajectory() {
    let l = ledger();
    declare_borrowed_types(&l);
    let (_, end) = a_wake(&l, "w1", WakeEndReason::Completed).await;
    refresh(
        &l,
        &cfg(),
        &WakeId::new("w1"),
        WakeEndReason::Completed,
        &end.id,
    )
    .await
    .unwrap()
    .unwrap();
    let all =
        l.0.steps(&StepQuery {
            trajs: vec![TrajId::new("t1")],
            order: Order::SeqAsc,
            ..Default::default()
        })
        .await
        .expect("the query runs");
    assert_eq!(invariant::evaluate(&all), Ok(()));
}

/// A wake that summarised to nothing still produces evidence with a cite: the ledger's
/// evidence-requires-cites rule is satisfied by construction, not by luck.
#[tokio::test]
async fn an_empty_wake_still_cites_its_own_wake_end() {
    let l = ledger();
    let end = put(
        &l,
        "w9",
        "wake/end",
        serde_json::json!({ "reason": "completed", "cause": null, "consumed": [] }),
    )
    .await;
    let step = refresh(
        &l,
        &cfg(),
        &WakeId::new("w9"),
        WakeEndReason::Completed,
        &end.id,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        step.cites
            .iter()
            .map(|c| c.r#ref.to_string())
            .collect::<Vec<_>>(),
        vec![format!("step:{}", end.id)]
    );
    assert_eq!(
        compose::compose(&[], &WakeId::new("w9"), &end.id, &cfg())
            .line
            .state,
        "nothing to report"
    );
}
