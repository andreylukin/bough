//! Invariant (§7): the ordering is intent row → outward act → `action/done`, and the marker the
//! journal derived is embedded IN the artifact, so reconciliation is a lookup against the world
//! rather than a guess. The two configured delays are the two windows a `kill -9` lands in.

use std::sync::Arc;

use bough_plugin_actions::{ActionArtifact, ActionError, ActionKind, ActionProvider, ExecuteRequest};

use crate::ShimConfig;

/// The Provider. Registers through `ActionsHandle::provider`, like any Phase-6 row will.
pub struct GhShimProvider {
    pub cfg: Arc<ShimConfig>,
}

impl GhShimProvider {
    /// WP-4.
    pub fn new(cfg: Arc<ShimConfig>) -> GhShimProvider {
        GhShimProvider { cfg }
    }
}

#[async_trait::async_trait]
impl ActionProvider for GhShimProvider {
    fn kinds(&self) -> Vec<ActionKind> {
        self.cfg.kinds.clone()
    }

    /// Embeds `req.marker` in the artifact (PR body / commit trailer / comment suffix) exactly as
    /// §7 requires.
    ///
    /// WP-4.
    async fn execute(&self, req: &ExecuteRequest) -> Result<ActionArtifact, ActionError> {
        let _ = req;
        todo!("WP-4: delay_before_ms, invoke cfg.gh on the canonical target, delay_after_ms")
    }
}

/// PURE: the argv this Provider runs for a kind and a canonical target. The binary name comes from
/// config and is NEVER hardcoded, so a test's recording shim is reachable without a PATH trick in
/// the crate itself.
///
/// WP-4.
pub fn argv(cfg: &ShimConfig, kind: ActionKind, canonical_target: &str, marker: &str) -> Vec<String> {
    let _ = (cfg, kind, canonical_target, marker);
    todo!("WP-4: [cfg.gh, …], with the marker in the body/trailer/suffix")
}
