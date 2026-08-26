//! Invariant: the file-view render is a PURE FUNCTION of the ledger (V8). It takes plain data and
//! returns a string, so "pure" is testable with no store, no provider and no I/O.

use bough_plugin_ledger::TrajectoryView;
use chrono::{DateTime, Utc};

/// Render a whole trajectory — steps, edges, rollups, the agent row — as text.
pub fn render_file_view(view: &TrajectoryView, at: DateTime<Utc>) -> String {
    todo!("WP-4: file_view::render_file_view")
}
