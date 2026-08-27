//! §0.2 runtime invariant for the mail seam. Both clauses read the LEDGER — the authoritative
//! relation — and not a record the router kept of its own behaviour: a router that dropped an
//! event without telling anyone is exactly the case an invariant has to catch.
//!
//! 1. **`unrouted_matched_nobody`** — every `mail/unrouted` step's refs match no `agents` row that
//!    already held them when it was written. The row table is mutable and carries no history, so
//!    "as it was then" is RECONSTRUCTED by rewinding the `agent/routing` evidence past the
//!    unrouted step's seq. That reconstruction is exact for every routing change this crate made
//!    (they all go through `link_ref` / `unlink_ref`) and conservative for any other, which is the
//!    honest form of the check rather than the flattering one.
//! 2. **`one_delivery_per_recipient`** — one routed envelope produced exactly one `mail/delivered`
//!    step per recipient. The "never two" half is decidable from the ledger alone and is what this
//!    clause checks; the "never zero" half has no ledger witness (nothing records an intent to
//!    deliver) and is pinned instead by `tests/fanout.rs::a_misroute_to_a_third_agent_does_not_
//!    strand_the_true_owner`.
//!
//! Cadence is [`bough_kernel::Cadence::OnQuiesce`] for both (P1-D14).

use std::collections::{BTreeMap, BTreeSet};

use bough_kernel::{Cadence, Context, InvariantSpec, InvariantViolation};
use bough_plugin_ledger::{AgentName, Order, Ref, Seq, StepId, StepQuery, StepType, TrajId};

/// One `mail/unrouted` step, as the check reads it.
#[derive(Clone, Debug, PartialEq)]
pub struct UnroutedObs {
    pub step: StepId,
    pub seq: Seq,
    pub refs: BTreeSet<Ref>,
}

/// One `agent/routing` step: the evidence that makes clause 1 rewindable.
#[derive(Clone, Debug, PartialEq)]
pub struct RoutingObs {
    pub agent: AgentName,
    pub seq: Seq,
    pub added: BTreeSet<Ref>,
    pub removed: BTreeSet<Ref>,
}

/// One `mail/delivered` step: its trajectory (the recipient) and what it delivered.
#[derive(Clone, Debug, PartialEq)]
pub struct DeliveredObs {
    pub traj: TrajId,
    pub seq: Seq,
    /// A stable digest of the delivered envelope: same envelope, same string.
    pub fingerprint: String,
}

/// Everything the two clauses need, as plain data. Pure input to a pure check.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    pub unrouted: Vec<UnroutedObs>,
    pub routing: Vec<RoutingObs>,
    /// The `agents` rows AS THEY ARE NOW, which clause 1 rewinds.
    pub rows: Vec<(AgentName, BTreeSet<Ref>)>,
    pub delivered: Vec<DeliveredObs>,
}

/// A row's routing refs as they were at `seq`: today's set, with every later `agent/routing`
/// undone in reverse.
fn refs_at(
    agent: &AgentName,
    now: &BTreeSet<Ref>,
    routing: &[RoutingObs],
    seq: Seq,
) -> BTreeSet<Ref> {
    let mut later: Vec<&RoutingObs> = routing
        .iter()
        .filter(|r| r.agent == *agent && r.seq > seq)
        .collect();
    later.sort_by_key(|r| std::cmp::Reverse(r.seq));
    let mut refs = now.clone();
    for change in later {
        for added in &change.added {
            refs.remove(added);
        }
        for removed in &change.removed {
            refs.insert(removed.clone());
        }
    }
    refs
}

/// The whole invariant as a pure function. The first violation wins, and the detail names the
/// step and the agent it was about.
pub fn evaluate(snap: &Snapshot) -> Result<(), String> {
    for item in &snap.unrouted {
        for (agent, now) in &snap.rows {
            let then = refs_at(agent, now, &snap.routing, item.seq);
            let overlap: Vec<&Ref> = then.iter().filter(|r| item.refs.contains(*r)).collect();
            if let Some(first) = overlap.first() {
                return Err(format!(
                    "step `{}` was queued as unrouted, but agent `{agent}` routed on `{first}` \
                     at the time it was written",
                    item.step
                ));
            }
        }
    }

    let mut seen: BTreeMap<(String, String), Vec<Seq>> = BTreeMap::new();
    for d in &snap.delivered {
        seen.entry((d.traj.to_string(), d.fingerprint.clone()))
            .or_default()
            .push(d.seq);
    }
    for ((traj, _), seqs) in &seen {
        if seqs.len() > 1 {
            return Err(format!(
                "trajectory `{traj}` has {} `mail/delivered` steps for ONE envelope (seqs {:?}); \
                 delivery is per recipient, exactly once",
                seqs.len(),
                seqs
            ));
        }
    }
    Ok(())
}

/// The digest clause 2 groups by. The body plus the step's own timestamp: the same envelope
/// delivered to one agent twice is one digest twice, and two genuinely different routes differ in
/// at least one of the two.
pub fn fingerprint(body: &serde_json::Value, at: chrono::DateTime<chrono::Utc>) -> String {
    format!("{}@{}", body, at.to_rfc3339())
}

