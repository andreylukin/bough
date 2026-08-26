//! Invariant: the two halves of an about-line are read out of the ledger by step-type NAME
//! (P3-D11), and the INTENT half is a self-declaration — every renderer draws it under its label,
//! never as truth (§2).
//!
//! It lives in the render library rather than in `tui-strip` because both panes read it and
//! nothing may depend on a pane crate (§1, dependency direction).

use bough_plugin_ledger::{Cite, Step};

/// The two halves of an about-line, plus what the state half cites.
#[derive(Clone, Debug, PartialEq)]
pub struct AboutView {
    pub state: String,
    pub intent: String,
    pub cites: Vec<Cite>,
}

/// PURE: an `about/line` step ⇒ its two halves. `None` for any other step type.
pub fn about_from_step(_step: &Step) -> Option<AboutView> {
    todo!("WP-4")
}
