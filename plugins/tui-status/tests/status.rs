//! phase ux1 §2.5 / M24 / M9: the status line says what the product is, where it is, what model it
//! is on, how much context and money are left — and it is EXACTLY one row at every width, dropping
//! fields in a documented order rather than wrapping onto the transcript.

use std::path::PathBuf;
use std::time::Duration;

use bough_plugin_tui_shell::{Theme, ThemeName};
use bough_plugin_tui_status::status::{self, Field, StatusView, SEP};
use bough_plugin_tui_status::{cost_of, header_facts, parse_hint, StatusConfig, StatusPane};

fn view() -> StatusView {
    StatusView {
        product: "bough 0.1".into(),
        cwd: Some(PathBuf::from("/Users/andrey/repos/bough-rebuild")),
        home: Some(PathBuf::from("/Users/andrey")),
        cwd_max: 40,
        model: Some("claude-haiku-4-5".into()),
        context_left: Some(82),
        cost_usd: Some(0.42),
        running: true,
        elapsed: Some(Duration::from_secs(12)),
        spinner_frame: '⠋',
        static_status: false,
        hints: vec![
            ("?".into(), "help".into()),
            ("esc".into(), "interrupt".into()),
            ("^f".into(), "search".into()),
        ],
        notice_pinned: false,
        agent: None,
        owed_question: false,
    }
}

/// The rendered width of a set of fields, the way `status_line` lays them out.
fn width_of(v: &StatusView, w: u16) -> usize {
    status::status_line(v, w, &Theme::of(ThemeName::Dark))
        .spans
        .iter()
        .map(|s| s.content.chars().count())
        .sum()
}

#[test]
fn the_line_drops_fields_in_the_documented_order_and_never_exceeds_its_width() {
    let v = view();
    let at = |w: u16| status::fields(&v, w);
    // "Everything" is every field the view HAS: a close key with no pinned notice (or, as here,
    // with a turn running) is absent by design, not dropped for width.
    let everything: Vec<status::Field> = status::RENDER_ORDER
        .iter()
        .copied()
        .filter(|f| status::field_text(&v, *f).is_some())
        .collect();

    // Wide: everything.
    assert_eq!(at(200), everything);
    // 140 and not 120: `Field::StopKey` (M14) is a real field now rather than a word inside the
    // static hints, so "everything fits" needs its columns too.
    assert_eq!(at(140), everything);

    // 80, IDLE: the hints are the first thing to go — they are learnable.
    let idle = StatusView {
        running: false,
        ..view()
    };
    let eighty_idle = status::fields(&idle, 80);
    assert!(!eighty_idle.contains(&Field::Hints));
    assert!(
        eighty_idle.contains(&Field::Cwd),
        "the cwd survives at 80 (B5)"
    );

    // 80, RUNNING: the STOP KEY outlives everything but the product name, because blocker 7 was
    // that nobody knew it existed (phase ux1 §2.4). It is `Field::StopKey` now rather than a word
    // inside the static hints, which is what makes its absence while idle assertable.
    let eighty = at(80);
    assert!(
        eighty.contains(&Field::StopKey),
        "a running line keeps the stop key: {eighty:?}"
    );
    assert!(!eighty.contains(&Field::Cwd), "the cwd goes first instead");

    // 40: the cwd, then the money, then the context. What is left is what tells you the harness
    // is alive and what it is talking to.
    let forty = at(40);
    assert!(!forty.contains(&Field::Cwd));
    assert!(!forty.contains(&Field::Cost));
    assert!(
        forty.contains(&Field::Elapsed),
        "the spinner is next-to-last to go (M32)"
    );
    assert!(forty.contains(&Field::Product));

    // The drop order is a SUBSET chain: nothing that was dropped comes back as the width shrinks.
    for pair in [(200u16, 120u16), (120, 80), (80, 40), (40, 20)] {
        let (wide, narrow) = (at(pair.0), at(pair.1));
        for f in narrow.iter() {
            assert!(
                wide.contains(f),
                "{f:?} appeared at {} after being dropped at {}",
                pair.1,
                pair.0
            );
        }
    }

    // And the line itself fits, at every width, including absurd ones.
    for w in [200u16, 120, 100, 80, 60, 40, 20, 10, 3, 1] {
        assert!(
            width_of(&v, w) <= w as usize,
            "the status line overflowed {w} columns"
        );
    }
}

