//! Invariant: both renderers are TOTAL. A share of 0.0 and a share of 1.0 both draw a bar, and an
//! inactive signal draws `∅` — never `0.00`, which would read as "no rejections" (§16).

use crate::dash::DashRow;

/// The glyph an inactive or uncomputable signal renders as.
pub const UNKNOWN: &str = "∅";

/// PURE: the rendered line, clipped to `cols`.
///
/// WP-3.
pub fn line(r: &DashRow, cols: u16, bar_cols: u16) -> String {
    let _ = (r, cols, bar_cols);
    todo!("WP-3: verdict | agent | samples | cv | entropy | bar | flags, clipped")
}

/// PURE: `share` as a `cols`-wide bar. Total: 0.0 and 1.0 both render.
///
/// WP-3.
pub fn bar(share: f64, cols: u16) -> String {
    let _ = (share, cols);
    todo!("WP-3: a cols-wide bar, total over [0.0, 1.0] and clamped outside it")
}
