//! Invariant (§3): re-accepting or editing a requirement SUPERSEDES its old pin rather than
//! editing one. A pin is a fact with a lifetime; rewriting it in place would make the projection
//! of an old wake unreconstructible.

use bough_plugin_ledger::StepId;

/// PURE: the `supersedes` list for a requirement's new pin, given the pins its claim has set
/// before.
///
/// Order-preserving and deduplicated: the same pin named twice (once by the claim, once by the
/// history) is superseded once.
pub fn supersedes_for(previous: &[StepId]) -> Vec<StepId> {
    let mut seen = std::collections::BTreeSet::new();
    previous
        .iter()
        .filter(|p| seen.insert((*p).clone()))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_first_acceptance_supersedes_nothing() {
        assert!(supersedes_for(&[]).is_empty());
    }

    #[test]
    fn a_repeated_pin_is_superseded_once() {
        let a = StepId::new("p1");
        let b = StepId::new("p2");
        assert_eq!(
            supersedes_for(&[a.clone(), b.clone(), a.clone()]),
            vec![a, b]
        );
    }
}