#[test]
fn the_line_is_one_row_at_every_width() {
    let v = view();
    for w in [200u16, 80, 40, 12, 1] {
        let line = status::status_line(&v, w, &Theme::of(ThemeName::Dark));
        let text: String = line.spans.iter().map(|s| s.content.to_string()).collect();
        assert!(!text.contains('\n'), "the status line wrapped at {w}");
    }
}

#[test]
fn an_empty_session_shows_nothing_for_what_it_does_not_know_and_never_a_zero() {
    let v = StatusView {
        product: "bough 0.1".into(),
        cwd_max: 40,
        ..Default::default()
    };
    let line = status::status_line(&v, 80, &Theme::of(ThemeName::Dark));
    let text: String = line.spans.iter().map(|s| s.content.to_string()).collect();
    // Visual audit F11: `— · — ctx · —` was three dashes saying nothing. A value the view does
    // not have is ABSENT; it is never invented as a zero.
    assert!(!text.contains('—'), "no placeholder dashes: {text:?}");
    assert!(
        !text.contains("ctx"),
        "no context chip without a number: {text:?}"
    );
    assert!(
        !text.contains('$'),
        "a cost nobody paid is not $0.00: {text:?}"
    );
    assert!(text.starts_with("bough 0.1"), "{text:?}");
}

#[test]
fn a_path_is_elided_in_the_middle_and_keeps_its_last_component() {
    let home = PathBuf::from("/Users/andrey");
    let p = PathBuf::from("/Users/andrey/repos/bough-rebuild/plugins/tui-status");

    // Room to spare: the home is a tilde and nothing is cut.
    assert_eq!(
        status::elide_path(&p, Some(&home), 60),
        "~/repos/bough-rebuild/plugins/tui-status"
    );

    let cut = status::elide_path(&p, Some(&home), 24);
    assert_eq!(cut.chars().count(), 24);
    assert!(
        cut.ends_with("/tui-status"),
        "the last component survives: {cut:?}"
    );
    assert!(cut.starts_with("~/repos"), "and so does the head: {cut:?}");
    assert!(
        cut.contains('…'),
        "a cut that nobody can see is a lie: {cut:?}"
    );

    // No home to strip: the absolute path is used as it is.
    assert!(status::elide_path(&p, None, 0).starts_with("/Users/andrey"));

    // Narrower than the last component: the tail is what matters, and the cut still shows.
    let tiny = status::elide_path(&p, Some(&home), 8);
    assert_eq!(tiny.chars().count(), 8);
    assert!(tiny.contains('…'));
}

#[test]
fn the_hints_sit_at_the_right_edge_when_there_is_room_for_them() {
    let v = view();
    let line = status::status_line(&v, 120, &Theme::of(ThemeName::Dark));
    let text: String = line.spans.iter().map(|s| s.content.to_string()).collect();
    assert!(text.ends_with("^f search"), "{text:?}");
    // A run of padding, not a separator run, is what pushes them there.
    assert!(text.contains("  "), "{text:?}");
}

