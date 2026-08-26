//! Invariant: the file-view render is a PURE FUNCTION of the ledger (V8). It takes plain data and
//! returns a string, so "pure" is testable with no store, no provider and no I/O.
//!
//! Nothing here reads a clock, an environment variable, a random number or a hash-ordered map:
//! `at` is an argument, every collection walked is already ordered, and the same
//! [`TrajectoryView`] therefore renders the same bytes on every machine and every call.

use std::fmt::Write as _;

use bough_plugin_ledger::{Class, TrajectoryView};
use chrono::{DateTime, SecondsFormat, Utc};

/// The one spelling of a class in the render. `Class::as_str` is the ledger Definition's business;
/// duplicating the two words here keeps the renderer a pure function of plain data.
fn class_word(class: Class) -> &'static str {
    match class {
        Class::Evidence => "evidence",
        Class::Thought => "thought",
    }
}

fn stamp(at: &DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// Render a whole trajectory — steps, edges, rollups, the agent row — as text.
pub fn render_file_view(view: &TrajectoryView, at: DateTime<Utc>) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# trajectory {}", view.traj);
    let _ = writeln!(out, "rendered-at: {}", stamp(&at));

    // A band with no rows renders NOTHING — not an empty header. Same rule as the assembler's
    // bands, for the same reason: an empty header is a lie about the ledger.
    if let Some(agent) = &view.agent {
        let _ = writeln!(out, "\n## agent");
        let _ = writeln!(out, "name: {}", agent.name);
        let refs: Vec<&str> = agent.routing_refs.iter().map(|r| r.as_str()).collect();
        let _ = writeln!(out, "routing-refs: {}", refs.join(", "));
        let classes: Vec<&str> = agent.wake_classes.iter().map(|c| c.as_str()).collect();
        let _ = writeln!(out, "wake-classes: {}", classes.join(", "));
        if let Some(m) = &agent.model_override {
            let _ = writeln!(out, "model-override: {m}");
        }
        if let Some(t) = agent.tick_floor {
            let _ = writeln!(out, "tick-floor-ms: {}", t.as_millis());
        }
        if let Some(d) = &agent.digest_rollup {
            let _ = writeln!(out, "digest-rollup: {d}");
        }
    }

    if !view.edges.is_empty() {
        let _ = writeln!(out, "\n## edges");
        for e in &view.edges {
            let kind = match e.kind {
                bough_plugin_ledger::EdgeKind::Ancestor => "ancestor",
                bough_plugin_ledger::EdgeKind::Merge => "merge",
            };
            let _ = writeln!(
                out,
                "- {kind} {parent} -> {child} at seq {seq} ({at})",
                parent = e.parent,
                child = e.child,
                seq = e.at_seq.0,
                at = stamp(&e.at),
            );
        }
    }

    if !view.rollups.is_empty() {
        let _ = writeln!(out, "\n## rollups");
        for r in &view.rollups {
            let kind = match r.kind {
                bough_plugin_ledger::RollupKind::Tier => "tier",
                bough_plugin_ledger::RollupKind::Digest => "digest",
                bough_plugin_ledger::RollupKind::Reconciliation => "reconciliation",
            };
            let _ = writeln!(
                out,
                "- {id} {kind} tier {tier} seq {from}..{to} sealed {sealed}{sup}",
                id = r.id,
                tier = r.tier,
                from = r.from_seq.0,
                to = r.to_seq.0,
                sealed = stamp(&r.sealed_at),
                sup = match &r.superseded_by {
                    Some(s) => format!(" superseded-by {s}"),
                    None => String::new(),
                },
            );
            let notable: Vec<&str> = r.notable_refs.iter().map(|x| x.as_str()).collect();
            if !notable.is_empty() {
                let _ = writeln!(out, "  notable: {}", notable.join(", "));
            }
            let _ = writeln!(out, "  {}", r.body);
        }
    }

    if !view.steps.is_empty() {
        let _ = writeln!(out, "\n## steps");
        for s in &view.steps {
            let _ = writeln!(
                out,
                "\n### seq {seq} {kind} [{class}] wake {wake} {at}",
                seq = s.seq.0,
                kind = s.kind,
                class = class_word(s.class),
                wake = s.wake,
                at = stamp(&s.at),
            );
            let _ = writeln!(out, "id: {}", s.id);
            if !s.cites.is_empty() {
                let cites: Vec<&str> = s.cites.iter().map(|c| c.r#ref.as_str()).collect();
                let _ = writeln!(out, "cites: {}", cites.join(", "));
            }
            if !s.refs.is_empty() {
                let refs: Vec<&str> = s.refs.iter().map(|r| r.as_str()).collect();
                let _ = writeln!(out, "refs: {}", refs.join(", "));
            }
            let _ = writeln!(out, "{}", s.body);
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::Arc;
    use std::time::Duration;

    use bough_plugin_ledger::{
        AgentName, AgentRow, Cite, Edge, EdgeKind, Ref, Rollup, RollupId, RollupKind, Seq, Step,
        StepId, StepType, TrajId, TrajectoryView, WakeId,
    };
    use chrono::TimeZone;

    use super::*;

    fn t(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_770_000_000 + secs, 0).unwrap()
    }

    fn step(seq: u64, kind: &str, class: Class, body: serde_json::Value) -> Step {
        Step {
            id: StepId::new(format!("s{seq}")),
            traj: TrajId::new("lane/sol"),
            seq: Seq(seq),
            at: t(seq as i64),
            wake: WakeId::new("w1"),
            kind: StepType::new(kind),
            class,
            body: Arc::new(body),
            cites: Arc::new(vec![Cite {
                r#ref: Ref::new("gh:o/r#12"),
                url: None,
            }]),
            refs: Arc::new(BTreeSet::from([Ref::new("gh:o/r#12")])),
            ignorable: false,
        }
    }

    fn empty_view() -> TrajectoryView {
        TrajectoryView {
            traj: TrajId::new("lane/sol"),
            steps: Vec::new(),
            edges: Vec::new(),
            rollups: Vec::new(),
            agent: None,
        }
    }

    fn full_view() -> TrajectoryView {
        TrajectoryView {
            traj: TrajId::new("lane/sol"),
            steps: vec![
                step(
                    1,
                    "wake/start",
                    Class::Thought,
                    serde_json::json!({"urgency":"immediate"}),
                ),
                step(
                    2,
                    "mail/delivered",
                    Class::Evidence,
                    serde_json::json!({"subject":"review"}),
                ),
            ],
            edges: vec![Edge {
                child: TrajId::new("lane/sol#fork"),
                parent: TrajId::new("lane/sol"),
                at_seq: Seq(1),
                kind: EdgeKind::Ancestor,
                at: t(10),
            }],
            rollups: vec![Rollup {
                id: RollupId::new("r1"),
                traj: TrajId::new("lane/sol"),
                kind: RollupKind::Tier,
                tier: 2,
                from_seq: Seq(1),
                to_seq: Seq(2),
                src_trajs: vec![TrajId::new("lane/sol")],
                body: serde_json::json!({"summary":"two steps"}),
                notable_refs: BTreeSet::from([Ref::new("gh:o/r#12")]),
                prompt_ver: "p1".into(),
                sealed_at: t(20),
                superseded_by: None,
            }],
            agent: Some(AgentRow {
                name: AgentName::new("sol"),
                traj: TrajId::new("lane/sol"),
                routing_refs: BTreeSet::from([Ref::new("gh:o/r#12")]),
                wake_classes: BTreeSet::from(["ask".to_string()]),
                model_override: None,
                tick_floor: Some(Duration::from_secs(60)),
                digest_rollup: None,
            }),
        }
    }

    #[test]
    fn render_is_a_pure_function_of_the_view() {
        // Two independently built, equal views render identical bytes...
        assert_eq!(
            render_file_view(&full_view(), t(99)),
            render_file_view(&full_view(), t(99))
        );
        // ...and a single changed byte of the input changes the output, so it is a function OF the
        // view rather than of something else.
        let mut changed = full_view();
        changed.steps[1].body = Arc::new(serde_json::json!({"subject":"deploy"}));
        assert_ne!(
            render_file_view(&full_view(), t(99)),
            render_file_view(&changed, t(99))
        );
        // `at` is an argument, never a clock read: a different `at` is the only other input.
        assert_ne!(
            render_file_view(&full_view(), t(99)),
            render_file_view(&full_view(), t(100))
        );
    }

    #[test]
    fn render_is_stable_across_calls() {
        let view = full_view();
        let first = render_file_view(&view, t(5));
        for _ in 0..25 {
            assert_eq!(render_file_view(&view, t(5)), first);
        }
    }

    #[test]
    fn an_empty_trajectory_renders_a_header_only() {
        let text = render_file_view(&empty_view(), t(0));
        assert_eq!(
            text,
            format!("# trajectory lane/sol\nrendered-at: {}\n", stamp(&t(0)))
        );
        for band in ["## agent", "## edges", "## rollups", "## steps"] {
            assert!(
                !text.contains(band),
                "a band with no rows must render nothing at all, not an empty header: {band}"
            );
        }
    }

    #[test]
    fn rollups_and_edges_appear_in_the_render() {
        let text = render_file_view(&full_view(), t(0));
        assert!(text.contains("## edges"));
        assert!(text.contains("- ancestor lane/sol -> lane/sol#fork at seq 1"));
        assert!(text.contains("## rollups"));
        assert!(text.contains("- r1 tier tier 2 seq 1..2 sealed"));
        assert!(text.contains("notable: gh:o/r#12"));
        assert!(text.contains(r#"{"summary":"two steps"}"#));
        assert!(text.contains("## steps"));
        assert!(text.contains("### seq 2 mail/delivered [evidence] wake w1"));
        assert!(text.contains("## agent"));
        assert!(text.contains("name: sol"));
    }
}
