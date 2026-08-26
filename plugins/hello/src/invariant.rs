//! §0.2 runtime invariant for `bough-plugin-hello`:
//!
//! **Every `hello/greeted` payload carries a `seq` strictly greater than the previous one for the
//! same fiber uid.**
//!
//! `hello` owns that stream, so it is authoritative about it. `HelloConfig::plant_violation` makes
//! the plugin emit a repeated seq on purpose; that is the planted violation V9 detects, and the
//! reason this file holds a real check rather than a placeholder (Phase 8 audits these).

use std::collections::HashMap;

use bough_kernel::{Cadence, Context, FiberUid, InvariantSpec, InvariantViolation};
use parking_lot::Mutex;

/// What the `hello/greeted` listener has seen, in arrival order. The invariant is a statement
/// about this stream, so the check reads exactly what was observed and nothing else.
static SEEN: Mutex<Vec<(FiberUid, u64)>> = Mutex::new(Vec::new());

/// Record one payload. Called from the listener `HelloPlugin::apply` registers.
pub fn record(fiber: FiberUid, seq: u64) {
    SEEN.lock().push((fiber, seq));
}

/// Everything recorded so far, oldest first.
pub fn seen() -> Vec<(FiberUid, u64)> {
    SEEN.lock().clone()
}

/// Drop the recorded stream. Test setup only.
pub fn clear() {
    SEEN.lock().clear();
}

/// The spec `HelloPlugin::invariants` returns.
pub fn greeted_seq_is_monotonic() -> InvariantSpec {
    InvariantSpec {
        name: "greeted_seq_is_monotonic",
        plugin: "hello",
        cadence: Cadence::OnQuiesce,
        check: |ctx| Box::pin(check(ctx)),
    }
}

/// The whole invariant, as a pure function of the observed stream: the first regression wins, and
/// the report names the fiber, the seq that regressed and the high-water mark it failed to beat.
pub fn evaluate(stream: &[(FiberUid, u64)]) -> Result<(), String> {
    let mut high: HashMap<FiberUid, u64> = HashMap::new();
    for (fiber, seq) in stream {
        match high.get(fiber) {
            Some(prev) if seq <= prev => {
                return Err(format!(
                    "fiber {fiber:?} emitted hello/greeted seq {seq} after seq {prev}; \
                     seq must be strictly increasing per fiber"
                ));
            }
            _ => {
                high.insert(*fiber, *seq);
            }
        }
    }
    Ok(())
}

/// Read the stream recorded by the `hello/greeted` listener and report the first regression.
async fn check(ctx: Context) -> Result<(), InvariantViolation> {
    evaluate(&seen()).map_err(|detail| InvariantViolation {
        invariant: "greeted_seq_is_monotonic",
        plugin: "hello",
        entry: ctx.entry_id().clone(),
        detail,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn greeted_seq_is_monotonic() {
        let a = FiberUid(1);
        let b = FiberUid(2);
        // Interleaved fibers: each stream is judged on its own, so b's low seqs are not a
        // regression of a's.
        let stream = vec![(a, 1), (b, 1), (a, 2), (b, 2), (a, 7)];
        assert_eq!(super::evaluate(&stream), Ok(()));
    }

    #[test]
    fn planted_violation_is_detected() {
        let a = FiberUid(1);
        // Exactly what `plant_violation: true` produces: the seq repeats instead of advancing.
        let stream = vec![(a, 1), (a, 1)];
        let detail = super::evaluate(&stream).expect_err("a repeated seq must be a violation");
        assert!(detail.contains("seq 1"), "unhelpful detail: {detail}");
        assert!(
            detail.contains("strictly increasing"),
            "the detail must state the invariant: {detail}"
        );
        // A non-advancing seq counts too, not only an equal one.
        assert!(super::evaluate(&[(a, 5), (a, 4)]).is_err());
    }
}
