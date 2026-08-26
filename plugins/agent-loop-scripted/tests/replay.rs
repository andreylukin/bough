//! The swap gate: a SECOND Provider of the wake seam, held to the LEDGER PROTOCOL and not to a
//! feature list. A two-wake transcript must append §5's durable steps in §5's order, carry the
//! consumed set on `wake/end`, and replay identically twice.

use std::sync::Arc;

use bough_kernel::{Context, KernelCore};
use bough_plugin_agent_loop_scripted::replay::{
    dump, run_wake, ReplayEnv, ScriptedClaim, WakeInput,
};
use bough_plugin_agent_loop_scripted::Script;
use bough_plugin_agents::AgentId;
use bough_plugin_ledger::vocabulary::{SpliceTarget, Urgency, WakeEnd, WakeEndReason};
use bough_plugin_ledger::{
    Append, Cite, Class, ClassRule, LedgerHandle, Order, Ref, SeqRange, Step, StepQuery, StepType,
    StepTypeDef, TrajId, WakeId,
};
use bough_plugin_ledger_memory::store::MemoryStore;
use bough_plugin_llm::WakeKind;
use chrono::{DateTime, TimeZone, Utc};

const TRANSCRIPT: &str = r#"
wakes:
  - steps:
      - chunks:
          - { chunk: text, text: "answering Andrey" }
          - { chunk: end, stop: end_turn }
  - steps:
      - chunks:
          - { chunk: reasoning, text: "weighing the mail" }
          - { chunk: text, text: "drained it" }
          - { chunk: end, stop: end_turn }
"#;

fn at() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap()
}

/// A ledger with the step types the loop writes that `agents` and `tools` own. In a composed
/// tree those rows declare them; a unit test declares what it writes.
fn ledger() -> LedgerHandle {
    #[derive(schemars::JsonSchema)]
    #[allow(dead_code)]
    struct ThoughtText {
        text: String,
        step_index: u32,
    }
    #[derive(schemars::JsonSchema)]
    #[allow(dead_code)]
    struct ThoughtReasoning {
        text: String,
        meta: Option<serde_json::Value>,
        step_index: u32,
    }
    let l = LedgerHandle(MemoryStore::new(Context::root(KernelCore::new())));
    for def in [
        StepTypeDef::of::<ThoughtText>("thought/text", "test").class_rule(ClassRule::Thought),
        StepTypeDef::of::<ThoughtReasoning>("thought/reasoning", "test")
            .class_rule(ClassRule::Thought),
    ] {
        drop(l.0.register_step_type(def).expect("a fresh step type"));
    }
    l
}

fn env(ctx: Context, l: LedgerHandle) -> ReplayEnv {
    ReplayEnv {
        ctx,
        ledger: l,
        projection: None,
        script: Arc::new(Script::parse(TRANSCRIPT).expect("the transcript parses")),
        strict: true,
        prompt_ver: "scripted".into(),
        composition: "test".into(),
        default_max_tokens: 8192,
        recorder: None,
    }
}

async fn deliver(l: &LedgerHandle, traj: &TrajId, subject: &str) -> Step {
    l.0.append(Append {
        traj: traj.clone(),
        wake: WakeId::new("w0"),
        kind: StepType::new("mail/delivered"),
        class: Class::Evidence,
        body: serde_json::json!({
            "class": "ordinary",
            "from": "agent:andrey",
            "subject": subject,
            "summary": subject,
            "refs": [],
        }),
        cites: vec![Cite {
            r#ref: Ref::new("agent:andrey"),
            url: None,
        }],
        at: at(),
        id: None,
    })
    .await
    .expect("the mail lands")
}

