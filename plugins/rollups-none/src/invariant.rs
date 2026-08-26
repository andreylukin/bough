//! §0.2 runtime invariant: this row returns the rollups SEAM's specs
//! ([`bough_plugin_rollups::invariant::seal_once`] and `tiers_are_an_index`) unchanged, so the
//! stub is judged by the same contract as the summarizer (P4-D1). It records nothing of its own,
//! because it seals nothing: a stub with an empty observation stream passing seal-once is exactly
//! the truthful outcome.
