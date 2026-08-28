//! §7/V4: the pane shows Andrey what his agents wrote and DID NOT SEND — and offers him no way to
//! send it. Asserted on the key hints AND on a real rendered buffer, because the absence is the
//! whole point.

use bough_plugin_drafts::{DraftId, DraftKind, DraftRow};
use bough_plugin_ledger::{AgentName, StepId};
use bough_plugin_tui_drafts::{
    copy_text, header, lines, paint, selected_line, PaneState, KEY_HINTS,
};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

fn row(id: &str, kind: DraftKind, subject: &str, body: &str) -> DraftRow {
    DraftRow {
        id: DraftId::new(id),
        step: StepId::new(format!("step-{id}")),
        kind,
        agent: AgentName::new("sol"),
        audience: "slack:#eng".into(),
        subject: subject.into(),
        body: body.into(),
        refs: Vec::new(),
        at: chrono::Utc::now(),
    }
}

fn state(rows: Vec<DraftRow>, expanded: bool) -> PaneState {
    PaneState {
        rows,
        selected: 0,
        expanded,
    }
}

/// Render into a real buffer and read it back as text, one string per line.
fn rendered(state: &PaneState, show_body_lines: usize) -> Vec<String> {
    let painted = lines(state, show_body_lines);
    let area = Rect::new(0, 0, 90, 12);
    let mut buf = Buffer::empty(area);
    paint(&painted, area, &mut buf, selected_line(state));
    (0..area.height)
        .map(|y| {
            (0..area.width)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect()
}

/// The subject on the summary line, the body when expanded — off a rendered buffer, not off the
/// pure line list alone.
#[tokio::test]
async fn the_pane_renders_a_drafts_subject_and_body() {
    let st = state(
        vec![row(
            "d1",
            DraftKind::Message,
            "deploy is green",
            "the pipeline went green at 14:02.\nsecond line",
        )],
        true,
    );
    let screen = rendered(&st, 4);
    let text = screen.join("\n");
    assert!(text.contains("deploy is green"), "no subject:\n{text}");
    assert!(
        text.contains("the pipeline went green at 14:02."),
        "no body:\n{text}"
    );
    assert!(text.contains("slack:#eng"), "no audience:\n{text}");
    assert!(
        text.contains("NOT sent"),
        "the caption is the point:\n{text}"
    );
}

/// The collapsed pane shows the summary and NOT the body: `enter` is what expands.
#[tokio::test]
async fn a_collapsed_draft_shows_no_body() {
    let st = state(
        vec![row(
            "d1",
            DraftKind::Message,
            "deploy is green",
            "secret body",
        )],
        false,
    );
    let text = rendered(&st, 4).join("\n");
    assert!(text.contains("deploy is green"));
    assert!(!text.contains("secret body"), "{text}");
}

/// NO SEND AFFORDANCE: not in the hints, and not anywhere on the screen.
#[tokio::test]
async fn the_pane_offers_no_send_affordance() {
    // 1. the hints.
    let hints: Vec<&str> = KEY_HINTS.iter().map(|(_, what)| *what).collect();
    assert_eq!(hints, vec!["select", "expand", "copy"]);
    bough_plugin_tui_drafts::invariant::check_hints(KEY_HINTS).expect("no hint sends");

    // 2. the rendered buffer. The only occurrence of "sent" is the caption saying there was none.
    let st = state(
        vec![
            row("d1", DraftKind::Message, "deploy is green", "body one"),
            row("d2", DraftKind::Ticket, "flaky test", "body two"),
        ],
        true,
    );
    let screen = rendered(&st, 4);
    // Line 0 is the CAPTION, which is prose about who sends (nobody here) rather than an
    // affordance; every other line is a draft's own text and must not read as one.
    for line in screen.iter().skip(1) {
        let lowered = line.to_lowercase();
        for word in ["send", "post", "deliver", "publish", "submit"] {
            assert!(
                !lowered.contains(word),
                "the pane painted `{word}` on `{line}`"
            );
        }
    }
    assert!(screen.join("\n").contains("NOT sent"));
}

/// `y` copies the draft to the terminal's clipboard: the text is the draft, addressed, and it
/// goes nowhere else.
#[tokio::test]
async fn copy_text_is_the_draft_and_names_its_audience() {
    let r = row("d1", DraftKind::Message, "deploy is green", "the body");
    let text = copy_text(&r);
    assert!(text.starts_with("to: slack:#eng"), "{text}");
    assert!(text.contains("subject: deploy is green"), "{text}");
    assert!(text.ends_with("the body"), "{text}");
}

/// An empty pane says so, rather than rendering a blank block a reader has to interpret.
#[tokio::test]
async fn an_empty_pane_says_nothing_was_written() {
    assert!(header(0).contains("nothing written yet"));
    let text = rendered(&state(Vec::new(), false), 4).join("\n");
    assert!(text.contains("nothing written yet"), "{text}");
}

/// The selection marks the row it is on, and the marker moves with it.
#[tokio::test]
async fn the_selection_marks_one_row() {
    let mut st = state(
        vec![
            row("d1", DraftKind::Message, "first", "b1"),
            row("d2", DraftKind::Message, "second", "b2"),
        ],
        false,
    );
    assert_eq!(selected_line(&st), Some(1));
    st.selected = 1;
    assert_eq!(selected_line(&st), Some(2));
    let painted = lines(&st, 4);
    assert!(painted[2].starts_with('>'), "{painted:?}");
    assert!(!painted[1].starts_with('>'), "{painted:?}");
}
