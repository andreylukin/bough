//! §0.2 runtime invariant: this row returns the rollups SEAM's two specs
//! ([`bough_plugin_rollups::invariant::seal_once`] and
//! [`bough_plugin_rollups::invariant::tiers_are_an_index`]) rather than declaring its own, so both
//! providers of `rollups` are judged by one statement of the contract (P4-D1).
//!
//! There is no recording side. Both specs read the sealed rows themselves
//! ([`bough_plugin_rollups::invariant::sealed_blocks`]), so a block this provider seals without
//! announcing — or one no provider in this process sealed at all, such as the old-feed adapter's
//! interim tier-1 blocks — is judged by exactly the same statement, and nothing process-global
//! survives an unload.
