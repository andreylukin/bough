//! Invariant: `render` queries NOTHING. The row's listeners assemble a [`StatusView`] and the
//! drawing is a pure function of it (phase ux1 §2.5).

use std::path::{Path, PathBuf};
use std::time::Duration;

use bough_plugin_tui_shell::Theme;
use ratatui::text::Line;

/// Everything the line can show.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StatusView {
    /// `"bough 0.x"`.
    pub product: String,
    /// From `ctx.workspace`, NOT from `std::env` (B5).
    pub cwd: Option<PathBuf>,
    /// Last `request/header.call.model`.
    pub model: Option<String>,
    /// `100 - 100 * projection_tokens / budget`.
    pub context_left: Option<u8>,
    /// Σ `usage/round.cost_usd` for this home. `None` renders as `—`, never as `$0.00`.
    pub cost_usd: Option<f64>,
    pub running: bool,
    pub elapsed: Option<Duration>,
    pub spinner_frame: char,
    pub hints: Vec<(String, String)>,
}

/// A field of the line, in drop order.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Field {
    Product,
    Cwd,
    Model,
    Context,
    Cost,
    Elapsed,
    Hints,
}

/// PURE: the fields that survive at `width`, in drop order. Nothing overflows, nothing wraps —
/// the status line is exactly one row (M9).
pub fn fields(v: &StatusView, width: u16) -> Vec<Field> {
    let _ = (v, width);
    todo!("WP-4")
}

/// PURE: `(view, width, theme) -> Line`. Every span names a theme ROLE.
pub fn status_line(v: &StatusView, width: u16, theme: &Theme) -> Line<'static> {
    let _ = (v, width, theme);
    todo!("WP-4")
}

/// PURE: a path elided in the MIDDLE (`~/repos/bou…/ux/cwd`), never at the end — the last
/// component is the one a user checks (B5).
pub fn elide_path(p: &Path, home: Option<&Path>, max: u16) -> String {
    let _ = (p, home, max);
    todo!("WP-4")
}
