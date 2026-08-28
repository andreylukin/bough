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
                "thought/text" | "tool/call" | "program/call" | "tool/result" | "worker/report"
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
///
/// The state half is what the agent DID (the TUI brief, round 6): the tools it ran and on what
/// — `viewed main.rs; patched README.md; ran \`cargo test\`` — typed calls and the calls made from
/// inside a program alike. The mail it was woken by is NOT a clause: every persona read
/// `read mail <their own prompt>` on the rail as a bug. A wake that ran no tool falls back to
/// the first line of what it said, so the half is never empty for a turn that only talked.
fn state_half(steps: &[&Step]) -> String {
    let mut clauses: Vec<String> = Vec::new();
    let mut said: Option<String> = None;
    for s in steps {
        let clause = match s.kind.as_str() {
            "thought/text" => {
                if said.is_none() {
                    said = body_str(s, "text").map(|t| first_line(&t));
                }
                None
            }
            // The code-mode `run` call is the program's envelope: its inner `program/call`
            // steps are the clauses, so the envelope itself says nothing.
            "tool/call" | "program/call" => body_str(s, "name")
                .filter(|n| n != "run")
                .map(|n| tool_clause(&n, s)),
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
    if clauses.is_empty() {
        return said.unwrap_or_default();
    }
    clauses.join("; ")
}

/// A code-mode file handle — `[README.md#B749]` — read as its path.
fn unhandle(v: &str) -> String {
    match v.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
        Some(inner) => inner
            .rsplit_once('#')
            .map(|(p, _)| p)
            .unwrap_or(inner)
            .to_string(),
        None => v.to_string(),
    }
}

/// PURE: one call as a past-tense clause — `viewed main.rs`, `patched README.md`, `ran \`ls\``,
/// `searched "TODO"` — from the tool's name and the argument a person names it by.
fn tool_clause(name: &str, s: &Step) -> String {
    let args = s.body.get("args");
    let arg = |keys: &[&str]| -> Option<String> {
        let o = args?.as_object()?;
        keys.iter()
            .find_map(|k| o.get(*k).and_then(|v| v.as_str()))
            .map(|v| clip(v.lines().next().unwrap_or(""), 40))
    };
    let path = arg(&["path", "file"]).map(|p| unhandle(&p));
    let cmd = arg(&["command", "cmd"]);
    let pattern = arg(&["pattern", "query", "q"]);
    match name {
        "view" | "read_file" => path.map(|p| format!("viewed {p}")),
        "patch" | "edit_file" => path.map(|p| format!("patched {p}")),
        "write_file" => path.map(|p| format!("wrote {p}")),
        "bash" | "sh" => cmd.map(|c| format!("ran `{c}`")),
        "grep" | "glob" => pattern.map(|p| format!("searched \"{p}\"")),
        _ => None,
    }
    .unwrap_or_else(|| format!("ran `{name}`"))
}

fn first_line(t: &str) -> String {
    t.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .to_string()
}

/// The INTENT half: the agent's own words, never a derivation of them (§2) — the FIRST line of
/// its last message (round 6). The last line read as a stale, backwards "intent" on the rail
/// (`→ You'll need to create this ticket manually.`); the first line is where a model says what
/// it is doing or about to do.
fn intent_half(steps: &[&Step]) -> String {
    steps
        .iter()
        .rev()
        .filter(|s| s.kind.as_str() == "thought/text")
        .find_map(|s| body_str(s, "text"))
        .map(|t| first_line(&t))
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
        assert_eq!(
            c.line.state, "ran `bash`",
            "the tools it ran, not the mail it read"
        );
        assert_eq!(
            c.line.intent, "read the plan",
            "the first line of what it said"
        );
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

#[cfg(test)]
mod clause_tests {
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
    fn the_state_half_is_the_work_and_the_mail_is_not_a_clause() {
        let steps = vec![
            step(
                "m",
                "mail/delivered",
                serde_json::json!({ "subject": "Explain this repo" }),
            ),
            step(
                "t1",
                "thought/text",
                serde_json::json!({ "text": "I'll read both files.\nThen edit." }),
            ),
            step(
                "c1",
                "program/call",
                serde_json::json!({ "name": "view", "args": { "path": "main.rs" } }),
            ),
            step(
                "c2",
                "tool/call",
                serde_json::json!({ "name": "patch", "args": { "path": "README.md" } }),
            ),
            step(
                "c3",
                "tool/call",
                serde_json::json!({ "name": "bash", "args": { "command": "cargo test -p x" } }),
            ),
            step(
                "c4",
                "tool/call",
                serde_json::json!({ "name": "grep", "args": { "pattern": "TODO" } }),
            ),
            step(
                "c5",
                "tool/call",
                serde_json::json!({ "name": "run", "args": { "program": "1+1" } }),
            ),
            step(
                "c6",
                "program/call",
                serde_json::json!({ "name": "patch", "args": { "path": "[lib.rs#C0DE]" } }),
            ),
            step(
                "t2",
                "thought/text",
                serde_json::json!({ "text": "Done. Added the line.\nAnything else?" }),
            ),
        ];
        let c = compose(&steps, &WakeId::new("w1"), &StepId::new("end"), &cfg());
        assert_eq!(
            c.line.state,
            "viewed main.rs; patched README.md; ran `cargo test -p x`; searched \"TODO\"; patched lib.rs"
        );
        assert!(!c.line.state.contains("read mail"), "{}", c.line.state);
        assert_eq!(
            c.line.intent, "Done. Added the line.",
            "the FIRST line of the last message"
        );
        // The mail step is not cited: it was not summarised.
        assert!(!c.cites.contains(&StepId::new("m")), "{:?}", c.cites);
        // A turn that only talked: the first line of what it said.
        let talk = vec![step(
            "t1",
            "thought/text",
            serde_json::json!({ "text": "Just a thought.\nMore." }),
        )];
        let c = compose(&talk, &WakeId::new("w1"), &StepId::new("end"), &cfg());
        assert_eq!(c.line.state, "Just a thought.");
        assert_eq!(c.line.intent, "Just a thought.");
    }
}
