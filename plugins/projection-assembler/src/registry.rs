//! Invariant: an `Agent`-scoped section SHADOWS a `Global` section with the same `SectionId`, for
//! that agent alone (§5). Registration is an effect: a disposed section stops rendering, and the
//! registry is left as if it had never mounted.

use bough_plugin_ledger::AgentName;
use bough_plugin_projection::{ProjectionError, SectionSpec, SectionToken};

/// Every contributed section, global and per-agent.
#[derive(Default)]
pub struct Registry {
    #[doc(hidden)]
    pub(crate) inner: parking_lot::RwLock<Vec<SectionSpec>>,
}

impl Registry {
    /// Add one section.
    pub fn add(&self, spec: SectionSpec) -> Result<SectionToken, ProjectionError> {
        todo!("WP-5: Registry::add")
    }
    /// The specs that apply to `agent`, with agent scope shadowing global by `SectionId`.
    pub fn for_agent(&self, agent: &AgentName) -> Vec<SectionSpec> {
        todo!("WP-5: Registry::for_agent")
    }
}
