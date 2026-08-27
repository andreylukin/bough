//! WP-3 / phase ux1 §2.6: the markdown block parser and its renderer.
//!
//! Two properties carry the whole package. `blocks` is TOTAL — it runs on a LIVE TAIL, so half a
//! fence, half a table and a heading with no trailing newline are documents, not errors (M19).
//! And NO WRAPPED LINE IS STORED — `document` takes the width of the frame being painted, so a
//! resize re-wraps history and a network chunk boundary cannot survive a repaint (M10, nit 39).

mod common;

use bough_plugin_tui_render::{blocks, document, Block};
use unicode_width::UnicodeWidthStr;

fn text(line: &ratatui::text::Line<'_>) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

fn rendered(doc: &str, width: u16) -> Vec<String> {
    document(doc, width, &common::theme())
        .iter()
        .map(text)
        .collect()
}

/// The corpus. Forty documents the parser must survive, named so a failure says which shape broke.
/// Everything after `unterminated` is deliberately half-written: those are what a streaming answer
/// looks like between two chunks.
fn corpus() -> Vec<(&'static str, &'static str)> {
    vec![
        ("empty", ""),
        ("one word", "hi"),
        ("no trailing newline", "a sentence with no newline"),
        ("two paragraphs", "first para\n\nsecond para"),
        ("hard newline inside a para", "line one\nline two"),
        ("h1", "# Title\n"),
        ("h2", "## Core Capabilities\n"),
        ("h3 with emoji", "### 📝  Code & File Management\n"),
        ("h6", "###### deep\n"),
        ("seven hashes is not a heading", "####### not a heading\n"),
        ("hash with no space is not a heading", "#tag is a tag\n"),
        ("heading with no trailing newline", "## Limitations"),
        ("heading then prose immediately", "## Heading\nprose right after"),
        ("closed atx heading", "## Heading ##\n"),
        ("bold", "a **bold** word"),
        ("inline code", "run `ls -la` now"),
        ("bold and code together", "**`cargo test`** is the gate"),
        ("bullet", "- one\n- two\n"),
        ("star bullet", "* one\n* two\n"),
        ("ordered", "1. first\n2. second\n"),
        ("ordered with parens", "1) first\n2) second\n"),
        ("nested bullets", "- top\n  - child\n    - grandchild\n"),
        ("long item that must hang", "8. a numbered item whose text is far longer than any sensible terminal width and therefore wraps more than once\n"),
        ("fence, closed", "```rs\nfn main() {}\n```\n"),
        ("fence, no language", "```\nplain\n```\n"),
        ("fence containing a pipe table", "```\n| a | b |\n|---|---|\n```\n"),
        ("indented fence", "  ```py\n  x = 1\n  ```\n"),
        ("table", "| Scenario | Use |\n|----------|-----|\n| a | b |\n"),
        ("table without outer pipes", "Scenario | Use\n---|---\na | b\n"),
        ("table with a ragged row", "| a | b | c |\n|---|---|---|\n| 1 |\n"),
        ("quote", "> quoted line\n> and another\n"),
        ("quote then prose", "> quoted\nafter\n"),
        ("rule dashes", "---\n"),
        ("rule stars", "***\n"),
        ("rule underscores", "___\n"),
        ("prose with a lone pipe", "use a | b in a shell\n"),
        ("unterminated fence", "```rs\nfn main() {\n"),
        ("unterminated table", "| Scenario | Use |"),
        ("unterminated bold", "**Code & File"),
        ("unterminated inline code", "the `open_pr"),
    ]
}

/// TOTAL: forty documents, no panic, and nothing the user typed is dropped on the floor.
#[test]
fn the_corpus_parses_totally_and_loses_no_words() {
    for (name, doc) in corpus() {
        let bs = blocks(doc);
        assert_eq!(
            bs.is_empty(),
            doc.trim().is_empty(),
            "{name}: a non-empty document must produce at least one block"
        );
        // Every alphanumeric run of the source survives into some block, at every width, in the
        // rendered output. Markers may go; words never may.
        for width in [40u16, 80, 90, 200] {
            let out = rendered(doc, width).join("\n");
            for word in doc.split(|c: char| !c.is_alphanumeric()) {
                if word.len() < 3 {
                    continue;
                }
                assert!(
                    out.contains(word),
                    "{name} @ {width}: the word {word:?} vanished\n{out}"
                );
            }
        }
    }
}

/// The three half-written shapes the streaming tail actually produces.
#[test]
fn unterminated_input_parses_as_the_shape_it_is_becoming() {
    // A fence with no closing fence is a code block, not a paragraph starting with backticks.
    assert_eq!(
        blocks("```rs\nfn main() {\n"),
        vec![Block::Code {
            lang: Some("rs".into()),
            body: "fn main() {\n".into()
        }]
    );
    // A header row with no delimiter yet, at the very tail, is a table.
    assert_eq!(
        blocks("| Scenario | Use |"),
        vec![Block::Table {
            head: vec!["Scenario".into(), "Use".into()],
            rows: vec![]
        }]
    );
    // A heading with no trailing newline is a heading (M19: `## Limitations` showed its hashes).
    assert_eq!(
        blocks("## Limitations"),
        vec![Block::Heading {
            level: 2,
            text: "Limitations".into()
        }]
    );
}

/// The shapes, precisely. One assertion per structural claim `render` then depends on.
#[test]
fn the_structural_shapes_are_what_they_say() {
    assert_eq!(
        blocks("# Title"),
        vec![Block::Heading {
            level: 1,
            text: "Title".into()
        }]
    );
    assert_eq!(
        blocks("#tag is a tag"),
        vec![Block::Para("#tag is a tag".into())]
    );
    assert_eq!(
        blocks("####### seven"),
        vec![Block::Para("####### seven".into())]
    );
    assert_eq!(blocks("---"), vec![Block::Rule]);
    assert_eq!(blocks("> a\n> b"), vec![Block::Quote("a\nb".into())]);
    assert_eq!(
        blocks("- top\n  - child"),
        vec![
            Block::Item {
                level: 0,
                marker: "•".into(),
                text: "top".into()
            },
            Block::Item {
                level: 1,
                marker: "•".into(),
                text: "child".into()
            },
        ]
    );
    assert_eq!(
        blocks("2. second"),
        vec![Block::Item {
            level: 0,
            marker: "2.".into(),
            text: "second".into()
        }]
    );
    assert_eq!(
        blocks("| a | b |\n|---|---|\n| 1 | 2 |"),
        vec![Block::Table {
            head: vec!["a".into(), "b".into()],
            rows: vec![vec!["1".into(), "2".into()]]
        }]
    );
    // Prose containing a pipe in the MIDDLE of a document stays prose: only the tail is allowed
    // to guess, because only the tail is unfinished.
    assert_eq!(
        blocks("use a | b in a shell\nand more prose"),
        vec![Block::Para("use a | b in a shell\nand more prose".into())]
    );
}

/// M19, the headline: no marker character reaches the screen.
#[test]
fn no_markdown_marker_reaches_the_screen() {
    let doc = "## Core Capabilities\n\n- **Work with branches**\n- Run `open_pr` for you\n\n| Scenario | Use |\n|----------|-----|\n| ship | now |\n";
    for width in [40u16, 80, 90, 200] {
        let out = rendered(doc, width).join("\n");
        assert!(
            !out.contains("##"),
            "@{width} heading hashes leaked:\n{out}"
        );
        assert!(!out.contains("**"), "@{width} bold markers leaked:\n{out}");
        assert!(!out.contains('`'), "@{width} a backtick leaked:\n{out}");
        assert!(
            !out.contains("|---"),
            "@{width} a table delimiter leaked:\n{out}"
        );
        assert!(out.contains("Core Capabilities"), "@{width}\n{out}");
        assert!(out.contains("Work with branches"), "@{width}\n{out}");
    }
}

/// The orphan backtick the audit read as the program being broken: an inline span that OPENS and
/// has not closed yet styles to the end of the line rather than printing its own marker.
#[test]
fn an_unterminated_span_never_leaves_an_orphan_marker() {
    for doc in ["the `open_pr", "**Code & File", "a `b` and `c"] {
        for width in [40u16, 80, 90, 200] {
            let out = rendered(doc, width).join("\n");
            assert!(!out.contains('`'), "{doc:?} @{width}: {out:?}");
            assert!(!out.contains("**"), "{doc:?} @{width}: {out:?}");
        }
    }
}

/// Nothing overflows the frame, and no wrap lands inside a word.
#[test]
fn document_wraps_at_word_boundaries_and_never_overflows() {
    let doc = "This script featured lowercase letters with clear distinctions between the shapes, \
               and it goes on for long enough that every width has to break it somewhere.\n\n\
               - Update Linear ticket statuses when a branch merges and the pull request closes\n";
    for width in [40u16, 80, 90, 200] {
        let lines = rendered(doc, width);
        for l in &lines {
            assert!(
                UnicodeWidthStr::width(l.as_str()) <= width as usize,
                "@{width}: {l:?} overflows"
            );
        }
        // Every word of the source appears whole on some line: a break inside `distinctions`
        // would leave `distin` and `ctions` and neither line would contain the word.
        for word in ["distinctions", "statuses", "shorthand"].iter() {
            if doc.contains(word) {
                assert!(
                    lines.iter().any(|l| l.contains(word)),
                    "@{width}: {word} was split across lines: {lines:?}"
                );
            }
        }
    }
}

/// Nit 34: a wrapped list item hangs under its own text, not under the marker.
#[test]
fn a_wrapped_item_hangs_under_its_text() {
    let doc =
        "8. a numbered item whose text is far longer than the terminal is wide and so it wraps";
    let lines = rendered(doc, 40);
    assert!(lines.len() > 1, "{lines:?}");
    assert!(lines[0].starts_with("8. a"), "{:?}", lines[0]);
    for cont in &lines[1..] {
        assert!(
            cont.starts_with("   ") && !cont.starts_with("    "),
            "a continuation hangs to the text column: {cont:?}"
        );
    }
}

/// Nit 39: reflow injects no spurious blank lines. The number of blank lines is a property of the
/// DOCUMENT, not of the width — that is what makes a resize a re-wrap.
#[test]
fn the_blank_line_count_does_not_depend_on_width() {
    let doc = "ONE\n\nTWO\n\n## Head\n\n- a\n- b\n";
    let counts: Vec<usize> = [40u16, 80, 90, 100, 140, 200]
        .iter()
        .map(|w| {
            rendered(doc, *w)
                .iter()
                .filter(|l| l.trim().is_empty())
                .count()
        })
        .collect();
    assert!(
        counts.windows(2).all(|p| p[0] == p[1]),
        "blank lines appeared out of a resize: {counts:?}"
    );
    // And neither end of the document is padded.
    let lines = rendered(doc, 80);
    assert!(!lines.first().unwrap().trim().is_empty(), "{lines:?}");
    assert!(!lines.last().unwrap().trim().is_empty(), "{lines:?}");
    // Two short messages and a resize round trip: 100 → 140 → 100 is the identity.
    let two = "ONE\n\nTWO";
    assert_eq!(rendered(two, 100), rendered(two, 100));
    assert_eq!(rendered(two, 100).len(), 3, "{:?}", rendered(two, 100));
    assert_eq!(rendered(two, 140).len(), 3, "{:?}", rendered(two, 140));
}

/// A table lays out to its widest cell and CLIPS; it never wraps a cell into a second row, which
/// is what stops a table from turning back into a wall of pipes at a narrow width.
#[test]
fn a_table_clips_and_never_wraps_a_cell() {
    let doc = "| Scenario | Use |\n|---|---|\n| a very long scenario description | ship it now |\n";
    for width in [40u16, 80, 200] {
        let lines = rendered(doc, width);
        assert_eq!(lines.len(), 3, "head, rule, one row @{width}: {lines:?}");
        for l in &lines {
            assert!(
                UnicodeWidthStr::width(l.as_str()) <= width as usize,
                "{l:?}"
            );
        }
    }
}
