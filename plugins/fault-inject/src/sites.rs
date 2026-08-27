//! Invariant: exactly ONE site per row, and every hit is counted. A test that mounts this row
//! names what it broke and reads how often the break fired; `after`/`times` are PROTOCOL counters
//! (they make "and the loop continues" observable by failing wake 1 and passing wake 2), not
//! deployment tunables.

use bough_plugin_ledger::AgentName;

/// WHERE a fault fires.
#[derive(
    Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum FaultSite {
    /// The row's own `apply` fails: the fiber goes FAILED. §7's "a row whose fiber FAILS is
    /// reported, not retried into a loop" — and the only way to produce one on purpose.
    Apply,
    /// A contributed projection section whose render returns `Err`: a plugin fiber failing
    /// mid-wake, at the point the wake is assembling its request.
    ProjectionSection,
    /// A registered tool whose execute returns `Err` / panics.
    ToolExecute,
    /// An `agent/wake-stopping` serial listener that fails.
    WakeStopping,
}

/// HOW it fails.
#[derive(
    Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum FaultKind {
    Error,
    Panic,
}

/// PURE: whether the `n`-th hit (1-based) of a site fires, given `after` and `times`.
/// `times: 0` is forever.
///
/// WP-4.
pub fn fires(hit: u32, after: u32, times: u32) -> bool {
    let _ = (hit, after, times);
    todo!("WP-4: hit >= after, and within `times` firings when times > 0")
}

/// PURE: whether this fault applies to `agent`. `None` is every agent.
///
/// WP-4.
pub fn applies_to(filter: Option<&AgentName>, agent: &AgentName) -> bool {
    let _ = (filter, agent);
    todo!("WP-4: None matches every agent")
}

/// Count one hit of `site` and report whether it fires. Process-global; hold [`crate::test_lock`].
///
/// WP-4.
pub fn hit(site: FaultSite) -> u32 {
    let _ = site;
    todo!("WP-4: bump and return the site's hit counter")
}
