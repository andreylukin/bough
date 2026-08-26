//! Invariant: every function here is PURE. A `gh` JSON payload becomes a [`Collected`] with its
//! ref, its refs and its class decided by data — never by a clock, a network read or a global — so
//! the class rules (§5: pushes, CI and state changes never wake a dormant agent) are testable
//! against fixtures alone.

use bough_plugin_agents::MailClass;
use bough_plugin_collect_core::{Collected, WakeClass};

/// The sources this collector sweeps; each has its own watermark.
pub const SOURCES: [&str; 4] = ["prs", "review_requests", "mentions", "checks"];

/// PURE: one `gh pr list` row becomes a [`Collected`]. WP-2.
pub fn pr_of(repo: &str, row: &serde_json::Value) -> Option<Collected> {
    let _ = (repo, row);
    todo!("WP-2")
}

/// PURE: one review request becomes a [`Collected`]. WP-2.
pub fn review_request_of(repo: &str, row: &serde_json::Value) -> Option<Collected> {
    let _ = (repo, row);
    todo!("WP-2")
}

/// PURE: one `@`-mention notification becomes a [`Collected`]. WP-2.
pub fn mention_of(repo: &str, row: &serde_json::Value) -> Option<Collected> {
    let _ = (repo, row);
    todo!("WP-2")
}

/// PURE: one check run becomes a [`Collected`]. Never wake-class (§5). WP-2.
pub fn check_of(repo: &str, row: &serde_json::Value) -> Option<Collected> {
    let _ = (repo, row);
    todo!("WP-2")
}

/// PURE: the mail class of a collected item, given the row's configured wake classes. Everything
/// not named is [`MailClass::Ordinary`] (§5). WP-2.
pub fn class_of(kind: WakeClass, wake_classes: &[WakeClass]) -> MailClass {
    let _ = (kind, wake_classes);
    todo!("WP-2")
}
