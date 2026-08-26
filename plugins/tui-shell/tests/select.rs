//! §11's selection and copy: the text comes out of the shell's LAST RENDERED BUFFER (P3-D6), the
//! copy path is OSC52 (P3-D7), and a clipboard that is not there is a NOTICE — never an error the
//! caller has to handle.

mod common;

use bough_plugin_tui_shell::clip::{copy, write_osc52, CopyOutcome};
use bough_plugin_tui_shell::{run, text_from_buffer, Selection, Slot, SlotSize};
use common::{add_pane, config, shell};
use ratatui::layout::Rect;

#[tokio::test]
async fn a_drag_rect_extracts_the_rendered_cells_with_trailing_space_trimmed() {
    let (ctx, tui) = shell();
    // The recorder paints `focus 80x23` at the top-left of its slot and nothing else, so the rest
    // of every row is spaces — which is exactly what the trim rule is about.
    add_pane(&ctx, &tui, "focus", Slot::Main, 0, SlotSize::Fill(1)).await;
    run::draw(&tui);

    let selection = Selection {
        anchor: (0, 0),
        head: (39, 1),
    };
    let text = text_from_buffer(&tui.last_frame(), selection.rect());

    assert_eq!(
        text, "focus 80x23\n",
        "the first row is the painted text with its padding trimmed, the second is empty"
    );
}

#[tokio::test]
async fn copy_writes_an_osc52_sequence_carrying_the_selection() {
    let mut out: Vec<u8> = Vec::new();
    let outcome = copy("rebase the loop", &config(), &mut out).await;

    let written = String::from_utf8(out).expect("the sequence is utf-8");
    // OSC 52 ; c ; <base64> BEL/ST — the payload is what was selected, not a description of it.
    assert!(written.starts_with("\x1b]52;c;"), "{written:?}");
    use base64::Engine;
    let payload = written
        .trim_start_matches("\x1b]52;c;")
        .trim_end_matches('\x07')
        .trim_end_matches("\x1b\\");
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(payload)
        .expect("the payload is base64");
    assert_eq!(String::from_utf8(decoded).unwrap(), "rebase the loop");
    // `clipboard: false` in the test config, so OSC52 is the whole story.
    assert_eq!(outcome, CopyOutcome::Osc52Only);
}

#[tokio::test]
async fn a_clipboard_failure_is_a_notice_not_an_error() {
    // `osc52: false` AND a local clipboard that cannot open: the worst case still RETURNS, with a
    // notice the surface can render. There is no error type for the caller to handle (P3-D7).
    let mut cfg = config();
    cfg.osc52 = false;
    cfg.clipboard = true;
    let mut out: Vec<u8> = Vec::new();
    let outcome = copy("something", &cfg, &mut out).await;

    match &outcome {
        // A machine with a display server really does copy; a headless CI box does not. Both are
        // outcomes, and neither is an error.
        // A machine with a display server: the local copy worked and OSC52 was off, which the
        // shell still SAYS — a notice, not an error.
        CopyOutcome::LocalOnly => {
            assert!(outcome.notice().is_some(), "a partial copy explains itself")
        }
        CopyOutcome::Nothing(why) => {
            assert!(!why.is_empty(), "a failure explains itself");
            assert!(outcome.notice().is_some(), "and it renders as a notice");
        }
        other => panic!("osc52 was off: {other:?}"),
    }
    assert!(out.is_empty(), "nothing was written to the terminal");

    // The name's claim, proven whichever arm this box takes: a failure is a NOTICE, never an
    // error. `copy` returns no `Result`, so there is nothing for a caller to handle (P3-D7), and
    // the failure outcome renders as text.
    let failed = CopyOutcome::Nothing("no clipboard on this box".to_string());
    let notice = failed
        .notice()
        .expect("a failed copy explains itself as a notice");
    assert!(
        notice.contains("no clipboard on this box"),
        "the notice carries the reason: {notice}"
    );
}

#[tokio::test]
async fn an_empty_selection_copies_nothing_and_says_so() {
    let mut out: Vec<u8> = Vec::new();
    let outcome = copy("", &config(), &mut out).await;
    assert!(matches!(outcome, CopyOutcome::Nothing(_)));
    assert!(out.is_empty());
}

#[tokio::test]
async fn the_osc52_writer_is_the_same_one_the_shell_uses() {
    let mut a: Vec<u8> = Vec::new();
    write_osc52("x", &mut a).expect("a Vec never fails");
    let mut b: Vec<u8> = Vec::new();
    copy("x", &config(), &mut b).await;
    assert_eq!(a, b);
}

#[tokio::test]
async fn a_selection_past_the_edge_of_the_frame_is_clipped_not_padded() {
    let (ctx, tui) = shell();
    add_pane(&ctx, &tui, "focus", Slot::Main, 0, SlotSize::Fill(1)).await;
    run::draw(&tui);
    let text = text_from_buffer(&tui.last_frame(), Rect::new(70, 0, 40, 1));
    assert_eq!(text, "", "ten cells of nothing, not thirty spaces");
}
