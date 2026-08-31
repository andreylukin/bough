//! Invariant: the exec row PRINTS what the ledger says and nothing else. Every function here is
//! pure over committed steps, so "what `bough exec` printed" is reconstructible from the ledger
//! alone — the same rule §0.2 puts on model-visible input, applied to the one output a script
//! reads (§17 Phase 2, V9).

use bough_plugin_ledger::{Step, WakeId};

use crate::Print;

/// The step type the assistant's streamed text lands under (§2.6, owner `agents`).
pub const THOUGHT_TEXT: &str = "thought/text";

/// The wake the printed answer belongs to: the LAST wake any of `steps` belongs to.
///
/// `steps` is expected in seq order, which every `StepQuery` with `Order::SeqAsc` gives.
pub fn last_wake(steps: &[Step]) -> Option<WakeId> {
    steps.last().map(|s| s.wake.clone())
}

/// Render the outcome for one wake.
///
/// `Text` is every `thought/text` of the wake, in seq order, joined with nothing — the flushes are
/// pieces of one stream, not lines. `Json` is the whole wake as an array of committed rows.
pub fn render(steps: &[Step], wake: &WakeId, print: Print) -> String {
    let of_wake: Vec<&Step> = steps.iter().filter(|s| &s.wake == wake).collect();
    match print {
        Print::Text => {
            let mut out = String::new();
            for s in of_wake {
                if s.kind.as_str() != THOUGHT_TEXT {
                    continue;
                }
                if let Some(t) = s.body.get("text").and_then(|v| v.as_str()) {
                    out.push_str(t);
                }
            }
            out
        }
        Print::Json => {
            let rows: Vec<serde_json::Value> = of_wake
                .iter()
                .map(|s| {
                    serde_json::json!({
                        "id": s.id.as_str(),
                        "traj": s.traj.as_str(),
                        "seq": s.seq.0,
                        "wake": s.wake.as_str(),
                        "kind": s.kind.as_str(),
                        "class": s.class.as_str(),
                        "body": &*s.body,
                    })
                })
                .collect();
            serde_json::to_string_pretty(&serde_json::json!({
                "wake": wake.as_str(),
                "steps": rows,
            }))
            .unwrap_or_default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bough_plugin_ledger::{Class, StepId, StepType, TrajId};
    use std::collections::BTreeSet;
    use std::sync::Arc;

    fn step(seq: u64, wake: &str, kind: &str, body: serde_json::Value) -> Step {
        Step {
            id: StepId::new(format!("s{seq}")),
            traj: TrajId::new("lane/sol"),
            seq: bough_plugin_ledger::Seq(seq),
            at: chrono::DateTime::from_timestamp(0, 0).unwrap(),
            wake: WakeId::new(wake),
            kind: StepType::new(kind),
            class: Class::Thought,
            body: Arc::new(body),
            cites: Arc::new(Vec::new()),
            refs: Arc::new(BTreeSet::new()),
            ignorable: false,
        }
    }

    fn conversation() -> Vec<Step> {
        vec![
            step(1, "w1", "wake/start", serde_json::json!({})),
            step(
                2,
                "w1",
                THOUGHT_TEXT,
                serde_json::json!({ "text": "old", "step_index": 0 }),
            ),
            step(3, "w1", "wake/end", serde_json::json!({})),
            step(4, "w2", "wake/start", serde_json::json!({})),
            step(
                5,
                "w2",
                THOUGHT_TEXT,
                serde_json::json!({ "text": "hello ", "step_index": 0 }),
            ),
            step(6, "w2", "tool/call", serde_json::json!({ "name": "bash" })),
            step(
                7,
                "w2",
                THOUGHT_TEXT,
                serde_json::json!({ "text": "world", "step_index": 1 }),
            ),
            step(8, "w2", "wake/end", serde_json::json!({})),
        ]
    }

    #[test]
    fn text_is_the_last_wakes_assistant_text_in_seq_order() {
        let steps = conversation();
        let wake = last_wake(&steps).unwrap();
        assert_eq!(wake.as_str(), "w2");
        assert_eq!(render(&steps, &wake, Print::Text), "hello world");
    }

    #[test]
    fn text_ignores_every_step_type_that_is_not_assistant_text() {
        let steps = conversation();
        let out = render(&steps, &WakeId::new("w2"), Print::Text);
        assert!(
            !out.contains("bash"),
            "a tool call is not assistant text: {out}"
        );
        assert!(
            !out.contains("old"),
            "an earlier wake is not this answer: {out}"
        );
    }

    #[test]
    fn json_carries_the_whole_wake_and_nothing_else() {
        let steps = conversation();
        let out = render(&steps, &WakeId::new("w2"), Print::Json);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["wake"], "w2");
        let rows = v["steps"].as_array().unwrap();
        assert_eq!(rows.len(), 5, "every step of w2, and no step of w1: {out}");
        assert_eq!(rows[0]["kind"], "wake/start");
        assert_eq!(rows[4]["kind"], "wake/end");
    }

    #[test]
    fn an_empty_ledger_prints_nothing_rather_than_failing() {
        assert_eq!(last_wake(&[]), None);
        assert_eq!(render(&[], &WakeId::new("w1"), Print::Text), "");
    }
}
