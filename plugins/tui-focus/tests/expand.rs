//! WP-4 / §2.4 + V2: clicking a tool-call header expands and collapses it, and the expansion is
//! keyed by CALL ID so it survives everything the ledger does underneath.

use std::collections::BTreeSet;
use std::sync::Arc;

use bough_plugin_ledger::{Class, Seq, Step, StepId, StepType, TrajId, WakeId};
use bough_plugin_llm::ToolCallId;
use bough_plugin_tui_focus::expand::{self, Expanded};
use bough_plugin_tui_focus::{hit_for_call, rows_from_steps, Row};
use bough_plugin_tui_shell::pane::{HitId, PaneOutcome};

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

fn call_step(n: u64, id: &str) -> Step {
    step(
        n,
        "tool/call",
        serde_json::json!({
            "call": id, "name": "bash", "args": { "cmd": "ls" },
            "render": "terminal", "step_index": 0
        }),
    )
}

/// The hit id a header mints round-trips back to its call, and toggling is a toggle.
#[test]
fn clicking_a_tool_header_toggles_expansion() {
    let c1 = ToolCallId::new("c1");
    let hit = hit_for_call(&c1);
    assert_eq!(expand::call_of_hit(&hit), Some(c1.clone()));

    let mut expanded = Expanded::new();
    assert!(!expanded.is_expanded(&c1));

    assert_eq!(
        expand::on_click(&mut expanded, Some(&hit)),
        PaneOutcome::Handled
    );
    assert!(expanded.is_expanded(&c1), "the first click expands");

    assert_eq!(
        expand::on_click(&mut expanded, Some(&hit)),
        PaneOutcome::Handled
    );
    assert!(!expanded.is_expanded(&c1), "the second click collapses");
    assert!(expanded.is_empty());

    // A region this pane did not mint is not the pane's to act on, and neither is a click that
    // landed on no region at all.
    assert_eq!(
        expand::on_click(&mut expanded, Some(&HitId::new("rail:sol"))),
        PaneOutcome::Ignored
    );
    assert_eq!(expand::on_click(&mut expanded, None), PaneOutcome::Ignored);
    assert!(expanded.is_empty());
}

/// Rows are recomputed from the ledger on every append and page in from underneath, so an
/// index-keyed expansion would jump to a DIFFERENT tool the moment anything arrived. Keyed by call
/// id it simply survives.
#[test]
fn expansion_survives_new_steps_arriving() {
    let mut steps = vec![call_step(1, "c1"), call_step(2, "c2")];
    let rows = rows_from_steps(&steps);
    let Row::Tool { call: c1, .. } = &rows[0] else {
        panic!("expected a Tool row");
    };
    let c1 = c1.clone();

    let mut expanded = Expanded::new();
    expand::on_click(&mut expanded, Some(&hit_for_call(&c1)));
    assert!(expanded.is_expanded(&c1));

    // A whole turn's worth of steps arrives, INCLUDING older ones paged in at the front, so every
    // row index shifts.
    steps.insert(
        0,
        step(
            0,
            "thought/text",
            serde_json::json!({ "text": "older", "step_index": 0 }),
        ),
    );
    steps.push(step(
        3,
        "tool/result",
        serde_json::json!({
            "call": "c1", "name": "bash", "outcome": "ok", "content": "a", "step_index": 0
        }),
    ));
    steps.push(call_step(4, "c3"));
    let rows2 = rows_from_steps(&steps);

    // The row moved and gained its result; the expansion is still on the same CALL.
    let at = rows2
        .iter()
        .position(|r| matches!(r, Row::Tool { call, .. } if *call == c1))
        .expect("c1 is still a row");
    assert_ne!(at, 0, "the row index moved");
    assert!(!rows2[at].is_pending_tool(), "and its result folded in");
    assert!(expanded.is_expanded(&c1), "the expansion followed the call");
    assert!(!expanded.is_expanded(&ToolCallId::new("c3")));
    assert_eq!(expanded.len(), 1);
}
