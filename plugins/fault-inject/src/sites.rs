//! Invariant: exactly ONE site per row, and every hit is counted. A test that mounts this row
//! names what it broke and reads how often the break fired; `after`/`times` are PROTOCOL counters
//! (they make "and the loop continues" observable by failing wake 1 and passing wake 2), not
//! deployment tunables.

use std::collections::BTreeMap;

use bough_plugin_ledger::AgentName;
use parking_lot::Mutex;

/// WHERE a fault fires.
#[derive(
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Debug,
    serde::Serialize,
    serde::Deserialize,
    schemars::JsonSchema,
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

impl FaultSite {
    /// The spelling used in counters and in messages.
    pub fn as_str(&self) -> &'static str {
        match self {
            FaultSite::Apply => "apply",
            FaultSite::ProjectionSection => "projection_section",
            FaultSite::ToolExecute => "tool_execute",
            FaultSite::WakeStopping => "wake_stopping",
        }
    }
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
pub fn fires(hit: u32, after: u32, times: u32) -> bool {
    if hit < after {
        return false;
    }
    if times == 0 {
        return true;
    }
    // The `times` firings are the `times` hits from `after` onwards.
    hit < after.saturating_add(times)
}

/// PURE: whether this fault applies to `agent`. `None` is every agent.
pub fn applies_to(filter: Option<&AgentName>, agent: &AgentName) -> bool {
    match filter {
        None => true,
        Some(a) => a == agent,
    }
}

/// The process-global hit counters. A test that mounts this row holds [`crate::test_lock`].
static HITS: Mutex<BTreeMap<&'static str, u32>> = Mutex::new(BTreeMap::new());

/// Count one hit of `site` and report the 1-based hit number. Process-global; hold
/// [`crate::test_lock`].
pub fn hit(site: FaultSite) -> u32 {
    let mut hits = HITS.lock();
    let n = hits.entry(site.as_str()).or_insert(0);
    *n += 1;
    *n
}

/// Hits recorded for `site` this process.
pub fn hits(site: FaultSite) -> u32 {
    HITS.lock().get(site.as_str()).copied().unwrap_or(0)
}

/// Zero every hit counter.
pub fn clear() {
    HITS.lock().clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn after_n_fires_on_the_nth_hit_and_not_before() {
        assert!(!fires(1, 3, 0));
        assert!(!fires(2, 3, 0));
        assert!(fires(3, 3, 0));
        assert!(fires(4, 3, 0));
    }

    #[test]
    fn times_zero_fires_forever() {
        for hit in 1..50 {
            assert!(fires(hit, 1, 0), "hit {hit} must fire");
        }
    }

    #[test]
    fn times_one_fires_once_then_passes() {
        assert!(fires(1, 1, 1));
        assert!(!fires(2, 1, 1));
        assert!(!fires(3, 1, 1));
        // And with `after`: the one firing is the `after`-th hit.
        assert!(!fires(1, 2, 1));
        assert!(fires(2, 2, 1));
        assert!(!fires(3, 2, 1));
    }

    #[test]
    fn an_agent_filter_leaves_other_agents_alone() {
        let sol = AgentName::new("sol");
        let terra = AgentName::new("terra");
        assert!(applies_to(Some(&sol), &sol));
        assert!(!applies_to(Some(&sol), &terra));
        assert!(applies_to(None, &terra));
    }

    #[test]
    fn panic_and_error_are_distinct_kinds() {
        assert_ne!(FaultKind::Error, FaultKind::Panic);
        // And they are spelled distinctly in a bundle patch, so a row says which it is.
        assert_eq!(
            serde_json::to_string(&FaultKind::Error).expect("a kind serializes"),
            "\"error\""
        );
        assert_eq!(
            serde_json::to_string(&FaultKind::Panic).expect("a kind serializes"),
            "\"panic\""
        );
    }
}
