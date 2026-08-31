//! §0.2 runtime invariant for `bough-plugin-tui-strip`:
//!
//! **A rendered `state` half comes only from an `about/line` step that cites at least one step.**
//! §16's cited-truth rule, enforced at the surface: the rail is where a claim about what an agent
//! did is most likely to be read as fact, so an uncited claim must never reach it.
//!
//! The recorder is a process-wide slot holding what the LAST frame drew. `render` is synchronous
//! and cannot await a check, and the invariant is a property of what reached the screen — not of
//! what the ledger holds — so the frame is what is recorded and the quiesce check is what reads it.

use bough_kernel::{Cadence, Context, InvariantSpec, InvariantViolation};
use parking_lot::Mutex;

use crate::rail::RailRow;
use crate::AboutView;

/// What the last rendered frame put on the rail: the agent's name, and the about-line it drew.
static LAST_FRAME: Mutex<Vec<(String, AboutView)>> = Mutex::new(Vec::new());

/// Record what this frame drew. Called from `StripPane::render`.
pub fn record_frame(rows: &[RailRow]) {
    let drawn: Vec<(String, AboutView)> = rows
        .iter()
        .filter_map(|r| r.about.clone().map(|v| (r.name.clone(), v)))
        .collect();
    *LAST_FRAME.lock() = drawn;
}

/// What the last frame drew, for the check and for tests.
pub fn last_frame() -> Vec<(String, AboutView)> {
    LAST_FRAME.lock().clone()
}

/// PURE: the check, over what the rail rendered this frame.
///
/// An empty state half is fine — it says nothing. A NON-EMPTY one is a truth claim, and §16 says a
/// truth claim carries its citations.
pub fn check_rendered(rendered: &[(String, AboutView)]) -> Result<(), String> {
    let bad: Vec<&str> = rendered
        .iter()
        .filter(|(_, v)| !v.state.trim().is_empty() && v.cites.is_empty())
        .map(|(name, _)| name.as_str())
        .collect();
    if bad.is_empty() {
        return Ok(());
    }
    Err(format!(
        "the rail drew an UNCITED state half for {}: §16 — a claim rendered as truth carries its \
         citations, and `about/line` is EVIDENCE precisely so the ledger refuses one without them",
        bad.join(", ")
    ))
}

/// The specs this crate contributes.
pub fn specs() -> Vec<InvariantSpec> {
    vec![InvariantSpec {
        name: "a_rendered_state_half_is_always_cited",
        plugin: crate::PLUGIN_NAME,
        cadence: Cadence::OnQuiesce,
        check: |ctx: Context| Box::pin(run(ctx)),
    }]
}

async fn run(ctx: Context) -> Result<(), InvariantViolation> {
    check_rendered(&last_frame()).map_err(|detail| InvariantViolation {
        invariant: "a_rendered_state_half_is_always_cited",
        plugin: crate::PLUGIN_NAME,
        entry: ctx.entry_id().clone(),
        detail,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use bough_plugin_ledger::{Cite, Ref};

    fn view(state: &str, cites: Vec<Cite>) -> AboutView {
        AboutView {
            state: state.into(),
            intent: "next".into(),
            cites,
        }
    }

    #[test]
    fn an_uncited_state_half_is_a_violation_and_a_cited_one_is_not() {
        let cited = vec![Cite {
            r#ref: Ref::new("step:s1"),
            url: None,
        }];
        check_rendered(&[("sol".into(), view("rebased the loop", cited))]).unwrap();
        // Nothing claimed, nothing to cite.
        check_rendered(&[("sol".into(), view("   ", vec![]))]).unwrap();
        let err = check_rendered(&[("sol".into(), view("rebased the loop", vec![]))])
            .expect_err("an uncited claim on the rail must be a violation");
        assert!(err.contains("sol"), "the violation names the agent: {err}");
    }
}
