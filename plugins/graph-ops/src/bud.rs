//! Invariant (§4): the parent NEVER pauses. A bud branches at a past seq and touches no step of
//! the parent's chain, so a wake running on the parent completes untouched and its consumed set
//! stays intact. A bud with `agent: None` is a FORK: a trajectory and an ancestor edge, no row and
//! no routing, promotable later by adding the row and nothing else.

//! The body is WP-3's; the entry point is called from [`crate::GraphHandle`].