fn input(
    traj: &TrajId,
    wake: &str,
    index: usize,
    kind: WakeKind,
    mail: Option<&Step>,
) -> WakeInput {
    WakeInput {
        traj: traj.clone(),
        agent: bough_plugin_ledger::AgentName::new("sol"),
        agent_id: AgentId::new("a1"),
        wake: WakeId::new(wake),
        index,
        kind,
        urgency: match kind {
            WakeKind::Answer => Urgency::Immediate,
            _ => Urgency::Coalesced,
        },
        trigger: mail.map(|s| s.id.clone()),
        answers_andrey: kind == WakeKind::Answer,
        model_override: None,
        claim: mail
            .map(|s| {
                vec![ScriptedClaim {
                    message: format!("m-{}", s.id),
                    target: SpliceTarget::NextWake,
                    wake: true,
                    mail_seq: Some(s.seq),
                }]
            })
            .unwrap_or_default(),
        handle: None,
        at: at(),
    }
}

async fn kinds(l: &LedgerHandle, traj: &TrajId) -> Vec<String> {
    all(l, traj)
        .await
        .iter()
        .map(|s| s.kind.to_string())
        .collect()
}

async fn all(l: &LedgerHandle, traj: &TrajId) -> Vec<Step> {
    l.0.steps(&StepQuery {
        trajs: vec![traj.clone()],
        order: Order::SeqAsc,
        ..Default::default()
    })
    .await
    .expect("the query runs")
}

/// Two wakes, replayed through the same seam. The order is §5's, verbatim.
#[tokio::test]
async fn a_two_wake_transcript_replays_in_order() {
    let l = ledger();
    let traj = TrajId::new("t1");
    let e = env(Context::root(KernelCore::new()), l.clone());

    let mail = deliver(&l, &traj, "look at the plan").await;
    let first = run_wake(&e, &input(&traj, "w1", 0, WakeKind::Answer, Some(&mail)))
        .await
        .expect("the first wake replays");
    let second = run_wake(&e, &input(&traj, "w2", 1, WakeKind::Drain, None))
        .await
        .expect("the second wake replays");

    assert_eq!(
        kinds(&l, &traj).await,
        vec![
            "mail/delivered",
            // wake 1: an answer wake that claimed its trigger.
            "wake/start",
            "inbox/spliced",
            "step/start",
            "request/header",
            "thought/text",
            "step/end",
            "wake/end",
            // wake 2: a drain wake with nothing to claim.
            "wake/start",
            "step/start",
            "request/header",
            "thought/reasoning",
            "thought/text",
            "step/end",
            "wake/end",
        ]
    );

    // The consumed set rides `wake/end`, and it is the mail the wake claimed.
    let ends: Vec<WakeEnd> = all(&l, &traj)
        .await
        .iter()
        .filter(|s| s.kind.as_str() == "wake/end")
        .map(|s| serde_json::from_value((*s.body).clone()).expect("a readable wake/end"))
        .collect();
    assert_eq!(ends.len(), 2);
    assert_eq!(ends[0].reason, WakeEndReason::Completed);
    assert_eq!(
        ends[0].consumed,
        vec![SeqRange {
            from: mail.seq,
            to: mail.seq
        }],
        "an answer wake consumes its trigger"
    );
    assert!(
        ends[1].consumed.is_empty(),
        "a wake that claimed nothing consumes nothing"
    );
    assert_eq!(first.consumed, ends[0].consumed);
    assert_eq!(second.reason, WakeEndReason::Completed);
    assert_eq!(second.steps, 1);
}

/// A replay is a function of the transcript: the same transcript, twice, produces the same
/// durable rows. Ids and clocks are excluded — they are the two things the ledger protocol does
/// not fix, and the two a replay cannot make equal.
#[tokio::test]
async fn the_same_transcript_replays_byte_identically() {
    async fn once(traj: &str) -> String {
        let l = ledger();
        let traj = TrajId::new(traj);
        let e = env(Context::root(KernelCore::new()), l.clone());
        let mail = deliver(&l, &traj, "look at the plan").await;
        run_wake(&e, &input(&traj, "w1", 0, WakeKind::Answer, Some(&mail)))
            .await
            .unwrap();
        run_wake(&e, &input(&traj, "w2", 1, WakeKind::Drain, None))
            .await
            .unwrap();
        // The trigger id is a minted uuid, so it is the one body field that cannot repeat; the
        // dump would otherwise be comparing the ledger's id minting, not the replay.
        dump(&all(&l, &traj).await).replace(mail.id.as_str(), "<mail>")
    }
    assert_eq!(once("t1").await, once("t2").await);
}

