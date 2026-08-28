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

/// WP-7 / §2.8 + P5-D14: the paragraph join (the field bug), and the claim card fold.
mod tests {
    use super::*;
    use bough_plugin_tui_focus::rows::ClaimState;

    /// A `thought/text` step as the loop's flush writes it: `step_index` is the MODEL step it
    /// belongs to, and the flush boundary inside it is a timer.
    fn text(n: u64, wake: &str, index: u32, s: &str) -> Step {
        let mut st = step(
            n,
            "thought/text",
            serde_json::json!({ "text": s, "step_index": index }),
        );
        st.wake = WakeId::new(wake);
        st
    }

    fn reasoning(n: u64, index: u32, s: &str) -> Step {
        step(
            n,
            "thought/reasoning",
            serde_json::json!({ "text": s, "step_index": index }),
        )
    }

    fn text_of(row: &Row) -> &str {
        match row {
            Row::Text { text, .. } | Row::Reasoning { text, .. } => text,
            other => panic!("expected a text row, got {other:?}"),
        }
    }

    /// THE FIELD BUG. Two durable steps of one model step rendered as two lines
    /// (`"I'll run that"` / `" shell command for you."`); they are one answer and they are one row.
    #[test]
    fn two_text_steps_of_one_step_index_join_into_one_row() {
        let rows = rows_from_steps(&[
            text(1, "w1", 0, "I'll run that"),
            text(2, "w1", 0, " shell command for you."),
        ]);
        assert_eq!(rows.len(), 1, "one model step is one row: {rows:?}");
        assert_eq!(text_of(&rows[0]), "I'll run that shell command for you.");
    }

    /// RAW concatenation. The chunks are a split of one stream, so anything inserted between them
    /// — a space, a newline — is text the model never wrote, and a space is exactly what produced
    /// the doubled space in the field report.
    #[test]
    fn the_join_is_raw_concatenation_with_no_separator() {
        let rows = rows_from_steps(&[
            text(1, "w1", 0, "half"),
            text(2, "w1", 0, "way"),
            text(3, "w1", 0, " there"),
        ]);
        assert_eq!(text_of(&rows[0]), "halfway there");
    }

    /// A tool call between two texts is a real break: the model spoke, acted, and spoke again,
    /// and joining across the call would put the answer before the tool after it.
    #[test]
    fn a_tool_call_between_two_texts_breaks_the_group() {
        let rows = rows_from_steps(&[
            text(1, "w1", 0, "let me look"),
            step(
                2,
                "tool/call",
                serde_json::json!({
                    "call": "c1", "name": "shell", "args": {}, "render": "generic"
                }),
            ),
            text(3, "w1", 0, "found it"),
        ]);
        assert_eq!(rows.len(), 3, "{rows:?}");
        assert_eq!(text_of(&rows[0]), "let me look");
        assert!(matches!(rows[1], Row::Tool { .. }));
        assert_eq!(text_of(&rows[2]), "found it");
    }

    /// A new model step is a new answer, whatever the timing looked like.
    #[test]
    fn a_new_step_index_breaks_the_group() {
        let rows = rows_from_steps(&[text(1, "w1", 0, "first"), text(2, "w1", 1, "second")]);
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert_eq!(text_of(&rows[0]), "first");
        assert_eq!(text_of(&rows[1]), "second");
    }

    /// Two wakes can each carry a step index 0. They are different turns and never one paragraph.
    #[test]
    fn a_new_wake_breaks_the_group() {
        let rows = rows_from_steps(&[text(1, "w1", 0, "yesterday"), text(2, "w2", 0, "today")]);
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert_eq!(text_of(&rows[0]), "yesterday");
    }

