//! §0.2 runtime invariant for `bough-plugin-tui-strip`:
//!
//! **A rendered `state` half comes only from an `about/line` step that cites at least one step.**
//! §16's cited-truth rule, enforced at the surface: the rail is where a claim about what an agent
//! did is most likely to be read as fact, so an uncited claim must never reach it.
//!
//! WP-4 owns the recorder and the check.

use bough_kernel::{Cadence, Context, InvariantSpec, InvariantViolation};

/// PURE: the check, over what the rail rendered this frame.
pub fn check_rendered(_rendered: &[(String, crate::AboutView)]) -> Result<(), String> {
    todo!("WP-4")
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

async fn run(_ctx: Context) -> Result<(), InvariantViolation> {
    todo!("WP-4")
}
