//! Invariant (§2): adoption ROUTES, it never decides. The leader reads the durable unsorted queue
//! and either places an item with a lane — appending `mail/adopted` naming the `mail/unrouted`
//! step it consumes, so the adoption is attributable — or HOLDS it. Holding is a legitimate
//! outcome: an item the leader cannot place stays in the queue rather than being forced into the
//! nearest lane.

use bough_plugin_ledger::{AgentName, StepId};
use chrono::{DateTime, Utc};

/// One adoption pass.
#[derive(Clone, Debug)]
pub struct AdoptRequest {
    /// `None` ⇒ up to `adopt_batch` items, oldest first.
    pub steps: Option<Vec<StepId>>,
    /// Where each item goes. An item with no entry is HELD.
    pub placements: Vec<(StepId, AgentName)>,
    pub at: DateTime<Utc>,
}

/// What one adoption pass did.
#[derive(Clone, Debug, PartialEq)]
pub struct AdoptReport {
    pub adopted: Vec<(StepId, AgentName)>,
    /// Items the leader could not place. They stay in the queue.
    pub held: Vec<StepId>,
}

/// PURE: which candidates are placed and which are held.
///
/// `candidates` is the queue as read (oldest first); `placements` is what the leader decided. An
/// item with no placement is HELD — the whole point of the type: holding is a legitimate outcome,
/// and a leader that had to name a lane for every item would push mail into the nearest one.
///
/// A placement naming a step that is not a candidate is IGNORED rather than obeyed: `adopt` may be
/// called with an explicit, bounded batch, and honouring a placement outside it would let one
/// pass consume items the caller never looked at.
pub fn plan(
    candidates: &[StepId],
    placements: &[(StepId, AgentName)],
) -> (Vec<(StepId, AgentName)>, Vec<StepId>) {
    let mut adopted = Vec::new();
    let mut held = Vec::new();
    for step in candidates {
        match placements.iter().find(|(s, _)| s == step) {
            Some((_, agent)) => adopted.push((step.clone(), agent.clone())),
            None => held.push(step.clone()),
        }
    }
    (adopted, held)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(id: &str) -> StepId {
        StepId::new(id)
    }

    #[test]
    fn an_item_with_no_placement_is_held() {
        let (adopted, held) = plan(&[s("u1"), s("u2")], &[(s("u1"), AgentName::new("terra"))]);
        assert_eq!(adopted, vec![(s("u1"), AgentName::new("terra"))]);
        assert_eq!(held, vec![s("u2")]);
    }

    #[test]
    fn a_placement_outside_the_batch_is_ignored() {
        let (adopted, held) = plan(&[s("u1")], &[(s("u9"), AgentName::new("terra"))]);
        assert!(adopted.is_empty());
        assert_eq!(held, vec![s("u1")], "the batch's own item is still held");
    }

    #[test]
    fn the_plan_is_total_over_the_batch() {
        let batch = vec![s("u1"), s("u2"), s("u3")];
        let (adopted, held) = plan(&batch, &[(s("u2"), AgentName::new("sol"))]);
        assert_eq!(adopted.len() + held.len(), batch.len());
    }
}