    /// The anchor is the FIRST step of the group — that is where a search hit, a citation and a
    /// flash point — and every folded step is listed, so nothing becomes unaddressable by joining.
    #[test]
    fn the_joined_row_anchors_on_the_first_step_and_lists_every_part() {
        let rows = rows_from_steps(&[
            text(1, "w1", 0, "a"),
            text(2, "w1", 0, "b"),
            text(3, "w1", 0, "c"),
        ]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].step(), &StepId::new("s1"));
        assert_eq!(
            rows[0].parts(),
            vec![StepId::new("s1"), StepId::new("s2"), StepId::new("s3")]
        );
        // An unjoined row still names itself, so `parts()` is total.
        let one = rows_from_steps(&[text(9, "w1", 0, "solo")]);
        assert_eq!(one[0].parts(), vec![StepId::new("s9")]);
    }

    /// Reasoning is flushed on the same timer and joins on the same rule — and it never joins
    /// with text: the two are drawn differently, and merging them would present a private thought
    /// as part of the answer.
    #[test]
    fn two_reasoning_steps_join_on_the_same_rule() {
        let rows = rows_from_steps(&[reasoning(1, 0, "think"), reasoning(2, 0, "ing")]);
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert!(matches!(rows[0], Row::Reasoning { .. }));
        assert_eq!(text_of(&rows[0]), "thinking");

        let mixed = rows_from_steps(&[reasoning(1, 0, "hmm"), text(2, "w1", 0, "answer")]);
        assert_eq!(mixed.len(), 2, "reasoning and text never join: {mixed:?}");
    }

    /// §16 at the surface: a claim is a PROPOSAL, and it renders as a card Andrey can decide.
    #[test]
    fn a_claim_proposed_step_renders_as_a_card() {
        let rows = rows_from_steps(&[step(
            1,
            "claim/proposed",
            serde_json::json!({
                "claim": "c1",
                "kind": "requirement",
                "title": "the leader drafts requirements",
                "body": "Andrey's words become a claim.",
            }),
        )]);
        let Row::Claim {
            claim,
            kind,
            title,
            body,
            state,
            ..
        } = &rows[0]
        else {
            panic!("expected a claim card, got {rows:?}");
        };
        assert_eq!(claim, "c1");
        assert_eq!(kind, "requirement");
        assert_eq!(title, "the leader drafts requirements");
        assert_eq!(body, "Andrey's words become a claim.");
        assert_eq!(*state, ClaimState::Open, "undecided until Andrey decides");
    }

    /// The decision folds INTO the card (P3-D11: by step-type name, no dependency on `claims`),
    /// so an accepted claim is one row that says so rather than two rows to reconcile by eye.
    #[test]
    fn an_accepted_claim_card_shows_its_state() {
        let rows = rows_from_steps(&[
            step(
                1,
                "claim/proposed",
                serde_json::json!({ "claim": "c1", "kind": "lane", "title": "t", "body": "b" }),
            ),
            step(
                2,
                "claim/accepted",
                serde_json::json!({ "claim": "c1", "proposal": "s1", "edited": true }),
            ),
        ]);
        assert_eq!(rows.len(), 1, "the decision folds in: {rows:?}");
        let Row::Claim { state, .. } = &rows[0] else {
            panic!("{rows:?}");
        };
        assert_eq!(*state, ClaimState::Accepted { edited: true });
        assert_eq!(state.word(), "accepted (edited)");
        assert!(!state.is_open(), "a decided card offers no buttons");
    }

    /// A rejection without its reason would leave the agent's proposal looking arbitrary, so the
    /// reason travels with the state.
    #[test]
    fn a_rejected_claim_card_shows_its_reason() {
        let rows = rows_from_steps(&[
            step(
                1,
                "claim/proposed",
                serde_json::json!({ "claim": "c1", "kind": "lane", "title": "t", "body": "b" }),
            ),
            step(
                2,
                "claim/rejected",
                serde_json::json!({
                    "claim": "c1", "proposal": "s1", "reason": "that lane already exists"
                }),
            ),
        ]);
        assert_eq!(rows.len(), 1);
        let Row::Claim { state, .. } = &rows[0] else {
            panic!("{rows:?}");
        };
        assert_eq!(
            *state,
            ClaimState::Rejected {
                reason: "that lane already exists".to_string()
            }
        );

        // A decision whose proposal paged out is still shown — the decision is the news.
        let orphan = rows_from_steps(&[step(
            3,
            "claim/rejected",
            serde_json::json!({ "claim": "c9", "proposal": "s0", "reason": "no" }),
        )]);
        assert!(matches!(orphan[0], Row::Other { .. }), "{orphan:?}");
    }
}

// ---------------------------------------------------------------------------
// WP-3 / phase ux1 §2.6: the golden — history re-wraps, it does not re-space (M13, nit 39)
// ---------------------------------------------------------------------------

