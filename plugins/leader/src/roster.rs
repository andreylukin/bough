//! Invariant: the LEADER SEES THE ROSTER EVERY WAKE. With `create_lane` open-handed (the claims
//! demolition), the counterweight is standing visibility: a section at `Slot::Tail`/`Before` —
//! the VOLATILE tier, so its churn never invalidates the stable prompt cache — listing every
//! lane with its routing and how long it has been quiet, so cleanup is something the leader can
//! see, not something it must remember to wonder about.
//!
//! Reproducibility (§2.7 item 3): the step reads honor `as_of` (append-only ledger + a fixed
//! bound = a fixed answer) and ages are rendered against the REQUEST'S `at`, never the wall
//! clock. The row set itself is live state, exactly as live as the `agents` rows it lists.

use std::sync::Arc;

use bough_kernel::{Context, EffectHandle, PluginError};
use bough_plugin_ledger::{AgentName, AgentRow, Order, StepQuery};
use bough_plugin_projection::{
    DropPriority, Place, Position, ProjectionError, ProjectionHandle, SectionBody, SectionCites,
    SectionId, SectionRender, SectionRequest, SectionScope, SectionSpec, Slot,
};
use chrono::{DateTime, Utc};

/// The section id the roster is contributed under. It moves with the leader SET, like
/// `leader.persona`.
pub const SECTION_ID: &str = "leader.lanes";

/// The title the band renders under.
pub const TITLE: &str = "Lanes";

/// Tail/Before: volatile tier — ages change every wake, and the stable tiers must not.
pub const POSITION: Position = Position {
    slot: Slot::Tail,
    place: Place::Before,
};

/// One roster line's facts, separated from rendering so the shape is testable.
#[derive(Clone, Debug, PartialEq)]
pub struct RosterRow {
    pub name: AgentName,
    pub routing: Vec<String>,
    /// The last step's moment on the lane's trajectory, honoring `as_of`. `None` ⇒ no steps yet.
    pub last: Option<DateTime<Utc>>,
}

/// PURE: the age bucket a roster line shows. Coarse on purpose: "quiet for days" is the cleanup
/// signal, minutes are noise.
pub fn age(now: DateTime<Utc>, last: Option<DateTime<Utc>>) -> String {
    let Some(last) = last else {
        return "no steps yet".to_string();
    };
    let mins = (now - last).num_minutes().max(0);
    match mins {
        0..=59 => "active this hour".to_string(),
        60..=1439 => format!("quiet {}h", mins / 60),
        _ => format!("quiet {}d", mins / 1440),
    }
}

/// PURE: the section body's text. The leader's own lane is marked and never a cleanup candidate.
pub fn render_lines(target: &AgentName, rows: &[RosterRow], now: DateTime<Utc>) -> String {
    let mut out = String::from(
        "Every lane, its routing, and how long it has been quiet. Lanes are yours: open one with \
         create_lane when a stream of work deserves its own, and fold finished or long-quiet \
         ones back with merge_lanes.\n",
    );
    for r in rows {
        let you = if r.name == *target { " (you)" } else { "" };
        let routing = if r.routing.is_empty() {
            "no routing".to_string()
        } else {
            r.routing.join(", ")
        };
        out.push_str(&format!(
            "- {}{you} · {routing} · {}\n",
            r.name,
            age(now, r.last)
        ));
    }
    out
}

/// The live render: `agents` rows, each with its trajectory's last step at-or-below `as_of`.
struct Roster(AgentName);

#[async_trait::async_trait]
impl SectionRender for Roster {
    async fn render(&self, req: &SectionRequest) -> Result<Option<SectionBody>, ProjectionError> {
        let mut rows = Vec::new();
        let agents = req
            .ledger
            .0
            .agents()
            .await
            .map_err(|e| ProjectionError::Other(anyhow::anyhow!(e)))?;
        for a in agents {
            rows.push(RosterRow {
                last: last_at(req, &a).await?,
                routing: a
                    .routing_refs
                    .iter()
                    .map(|r| r.as_str().to_string())
                    .collect(),
                name: a.name,
            });
        }
        rows.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(Some(SectionBody {
            title: TITLE.to_string(),
            body: render_lines(&self.0, &rows, req.at),
            cites: SectionCites::default(),
        }))
    }
}

async fn last_at(
    req: &SectionRequest,
    row: &AgentRow,
) -> Result<Option<DateTime<Utc>>, ProjectionError> {
    let steps = req
        .ledger
        .0
        .steps(&StepQuery {
            trajs: vec![row.traj.clone()],
            order: Order::SeqDesc,
            limit: Some(1),
            before: req.as_of,
            ..Default::default()
        })
        .await
        .map_err(|e| ProjectionError::Other(anyhow::anyhow!(e)))?;
    Ok(steps.first().map(|s| s.at))
}

/// The spec, scoped to `target` by SPEC, like the persona.
pub fn spec(target: &AgentName) -> SectionSpec {
    SectionSpec {
        id: SectionId::new(SECTION_ID),
        position: POSITION,
        scope: SectionScope::Agent,
        agent: Some(target.clone()),
        // Droppable under pressure: an answer wake without the roster is degraded, not broken.
        priority: DropPriority::Coarse,
        render: Arc::new(Roster(target.clone())),
    }
}

/// Register the roster section for `target`, owned by the CALLING row's ctx.
pub async fn register(
    ctx: &Context,
    projection: &ProjectionHandle,
    target: &AgentName,
) -> Result<EffectHandle, PluginError> {
    projection.section(ctx, spec(target)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s)
            .expect("rfc3339")
            .with_timezone(&Utc)
    }

    #[test]
    fn ages_bucket_coarsely_and_absence_is_said() {
        let now = at("2026-08-30T12:00:00Z");
        assert_eq!(age(now, None), "no steps yet");
        assert_eq!(
            age(now, Some(at("2026-08-30T11:30:00Z"))),
            "active this hour"
        );
        assert_eq!(age(now, Some(at("2026-08-30T05:00:00Z"))), "quiet 7h");
        assert_eq!(age(now, Some(at("2026-08-27T12:00:00Z"))), "quiet 3d");
    }

    #[test]
    fn the_roster_marks_the_leader_and_says_the_routing() {
        let rows = vec![
            RosterRow {
                name: AgentName::new("sol"),
                routing: vec!["class:ask".to_string()],
                last: Some(at("2026-08-30T11:59:00Z")),
            },
            RosterRow {
                name: AgentName::new("terra"),
                routing: vec![],
                last: None,
            },
        ];
        let text = render_lines(&AgentName::new("sol"), &rows, at("2026-08-30T12:00:00Z"));
        assert!(
            text.contains("- sol (you) · class:ask · active this hour"),
            "{text}"
        );
        assert!(
            text.contains("- terra · no routing · no steps yet"),
            "{text}"
        );
        assert!(
            text.contains("merge_lanes"),
            "the cleanup duty is in the band: {text}"
        );
    }
}