/// §0.2: misconfiguration fails LOUD. Running out of script under `strict` is an error, not a
/// silent idle.
#[tokio::test]
async fn running_out_of_script_is_an_error_under_strict() {
    let l = ledger();
    let traj = TrajId::new("t1");
    let mut e = env(Context::root(KernelCore::new()), l.clone());
    let err = run_wake(&e, &input(&traj, "w9", 7, WakeKind::Drain, None))
        .await
        .expect_err("wake 7 is not in a two-wake transcript");
    assert!(err.to_string().contains("no wake at index 7"), "{err}");

    // Not strict: the wake still CLOSES durably, having spent no step. A wake that opened and
    // never closed is the one shape §5 does not allow.
    e.strict = false;
    let out = run_wake(&e, &input(&traj, "w9", 7, WakeKind::Drain, None))
        .await
        .expect("a lenient replay closes the wake");
    assert_eq!(out.steps, 0);
    assert_eq!(out.reason, WakeEndReason::Completed);
    assert_eq!(kinds(&l, &traj).await, vec!["wake/start", "wake/end"]);
}

/// §5 step 7: `request/header` is appended ONLY when it differs from the last one in the wake.
#[tokio::test]
async fn an_unchanged_request_header_is_not_appended_twice() {
    let l = ledger();
    let traj = TrajId::new("t1");
    let mut e = env(Context::root(KernelCore::new()), l.clone());
    e.script = Arc::new(
        Script::parse(
            r#"
wakes:
  - steps:
      - chunks: [ { chunk: text, text: "one" }, { chunk: end } ]
      - chunks: [ { chunk: text, text: "two" }, { chunk: end } ]
"#,
        )
        .expect("the transcript parses"),
    );
    run_wake(&e, &input(&traj, "w1", 0, WakeKind::Drain, None))
        .await
        .expect("the wake replays");
    let headers = kinds(&l, &traj)
        .await
        .iter()
        .filter(|k| *k == "request/header")
        .count();
    assert_eq!(
        headers, 1,
        "two steps with the same composition write one header"
    );
    assert_eq!(
        kinds(&l, &traj).await,
        vec![
            "wake/start",
            "step/start",
            "request/header",
            "thought/text",
            "step/end",
            "step/start",
            "thought/text",
            "step/end",
            "wake/end",
        ]
    );
}

/// §12: a failure is a terminal chunk, and this row implements no retry — the wake ends `error`.
#[tokio::test]
async fn a_failed_stream_ends_the_wake_with_reason_error() {
    let l = ledger();
    let traj = TrajId::new("t1");
    let mut e = env(Context::root(KernelCore::new()), l.clone());
    e.script = Arc::new(
        Script::parse(
            r#"
wakes:
  - steps:
      - chunks:
          - { chunk: failed, failure: { kind: overloaded, message: "upstream said no", retryable: true, status: 529, adapter: llm-replay } }
      - chunks: [ { chunk: text, text: "never reached" }, { chunk: end } ]
"#,
        )
        .expect("the transcript parses"),
    );
    let out = run_wake(&e, &input(&traj, "w1", 0, WakeKind::Drain, None))
        .await
        .expect("the wake replays");
    assert_eq!(out.reason, WakeEndReason::Error);
    assert_eq!(out.steps, 1, "the second step never ran");
    let steps = all(&l, &traj).await;
    let end = steps
        .iter()
        .find(|s| s.kind.as_str() == "step/end")
        .expect("a step/end");
    assert_eq!(
        end.body.get("outcome").and_then(|v| v.as_str()),
        Some("error")
    );
    assert_eq!(
        end.body.get("detail").and_then(|v| v.as_str()),
        Some("upstream said no")
    );
}