/// A trajectory with one of everything the transcript can draw.
fn trajectory() -> Vec<Step> {
    vec![
        step(1, "wake/start", serde_json::json!({ "urgency": "now" })),
        step(
            2,
            "mail/delivered",
            serde_json::json!({ "from": "andrey", "subject": "hi", "summary": "ONE" }),
        ),
        step(
            3,
            "thought/text",
            serde_json::json!({ "step_index": 0, "text": "## Core Capabilities\n\n**Code & File" }),
        ),
        step(
            4,
            "thought/text",
            serde_json::json!({ "step_index": 0, "text": " Operations:**\n\n- I'll create a file named notes.txt for you\n" }),
        ),
        step(
            5,
            "tool/call",
            serde_json::json!({
                "call": "c1", "name": "write_file", "render": "diff",
                "args": { "path": "notes.txt", "content": "hello\n" }
            }),
        ),
        step(
            6,
            "tool/result",
            serde_json::json!({
                "call": "c1", "name": "write_file", "outcome": "ok", "content": "wrote 6 bytes"
            }),
        ),
        step(
            7,
            "thought/text",
            serde_json::json!({ "step_index": 1, "text": "TWO" }),
        ),
        step(8, "wake/end", serde_json::json!({ "reason": "completed" })),
    ]
}

fn paint(rows: &[Row], width: u16) -> Vec<String> {
    paint_as(rows, width, None)
}

fn paint_as(rows: &[Row], width: u16, agent_name: Option<&str>) -> Vec<String> {
    use bough_plugin_tui_focus::{FocusConfig, FocusPane, FocusState, LiveText};
    let cfg = Arc::new(FocusConfig {
        max_rows: 100,
        max_tool_lines: 50,
        page_lines: 10,
        expand_new_tools: false,
        show_reasoning: true,
    });
    let pane = FocusPane::new(
        cfg,
        Arc::new(parking_lot::Mutex::new(FocusState::default())),
        Arc::new(parking_lot::Mutex::new(LiveText::default())),
    );
    let state = FocusState {
        rows: rows.to_vec(),
        agent_name: agent_name.map(str::to_string),
        ..Default::default()
    };
    let (lines, _, _) = pane.lines(
        &state,
        &LiveText::default(),
        width,
        &bough_plugin_tui_shell::Theme::of(bough_plugin_tui_shell::ThemeName::Dark),
    );
    lines
        .iter()
        .map(|l| l.spans.iter().map(|s| s.content.to_string()).collect())
        .collect()
}

/// The golden: the SAME trajectory renders identically before and after a width change.
///
/// This is the whole no-stored-wrap rule as one assertion. Nothing about the previous paint is
/// remembered, so 80 → 200 → 80 is the identity — and the audit's `resize 80 24` / `resize 200 50`
/// walk, which found history frozen at its first width, cannot recur (M13).
#[test]
fn the_same_trajectory_renders_identically_before_and_after_a_width_change() {
    let rows = rows_from_steps(&trajectory());
    let at80 = paint(&rows, 80);
    let at200 = paint(&rows, 200);
    let back = paint(&rows, 80);
    assert_eq!(at80, back, "80 → 200 → 80 is the identity");
    // Nothing in this fixture is longer than 80 columns, so 80 and 200 paint the SAME lines —
    // the old inequality held only because the tool header padded itself to the width (gone,
    // visual audit F7). A width the bullet line cannot fit is what genuinely changes the wrap.
    let at30 = paint(&rows, 30);
    assert_ne!(at80, at30, "and a narrow width genuinely changed the wrap");
    assert_eq!(
        paint(&rows, 80),
        at80,
        "…and back again is still the identity"
    );
    let _ = at200;

    // Nit 39: the number of blank lines is a property of the document, not of the width. A
    // resize re-wraps; it never injects spacing.
    let blanks = |v: &Vec<String>| v.iter().filter(|l| l.trim().is_empty()).count();
    for w in [80u16, 100, 140, 200] {
        assert_eq!(
            blanks(&paint(&rows, w)),
            blanks(&at80),
            "a resize to {w} injected or dropped blank lines"
        );
    }
}

