//! Invariant (§17): Phase 5 curates the cross-agent timeline's DATA; the PANE is Phase 8. A
//! `timeline/entry` is Evidence and carries cites, because a timeline is rendered as truth.

use bough_plugin_ledger::{AgentName, Cite, Ref};
use chrono::{DateTime, Utc};

/// One entry the leader notes.
#[derive(Clone, Debug)]
pub struct TimelineEntry {
    pub title: String,
    /// The moment the entry is ABOUT, which is not the moment it was written.
    pub at: DateTime<Utc>,
    pub agents: Vec<AgentName>,
    pub refs: Vec<Ref>,
    pub cites: Vec<Cite>,
}

/// Which entries to read.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TimelineQuery {
    pub agent: Option<AgentName>,
    pub since: Option<DateTime<Utc>>,
    pub limit: Option<usize>,
}

/// One entry as it was stored.
#[derive(Clone, Debug, PartialEq)]
pub struct TimelineRow {
    pub step: bough_plugin_ledger::StepId,
    pub title: String,
    pub at: DateTime<Utc>,
    pub agents: Vec<AgentName>,
    pub refs: Vec<Ref>,
}

impl TimelineEntry {
    /// The durable body. `at` is RFC3339 so the entry's own moment survives a schema-free read.
    pub fn body(&self) -> crate::vocabulary::TimelineEntryBody {
        crate::vocabulary::TimelineEntryBody {
            title: self.title.clone(),
            at: self.at.to_rfc3339(),
            agents: self.agents.clone(),
            refs: self.refs.clone(),
        }
    }
}

impl TimelineRow {
    /// PURE: one stored step read back. An entry whose body will not parse is SKIPPED rather than
    /// failing the read — a timeline written by another binary must not make this one unreadable.
    pub fn of(step: &bough_plugin_ledger::Step) -> Option<TimelineRow> {
        let body: crate::vocabulary::TimelineEntryBody =
            serde_json::from_value((*step.body).clone()).ok()?;
        let at = DateTime::parse_from_rfc3339(&body.at)
            .ok()?
            .with_timezone(&Utc);
        Some(TimelineRow {
            step: step.id.clone(),
            title: body.title,
            at,
            agents: body.agents,
            refs: body.refs,
        })
    }
}

/// PURE: the rows of `steps` this query keeps, newest-about-moment first.
pub fn select(steps: &[bough_plugin_ledger::Step], q: &TimelineQuery) -> Vec<TimelineRow> {
    let mut rows: Vec<TimelineRow> = steps
        .iter()
        .filter_map(TimelineRow::of)
        .filter(|r| q.agent.as_ref().is_none_or(|a| r.agents.contains(a)))
        .filter(|r| q.since.is_none_or(|s| r.at >= s))
        .collect();
    // The timeline is ordered by the moment each entry is ABOUT, not by when it was written:
    // that is the whole difference between a timeline and a log.
    rows.sort_by(|a, b| b.at.cmp(&a.at).then_with(|| a.step.cmp(&b.step)));
    if let Some(limit) = q.limit {
        rows.truncate(limit);
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use bough_plugin_ledger::{Class, Seq, StepId, StepType, TrajId, WakeId};
    use std::collections::BTreeSet;
    use std::sync::Arc;

    fn step(id: &str, about: &str, agents: &[&str]) -> bough_plugin_ledger::Step {
        let body = crate::vocabulary::TimelineEntryBody {
            title: id.to_string(),
            at: about.to_string(),
            agents: agents.iter().map(AgentName::new).collect(),
            refs: vec![],
        };
        bough_plugin_ledger::Step {
            id: StepId::new(id),
            traj: TrajId::new("t-sol"),
            seq: Seq(1),
            at: Utc::now(),
            wake: WakeId::new("w"),
            kind: StepType::new(crate::vocabulary::TIMELINE_ENTRY),
            class: Class::Evidence,
            body: Arc::new(serde_json::to_value(body).expect("serializes")),
            cites: Arc::new(vec![]),
            refs: Arc::new(BTreeSet::new()),
            ignorable: false,
        }
    }

    #[test]
    fn entries_order_by_the_moment_they_are_about() {
        let steps = vec![
            step("early", "2026-01-01T00:00:00Z", &["sol"]),
            step("late", "2026-06-01T00:00:00Z", &["sol"]),
        ];
        let rows = select(&steps, &TimelineQuery::default());
        assert_eq!(
            rows.iter().map(|r| r.title.as_str()).collect::<Vec<_>>(),
            vec!["late", "early"]
        );
    }

    #[test]
    fn the_agent_filter_is_membership_not_authorship() {
        let steps = vec![
            step("a", "2026-01-01T00:00:00Z", &["sol", "terra"]),
            step("b", "2026-01-02T00:00:00Z", &["sol"]),
        ];
        let rows = select(
            &steps,
            &TimelineQuery {
                agent: Some(AgentName::new("terra")),
                ..Default::default()
            },
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].title, "a");
    }

    #[test]
    fn an_unparseable_entry_is_skipped_not_fatal() {
        let mut bad = step("bad", "not-a-time", &["sol"]);
        bad.body = Arc::new(serde_json::json!({ "nonsense": true }));
        let steps = vec![bad, step("good", "2026-01-01T00:00:00Z", &["sol"])];
        let rows = select(&steps, &TimelineQuery::default());
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].title, "good");
    }
}
