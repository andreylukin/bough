//! Invariant (§2): the initiator is AMBIENT ATTRIBUTION, never authorization. Nothing in this
//! phase reads it to make a decision — the journal and mail routing read it to write a name. A
//! future reader that gates on it is a failed review.

use std::future::Future;

use crate::ids::AgentId;

tokio::task_local! {
    static INITIATOR: AgentId;
}

/// The agent whose work the current task is doing, if any.
pub fn current() -> Option<AgentId> {
    INITIATOR.try_with(|id| id.clone()).ok()
}

/// Run `fut` with `id` as the ambient initiator.
pub async fn with<F: Future>(id: AgentId, fut: F) -> F::Output {
    INITIATOR.scope(id, fut).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Attribution is ambient and nested: the innermost scope names the initiator, and outside
    /// every scope there is none.
    #[tokio::test]
    async fn the_initiator_is_ambient_and_nests() {
        assert_eq!(current(), None);
        let outer = AgentId::new("a");
        let inner = AgentId::new("b");
        with(outer.clone(), async {
            assert_eq!(current(), Some(outer.clone()));
            with(inner.clone(), async {
                assert_eq!(current(), Some(inner.clone()));
            })
            .await;
            assert_eq!(current(), Some(outer.clone()));
        })
        .await;
        assert_eq!(current(), None);
    }
}
