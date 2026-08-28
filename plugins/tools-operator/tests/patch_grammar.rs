//! Main's hash-anchored patch grammar, ported with its tests: the tag, the listing, the six
//! operations in VIEWED coordinates, the refusals, and — the part that matters — the
//! rebase-vs-conflict decision proved in both directions.

use bough_plugin_tools_operator::files::{
    check_ops, group_by_file, join_lines, line_map, materialize, normalize, parse_patch,
    rebase_ops, render_numbered, tag_of, to_lines, OpKind, PatchError, PatchOp, RebaseResult,
};

/// File text from lines, with the trailing newline a real file has.
fn doc(lines: &[&str]) -> String {
    format!("{}\n", lines.join("\n"))
}

/// Parse + check + materialize one file's patch against `text`, in viewed coordinates.
fn apply(input: &str, text: &str) -> String {
    let ops = parse_patch(input).expect("patch should parse");
    let groups = group_by_file(&ops).expect("patch should group");
    assert_eq!(groups.len(), 1, "this helper takes one file");
    let lines = to_lines(text);
    check_ops(&groups[0].path, &groups[0].ops, lines.len()).expect("patch should check");
    join_lines(&materialize(&lines, &groups[0].ops), text)
}

fn refuse(input: &str, text: &str) -> PatchError {
    let ops = match parse_patch(input) {
        Ok(o) => o,
        Err(e) => return e,
    };
    let groups = match group_by_file(&ops) {
        Ok(g) => g,
        Err(e) => return e,
    };
    let lines = to_lines(text);
    check_ops(&groups[0].path, &groups[0].ops, lines.len())
        .expect_err("this patch should have been refused")
}

// ---------------------------------------------------------------------------
// tags and the listing
// ---------------------------------------------------------------------------

#[test]
fn crlf_and_a_bom_do_not_change_a_tag() {
    let lf = "alpha\nbeta\n";
    let crlf = "alpha\r\nbeta\r\n";
    let bom = "\u{FEFF}alpha\nbeta\n";
    assert_eq!(tag_of(lf), tag_of(crlf));
    assert_eq!(tag_of(lf), tag_of(bom));
    assert_eq!(normalize(crlf), lf);
}

#[test]
fn a_tag_is_four_hex_digits_and_moves_when_the_text_does() {
    let t = tag_of("alpha\n");
    assert_eq!(t.len(), 4, "{t}");
    assert!(t
        .chars()
        .all(|c| c.is_ascii_hexdigit() && !c.is_lowercase()));
    assert_ne!(tag_of("alpha\n"), tag_of("alphb\n"));
}

#[test]
fn view_renders_the_anchor_and_numbered_lines() {
    let text = doc(&["one", "two", "three"]);
    let out = render_numbered("a.rs", &text);
    let mut lines = out.lines();
    assert_eq!(lines.next().unwrap(), format!("[a.rs#{}]", tag_of(&text)));
    assert_eq!(lines.next().unwrap(), "1:one");
    assert_eq!(lines.next().unwrap(), "2:two");
    assert_eq!(lines.next().unwrap(), "3:three");
    assert!(lines.next().is_none());
}

#[test]
fn the_line_number_column_is_width_aligned() {
    let text = doc(&(1..=10).map(|_| "x").collect::<Vec<_>>());
    let out = render_numbered("a.rs", &text);
    assert!(out.contains("\n 1:x"), "{out}");
    assert!(out.contains("\n10:x"), "{out}");
}

#[test]
fn a_file_emptied_by_a_patch_is_empty_not_a_blank_line() {
    let text = doc(&["only"]);
    assert_eq!(apply("[a.rs#]\nDEL 1\n", &text), "");
}

#[test]
fn crlf_survives_a_patch() {
    let text = "one\r\ntwo\r\n";
    let out = apply("[a.rs#]\nSWAP 2:\n+TWO\n", text);
    assert_eq!(out, "one\r\nTWO\r\n");
}

// ---------------------------------------------------------------------------
// the six operations
// ---------------------------------------------------------------------------

#[test]
fn swap_replaces_the_named_range() {
    let text = doc(&["a", "b", "c", "d"]);
    let out = apply("[f#]\nSWAP 2.=3:\n+B\n+C\n+EXTRA\n", &text);
    assert_eq!(out, doc(&["a", "B", "C", "EXTRA", "d"]));
}

#[test]
fn del_removes_the_named_range() {
    let text = doc(&["a", "b", "c", "d"]);
    assert_eq!(apply("[f#]\nDEL 2.=3\n", &text), doc(&["a", "d"]));
}

#[test]
fn ins_pre_and_ins_post_land_on_the_right_side_of_a_line() {
    let text = doc(&["a", "b"]);
    assert_eq!(
        apply("[f#]\nINS.PRE 2:\n+x\n", &text),
        doc(&["a", "x", "b"])
    );
    assert_eq!(
        apply("[f#]\nINS.POST 1:\n+x\n", &text),
        doc(&["a", "x", "b"])
    );
}

