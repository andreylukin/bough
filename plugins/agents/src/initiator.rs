//! Invariant (§2): the initiator is AMBIENT ATTRIBUTION, never authorization. Nothing in this
//! phase reads it to make a decision — the journal and mail routing read it to write a name. A
//! future reader that gates on it is a failed review.

use std::future::Future;

use crate::ids::AgentId;

/// The agent whose work the current task is doing, if any.
///
/// WP-2.
pub fn current() -> Option<AgentId> {
    todo!("WP-2: read the task-local")
}

/// Run `fut` with `id` as the ambient initiator.
///
/// WP-2.
pub async fn with<F: Future>(_id: AgentId, _fut: F) -> F::Output {
    todo!("WP-2: tokio::task_local scope")
}
