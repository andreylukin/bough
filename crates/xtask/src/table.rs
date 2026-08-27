//! Invariant: the committed table is a STABLE function of the catalog — sorted by name — so a diff
//! of `docs/event-catalog.md` shows what changed about the events and nothing about scan order.

use crate::scan::Catalog;

/// §15 item 7's threshold: the gate is worth having past ~30 events.
pub const CATALOG_FLOOR: usize = 30;

/// The committed table: name | mode | type | crate | dispatch sites | listen sites.
///
/// WP-6.
pub fn table(c: &Catalog) -> String {
    let _ = c;
    todo!("WP-6: the markdown table, sorted by name")
}
