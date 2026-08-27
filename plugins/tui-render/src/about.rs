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

/// How much of the state half can reach the screen. Generous: the rail clips to its own width,
/// and this bound exists only so one runaway line cannot become the whole pane.
pub const STATE_MAX_CHARS: usize = 200;

/// PURE: an `about/line` step ⇒ its two halves. `None` for any other step type, and `None` for an
/// `about/line` whose body is not the shape the writer declared — a renderer that guessed at a
/// malformed body would put an uncited claim on the rail.
pub fn about_from_step(step: &Step) -> Option<AboutView> {
    if step.kind.as_str() != ABOUT_LINE {
        return None;
    }
    // phase ux1 (minor 29): the state half reaches the SCREEN as one clean sentence — markdown
    // markers stripped, clause splices dropped, whitespace collapsed. The ledger keeps what the
    // writer wrote; only the rendering is cleaned, so no evidence is rewritten.
    let state = bough_util::text::one_sentence(step.body.get("state")?.as_str()?, STATE_MAX_CHARS);
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

#[cfg(test)]
mod tests {
    use super::*;
    use bough_plugin_ledger::{Class, Seq, StepId, StepType, TrajId, WakeId};
    use std::collections::BTreeSet;
    use std::sync::Arc;

    fn about_step(state: &str) -> Step {
        Step {
            id: StepId::new("s1"),
            traj: TrajId::new("lane/terra"),
            seq: Seq(1),
            at: chrono::Utc::now(),
            wake: WakeId::new("w1"),
            kind: StepType::new(ABOUT_LINE),
            class: Class::Evidence,
            body: Arc::new(serde_json::json!({ "state": state, "intent": "" })),
            cites: Arc::new(vec![]),
            refs: Arc::new(BTreeSet::new()),
            ignorable: false,
        }
    }

    /// phase ux1 (minor 29): the audit's own line, cleaned on the way to the screen.
    #[test]
    fn the_state_half_reaches_the_screen_as_one_clean_sentence() {
        let v = about_from_step(&about_step("read mail `say hi`; Hi; ! \u{1f44b} ; **")).unwrap();
        assert_eq!(v.state, "read mail say hi");
        assert!(!v.state.contains('`') && !v.state.contains('*') && !v.state.contains(';'));
    }
}
