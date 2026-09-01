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
        ..
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
    paint_live(rows, width, agent_name, "")
}

fn paint_live(rows: &[Row], width: u16, agent_name: Option<&str>, live: &str) -> Vec<String> {
    use bough_plugin_tui_focus::{FocusConfig, FocusPane, FocusState, LiveText};
    let cfg = Arc::new(FocusConfig {
        max_rows: 100,
        max_tool_lines: 50,
        page_lines: 10,
        expand_new_tools: false,
        show_reasoning: true,
        context: true,
        context_refresh_ms: 150,
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
    let live = LiveText {
        agent: None,
        text: live.to_string(),
    };
    let (lines, _, _) = pane.lines(
        &state,
        &live,
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
        // No rule on a completed turn (the TUI brief, D5): the speaker labels say it. The
        // fixture's turn completes, so nothing is ruled at all.
        assert_eq!(out.matches("── turn").count(), 0, "@{w}: {out}");
        assert!(!out.contains("turn ended"), "@{w}: {out}");
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
    // No start mark, no about echo, no rule on a completed end: Andrey, text, tool, text.
    assert!(matches!(rows[0], Row::Andrey { .. }), "{rows:?}");
    assert!(
        !rows.iter().any(|r| matches!(r, Row::WakeMark { .. })),
        "{rows:?}"
    );
    assert!(
        !rows.iter().any(|r| matches!(r, Row::About { .. })),
        "{rows:?}"
    );
    assert_eq!(rows.len(), 4, "{rows:?}");
    // The first text after Andrey opens the agent's speech; the text after its own tool does not.
    assert!(opens_speech(&rows, 1));
    assert!(!opens_speech(&rows, 3));
    assert!(
        !opens_speech(&rows, 0),
        "Andrey's row is not the agent speaking"
    );
    // A turn that opens with a tool call wears the label on the tool header (the replay
    // fixture's shape): the speaker is who acts.
    let tool_first = rows_from_steps(&[
        step(
            1,
            "mail/delivered",
            serde_json::json!({ "from": "andrey", "subject": "hi", "summary": "ONE" }),
        ),
        step(
            2,
            "tool/call",
            serde_json::json!({ "call": "c1", "name": "bash", "args": {} }),
        ),
        step(
            3,
            "thought/text",
            serde_json::json!({ "step_index": 1, "text": "done" }),
        ),
    ]);
    assert!(opens_speech(&tool_first, 1) && !opens_speech(&tool_first, 2));
    assert!(
        !opens_speech(&rows, 2),
        "a tool after the agent's words continues the span"
    );
    let painted = paint_as(&tool_first, 80, Some("sol"));
    let at = painted
        .iter()
        .position(|l| l.trim() == "sol:")
        .expect("label");
    assert!(painted[at + 1].contains("bash"), "{painted:?}");

    let out = paint_as(&rows, 80, Some("sol"));
    let labels: Vec<&String> = out.iter().filter(|l| l.trim() == "sol:").collect();
    assert_eq!(labels.len(), 1, "{out:?}");
    assert!(out.iter().any(|l| l.trim() == "andrey:"), "{out:?}");
    assert_eq!(
        out.iter().filter(|l| l.contains("── turn")).count(),
        0,
        "{out:?}"
    );
    // An ending that is news keeps its rule.
    let cut = rows_from_steps(&[
        step(
            1,
            "thought/text",
            serde_json::json!({ "step_index": 0, "text": "half a" }),
        ),
        step(
            2,
            "wake/end",
            serde_json::json!({ "reason": "aborted", "cause": "user" }),
        ),
    ]);
    assert!(matches!(cut[1], Row::WakeMark { .. }), "{cut:?}");
    let painted = paint_as(&cut, 80, Some("sol")).join("\n");
    assert!(painted.contains("── turn interrupted"), "{painted}");
    // With no name known, no label is invented.
    let unnamed = paint(&rows, 80);
    assert!(!unnamed.iter().any(|l| l.trim() == "sol:"), "{unnamed:?}");
    assert!(unnamed.iter().any(|l| l.trim() == "andrey:"), "{unnamed:?}");
}

/// Visual audit F15: the empty transcript says what it is for; a transcript with rows does not.
#[test]
fn the_empty_transcript_says_what_it_is_for() {
    let out = paint_as(&[], 80, Some("sol"));
    let text = out.join("\n");
    assert!(
        text.contains("sol is waiting for your first message"),
        "{text}"
    );
    assert!(
        text.contains("/ for commands") && text.contains("? for help"),
        "{text}"
    );
    // One line at the measure: the hint is the status line's vocabulary, not a paragraph.
    assert_eq!(out.len(), 2, "{out:?}");
    let unnamed = paint(&[], 80).join("\n");
    assert!(unnamed.starts_with("Nothing here yet."), "{unnamed}");
    // Narrow: the hint wraps rather than clipping.
    assert!(paint_as(&[], 30, Some("sol")).len() > 2);
    // With a row, no welcome.
    let rows = rows_from_steps(&[step(
        1,
        "mail/delivered",
        serde_json::json!({ "from": "andrey", "subject": "hi", "summary": "ONE" }),
    )]);
    assert!(!paint_as(&rows, 80, Some("sol"))
        .join("\n")
        .contains("Nothing here yet"));
}

/// F2 on the live tail: the first streamed words wear the name before any durable row exists.
#[test]
fn the_live_tail_opens_with_the_speaker_label() {
    let andrey = rows_from_steps(&[step(
        1,
        "mail/delivered",
        serde_json::json!({ "from": "andrey", "subject": "hi", "summary": "ONE" }),
    )]);
    let out = paint_live(&andrey, 80, Some("sol"), "first streamed words");
    let at = out
        .iter()
        .position(|l| l.trim() == "sol:")
        .expect("label on the live tail");
    assert!(out[at + 1].contains("first streamed words"), "{out:?}");
    // No welcome block while text streams, and no label without a name.
    assert!(!out.join("\n").contains("Nothing here yet"), "{out:?}");
    assert!(!paint_live(&andrey, 80, None, "words")
        .iter()
        .any(|l| l.trim() == "sol:"));
    // After the agent's own durable row the live tail continues the span: one label, not two.
    let agent = rows_from_steps(&[
        step(
            1,
            "thought/text",
            serde_json::json!({ "step_index": 0, "text": "durable" }),
        ),
        step(
            2,
            "tool/call",
            serde_json::json!({ "call": "c1", "name": "bash", "args": {} }),
        ),
    ]);
    let out = paint_live(&agent, 80, Some("sol"), "more");
    assert_eq!(
        out.iter().filter(|l| l.trim() == "sol:").count(),
        1,
        "{out:?}"
    );
}

/// The row-focus fill keeps the row's own colour: a label under the highlight is still the accent.
#[test]
fn the_focus_fill_keeps_the_labels_colour() {
    use bough_plugin_tui_focus::{FocusConfig, FocusPane, FocusState, LiveText, RowFocus};
    let rows = rows_from_steps(&[
        step(
            1,
            "mail/delivered",
            serde_json::json!({ "from": "andrey", "subject": "hi", "summary": "ONE" }),
        ),
        step(
            2,
            "thought/text",
            serde_json::json!({ "step_index": 0, "text": "words" }),
        ),
    ]);
    let cfg = Arc::new(FocusConfig {
        max_rows: 100,
        max_tool_lines: 50,
        page_lines: 10,
        expand_new_tools: false,
        show_reasoning: true,
        context: true,
        context_refresh_ms: 150,
    });
    let pane = FocusPane::new(
        cfg,
        Arc::new(parking_lot::Mutex::new(FocusState::default())),
        Arc::new(parking_lot::Mutex::new(LiveText::default())),
    );
    let theme = bough_plugin_tui_shell::Theme::of(bough_plugin_tui_shell::ThemeName::Light);
    let state = FocusState {
        rows: rows.clone(),
        agent_name: Some("sol".into()),
        row_focus: RowFocus { index: Some(1) },
        keyboard_here: true,
        ..Default::default()
    };
    let (lines, _, _) = pane.lines(&state, &LiveText::default(), 80, &theme);
    let label = lines
        .iter()
        .find(|l| l.spans.iter().any(|s| s.content == "sol:"))
        .expect("the label line");
    assert_eq!(label.style.bg, Some(theme.sel_bg), "{label:?}");
    assert_eq!(label.style.fg, Some(theme.accent), "{label:?}");
}

/// Code mode: a turn that opens with `run(program)` is the agent acting, so it wears the label.
#[test]
fn a_program_row_opens_the_agents_speech() {
    use bough_plugin_tui_focus::rows::{is_agent_row, opens_speech};
    let rows = rows_from_steps(&[
        step(
            1,
            "mail/delivered",
            serde_json::json!({ "from": "andrey", "subject": "hi", "summary": "ONE" }),
        ),
        step(
            2,
            "tool/call",
            serde_json::json!({ "call": "p1", "name": "run", "args": { "program": "1+1" } }),
        ),
    ]);
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert!(is_agent_row(&rows[1]), "{:?}", rows[1]);
    assert!(opens_speech(&rows, 1));
}

/// Round 10: a `draft_*` call row stays visible above its card — a draft attempt that produced
/// no card (the tool unreachable, a bad argument) must never be invisible.
#[test]
fn a_draft_call_stays_visible_above_its_card() {
    let rows = rows_from_steps(&[
        step(
            1,
            "tool/call",
            serde_json::json!({ "call": "c1", "name": "draft_ticket", "args": { "audience": "linear" } }),
        ),
        step(
            2,
            "draft/ticket",
            serde_json::json!({ "draft": "d1", "audience": "linear", "title": "T", "body": "B" }),
        ),
        step(
            3,
            "tool/result",
            serde_json::json!({ "call": "c1", "name": "draft_ticket", "outcome": "ok", "content": "drafted d1", "step_index": 0 }),
        ),
    ]);
    assert!(matches!(rows[0], Row::Tool { .. }), "{rows:?}");
    assert!(
        matches!(&rows[1], Row::Draft { subject, .. } if subject == "T"),
        "{rows:?}"
    );
    // A call with no card behind it is still a row.
    let bare = rows_from_steps(&[step(
        1,
        "tool/call",
        serde_json::json!({ "call": "c1", "name": "draft_ticket", "args": { "audience": "linear" } }),
    )]);
    assert_eq!(bare.len(), 1, "{bare:?}");
}

/// Round 5: while a call is in flight the span ends with `▸ running · <call> · 12s`.
#[test]
fn an_in_flight_call_gets_a_live_line_with_its_clock() {
    use bough_plugin_tui_focus::rows::running_line;
    let rows = rows_from_steps(&[
        step(
            1,
            "thought/text",
            serde_json::json!({ "step_index": 0, "text": "on it" }),
        ),
        step(
            2,
            "tool/call",
            serde_json::json!({ "call": "c1", "name": "bash", "args": { "command": "cargo test -p x" } }),
        ),
    ]);
    let at = match &rows[1] {
        Row::Tool { at, .. } => *at,
        other => panic!("{other:?}"),
    };
    let line = running_line(&rows, at + chrono::Duration::seconds(12)).expect("in flight");
    assert_eq!(line, "▸ running · bash cargo test -p x · 12s");
    assert_eq!(
        running_line(&rows, at + chrono::Duration::seconds(75)).unwrap(),
        "▸ running · bash cargo test -p x · 1m15"
    );
    let done = rows_from_steps(&[
        step(
            1,
            "thought/text",
            serde_json::json!({ "step_index": 0, "text": "on it" }),
        ),
        step(
            2,
            "tool/call",
            serde_json::json!({ "call": "c1", "name": "bash", "args": { "command": "ls" } }),
        ),
        step(
            3,
            "tool/result",
            serde_json::json!({ "call": "c1", "name": "bash", "outcome": "ok", "content": "a", "step_index": 0 }),
        ),
    ]);
    assert_eq!(running_line(&done, at), None);
    assert_eq!(running_line(&[], at), None);
}

/// Round 6: the files a turn changed are said once, at the turn's end, in the added colour.
#[test]
fn a_turn_that_changed_files_says_so_at_its_end() {
    use bough_plugin_tui_focus::rows::changed_files;
    let rows = rows_from_steps(&[
        step(
            1,
            "mail/delivered",
            serde_json::json!({ "from": "andrey", "subject": "hi", "summary": "fix it" }),
        ),
        step(
            2,
            "tool/call",
            serde_json::json!({ "call": "c1", "name": "view", "args": { "path": "a.rs" } }),
        ),
        step(
            3,
            "tool/result",
            serde_json::json!({ "call": "c1", "name": "view", "outcome": "ok", "content": "x", "step_index": 0 }),
        ),
        step(
            4,
            "tool/call",
            serde_json::json!({ "call": "c2", "name": "patch", "args": { "path": "a.rs" } }),
        ),
        step(
            5,
            "tool/result",
            serde_json::json!({ "call": "c2", "name": "patch", "outcome": "ok", "content": "ok", "step_index": 1 }),
        ),
        step(
            6,
            "tool/call",
            serde_json::json!({ "call": "c3", "name": "write_file", "args": { "path": "b.md" } }),
        ),
        step(
            7,
            "tool/result",
            serde_json::json!({ "call": "c3", "name": "write_file", "outcome": "ok", "content": "ok", "step_index": 2 }),
        ),
        step(
            8,
            "thought/text",
            serde_json::json!({ "step_index": 3, "text": "done" }),
        ),
    ]);
    assert_eq!(
        changed_files(&rows),
        vec!["a.rs".to_string(), "b.md".to_string()]
    );
    let out = paint_as(&rows, 80, Some("sol"));
    let last = out.last().unwrap();
    assert_eq!(last.trim(), "✎ changed a.rs · b.md", "{out:?}");
    // A turn that only read changes nothing and says nothing.
    let quiet = paint_as(&rows[..2], 80, Some("sol"));
    assert!(!quiet.iter().any(|l| l.contains("changed")), "{quiet:?}");
}

/// Round 8: a message sent while a turn runs is a `andrey: · queued` row from its splice until
/// its own wake claims it.
#[test]
fn a_message_sent_while_running_is_queued_until_its_wake_claims_it() {
    let spliced = |n: u64, op: &str| {
        step(
            n,
            "inbox/spliced",
            serde_json::json!({
                "message": "m-1", "op": op, "target": "next_wake", "wake": false,
                "payload": { "id": "m-1", "from": { "kind": "andrey", "name": null }, "class": "wake",
                             "subject": "second", "text": "second message while it runs" }
            }),
        )
    };
    let rows = rows_from_steps(&[
        step(
            1,
            "thought/text",
            serde_json::json!({ "step_index": 0, "text": "working" }),
        ),
        spliced(2, "insert"),
    ]);
    assert!(
        matches!(&rows[1], Row::Queued { message, text, .. } if message == "m-1" && text.starts_with("second")),
        "{rows:?}"
    );
    let out = paint_as(&rows, 80, Some("sol"));
    assert!(
        out.iter().any(|l| l.trim() == "andrey: · queued"),
        "{out:?}"
    );
    assert!(
        out.iter()
            .any(|l| l.contains("second message while it runs")),
        "{out:?}"
    );
    // The wake claims it: the queued row is gone (its mail/delivered draws it as Andrey's).
    let rows = rows_from_steps(&[
        step(
            1,
            "thought/text",
            serde_json::json!({ "step_index": 0, "text": "working" }),
        ),
        spliced(2, "insert"),
        spliced(3, "claim"),
    ]);
    assert!(
        !rows.iter().any(|r| matches!(r, Row::Queued { .. })),
        "{rows:?}"
    );
    // A splice from someone other than Andrey is not a queued row of the conversation.
    let other = step(
        4,
        "inbox/spliced",
        serde_json::json!({ "message": "m-2", "op": "insert", "payload": { "from": { "kind": "agent", "name": "terra" }, "text": "x" } }),
    );
    assert!(rows_from_steps(&[other]).is_empty());
}

/// Round 7: a code-mode file handle reads as its path everywhere a call is named.
#[test]
fn a_file_handle_reads_as_its_path() {
    use bough_plugin_tui_focus::rows::{changed_files, unhandle};
    assert_eq!(unhandle("[README.md#B749]"), "README.md");
    assert_eq!(unhandle("src/main.rs"), "src/main.rs");
    let rows = rows_from_steps(&[step(
        1,
        "tool/call",
        serde_json::json!({ "call": "c1", "name": "patch", "args": { "path": "[README.md#B749]" } }),
    )]);
    assert_eq!(changed_files(&rows), vec!["README.md".to_string()]);
}

/// Round 8: failed attempts before the call that succeeded fold under one line; a failure that
/// never succeeded stays inline.
#[test]
fn failed_attempts_fold_under_the_call_that_succeeded() {
    use bough_plugin_tui_focus::rows::{retry_folds, RetryFold};
    let call = |n: u64, ok: bool| {
        let outcome = if ok { "ok" } else { "error" };
        vec![
            step(
                n,
                "tool/call",
                serde_json::json!({ "call": format!("c{n}"), "name": "patch", "args": { "path": "a.rs" } }),
            ),
            step(
                n + 1,
                "tool/result",
                serde_json::json!({ "call": format!("c{n}"), "name": "patch", "outcome": outcome, "content": "x", "step_index": n }),
            ),
        ]
    };
    let mut steps = vec![step(
        1,
        "thought/text",
        serde_json::json!({ "step_index": 0, "text": "trying" }),
    )];
    steps.extend(call(10, false));
    steps.push(step(
        12,
        "thought/text",
        serde_json::json!({ "step_index": 1, "text": "let me fix the path" }),
    ));
    steps.extend(call(20, false));
    steps.extend(call(30, true));
    steps.push(step(
        40,
        "thought/text",
        serde_json::json!({ "step_index": 4, "text": "done" }),
    ));
    let rows = rows_from_steps(&steps);
    // rows: text, fail, text, fail, ok, text
    assert_eq!(
        retry_folds(&rows),
        vec![RetryFold {
            start: 1,
            end: 4,
            attempts: 2
        }]
    );
    let out = paint_as(&rows, 80, Some("sol"));
    let fold = out
        .iter()
        .position(|l| l.contains("2 failed attempts · open"))
        .expect("fold line");
    assert!(
        out[fold + 1].contains("▸ patch a.rs ✓"),
        "the success follows the fold: {out:?}"
    );
    assert!(
        !out.iter().any(|l| l.contains("let me fix the path")),
        "narration folded: {out:?}"
    );
    assert!(out.iter().any(|l| l.contains("done")), "{out:?}");
    // A failed call of ONE tool followed by a success of ANOTHER is not a retry (02-tool-calls:
    // a missing notes/demo.txt read, then a write of notes/demo.rs).
    let mixed = rows_from_steps(&[
        step(
            1,
            "tool/call",
            serde_json::json!({ "call": "r1", "name": "read_file", "args": { "path": "notes/demo.txt" } }),
        ),
        step(
            2,
            "tool/result",
            serde_json::json!({ "call": "r1", "name": "read_file", "outcome": "error", "content": "no such file", "step_index": 0 }),
        ),
        step(
            3,
            "tool/call",
            serde_json::json!({ "call": "w1", "name": "write_file", "args": { "path": "notes/demo.rs" } }),
        ),
        step(
            4,
            "tool/result",
            serde_json::json!({ "call": "w1", "name": "write_file", "outcome": "ok", "content": "ok", "step_index": 1 }),
        ),
    ]);
    assert!(retry_folds(&mixed).is_empty(), "{:?}", retry_folds(&mixed));
    // A run that never succeeded is not folded.
    let mut lone = vec![];
    lone.extend(call(10, false));
    lone.push(step(
        12,
        "thought/text",
        serde_json::json!({ "step_index": 1, "text": "gave up" }),
    ));
    let rows = rows_from_steps(&lone);
    assert!(retry_folds(&rows).is_empty());
    assert!(paint_as(&rows, 80, Some("sol"))
        .iter()
        .any(|l| l.contains("▸ patch a.rs ✗")));
}

/// Round 9: a program with no calls and nothing printed draws nothing; the label opens on the
/// text that follows it.
#[test]
fn an_empty_program_draws_nothing() {
    let rows = rows_from_steps(&[
        step(
            1,
            "mail/delivered",
            serde_json::json!({ "from": "andrey", "subject": "say 1", "summary": "say 1" }),
        ),
        step(
            2,
            "tool/call",
            serde_json::json!({ "call": "p1", "name": "run", "args": { "program": "" } }),
        ),
        step(
            3,
            "tool/result",
            serde_json::json!({ "call": "p1", "name": "run", "outcome": "ok", "content": "", "step_index": 0 }),
        ),
        step(
            4,
            "thought/text",
            serde_json::json!({ "step_index": 1, "text": "Done." }),
        ),
    ]);
    assert!(
        rows.iter()
            .any(bough_plugin_tui_focus::rows::is_empty_program),
        "{rows:?}"
    );
    let out = paint_as(&rows, 80, Some("sol"));
    assert!(!out.iter().any(|l| l.contains("program")), "{out:?}");
    let label = out.iter().position(|l| l.trim() == "sol:").expect("label");
    assert_eq!(out[label + 1].trim(), "Done.", "{out:?}");
}

/// Round 10: what the lane is waiting on — a trailing question once the turn is over and
/// nothing from Andrey followed it.
#[test]
fn owed_flags_a_trailing_question() {
    use bough_plugin_tui_focus::rows::{owed, Owed};
    let rows = rows_from_steps(&[step(
        4,
        "thought/text",
        serde_json::json!({ "step_index": 0, "text": "Which team should the ticket go to?" }),
    )]);
    assert_eq!(owed(&rows, false), Owed { question: true });
    assert_eq!(
        owed(&rows, true),
        Owed { question: false },
        "still running: not yet a question to answer"
    );
    let answered = rows_from_steps(&[
        step(
            4,
            "thought/text",
            serde_json::json!({ "step_index": 0, "text": "Which team?" }),
        ),
        step(
            5,
            "mail/delivered",
            serde_json::json!({ "from": "andrey", "subject": "x", "summary": "TEAM-1" }),
        ),
    ]);
    assert_eq!(owed(&answered, false), Owed::default());
}

/// Drivability (2026-08-31): a turn that showed NOTHING says so on its end mark — a reasoning
/// model can spend the whole output budget thinking, and a bare `max_tokens` after dead air read
/// as the harness failing silently. A turn that showed something keeps the plain mark, and a
/// window that opens mid-turn never accuses.
#[test]
fn an_empty_turn_end_mark_says_nothing_was_shown() {
    use bough_plugin_tui_focus::rows_from_steps;
    let empty_turn = rows_from_steps(&[
        step(1, "wake/start", serde_json::json!({})),
        step(2, "wake/end", serde_json::json!({ "reason": "max_tokens" })),
    ]);
    match empty_turn.last() {
        Some(Row::WakeMark { empty, reason, .. }) => {
            assert!(*empty);
            assert_eq!(reason.as_deref(), Some("max_tokens"));
        }
        other => panic!("expected the end mark, got {other:?}"),
    }
    let showed_something = rows_from_steps(&[
        step(1, "wake/start", serde_json::json!({})),
        step(
            2,
            "thought/text",
            serde_json::json!({ "text": "hi", "step_index": 0 }),
        ),
        step(3, "wake/end", serde_json::json!({ "reason": "max_tokens" })),
    ]);
    match showed_something.last() {
        Some(Row::WakeMark { empty, .. }) => assert!(!*empty),
        other => panic!("expected the end mark, got {other:?}"),
    }
    // No `wake/start` in the window: the mark cannot know, so it never says empty.
    let mid_turn = rows_from_steps(&[step(
        9,
        "wake/end",
        serde_json::json!({ "reason": "max_tokens" }),
    )]);
    match mid_turn.last() {
        Some(Row::WakeMark { empty, .. }) => assert!(!*empty),
        other => panic!("expected the end mark, got {other:?}"),
    }
    // The wording, both ways.
    use bough_plugin_agents::Phase;
    use bough_plugin_tui_focus::rows::turn_mark_words;
    assert_eq!(
        turn_mark_words(&Phase::End, Some("max_tokens"), None, false),
        "turn ended \u{b7} ran out of output tokens"
    );
    assert!(turn_mark_words(&Phase::End, Some("max_tokens"), None, true)
        .contains("before showing anything"));
    assert!(
        turn_mark_words(&Phase::End, Some("error"), None, true).contains("nothing was produced")
    );
}

/// A model that interleaves its reasoning channel with its text (GLM does) must not have its
/// SENTENCE broken by the thought between two flushes: `"Anytime"` rendered as `Any` /
/// `▸ thinking` / `time` on screen (2026-09-01).
#[test]
fn a_thought_between_two_text_flushes_does_not_split_the_reply() {
    let rows = rows_from_steps(&[
        step(
            1,
            "thought/text",
            serde_json::json!({ "text": "Any", "step_index": 0 }),
        ),
        step(
            2,
            "thought/reasoning",
            serde_json::json!({ "text": "considering", "step_index": 0 }),
        ),
        step(
            3,
            "thought/text",
            serde_json::json!({ "text": "time", "step_index": 0 }),
        ),
        step(
            4,
            "thought/reasoning",
            serde_json::json!({ "text": " more", "step_index": 0 }),
        ),
    ]);
    let texts: Vec<String> = rows
        .iter()
        .map(|r| match r {
            Row::Text { text, .. } => format!("text:{text}"),
            Row::Reasoning { text, .. } => format!("think:{text}"),
            other => format!("other:{other:?}"),
        })
        .collect();
    assert_eq!(
        texts,
        vec![
            "text:Anytime".to_string(),
            "think:considering more".to_string()
        ],
        "each channel joins across the other"
    );
}

/// …but a real boundary still closes both groups: a tool call between two flushes is two rows.
#[test]
fn a_tool_call_between_flushes_still_breaks_the_group() {
    let rows = rows_from_steps(&[
        step(
            1,
            "thought/text",
            serde_json::json!({ "text": "before", "step_index": 0 }),
        ),
        step(
            2,
            "tool/call",
            serde_json::json!({
                "call": "c1", "name": "bash",
                "args": { "cmd": "ls" }, "render": "terminal", "step_index": 0
            }),
        ),
        step(
            3,
            "thought/text",
            serde_json::json!({ "text": "after", "step_index": 1 }),
        ),
    ]);
    let texts: Vec<String> = rows
        .iter()
        .filter_map(|r| match r {
            Row::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(texts, vec!["before".to_string(), "after".to_string()]);
}
