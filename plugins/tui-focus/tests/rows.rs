//! WP-4 / §2.4: `rows_from_steps` is the heart of the focus pane, and it is PURE and TOTAL. These
//! drive it against a fixture step list — no ledger, no terminal, no model.

use std::collections::BTreeSet;
use std::sync::Arc;

use bough_plugin_ledger::vocabulary::MailClass;
use bough_plugin_ledger::{Class, Seq, Step, StepId, StepType, TrajId, WakeId};
use bough_plugin_llm::ToolCallId;
use bough_plugin_tools::RenderIntent;
use bough_plugin_tui_focus::{rows_from_steps, Row};

fn step(n: u64, kind: &str, body: serde_json::Value) -> Step {
    Step {
        id: StepId::new(format!("s{n}")),
        traj: TrajId::new("lane/sol"),
        seq: Seq(n),
        at: chrono::Utc::now(),
        wake: WakeId::new("w1"),
        kind: StepType::new(kind),
        class: Class::Thought,
        body: Arc::new(body),
        cites: Arc::new(vec![]),
        refs: Arc::new(BTreeSet::new()),
        ignorable: false,
    }
}

/// The fold §2.4 names: a `tool/call` and the `tool/result` that answers it are ONE row. Rendering
/// them as two would show the same tool call twice.
#[test]
fn a_call_and_its_result_fold_into_one_row() {
    let rows = rows_from_steps(&[
        step(
            1,
            "tool/call",
            serde_json::json!({
                "call": "c1", "name": "bash",
                "args": { "cmd": "ls -la" }, "render": "terminal", "step_index": 0
            }),
        ),
        step(
            2,
            "tool/result",
            serde_json::json!({
                "call": "c1", "name": "bash", "outcome": "ok",
                "content": "a\nb", "step_index": 0
            }),
        ),
    ]);
    assert_eq!(rows.len(), 1, "one call + one result = one row: {rows:?}");
    let Row::Tool {
        call,
        name,
        intent,
        args,
        result,
        call_step,
    } = &rows[0]
    else {
        panic!("expected a Tool row, got {:?}", rows[0]);
    };
    assert_eq!(*call, ToolCallId::new("c1"));
    assert_eq!(name, "bash");
    // §9: the surface dispatches on the DECLARED intent, never on the tool's name.
    assert_eq!(*intent, RenderIntent::Terminal);
    assert_eq!(args["cmd"], "ls -la");
    assert_eq!(
        result.as_ref().expect("the result folded in").content,
        "a\nb"
    );
    // The row names the CALL step: the pair is one step id on screen, which is what the pane's
    // invariant ("no step is rendered twice") is stated over.
    assert_eq!(*call_step, StepId::new("s1"));
}

/// The call is out and the answer has not come back. The row exists, and it says so.
#[test]
fn an_unanswered_call_renders_as_pending() {
    let rows = rows_from_steps(&[step(
        1,
        "tool/call",
        serde_json::json!({
            "call": "c9", "name": "read", "args": { "path": "x" },
            "render": "generic", "step_index": 0
        }),
    )]);
    assert_eq!(rows.len(), 1);
    assert!(rows[0].is_pending_tool(), "{:?}", rows[0]);
    assert!(matches!(rows[0], Row::Tool { result: None, .. }));
}

/// The machinery of a wake is not what Andrey reads. `inbox/spliced` in particular is the durable
/// twin of the `mail/delivered` right next to it — drawing both would show one message twice.
#[test]
fn envelope_steps_do_not_produce_rows() {
    let rows = rows_from_steps(&[
        step(1, "step/start", serde_json::json!({ "index": 0 })),
        step(
            2,
            "request/header",
            serde_json::json!({ "prompt_ver": "1", "as_of": 1 }),
        ),
        step(
            3,
            "inbox/spliced",
            serde_json::json!({ "message": "hi", "op": "insert", "target": "next_wake", "wake": true }),
        ),
        step(
            4,
            "step/end",
            serde_json::json!({ "index": 0, "outcome": "ok" }),
        ),
        step(
            5,
            "thought/text",
            serde_json::json!({ "text": "hello", "step_index": 0 }),
        ),
    ]);
    assert_eq!(rows.len(), 1, "only the thought survives: {rows:?}");
    assert!(matches!(&rows[0], Row::Text { text, .. } if text == "hello"));
}

/// Andrey's own messages are not "mail from someone": they are the other half of the conversation
/// and get their own row, while mail from anyone else keeps its sender and its class.
#[test]
fn mail_and_andrey_messages_render_as_their_own_rows() {
    let rows = rows_from_steps(&[
        step(
            1,
            "mail/delivered",
            serde_json::json!({
                "class": "wake", "from": "andrey",
                "subject": "do the thing", "summary": "please do the thing"
            }),
        ),
        step(
            2,
            "mail/delivered",
            serde_json::json!({
                "class": "ordinary", "from": "agent:terra",
                "subject": "fyi", "summary": "the branch is green"
            }),
        ),
    ]);
    assert_eq!(rows.len(), 2);
    let Row::Andrey { text, step } = &rows[0] else {
        panic!("andrey's message must be its own row, got {:?}", rows[0]);
    };
    assert_eq!(text, "please do the thing");
    assert_eq!(*step, StepId::new("s1"));

    let Row::Mail {
        from,
        subject,
        class,
        ..
    } = &rows[1]
    else {
        panic!("expected a Mail row, got {:?}", rows[1]);
    };
    assert_eq!(from, "agent:terra");
    assert_eq!(subject, "fyi");
    assert_eq!(*class, MailClass::Ordinary);
}

/// §3's step-type map is merge-extensible, so a renderer WILL meet types it does not own — and a
/// surface that panicked on one would take the terminal down with it.
#[test]
fn an_unknown_step_type_renders_as_other_and_never_panics() {
    let rows = rows_from_steps(&[
        step(1, "phase9/something-new", serde_json::json!({ "x": 1 })),
        // A KNOWN type whose body is not the declared shape degrades the same way.
        step(2, "thought/text", serde_json::json!({ "not_text": true })),
        step(3, "tool/result", serde_json::json!({ "garbage": true })),
        step(4, "mail/delivered", serde_json::json!(null)),
    ]);
    assert_eq!(rows.len(), 4, "{rows:?}");
    assert!(matches!(&rows[0], Row::Other { kind, .. } if kind.as_str() == "phase9/something-new"));
    assert!(matches!(rows[1], Row::Other { .. }), "{:?}", rows[1]);
    assert!(matches!(rows[2], Row::Other { .. }), "{:?}", rows[2]);
    // A null body is not a shape at all; the mail row degrades to empty halves rather than panicking.
    assert!(matches!(rows[3], Row::Mail { .. } | Row::Other { .. }));

    // Total over the empty list, too.
    assert!(rows_from_steps(&[]).is_empty());
}