#[test]
fn the_numbers_come_from_ledgered_steps_read_by_name() {
    use bough_plugin_ledger::{Class, Seq, Step, StepId, StepType, TrajId, WakeId};
    use std::collections::BTreeSet;
    use std::sync::Arc;
    let step = |kind: &str, body: serde_json::Value| Step {
        id: StepId::new("s1"),
        traj: TrajId::new("lane/sol"),
        seq: Seq(1),
        wake: WakeId::new("w1"),
        kind: StepType::new(kind),
        class: Class::Thought,
        at: chrono::Utc::now(),
        body: Arc::new(body),
        cites: Arc::new(Vec::new()),
        refs: Arc::new(BTreeSet::new()),
        ignorable: false,
    };

    let h = step(
        "request/header",
        serde_json::json!({"call": {"model": "claude-haiku-4-5"}, "budget": 200000, "projection_tokens": 36000}),
    );
    assert_eq!(
        header_facts(&h),
        Some((Some("claude-haiku-4-5".to_string()), Some(82)))
    );
    // A header without the numbers changes nothing rather than guessing.
    let bare = step("request/header", serde_json::json!({"call": {}}));
    assert_eq!(header_facts(&bare), Some((None, None)));
    assert_eq!(
        header_facts(&step("wake/opened", serde_json::json!({}))),
        None
    );

    assert_eq!(
        cost_of(&step(
            "usage/round",
            serde_json::json!({"cost_usd": 0.0042})
        )),
        Some(0.0042)
    );
    // UNKNOWN cost: contributes nothing, rather than contributing zero.
    assert_eq!(
        cost_of(&step("usage/round", serde_json::json!({"cost_usd": null}))),
        None
    );

    // And the pane folds them the same way the listener does.
    let pane = StatusPane::new(std::sync::Arc::new(cfg()));
    assert_eq!(pane.view().cost_usd, None);
    pane.absorb(&h);
    pane.absorb(&step("usage/round", serde_json::json!({"cost_usd": 0.25})));
    pane.absorb(&step("usage/round", serde_json::json!({"cost_usd": 0.25})));
    let v = pane.view();
    assert_eq!(v.model.as_deref(), Some("claude-haiku-4-5"));
    assert_eq!(v.context_left, Some(82));
    assert_eq!(v.cost_usd, Some(0.5));
}

fn cfg() -> StatusConfig {
    StatusConfig {
        cwd_max: 40,
        spinner: "⠋⠙⠹".into(),
        spinner_ms: 120,
        // The human default. The suite's own patch is the only place this is `true`.
        static_status: false,
        hints: vec!["?=help".into(), "esc=interrupt".into(), "^f=search".into()],
    }
}

#[test]
fn a_malformed_hint_is_rejected_rather_than_half_rendered() {
    assert_eq!(parse_hint("?=help"), Some(("?".into(), "help".into())));
    assert_eq!(parse_hint("help"), None);
    assert_eq!(parse_hint("=help"), None);
    assert_eq!(parse_hint("? ="), None);
}

#[test]
fn the_spinner_and_the_clock_only_move_while_a_turn_runs() {
    let pane = StatusPane::new(std::sync::Arc::new(cfg()));
    let t0 = chrono::Utc::now();
    pane.tick(t0);
    assert!(!pane.view().running);
    assert_eq!(pane.view().elapsed, None);

    pane.set_running(true, t0);
    pane.tick(t0 + chrono::Duration::seconds(7));
    let v = pane.view();
    assert!(v.running);
    assert_eq!(v.elapsed, Some(Duration::from_secs(7)));
    let first = v.spinner_frame;
    pane.tick(t0 + chrono::Duration::seconds(8));
    assert_ne!(pane.view().spinner_frame, first, "the spinner turns (M32)");

    pane.set_running(false, t0 + chrono::Duration::seconds(9));
    assert_eq!(pane.view().elapsed, None);
}

#[test]
fn the_separator_is_the_one_the_fields_are_measured_against() {
    // A drift between the separator `fields` counts and the one `status_line` draws is how a line
    // that "fits" overflows by two cells per field.
    assert_eq!(SEP.chars().count(), 3);
}

/// The model id is shown the way a person says it: the trailing snapshot date is nine cells of
/// nothing on the one row that must also fit the cwd, the cost and the key hints.
#[test]
fn the_model_id_drops_its_snapshot_date_and_nothing_else() {
    assert_eq!(
        status::short_model("claude-haiku-4-5-20251001"),
        "claude-haiku-4-5"
    );
    assert_eq!(status::short_model("gpt-4o-mini"), "gpt-4o-mini");
    assert_eq!(status::short_model("a-2025"), "a-2025");
}

