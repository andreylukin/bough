//! Invariant (§17 Phase 8): this row breaks exactly one NAMED site, exactly as often as it says,
//! and counts every hit. It is CATALOG-ONLY (decision D-C8): compiled into the binary, named by no
//! bundle, mounted by a test's own `--patch`, and invisible to `--dump-config` on every shipped
//! profile.
//!
//! The counters are process-global, so a test that mounts this row holds [`test_lock`] for its
//! whole body (the `hello::trace` precedent).
//!
//! SCAFFOLD: `allow(unused_variables)` covers the `todo!()` bodies and comes out with them.
#![allow(unused_variables)]

pub mod invariant;
pub mod sites;

use std::sync::Arc;

use bough_kernel::{ConfigError, Context, Inject, InvariantSpec, Plugin, PluginError};
use bough_plugin_ledger::AgentName;

pub use crate::sites::{FaultKind, FaultSite};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "fault-inject";

/// The row's config. One site per row, so a test names what it broke.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FaultConfig {
    /// WHERE.
    pub at: FaultSite,
    /// HOW.
    pub how: FaultKind,
    /// Fire on the Nth hit of the site, 1-based. A PROTOCOL counter: it is what makes "and the
    /// loop CONTINUES" observable — fail wake 1, pass wake 2.
    pub after: u32,
    /// Fire this many times then stop. `0` = forever.
    pub times: u32,
    /// Restrict to one agent. `None` = every agent.
    pub agent: Option<AgentName>,
}

/// Hits recorded for `site` this process.
///
/// WP-4.
pub fn hits(site: FaultSite) -> u32 {
    let _ = site;
    todo!("WP-4: the site's hit counter")
}

/// How many times `apply` ran. The "not retried" evidence: a FAILED row's `apply` is called once
/// and never again, and this counter is what a test reads to say so.
///
/// WP-4.
pub fn applies() -> u32 {
    todo!("WP-4: the apply counter")
}

/// Zero every counter. A test's setup.
///
/// WP-4.
pub fn reset() {
    todo!("WP-4: zero the counters")
}

/// The lock a test holds for its whole body: the counters are process-global.
///
/// WP-4.
pub fn test_lock() -> std::sync::MutexGuard<'static, ()> {
    todo!("WP-4: a process-global mutex, poison-tolerant like hello::trace's")
}

/// The row.
pub struct FaultInjectPlugin;

#[async_trait::async_trait]
impl Plugin for FaultInjectPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = FaultConfig;

    fn inject() -> Inject {
        Inject::optional(["projection", "tools", "agents"])
    }

    fn validate(cfg: &Self::Config) -> Result<(), ConfigError> {
        let _ = cfg;
        todo!("WP-4: reject after 0 (hits are 1-based)")
    }

    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let _ = (ctx, cfg);
        todo!("WP-4: count the apply, then arm exactly the one site `cfg.at` names")
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

bough_kernel::register_plugin!(FaultInjectPlugin);
