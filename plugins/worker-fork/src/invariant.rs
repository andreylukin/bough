//! §0.2 runtime invariant: **`pinned_prefix_reconstructs`** — every `fork/prefix` step names an
//! agent and a seq at which the parent's projection CAN still be assembled, and the child's
//! `request/header` digest for its one call equals the parent's at that seq. It reads the ledger,
//! not what the provider reported: a pin that quietly diverged from the parent's projection is the
//! exact failure §0.2's reconstruction rule exists to catch.
//!
//! Cadence [`bough_kernel::Cadence::OnQuiesce`] (P1-D14).

use bough_kernel::{Cadence, Context, InvariantSpec, InvariantViolation};
use bough_plugin_ledger::{AgentName, Ledger, Order, Seq, Step, StepQuery, StepType};
use bough_plugin_projection::{AssembleRequest, Projection};

/// The step type the loop appends when it sends a request. Read by NAME (P3-D11): the request
/// vocabulary belongs to `agent-loop`, and this crate does not depend on it.
const REQUEST_HEADER: &str = "request/header";

/// sha256, hex — the spelling `agent-loop`'s `request::digest` uses. Duplicated rather than
/// depended on, for the same reason the step type is read by name.
pub fn digest(text: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(text.as_bytes());
    format!("{:x}", h.finalize())
}

/// The `projection_digest` spelling `agent-loop`'s `request::tiers_digest` uses (§12's cache
/// tiers: stable then volatile, joined by a record separator). Duplicated for the same reason as
/// [`digest`]; `tests/prefix.rs` pins the two spellings to each other.
pub fn tiers_digest(stable: &str, volatile: &str) -> String {
    digest(&format!("{stable}\u{1e}{volatile}"))
}

/// The clause above.
pub fn pinned_prefix_reconstructs() -> InvariantSpec {
    InvariantSpec {
        name: "pinned_prefix_reconstructs",
        plugin: crate::PLUGIN_NAME,
        cadence: Cadence::OnQuiesce,
        check: |ctx| Box::pin(check(ctx)),
    }
}

/// One `fork/prefix` row, read back out of the ledger.
#[derive(Clone, Debug, PartialEq)]
pub struct Anchor {
    pub traj: bough_plugin_ledger::TrajId,
    pub of_agent: AgentName,
    pub as_of: Seq,
}

/// PURE: the anchors a chain of steps declares. A row whose body does not parse is not an anchor
/// this crate wrote and is skipped, the way an unknown step type is (§3).
pub fn anchors(steps: &[Step]) -> Vec<Anchor> {
    steps
        .iter()
        .filter(|s| s.kind.as_str() == crate::FORK_PREFIX)
        .filter_map(|s| {
            let body: crate::ForkPrefix = serde_json::from_value((*s.body).clone()).ok()?;
            Some(Anchor {
                traj: s.traj.clone(),
                of_agent: body.of_agent,
                as_of: body.as_of,
            })
        })
        .collect()
}

/// PURE: the newest `request/header` digest on a chain, if the chain sent a request at all.
pub fn newest_header_digest(steps: &[Step]) -> Option<String> {
    steps
        .iter()
        .filter(|s| s.kind.as_str() == REQUEST_HEADER)
        .max_by_key(|s| s.seq.0)
        .and_then(|s| {
            s.body
                .get("projection_digest")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
}

async fn check(ctx: Context) -> Result<(), InvariantViolation> {
    let entry = ctx.entry_id().clone();
    let fail = move |detail: String| InvariantViolation {
        invariant: "pinned_prefix_reconstructs",
        plugin: crate::PLUGIN_NAME,
        entry: entry.clone(),
        detail,
    };
    // Without a ledger or a projection there is nothing to check: this row cannot be mounted
    // without both, so a tree that has neither has no forks either.
    let (Ok(Some(ledger)), Ok(Some(projection))) =
        (ctx.try_get::<Ledger>(), ctx.try_get::<Projection>())
    else {
        return Ok(());
    };
    let rows = ledger
        .0
        .steps(&StepQuery {
            kinds: vec![StepType::new(crate::FORK_PREFIX)],
            order: Order::SeqDesc,
            ..Default::default()
        })
        .await
        .map_err(|e| fail(format!("reading fork/prefix rows: {e}")))?;

    for anchor in anchors(&rows) {
        // The parent's projection AT THAT SEQ must still be assemblable — that is the whole
        // reconstruction claim.
        let parent = projection
            .0
            .assemble(&AssembleRequest {
                agent: anchor.of_agent.clone(),
                wake: None,
                at: chrono::Utc::now(),
                budget: None,
                as_of: Some(anchor.as_of),
            })
            .await
            .map_err(|e| {
                fail(format!(
                    "fork/prefix on `{}` names `{}`@{}, which no longer assembles: {e}",
                    anchor.traj, anchor.of_agent, anchor.as_of.0
                ))
            })?;
        // FILTERED: `newest_header_digest` reads `request/header` and nothing else, and an
        // unfiltered whole-chain read fails as soon as any step type on the chain has been
        // un-registered by a patch (D-WP8-5).
        let child = ledger
            .0
            .steps(&StepQuery {
                trajs: vec![anchor.traj.clone()],
                kinds: vec![bough_plugin_ledger::StepType::new(REQUEST_HEADER)],
                ..Default::default()
            })
            .await
            .map_err(|e| fail(format!("reading `{}`: {e}", anchor.traj)))?;
        // A fork that never sent a request has nothing to compare; it is not a violation.
        let Some(sent) = newest_header_digest(&child) else {
            continue;
        };
        let expected = {
            let (stable, volatile) = parent.tier_split();
            tiers_digest(&stable, &volatile)
        };
        if sent != expected {
            return Err(fail(format!(
                "`{}` sent a system prefix that is not `{}`@{}: the pin diverged from the \
                 parent's projection",
                anchor.traj, anchor.of_agent, anchor.as_of.0
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use bough_plugin_ledger::{Class, StepId, TrajId, WakeId};
    use chrono::{TimeZone, Utc};
    use std::collections::BTreeSet;
    use std::sync::Arc;

    fn step(seq: u64, kind: &str, body: serde_json::Value) -> Step {
        Step {
            id: StepId::new(format!("s{seq}")),
            traj: TrajId::new("worker-fork-w1"),
            seq: Seq(seq),
            at: Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).unwrap(),
            wake: WakeId::new("w1"),
            kind: StepType::new(kind),
            class: Class::Thought,
            body: Arc::new(body),
            cites: Arc::new(Vec::new()),
            refs: Arc::new(BTreeSet::new()),
            ignorable: false,
        }
    }

    #[test]
    fn an_anchor_names_the_parent_and_the_seq() {
        let rows = vec![
            step(1, "fork/end-seed", serde_json::json!({})),
            step(
                2,
                crate::FORK_PREFIX,
                serde_json::json!({ "of_agent": "sol", "as_of": 12 }),
            ),
        ];
        assert_eq!(
            anchors(&rows),
            vec![Anchor {
                traj: TrajId::new("worker-fork-w1"),
                of_agent: AgentName::new("sol"),
                as_of: Seq(12),
            }]
        );
    }

    #[test]
    fn the_newest_header_is_the_one_compared() {
        let rows = vec![
            step(
                1,
                REQUEST_HEADER,
                serde_json::json!({ "projection_digest": "old" }),
            ),
            step(
                2,
                REQUEST_HEADER,
                serde_json::json!({ "projection_digest": "new" }),
            ),
        ];
        assert_eq!(newest_header_digest(&rows), Some("new".to_string()));
        assert_eq!(newest_header_digest(&[]), None);
    }
}