/// M14, the half no test had. `esc to interrupt` must be ABSENT while nothing runs, or the bullet
/// that claims to pin it is a `see "esc"` over a string that is always there.
#[test]
fn the_stop_key_exists_only_while_a_turn_is_running() {
    let running = StatusView {
        running: true,
        ..view()
    };
    let idle = StatusView {
        running: false,
        ..view()
    };
    assert_eq!(
        status::field_text(&running, Field::StopKey).as_deref(),
        Some(status::STOP_KEY)
    );
    assert_eq!(status::field_text(&idle, Field::StopKey), None);

    let text = |v: &StatusView| -> String {
        status::status_line(v, 200, &Theme::of(ThemeName::Dark))
            .spans
            .iter()
            .map(|s| s.content.to_string())
            .collect()
    };
    assert!(text(&running).contains(status::STOP_KEY));
    assert!(
        !text(&idle).contains(status::STOP_KEY),
        "an idle line names no stop key: {:?}",
        text(&idle)
    );
}

/// ux-visual: a pinned notice (a command's output, `/help`) says how it closes — and yields to the
/// stop key while a turn runs, because Esc means interrupt then.
#[test]
fn a_pinned_notice_puts_esc_to_close_on_the_line_unless_a_turn_runs() {
    let mut v = view();
    v.notice_pinned = true;
    v.running = false;
    let line = status::status_line(&v, 120, &Theme::of(ThemeName::Dark));
    let text: String = line.spans.iter().map(|s| s.content.to_string()).collect();
    assert!(text.contains(status::CLOSE_KEY), "{text:?}");
    v.running = true;
    let line = status::status_line(&v, 120, &Theme::of(ThemeName::Dark));
    let text: String = line.spans.iter().map(|s| s.content.to_string()).collect();
    assert!(!text.contains(status::CLOSE_KEY), "{text:?}");
    assert!(text.contains(status::STOP_KEY), "{text:?}");
}

#[test]
fn the_lane_is_named_on_the_line_only_while_the_rail_is_collapsed() {
    // `agent` is set by the row from `ShellView::rail_collapsed`; here the view is the contract.
    let mut v = view();
    v.running = false;
    assert!(!status::fields(&v, 200).contains(&status::Field::Agent));
    v.agent = Some("sol".into());
    let at = |w: u16| status::fields(&v, w);
    let wide = at(200);
    assert_eq!(wide[..2], [status::Field::Product, status::Field::Agent]);
    // At 80 columns — under the rail's collapse width — the name outlives the cwd and the
    // numbers: it is the only thing left that says who is being spoken to.
    let narrow = at(80);
    assert!(narrow.contains(&status::Field::Agent), "{narrow:?}");
    let text = status::status_line(&v, 80, &Theme::of(ThemeName::Dark))
        .spans
        .iter()
        .map(|s| s.content.to_string())
        .collect::<String>();
    assert!(text.contains("sol · idle"), "{text}");
    v.running = true;
    let text = status::status_line(&v, 80, &Theme::of(ThemeName::Dark))
        .spans
        .iter()
        .map(|s| s.content.to_string())
        .collect::<String>();
    assert!(text.contains("sol · running"), "{text}");
}

/// Round 10: the "what do I owe" chip is present only while something is owed, and outlives
/// the cwd, cost and context at narrow widths.
#[test]
fn the_owed_chip_says_questions_and_survives_narrow_widths() {
    let mut v = view();
    v.running = false;
    assert!(!status::fields(&v, 200).contains(&Field::Owed));
    v.owed_question = true;
    let text = status::status_line(&v, 200, &Theme::of(ThemeName::Dark))
        .spans
        .iter()
        .map(|s| s.content.to_string())
        .collect::<String>();
    assert!(text.contains("? question"), "{text}");
    let narrow = status::fields(&v, 60);
    assert!(narrow.contains(&Field::Owed), "{narrow:?}");
    assert!(!narrow.contains(&Field::Cwd), "{narrow:?}");
}
