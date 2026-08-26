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
use bough_plugin_tui_strip::rail::{self, RailRow, INTENT_LABEL};

fn theme() -> Theme {
    // A literal palette rather than `Theme::of`, which is WP-2's: these tests are about the rail's
    // structure, and a role's exact colour is `tui-shell`'s to decide.
    Theme {
        bg: ratatui::style::Color::Black,
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
        about,
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
    let idle = glyph(Status::Idle, false, false);
    let waking = glyph(Status::Idle, true, false);
    let running = glyph(Status::Running, false, false);
    let disposed = glyph(Status::Running, true, true);

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
    assert!(
        intent.contains(INTENT_LABEL),
        "the intent half must carry its label: {intent:?}"
    );
    assert!(
        intent.find(INTENT_LABEL).unwrap() < intent.find("finish the swap gate").unwrap(),
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
