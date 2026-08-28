//! The pin the program row's doc comment promises: the surface folds a program on the string
//! `"run"`, spelled independently in `plugins/tui-focus` so a surface takes no dependency on the
//! Consumer that happens to be mounted. Nothing else makes the two spellings move together, so
//! this test is the joint: if either constant is renamed, programs would silently render as bare
//! tool calls — here they go red instead.

use std::collections::BTreeSet;
use std::sync::Arc;

use bough_plugin_ledger::{Class, Seq, Step, StepId, StepType, TrajId, WakeId};
use bough_plugin_tui_focus::{rows_from_steps, Row};

#[test]
fn run_tool_name_is_pinned_to_the_focus_pane_fold() {
    assert_eq!(
        bough_plugin_tools_codemode::RUN_TOOL,
        bough_plugin_tui_focus::program::RUN_TOOL,
        "the focus pane folds program rows on its own spelling of the codemode API tool name"
    );
}

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

/// The behaviour the constant equality stands for: a `tool/call` carrying the CONSUMER's tool
/// name folds into a `Row::Program` in the pane, not into a plain tool row.
#[test]
fn a_call_named_by_the_consumer_constant_folds_into_a_program_row() {
    let name = bough_plugin_tools_codemode::RUN_TOOL;
    let steps = vec![
        step(
            1,
            "tool/call",
            serde_json::json!({
                "call": "c1", "name": name,
                "args": { "program": "console.log(1);" }, "render": "generic", "step_index": 0
            }),
        ),
        step(
            2,
            "tool/result",
            serde_json::json!({
                "call": "c1", "name": name, "outcome": "ok", "content": "1\n", "step_index": 0
            }),
        ),
    ];
    let rows = rows_from_steps(&steps);
    assert!(
        matches!(rows.as_slice(), [Row::Program { .. }]),
        "expected one Program row, got {rows:?}"
    );
}
