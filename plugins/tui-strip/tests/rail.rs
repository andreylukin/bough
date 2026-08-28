//! WP-4 / §2.4: the agent rail. These are the pane's PURE halves — the glyph table, the two-half
//! about-line rendering, and the click-to-focus mapping — driven without a terminal, because
//! `Pane::render` is a pure function of state the pane already holds and `handle`'s rail branch
//! needs nothing from the shell.

use std::collections::BTreeSet;
use std::sync::Arc;

use bough_plugin_agents::{AgentId, Status};
use bough_plugin_ledger::{Cite, Class, Ref, Seq, Step, StepId, StepType, TrajId, WakeId};
use bough_plugin_tui_render::{about_from_step, AboutView};
use bough_plugin_tui_shell::pane::HitId;
use bough_plugin_tui_shell::pane::PaneOutcome;
use bough_plugin_tui_shell::Theme;
use bough_plugin_tui_strip::glyph;
use bough_plugin_tui_strip::rail::{self, RailRow};

fn theme() -> Theme {
    // A literal palette rather than `Theme::of`, which is WP-2's: these tests are about the rail's
    // structure, and a role's exact colour is `tui-shell`'s to decide.
    Theme {
        bg: ratatui::style::Color::Black,
        measure_bg: ratatui::style::Color::Black,
        fg: ratatui::style::Color::White,
        dim: ratatui::style::Color::DarkGray,
        accent: ratatui::style::Color::Cyan,
        evidence: ratatui::style::Color::Green,
        thought: ratatui::style::Color::Gray,
        warn: ratatui::style::Color::Yellow,
        error: ratatui::style::Color::Red,
        added: ratatui::style::Color::Green,
        removed: ratatui::style::Color::Red,
        sel_bg: ratatui::style::Color::Blue,
        hint: ratatui::style::Color::Magenta,
        interactive: ratatui::style::Color::Magenta,
        code: ratatui::style::Color::Magenta,
        code_bg: ratatui::style::Color::Magenta,
        field_bg: ratatui::style::Color::Magenta,
    }
}

fn row(name: &str, about: Option<AboutView>) -> RailRow {
    RailRow {
        agent: AgentId::new(name),
        traj: Some(TrajId::new(format!("lane/{name}"))),
        name: name.into(),
        status: Status::Idle,
        wake_pending: false,
        disposed: false,
        dormant: false,
        about,
        leader: false,
    }
}

