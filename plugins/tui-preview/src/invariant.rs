//! §0.2 runtime invariant for `bough-plugin-tui-preview`:
//!
//! **A preview rendered AT HEAD whose `as_of` names a `request/header` in the ledger carries that
//! header's `projection_digest`.** The pane cannot render bytes the ledger does not describe:
//! if the two disagree, the pane is showing something no wake ever sent, which is exactly the lie
//! a "byte-exact preview" must never tell (§16).
//!
//! The mode qualifier is the whole of the relation, not a softening of it. D-C8 and
//! `crates/bough/tests/preview_bytes.rs` establish the opposite fact for the anchored mode: a
//! preview taken at a past `as_of` AFTER that wake assembles over the steps the ledger holds
//! NOW, so it legitimately differs from the digest the wake recorded, and the projection is not
//! replayable. An invariant that asserted equality for both modes would state a falsehood the
//! tree's own integration test disproves, so it asserts it exactly where it holds.
//!
//! The recorder is a crate-level slot rather than a service binding, for the same reason
//! `tui-timeline`'s is: there is one terminal, `render` is synchronous, and the check must not
//! have to reach back into a pane that may already be disposed.

use bough_kernel::{Cadence, Context, InvariantSpec, InvariantViolation};
use bough_plugin_ledger::{Ledger, Order, Seq, StepQuery, StepType};
use parking_lot::Mutex;

/// The invariant's name, as a violation reports it.
pub const NAME: &str = "every_render_matches_its_headers_projection_digest";

/// The step kind carrying the digest this invariant compares against.
const HEADER_KIND: &str = "request/header";

static LAST: Mutex<Option<(Seq, String, bool)>> = Mutex::new(None);

/// What the last frame put on screen: the `as_of` it assembled at, the digest of the bytes it
/// painted, and whether it was taken at HEAD (`false` for an anchored preview, which the relation
/// above deliberately does not cover). Recorded from `Pane::render`; allocation-only, no I/O.
pub fn record(as_of: Seq, digest: &str, at_head: bool) {
    *LAST.lock() = Some((as_of, digest.to_string(), at_head));
}

/// The recorded frame, if there is one.
pub fn rendered() -> Option<(Seq, String, bool)> {
    LAST.lock().clone()
}

/// Forget the recorded frame. The row's disposal path: a disabled row leaves nothing behind.
pub fn forget() {
    *LAST.lock() = None;
}

/// PURE: the check, over the rendered frame and the digest the matching `request/header` carries.
/// `None` for the header means no wake assembled at that `as_of`, which is not a violation; an
/// anchored frame (`at_head` false) is out of the relation's scope for the reason in the module
/// comment.
pub fn check_render(rendered: Option<(&str, Option<&str>, bool)>) -> Result<(), String> {
    let Some((painted, header, at_head)) = rendered else {
        return Ok(());
    };
    if !at_head {
        return Ok(());
    }
    let Some(header) = header else {
        return Ok(());
    };
    if painted == header {
        return Ok(());
    }
    Err(format!(
        "the preview painted bytes digesting to `{painted}` at an `as_of` whose `request/header` \
         records `{header}`: the pane is showing a context no request ever carried"
    ))
}

/// The specs this crate contributes.
pub fn specs() -> Vec<InvariantSpec> {
    vec![InvariantSpec {
        name: NAME,
        plugin: crate::PLUGIN_NAME,
        cadence: Cadence::OnQuiesce,
        check: |ctx: Context| Box::pin(run(ctx)),
    }]
}

async fn run(ctx: Context) -> Result<(), InvariantViolation> {
    let Some((as_of, painted, at_head)) = rendered() else {
        return Ok(());
    };
    let violation = |detail: String| InvariantViolation {
        invariant: NAME,
        plugin: crate::PLUGIN_NAME,
        entry: ctx.entry_id().clone(),
        detail,
    };
    let ledger = match ctx.get::<Ledger>() {
        Ok(l) => l,
        // No ledger bound is not this invariant's business to report.
        Err(_) => return Ok(()),
    };
    let agents = ledger
        .0
        .agents()
        .await
        .map_err(|e| violation(format!("the ledger refused the agents read: {e}")))?;
    let mut header: Option<String> = None;
    for agent in agents {
        let steps = ledger
            .0
            .steps(&StepQuery {
                trajs: vec![agent.traj.clone()],
                kinds: vec![StepType::new(HEADER_KIND)],
                class: None,
                wake: None,
                after: None,
                before: None,
                refs: Vec::new(),
                order: Order::SeqDesc,
                limit: None,
            })
            .await
            .map_err(|e| violation(format!("the ledger refused the header read: {e}")))?;
        for step in steps {
            let body = &step.body;
            if body.get("as_of").and_then(|v| v.as_u64()) == Some(as_of.0) {
                header = body
                    .get("projection_digest")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                break;
            }
        }
        if header.is_some() {
            break;
        }
    }
    check_render(Some((painted.as_str(), header.as_deref(), at_head))).map_err(violation)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_render_with_no_matching_header_is_not_a_violation() {
        assert!(check_render(None).is_ok());
        assert!(check_render(Some(("abc", None, true))).is_ok());
    }

    #[test]
    fn a_matching_digest_passes() {
        assert!(check_render(Some(("abc", Some("abc"), true))).is_ok());
    }

    #[test]
    fn a_disagreeing_digest_is_reported() {
        let err = check_render(Some(("abc", Some("def"), true))).expect_err("the digests disagree");
        assert!(err.contains("abc") && err.contains("def"), "{err}");
    }

    /// D-C8: an anchored preview assembles over the steps the ledger holds NOW and legitimately
    /// differs from the digest the wake recorded. The relation does not cover it, and asserting
    /// it would contradict `crates/bough/tests/preview_bytes.rs`.
    #[test]
    fn an_anchored_frame_that_differs_is_not_a_violation() {
        assert!(check_render(Some(("abc", Some("def"), false))).is_ok());
    }

    #[test]
    fn the_recorded_frame_is_forgotten_on_disposal() {
        record(Seq(3), "abc", true);
        assert_eq!(rendered(), Some((Seq(3), "abc".to_string(), true)));
        forget();
        assert_eq!(rendered(), None);
    }
}