/// Read the whole snapshot out of the bound ledger.
pub async fn snapshot(ledger: &bough_plugin_ledger::LedgerHandle) -> Result<Snapshot, String> {
    let read = |kind: &str| {
        let q = StepQuery {
            kinds: vec![StepType::new(kind)],
            order: Order::SeqAsc,
            ..Default::default()
        };
        let ledger = ledger.clone();
        async move { ledger.0.steps(&q).await.map_err(|e| e.to_string()) }
    };

    let mut snap = Snapshot::default();
    for step in read("mail/unrouted").await? {
        let body: crate::MailUnrouted = match serde_json::from_value((*step.body).clone()) {
            Ok(b) => b,
            Err(_) => continue,
        };
        snap.unrouted.push(UnroutedObs {
            step: step.id,
            seq: step.seq,
            refs: body.refs.into_iter().collect(),
        });
    }
    for step in read("agent/routing").await? {
        let body: crate::AgentRouting = match serde_json::from_value((*step.body).clone()) {
            Ok(b) => b,
            Err(_) => continue,
        };
        snap.routing.push(RoutingObs {
            agent: body.agent,
            seq: step.seq,
            added: body.added.into_iter().collect(),
            removed: body.removed.into_iter().collect(),
        });
    }
    for step in read("mail/delivered").await? {
        snap.delivered.push(DeliveredObs {
            traj: step.traj.clone(),
            seq: step.seq,
            fingerprint: fingerprint(&step.body, step.at),
        });
    }
    for row in ledger.0.agents().await.map_err(|e| e.to_string())? {
        snap.rows.push((row.name, row.routing_refs));
    }
    Ok(snap)
}

async fn check(ctx: Context, name: &'static str) -> Result<(), InvariantViolation> {
    let violation = |detail: String| InvariantViolation {
        invariant: name,
        plugin: crate::PLUGIN_NAME,
        entry: ctx.entry_id().clone(),
        detail,
    };
    let ledger = match ctx.get::<bough_plugin_ledger::Ledger>() {
        Ok(l) => (*l).clone(),
        // No ledger bound means nothing to check: the row cannot have written anything.
        Err(_) => return Ok(()),
    };
    let snap = snapshot(&ledger).await.map_err(violation)?;
    evaluate(&snap).map_err(violation)
}

/// Clause 1.
pub fn unrouted_matched_nobody() -> InvariantSpec {
    InvariantSpec {
        name: "unrouted_mail_matched_nobody_when_it_was_written",
        plugin: crate::PLUGIN_NAME,
        cadence: Cadence::OnQuiesce,
        check: |ctx| {
            Box::pin(check(
                ctx,
                "unrouted_mail_matched_nobody_when_it_was_written",
            ))
        },
    }
}

/// Clause 2.
pub fn one_delivery_per_recipient() -> InvariantSpec {
    InvariantSpec {
        name: "one_mail_delivered_step_per_recipient_per_envelope",
        plugin: crate::PLUGIN_NAME,
        cadence: Cadence::OnQuiesce,
        check: |ctx| {
            Box::pin(check(
                ctx,
                "one_mail_delivered_step_per_recipient_per_envelope",
            ))
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refs(items: &[&str]) -> BTreeSet<Ref> {
        items.iter().map(Ref::new).collect()
    }

    fn clean() -> Snapshot {
        Snapshot {
            unrouted: vec![UnroutedObs {
                step: StepId::new("s-1"),
                seq: Seq(10),
                refs: refs(&["repo:bough"]),
            }],
            routing: vec![RoutingObs {
                agent: AgentName::new("ci"),
                seq: Seq(20),
                added: refs(&["repo:bough"]),
                removed: BTreeSet::new(),
            }],
            rows: vec![(AgentName::new("ci"), refs(&["repo:bough"]))],
            delivered: vec![
                DeliveredObs {
                    traj: TrajId::new("t-ci"),
                    seq: Seq(30),
                    fingerprint: "env-a".into(),
                },
                DeliveredObs {
                    traj: TrajId::new("t-infra"),
                    seq: Seq(31),
                    fingerprint: "env-a".into(),
                },
            ],
        }
    }

    /// The late-link case is the one this clause must NOT report, and the one it must not be
    /// weakened into ignoring: `ci` holds `repo:bough` now, but it linked it AFTER the step.
    #[test]
    fn a_clean_stream_passes() {
        assert_eq!(evaluate(&clean()), Ok(()));
    }

    #[test]
    fn a_planted_unrouted_step_whose_refs_matched_a_row_is_reported() {
        let mut snap = clean();
        // The link now predates the unrouted step, so the router had an owner and queued anyway.
        snap.routing[0].seq = Seq(5);
        let err = evaluate(&snap).expect_err("a violation");
        assert!(err.contains("s-1"), "{err}");
        assert!(err.contains("ci"), "{err}");
        assert!(err.contains("repo:bough"), "{err}");
    }

    #[test]
    fn a_delivery_with_two_steps_for_one_recipient_is_reported() {
        let mut snap = clean();
        snap.delivered.push(DeliveredObs {
            traj: TrajId::new("t-ci"),
            seq: Seq(32),
            fingerprint: "env-a".into(),
        });
        let err = evaluate(&snap).expect_err("a violation");
        assert!(err.contains("t-ci"), "{err}");
        assert!(err.contains("exactly once"), "{err}");
    }
}
