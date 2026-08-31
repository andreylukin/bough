//! One drawn clickable region of the transcript pane: where it is, and what it means. The type
//! every card and fold line mints (drafts, retry folds, context pieces); the pane turns each into
//! a `cx.hit` rectangle after scroll. It carried the claim cards too until the claims demolition
//! (2026-08-30) removed them.

use bough_plugin_tui_shell::pane::HitId;

/// One drawn button: where it is, and what it means.
#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    pub id: HitId,
    /// The card-relative line index within the frame's line list.
    pub line: u16,
    /// The column the button starts at, and how wide it is.
    pub x: u16,
    pub width: u16,
}
