//! Invariant (§2): the two halves are built by DIFFERENT rules and can never be confused. The
//! STATE half is a deterministic fold over the wake's own steps and carries their ids as cites;
//! the INTENT half is whatever the agent last said about itself, copied verbatim and labelled
//! self-declared. Neither half calls a model: a wake-end refresh that could fail or drift would
//! make the identity band non-deterministic.

use bough_plugin_ledger::{Cite, Ref, Step, StepId, WakeId};

use crate::{AboutConfig, AboutLine};

/// A composed line together with the steps its STATE half summarises.
#[derive(Clone, Debug, PartialEq)]
pub struct Composed {
    pub line: AboutLine,
    /// Never empty: evidence carries cites (§3), and the `wake/end` step is the floor.
    pub cites: Vec<StepId>,
}

/// The steps the state half is allowed to summarise, in ledger order.
fn summarisable(steps: &[Step]) -> Vec<&Step> {
    steps
        .iter()
        .filter(|s| {
            matches!(
                s.kind.as_str(),
                "thought/text" | "tool/call" | "tool/result" | "mail/delivered" | "worker/report"
            )
        })
        .collect()
}

/// Truncate on a char boundary, marking the cut so a reader never mistakes a clipped line for a
/// short one.
fn clip(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", head.trim_end())
}

fn body_str(step: &Step, field: &str) -> Option<String> {
    step.body
        .get(field)
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// One clause per summarised step, in ledger order. Deterministic and clock-free.
fn state_half(steps: &[&Step]) -> String {
    let mut clauses: Vec<String> = Vec::new();
    for s in steps {
        let clause = match s.kind.as_str() {
            "thought/text" => body_str(s, "text").map(|t| first_line(&t)),
            "mail/delivered" => body_str(s, "subject").map(|t| format!("read mail `{t}`")),
            "tool/call" => body_str(s, "name").map(|n| format!("ran `{n}`")),
            "tool/result" => body_str(s, "outcome")
                .filter(|o| o != "ok")
                .map(|o| format!("a tool returned {o}")),
            "worker/report" => {
                body_str(s, "summary").map(|t| format!("a worker reported: {}", first_line(&t)))
            }
            _ => None,
        };
        if let Some(c) = clause {
            if !c.is_empty() && !clauses.contains(&c) {
                clauses.push(c);
            }
        }
    }
    clauses.join("; ")
}

fn first_line(t: &str) -> String {
    t.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .to_string()
}

/// The INTENT half: the agent's own last word, never a derivation of it (§2).
fn intent_half(steps: &[&Step]) -> String {
    steps
        .iter()
        .rev()
        .filter(|s| s.kind.as_str() == "thought/text")
        .find_map(|s| body_str(s, "text"))
        .map(|t| {
            t.lines()
                .map(str::trim)
                .rfind(|l| !l.is_empty())
                .unwrap_or("")
                .to_string()
        })
        .unwrap_or_default()
}

/// Compose the line for one COMPLETED wake.
///
/// `steps` are that wake's own steps in ledger order; `end_step` is its `wake/end`, which is the
/// citation floor — a wake that summarised to nothing still produces EVIDENCE with a cite, so
/// the ledger's class rule is satisfied by construction rather than by luck.
pub fn compose(steps: &[Step], wake: &WakeId, end_step: &StepId, cfg: &AboutConfig) -> Composed {
    let picked = summarisable(steps);
    let state = state_half(&picked);
    let intent = intent_half(&picked);

    let mut cites: Vec<StepId> = picked.iter().map(|s| s.id.clone()).collect();
    if cites.is_empty() {
        cites.push(end_step.clone());
    }

    Composed {
        line: AboutLine {
            state: clip(
                if state.is_empty() {
                    "nothing to report"
                } else {
                    &state
                },
                cfg.max_state_chars,
            ),
            intent: clip(&intent, cfg.max_intent_chars),
            of_wake: wake.clone(),
        },
        cites,
    }
}

/// The cites as the ledger spells them.
pub fn cites_of(ids: &[StepId]) -> Vec<Cite> {
    ids.iter()
        .map(|id| Cite {
            r#ref: Ref::new(format!("step:{id}")),
            url: None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use std::collections::BTreeSet;
    use std::sync::Arc;

    fn step(id: &str, kind: &str, body: serde_json::Value) -> Step {
        Step {
            id: StepId::new(id),
            traj: bough_plugin_ledger::TrajId::new("t1"),
            seq: bough_plugin_ledger::Seq(1),
            at: Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
            wake: WakeId::new("w1"),
            kind: bough_plugin_ledger::StepType::new(kind),
            class: bough_plugin_ledger::Class::Thought,
            body: Arc::new(body),
            cites: Arc::new(vec![]),
            refs: Arc::new(BTreeSet::new()),
            ignorable: false,
        }
    }

    fn cfg() -> AboutConfig {
        AboutConfig {
            max_state_chars: 400,
            max_intent_chars: 200,
        }
    }

    #[test]
    fn the_state_half_cites_every_step_it_summarises() {
        let steps = vec![
            step(
                "s1",
                "thought/text",
                serde_json::json!({ "text": "read the plan" }),
            ),
            step("s2", "tool/call", serde_json::json!({ "name": "bash" })),
            step("s3", "step/end", serde_json::json!({ "index": 0 })),
        ];
        let c = compose(&steps, &WakeId::new("w1"), &StepId::new("end"), &cfg());
        assert_eq!(
            c.cites,
            vec![StepId::new("s1"), StepId::new("s2")],
            "step/end is not something the state half summarises"
        );
        assert_eq!(c.line.state, "read the plan; ran `bash`");
    }

    #[test]
    fn a_wake_that_summarised_to_nothing_still_cites_its_wake_end() {
        let c = compose(&[], &WakeId::new("w1"), &StepId::new("end"), &cfg());
        assert_eq!(c.cites, vec![StepId::new("end")]);
        assert_eq!(c.line.state, "nothing to report");
        assert_eq!(c.line.intent, "");
    }

    #[test]
    fn both_halves_are_clipped_to_their_configured_length() {
        let long = "x".repeat(1000);
        let steps = vec![step(
            "s1",
            "thought/text",
            serde_json::json!({ "text": long }),
        )];
        let c = compose(&steps, &WakeId::new("w1"), &StepId::new("end"), &cfg());
        assert_eq!(c.line.state.chars().count(), 400);
        assert_eq!(c.line.intent.chars().count(), 200);
        assert!(c.line.state.ends_with('…'));
    }
}