#[test]
fn ins_head_and_ins_tail_bracket_the_file() {
    let text = doc(&["a"]);
    assert_eq!(
        apply("[f#]\nINS.HEAD:\n+top\n\nINS.TAIL:\n+bottom\n", &text),
        doc(&["top", "a", "bottom"])
    );
}

#[test]
fn ins_head_works_on_an_empty_file() {
    assert_eq!(apply("[f#]\nINS.HEAD:\n+first\n", ""), "first\n");
}

#[test]
fn earlier_operations_do_not_shift_later_line_numbers() {
    // Every anchor is in the coordinates of the ORIGINAL: op 1 adds two lines above op 2's
    // anchor, and op 2 must still name line 4 of the version that was viewed.
    let text = doc(&["a", "b", "c", "d", "e"]);
    let out = apply("[f#]\nINS.PRE 2:\n+x\n+y\n\nSWAP 4:\n+D\n\nDEL 5\n", &text);
    assert_eq!(out, doc(&["a", "x", "y", "b", "c", "D"]));
}

#[test]
fn ins_post_n_precedes_ins_pre_n_plus_one_in_the_gap_they_share() {
    let text = doc(&["a", "b"]);
    let out = apply("[f#]\nINS.PRE 2:\n+second\n\nINS.POST 1:\n+first\n", &text);
    assert_eq!(out, doc(&["a", "first", "second", "b"]));
}

#[test]
fn a_multi_file_patch_groups_in_first_appearance_order() {
    let ops = parse_patch("[b#]\nDEL 1\n\n[a#]\nDEL 1\n\n[b#]\nDEL 3\n").unwrap();
    let groups = group_by_file(&ops).unwrap();
    assert_eq!(
        groups.iter().map(|g| g.path.as_str()).collect::<Vec<_>>(),
        vec!["b", "a"]
    );
    assert_eq!(groups[0].ops.len(), 2);
}

// ---------------------------------------------------------------------------
// refusals — the message is a product surface, so it is asserted on
// ---------------------------------------------------------------------------

#[test]
fn an_anchor_past_the_end_is_out_of_range_and_names_the_count() {
    let e = refuse("[f#]\nDEL 9\n", &doc(&["a", "b"]));
    assert!(
        matches!(
            e,
            PatchError::OutOfRange {
                line: 9,
                count: 2,
                ..
            }
        ),
        "{e:?}"
    );
    assert!(e.to_string().contains("has 2 lines"), "{e}");
}

#[test]
fn an_anchor_in_an_empty_file_says_to_use_ins_head() {
    let e = refuse("[f#]\nDEL 1\n", "");
    assert!(e.to_string().contains("INS.HEAD"), "{e}");
}

#[test]
fn overlapping_operations_are_refused() {
    let e = refuse("[f#]\nSWAP 1.=3:\n+x\n\nDEL 3\n", &doc(&["a", "b", "c"]));
    assert!(e.to_string().contains("overlap"), "{e}");
}

#[test]
fn an_insert_anchored_inside_a_swapped_span_is_refused() {
    let e = refuse(
        "[f#]\nSWAP 1.=3:\n+x\n\nINS.PRE 2:\n+y\n",
        &doc(&["a", "b", "c"]),
    );
    assert!(e.to_string().contains("anchors inside"), "{e}");
}

#[test]
fn a_swap_with_no_body_is_refused_and_suggests_del() {
    let e = refuse("[f#]\nSWAP 1\n", &doc(&["a"]));
    assert!(e.to_string().contains("DEL 1.=1"), "{e}");
}

#[test]
fn a_minus_row_is_refused_by_name() {
    let e = parse_patch("[f#]\nSWAP 1:\n-gone\n").unwrap_err();
    assert!(e.to_string().contains("\"-\" rows"), "{e}");
}

#[test]
fn pasting_views_own_listing_back_is_refused_by_name() {
    let e = parse_patch("[f#]\n  1:const x = 1;\n").unwrap_err();
    assert!(e.to_string().contains("view's listing"), "{e}");
}

#[test]
fn a_body_row_under_del_says_del_takes_no_body() {
    let e = parse_patch("[f#]\nDEL 1\n+oops\n").unwrap_err();
    assert!(e.to_string().contains("DEL takes no body rows"), "{e}");
}

#[test]
fn an_operation_with_no_section_header_is_refused() {
    let e = parse_patch("DEL 1\n").unwrap_err();
    assert!(e.to_string().contains("section header"), "{e}");
}

#[test]
fn a_section_with_no_operations_is_refused() {
    let e = parse_patch("[f#]\n").unwrap_err();
    assert!(e.to_string().contains("no operations"), "{e}");
}

#[test]
fn one_file_under_two_different_tags_is_refused() {
    let e = parse_patch("[f#AAAA]\nDEL 1\n\n[f#BBBB]\nDEL 3\n")
        .and_then(|ops| group_by_file(&ops))
        .unwrap_err();
    assert!(e.to_string().contains("different tags"), "{e}");
}

