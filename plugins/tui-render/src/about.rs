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

/// The step type the two halves are read out of, spelled by NAME (P3-D11).
pub const ABOUT_LINE: &str = "about/line";

/// The label the INTENT half is rendered under. §2: never as truth. Spelled here rather than
/// imported from `bough-plugin-about-line`, which nothing on the render side may depend on.
pub const INTENT_LABEL: &str = "intent (self-declared)";

/// PURE: an `about/line` step ⇒ its two halves. `None` for any other step type, and `None` for an
/// `about/line` whose body is not the shape the writer declared — a renderer that guessed at a
/// malformed body would put an uncited claim on the rail.
pub fn about_from_step(step: &Step) -> Option<AboutView> {
    if step.kind.as_str() != ABOUT_LINE {
        return None;
    }
    let state = step.body.get("state")?.as_str()?.to_string();
    // The intent half is optional in practice (an agent may decline to declare one); the state
    // half is not, because it is the half that is rendered as truth.
    let intent = step
        .body
        .get("intent")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    Some(AboutView {
        state,
        intent,
        cites: step.cites.as_ref().clone(),
    })
}