fn text_of(lines: &[ratatui::text::Line<'static>]) -> Vec<String> {
    lines
        .iter()
        .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
        .collect()
}

/// Every state the rail can show has its OWN glyph and its own style role: four distinct marks,
/// so a glance at the rail is unambiguous.
#[test]
fn each_status_maps_to_its_glyph() {
    let idle = glyph(Status::Idle, false, false, false);
    let waking = glyph(Status::Idle, true, false, false);
    let running = glyph(Status::Running, false, false, false);
    let disposed = glyph(Status::Running, true, true, false);

    assert_eq!(idle.0, '○');
    assert_eq!(running.0, '●');
    assert_eq!(waking.0, '◐');
    // Disposed wins over whatever status the handle still carries: what it IS is gone.
    assert_eq!(disposed.0, '×');
    assert_eq!(disposed.1, "dim");

    let marks: BTreeSet<char> = [idle.0, waking.0, running.0, disposed.0]
        .into_iter()
        .collect();
    assert_eq!(marks.len(), 4, "four states, four distinguishable glyphs");
    // Roles, never colours (`tui-shell`'s theme invariant).
    let roles: BTreeSet<&str> = [idle.1, waking.1, running.1, disposed.1]
        .into_iter()
        .collect();
    assert_eq!(roles.len(), 4);
}

/// §2: the intent half is SELF-DECLARED. It is drawn under its explicit label on its own line,
/// never merged into the state half and never presented as truth.
#[test]
fn the_intent_half_is_always_rendered_under_its_label() {
    let view = AboutView {
        state: "rebased the loop onto the new header rule".into(),
        intent: "finish the swap gate".into(),
        cites: vec![Cite {
            r#ref: Ref::new("step:s41"),
            url: None,
        }],
    };
    let lines = rail::row_lines(&row("sol", Some(view)), false, true, 2, 60, &theme());
    let text = text_of(&lines);
    assert_eq!(text.len(), 3, "head + state half + intent half: {text:?}");
    assert!(text[1].contains("rebased the loop"), "{text:?}");
    let intent = &text[2];
    // The label is the MARK now (visual audit F8): the words spent 23 of the rail's columns.
    assert!(
        intent.contains(rail::INTENT_MARK.trim_end()),
        "the intent half must carry its mark: {intent:?}"
    );
    assert!(
        intent.find(rail::INTENT_MARK.trim_end()).unwrap()
            < intent.find("finish the swap gate").unwrap(),
        "the label comes FIRST, so the claim is never read before it is qualified: {intent:?}"
    );
    // The state half never carries the intent text: the two halves are separate lines.
    assert!(!text[1].contains("finish the swap gate"), "{text:?}");
}

/// P3-D11: with the `about-line` row disabled — or simply before the first completed wake — an
/// agent has no about-line at all, and the rail still draws its glyph and its name.
#[test]
fn an_agent_with_no_about_line_still_renders() {
    let mut r = row("terra", None);
    r.status = Status::Running;
    let lines = rail::row_lines(&r, true, true, 2, 40, &theme());
    let text = text_of(&lines);
    assert_eq!(text.len(), 1, "one line, no about-line: {text:?}");
    assert!(text[0].contains('●') && text[0].contains("terra") && text[0].contains("running"));

    // `about_from_step` is the only door an about-line comes through, and it is shut for every
    // other step type — so a rail with `about-line` unloaded can never acquire one.
    let step = Step {
        id: StepId::new("s1"),
        traj: TrajId::new("lane/terra"),
        seq: Seq(1),
        at: chrono::Utc::now(),
        wake: WakeId::new("w1"),
        kind: StepType::new("thought/text"),
        class: Class::Thought,
        body: Arc::new(serde_json::json!({ "text": "hello", "step_index": 0 })),
        cites: Arc::new(vec![]),
        refs: Arc::new(BTreeSet::new()),
        ignorable: false,
    };
    assert_eq!(about_from_step(&step), None);

    // And an `about/line` step DOES yield the two halves plus its cites, read by NAME.
    let cite = Cite {
        r#ref: Ref::new("step:s0"),
        url: None,
    };
    let about = Step {
        kind: StepType::new("about/line"),
        class: Class::Evidence,
        body: Arc::new(
            serde_json::json!({ "state": "did a thing", "intent": "do another", "of_wake": "w1" }),
        ),
        cites: Arc::new(vec![cite.clone()]),
        ..step
    };
    let view = about_from_step(&about).expect("an about/line yields a view");
    assert_eq!(view.state, "did a thing");
    assert_eq!(view.intent, "do another");
    assert_eq!(view.cites, vec![cite]);
}

/// The rail's whole interaction: a click on a row asks the SHELL to focus that agent. The pane
/// never moves focus itself (§2.1's `PaneOutcome`).
#[test]
fn a_click_on_a_rail_row_returns_a_focus_outcome() {
    let rows = vec![row("sol", None), row("terra", None)];
    let (_lines, spans) = rail::rail(&rows, None, true, 2, 40, &theme());
    assert_eq!(spans.len(), 2);
    // Every row got a clickable span, and the spans do not overlap.
    assert!(spans[0].1 + spans[0].2 <= spans[1].1);

    let hit = rail::hit_for_agent(&rows[1].agent);
    let out = rail::on_click(Some(&hit));
    let PaneOutcome::Focus(req) = out else {
        panic!("a click on a rail row must return a Focus outcome, got {out:?}");
    };
    assert_eq!(req.agent, Some(AgentId::new("terra")));
    assert_eq!(req.pane, None, "the rail asks for an AGENT, not a pane");
    assert_eq!(req.step, None);

    // A region this pane did not mint is NOT the rail's to act on, and neither is a click that
    // landed on no region at all.
    assert_eq!(
        rail::on_click(Some(&HitId::new("tool:c1"))),
        PaneOutcome::Ignored
    );
    assert_eq!(
        rail::on_click(Some(&HitId::new("rail:"))),
        PaneOutcome::Ignored
    );
    assert_eq!(rail::on_click(None), PaneOutcome::Ignored);
}

/// WP-7 / §1 + §11: the dormancy surface, and the click-to-focus check Phase 3 deferred because
/// there was only ever one agent to click on.
mod tests {
    use super::*;
    use bough_plugin_ledger::Step;
    use bough_plugin_tui_strip::{dormant_from_step, glyph, row_lines, set_dormant, status_word};
    use std::sync::Arc;

    fn dormancy_step(traj: &str, dormant: bool) -> Step {
        Step {
            id: StepId::new("d1"),
            traj: TrajId::new(traj),
            seq: Seq(7),
            at: chrono::Utc::now(),
            wake: WakeId::new("op:1"),
            kind: StepType::new("agent/dormancy"),
            class: Class::Evidence,
            body: Arc::new(serde_json::json!({
                "dormant": dormant,
                "reason": "the lane has nothing to do",
                "by": "andrey",
            })),
            cites: Arc::new(vec![]),
            refs: Arc::new(BTreeSet::new()),
            ignorable: false,
        }
    }

    /// §1: a dormant lane gets no ticks and no wakes. It keeps whatever status it had when it
    /// went to sleep, so the rail must say `dormant` rather than `idle` — the one word that would
    /// otherwise promise a wake that is never coming.
    #[test]
    fn a_dormant_row_draws_the_dormant_glyph_and_word() {
        let mut r = row("sol", None);
        r.dormant = true;
        assert_eq!(glyph(r.status, false, false, true), ('\u{25CC}', "warn"));
        assert_eq!(status_word(&r), "dormant");
        let text = text_of(&row_lines(&r, false, false, 0, 40, &theme()));
        assert!(text[0].contains('\u{25CC}'), "{text:?}");
        assert!(text[0].contains("dormant"), "{text:?}");
        // And it is its OWN mark: not the idle circle, not the disposed cross.
        assert_ne!(glyph(r.status, false, false, true).0, '\u{25CB}');
        assert_ne!(glyph(r.status, false, false, true).0, '\u{00D7}');
    }

    /// A disposed agent that was asleep when it was disposed is GONE, which outranks asleep.
    #[test]
    fn disposed_still_wins_over_dormant() {
        let mut r = row("sol", None);
        r.dormant = true;
        r.disposed = true;
        assert_eq!(glyph(r.status, true, true, true), ('\u{00D7}', "dim"));
        assert_eq!(status_word(&r), "disposed");
    }

    /// P3-D11: the rail reads `agent/dormancy` by step-type NAME out of the ledger body. It has
    /// no dependency on the `dormancy` row, and every other step type leaves it alone.
    #[test]
    fn dormancy_is_read_from_the_step_by_name() {
        assert_eq!(
            dormant_from_step(&dormancy_step("lane/sol", true)),
            Some(true)
        );
        assert_eq!(
            dormant_from_step(&dormancy_step("lane/sol", false)),
            Some(false)
        );
        let mut other = dormancy_step("lane/sol", true);
        other.kind = StepType::new("thought/text");
        assert_eq!(dormant_from_step(&other), None);

        let mut rows = vec![row("sol", None), row("terra", None)];
        set_dormant(&mut rows, &TrajId::new("lane/sol"), true);
        assert!(rows[0].dormant);
        assert!(!rows[1].dormant, "one step touches one lane");
        // A step on a trajectory the rail does not know changes nothing.
        set_dormant(&mut rows, &TrajId::new("lane/nobody"), true);
        assert!(!rows[1].dormant);
    }

    /// §11's click-to-focus, over a POPULATION: three rails, three hit regions, and each one maps
    /// to its own agent. Phase 3 deferred this check because one agent cannot show a mix-up.
    #[test]
    fn focus_for_hit_maps_each_of_three_rails_to_its_own_agent() {
        let rows = vec![row("sol", None), row("terra", None), row("luna", None)];
        let (_, spans) = rail::rail(&rows, None, false, 0, 40, &theme());
        assert_eq!(spans.len(), 3);
        for (agent, _, _) in &spans {
            let req = rail::focus_for_hit(&rail::hit_for_agent(agent))
                .expect("the rail's own region must parse back");
            assert_eq!(req.agent.as_ref(), Some(agent));
        }
        // The three regions do not overlap: each rail's span starts after the previous one ends.
        let mut cursor = 0;
        for (_, top, height) in &spans {
            assert!(*top >= cursor, "rails overlap: {spans:?}");
            cursor = top + height;
        }
    }
}

// ---------------------------------------------------------------------------
// phase ux1 §2.5: the rail's breakpoint, and a clip that cannot overflow
// ---------------------------------------------------------------------------

fn cfg() -> bough_plugin_tui_strip::StripConfig {
    bough_plugin_tui_strip::StripConfig {
        width: 28,
        show_about: true,
        about_lines: 2,
        collapse_cols: 100,
        min_width: 22,
        max_width: 40,
    }
}

#[test]
fn the_rail_collapses_under_a_hundred_columns_and_is_clamped_above_it() {
    let c = cfg();
    assert_eq!(
        rail::rail_width(80, &c),
        0,
        "M13: at 80 columns a 34-cell rail left the conversation 46"
    );
    assert_eq!(rail::rail_width(99, &c), 0);
    let at_120 = rail::rail_width(120, &c);
    assert!(
        at_120 >= c.min_width && at_120 <= c.max_width,
        "{at_120} is outside {}..={}",
        c.min_width,
        c.max_width
    );
    let at_200 = rail::rail_width(200, &c);
    assert!(at_200 <= c.max_width, "the rail never grows past max_width");

    // A preferred width outside the band is pulled into it rather than honoured.
    let narrow = bough_plugin_tui_strip::StripConfig { width: 4, ..cfg() };
    assert_eq!(rail::rail_width(120, &narrow), 22);
    let huge = bough_plugin_tui_strip::StripConfig {
        width: 200,
        ..cfg()
    };
    assert_eq!(rail::rail_width(200, &huge), 40);
}

#[test]
fn a_clipped_line_never_exceeds_its_width_and_says_that_it_cut() {
    use ratatui::text::{Line, Span};
    let long = Line::from(vec![
        Span::raw("● ".to_string()),
        Span::raw("a-very-long-agent-name-that-runs-on".to_string()),
        Span::raw("  running".to_string()),
    ]);
    let cut = rail::clip(long.clone(), 12);
    let text: String = cut.spans.iter().map(|s| s.content.to_string()).collect();
    assert_eq!(text.chars().count(), 12, "hard clip: exactly the width");
    assert!(
        text.ends_with('…'),
        "a cut that nobody can see is a lie: {text:?}"
    );

    // A line that fits is handed back untouched — no ellipsis, no padding.
    let short = Line::from(vec![Span::raw("● lane".to_string())]);
    let kept = rail::clip(short, 12);
    let text: String = kept.spans.iter().map(|s| s.content.to_string()).collect();
    assert_eq!(text, "● lane");

    // Zero columns paint nothing at all, which is what a collapsed rail is handed.
    assert!(rail::clip(long, 0).spans.is_empty());
}

#[test]
fn every_rendered_rail_line_fits_the_column_it_was_given() {
    let long = row(
        "an-agent-with-a-really-quite-long-name",
        Some(AboutView {
            state: "rewrote the whole of the projection assembler and then some".into(),
            intent: "keep going until the suite is green".into(),
            cites: Vec::new(),
        }),
    );
    for width in [8u16, 22, 28, 40] {
        let (lines, _) = rail::rail(std::slice::from_ref(&long), None, true, 2, width, &theme());
        for line in lines {
            let cut = rail::clip(line, width);
            let n: usize = cut.spans.iter().map(|s| s.content.chars().count()).sum();
            assert!(
                n <= width as usize,
                "a rail line overflowed {width} columns"
            );
        }
    }
}