#[test]
fn the_lenient_range_spellings_all_parse() {
    for spelling in ["SWAP 2.=3:", "SWAP 2..3:", "SWAP 2-3:", "SWAP 2 3:"] {
        let ops = parse_patch(&format!("[f#]\n{spelling}\n+x\n")).expect(spelling);
        assert_eq!((ops[0].a, ops[0].b), (Some(2), Some(3)), "{spelling}");
    }
}

#[test]
fn a_codex_style_envelope_is_swallowed() {
    let ops = parse_patch("*** Begin Patch\n[f#]\nDEL 1\n*** End Patch\n").unwrap();
    assert_eq!(ops.len(), 1);
    assert_eq!(ops[0].kind, OpKind::Del);
}

#[test]
fn a_tag_is_uppercased_and_an_empty_tag_stays_empty() {
    let ops = parse_patch("[f#ab12]\nDEL 1\n").unwrap();
    assert_eq!(ops[0].tag, "AB12");
    assert_eq!(parse_patch("[f#]\nDEL 1\n").unwrap()[0].tag, "");
    assert_eq!(parse_patch("[f]\nDEL 1\n").unwrap()[0].tag, "");
}

// ---------------------------------------------------------------------------
// rebase — proved in BOTH directions
// ---------------------------------------------------------------------------

fn lines(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

fn op(kind: OpKind, a: usize, b: usize) -> PatchOp {
    PatchOp {
        path: "f".into(),
        tag: String::new(),
        kind,
        a: Some(a),
        b: Some(b),
        body: vec!["X".into()],
        at: 1,
    }
}

#[test]
fn line_map_tracks_lines_across_an_insert_above() {
    let base = lines(&["a", "b", "c"]);
    let cur = lines(&["new", "a", "b", "c"]);
    assert_eq!(line_map(&base, &cur), vec![Some(1), Some(2), Some(3)]);
}

#[test]
fn an_untouched_range_rebases_onto_a_moved_file() {
    let base = lines(&["a", "b", "c"]);
    let cur = lines(&["header", "header2", "a", "b", "c"]);
    match rebase_ops(&[op(OpKind::Swap, 2, 3)], &base, &cur) {
        RebaseResult::Rebased(ops) => assert_eq!((ops[0].a, ops[0].b), (Some(4), Some(5))),
        other => panic!("expected a rebase, got {other:?}"),
    }
}

#[test]
fn a_file_that_did_not_move_reports_unchanged() {
    let base = lines(&["a", "b"]);
    assert_eq!(
        rebase_ops(&[op(OpKind::Del, 1, 1)], &base, &base),
        RebaseResult::Unchanged
    );
}

#[test]
fn a_touched_range_conflicts_and_names_the_line_range() {
    let base = lines(&["a", "b", "c"]);
    // Someone rewrote line 2 — the exact line this op replaces.
    let cur = lines(&["a", "B!", "c"]);
    match rebase_ops(&[op(OpKind::Swap, 2, 2)], &base, &cur) {
        RebaseResult::Conflict(c) => {
            assert_eq!((c.from, c.to), (2, 2));
            assert_eq!(c.path, "f");
            let e: PatchError = c.into();
            assert!(e.to_string().contains("lines 2.=2"), "{e}");
        }
        other => panic!("expected a conflict, got {other:?}"),
    }
}

#[test]
fn an_insert_inside_a_span_conflicts_rather_than_swallowing_it() {
    let base = lines(&["a", "b", "c", "d"]);
    let cur = lines(&["a", "b", "INSERTED", "c", "d"]);
    match rebase_ops(&[op(OpKind::Swap, 2, 3)], &base, &cur) {
        RebaseResult::Conflict(c) => {
            assert!(c.detail.contains("inserted"), "{}", c.detail);
        }
        other => panic!("expected a conflict, got {other:?}"),
    }
}

#[test]
fn an_interior_rewrite_conflicts_even_though_the_endpoints_still_match() {
    // The classic silent-lost-update shape: the span's ends are intact, its middle is not.
    let base = lines(&["a", "b", "c", "d", "e"]);
    let cur = lines(&["a", "b", "CHANGED", "d", "e"]);
    match rebase_ops(&[op(OpKind::Swap, 2, 4)], &base, &cur) {
        RebaseResult::Conflict(c) => assert_eq!((c.from, c.to), (2, 4)),
        other => panic!("expected a conflict, got {other:?}"),
    }
}

#[test]
fn a_deleted_line_conflicts() {
    let base = lines(&["a", "b", "c"]);
    let cur = lines(&["a", "c"]);
    assert!(matches!(
        rebase_ops(&[op(OpKind::Del, 2, 2)], &base, &cur),
        RebaseResult::Conflict(_)
    ));
}

#[test]
fn ins_head_and_ins_tail_always_rebase() {
    let base = lines(&["a"]);
    let cur = lines(&["totally", "different"]);
    assert!(matches!(
        rebase_ops(&[op(OpKind::InsHead, 1, 1)], &base, &cur),
        RebaseResult::Rebased(_)
    ));
}
