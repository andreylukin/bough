//! Invariant: the two queries this row sends are CONSTANTS in this file — a protocol detail, not a
//! deployment knob (§0.2) — and parsing is PURE, so the whole collector is testable against a
//! local stub and recorded payloads.

use bough_plugin_collect_core::Collected;

/// Issues assigned to, or mentioning, the authenticated user, newest first.
pub const ISSUES_QUERY: &str = "query BoughIssues($after: String, $first: Int!) { /* WP-2 */ }";

/// Comments on those issues since a cursor.
pub const COMMENTS_QUERY: &str = "query BoughComments($after: String, $first: Int!) { /* WP-2 */ }";

/// PURE: one issue node becomes a [`Collected`] carrying `linear:TEAM-123`. WP-2.
pub fn issue_of(node: &serde_json::Value) -> Option<Collected> {
    let _ = node;
    todo!("WP-2")
}

/// PURE: one comment node becomes a [`Collected`] carrying `linear:TEAM-123:comment:<id>`. WP-2.
pub fn comment_of(node: &serde_json::Value) -> Option<Collected> {
    let _ = node;
    todo!("WP-2")
}