/// Nit 37 and M19 on the real trajectory: turn/message vocabulary, and no marker on screen.
#[test]
fn the_painted_transcript_says_turn_and_shows_no_markdown_markers() {
    let rows = rows_from_steps(&trajectory());
    for w in [80u16, 200] {
        let out = paint(&rows, w).join("\n");
        // ONE rule per turn (visual audit F2): the end. The start is the speaker label.
        assert_eq!(out.matches("── turn").count(), 1, "@{w}: {out}");
        assert!(out.contains("── turn ended · completed"), "@{w}: {out}");
        assert!(
            !out.contains("wake"),
            "@{w}: internal vocabulary on screen:\n{out}"
        );
        assert!(!out.contains("**"), "@{w}: {out}");
        assert!(!out.contains("## "), "@{w}: {out}");
        assert!(out.contains("Core Capabilities"), "@{w}: {out}");
        // The chunk split through the bold pair is gone: one phrase, one style run.
        assert!(out.contains("Code & File Operations:"), "@{w}: {out}");
    }
}

/// Visual audit F3: the ledger's bookkeeping about itself is not a row; an unknown type still is.
#[test]
fn machinery_steps_are_not_rows_but_unknown_types_still_are() {
    let rows = rows_from_steps(&[
        step(1, "agent/routing", serde_json::json!({})),
        step(2, "usage/round", serde_json::json!({ "cost_usd": 0.01 })),
        step(
            3,
            "thought/text",
            serde_json::json!({ "text": "hello", "step_index": 0 }),
        ),
        step(4, "rollup/sealed", serde_json::json!({})),
        step(5, "phase9/something-new", serde_json::json!({})),
    ]);
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert!(matches!(rows[0], Row::Text { .. }));
    assert!(matches!(&rows[1], Row::Other { kind, .. } if kind.as_str() == "phase9/something-new"));
    for kind in bough_plugin_tui_focus::rows::MACHINERY {
        assert!(
            rows_from_steps(&[step(1, kind, serde_json::json!({}))]).is_empty(),
            "{kind}"
        );
    }
}

/// Visual audit F2: the turn is a label and a rule, not three chrome lines; the agent's words
/// wear its name the way Andrey's wear his; the about-line is the rail's, not the transcript's.
#[test]
fn a_turn_is_a_speaker_label_and_one_rule() {
    use bough_plugin_tui_focus::rows::opens_speech;
    let rows = rows_from_steps(&[
        step(1, "wake/start", serde_json::json!({ "urgency": "now" })),
        step(
            2,
            "mail/delivered",
            serde_json::json!({ "from": "andrey", "subject": "hi", "summary": "ONE" }),
        ),
        step(
            3,
            "thought/text",
            serde_json::json!({ "step_index": 0, "text": "first words" }),
        ),
        step(
            4,
            "tool/call",
            serde_json::json!({ "call": "c1", "name": "bash", "args": {} }),
        ),
        step(
            5,
            "thought/text",
            serde_json::json!({ "step_index": 2, "text": "after the tool" }),
        ),
        step(
            6,
            "about/line",
            serde_json::json!({ "state": "doing x", "intent": "y" }),
        ),
        step(7, "wake/end", serde_json::json!({ "reason": "completed" })),
    ]);
    // No start mark, no about echo: Andrey, text, tool, text, end.
    assert!(matches!(rows[0], Row::Andrey { .. }), "{rows:?}");
    assert!(
        matches!(rows[rows.len() - 1], Row::WakeMark { .. }),
        "{rows:?}"
    );
    assert!(
        !rows.iter().any(|r| matches!(r, Row::About { .. })),
        "{rows:?}"
    );
    assert_eq!(rows.len(), 5, "{rows:?}");
    // The first text after Andrey opens the agent's speech; the text after its own tool does not.
    assert!(opens_speech(&rows, 1));
    assert!(!opens_speech(&rows, 3));
    assert!(
        !opens_speech(&rows, 0),
        "Andrey's row is not the agent speaking"
    );

    let out = paint_as(&rows, 80, Some("sol"));
    let labels: Vec<&String> = out.iter().filter(|l| l.trim() == "sol:").collect();
    assert_eq!(labels.len(), 1, "{out:?}");
    assert!(out.iter().any(|l| l.trim() == "andrey:"), "{out:?}");
    assert_eq!(
        out.iter().filter(|l| l.contains("── turn")).count(),
        1,
        "{out:?}"
    );
    // With no name known, no label is invented.
    let unnamed = paint(&rows, 80);
    assert!(!unnamed.iter().any(|l| l.trim() == "sol:"), "{unnamed:?}");
    assert!(unnamed.iter().any(|l| l.trim() == "andrey:"), "{unnamed:?}");
}
