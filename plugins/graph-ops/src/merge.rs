//! Invariant (§3): a merge deletes the losing ROW and keeps BOTH trajectories. Two
//! [`bough_plugin_ledger::EdgeKind::Merge`] edges point into ONE new head; one reconciliation
//! digest spans both parents; the survivor's row takes the UNION of `routing_refs` and its OWN
//! `model_override` / `tick_floor` / `wake_classes`. Nothing is ever deleted from the past —
//! sealed tiers on both parents stay valid because neither trajectory moved.

//! The body is WP-3's; the entry point is called from [`crate::GraphHandle`].
