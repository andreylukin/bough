//! §0.2 runtime invariant for `bough-plugin-llm`:
//!
//! **Every `llm/stream` stream ends with exactly ONE terminal chunk, and nothing follows it.**
//!
//! The seam wraps every stream it hands out and records the chunk shape it saw, so a provider
//! that yields two `End`s, an `End` after a `Failed`, or nothing at all is reported here rather
//! than being discovered as a hung wake. WP-1 owns the recorder and the check.

use bough_kernel::{Cadence, Context, FiberUid, InvariantSpec, InvariantViolation};

/// One observed stream, as the seam's wrapper saw it.
#[derive(Clone, Debug, PartialEq)]
pub struct Obs {
    pub fiber: FiberUid,
    /// The request digest, so a violation names the request it belongs to.
    pub request: String,
    /// How many terminal chunks the stream carried.
    pub terminals: u32,
    /// How many chunks arrived AFTER the first terminal one.
    pub after_terminal: u32,
}

/// What the seam's wrapper recorded this session, in completion order.
static SEEN: parking_lot::Mutex<Vec<Obs>> = parking_lot::Mutex::new(Vec::new());

/// Record one finished stream. Called by the seam's wrapper when the stream is dropped or ends.
pub fn record(obs: Obs) {
    SEEN.lock().push(obs);
}

/// Forget everything recorded for `fiber` (registered as an inverse by `apply`).
pub fn forget(fiber: FiberUid) {
    SEEN.lock().retain(|o| o.fiber != fiber);
}

/// Everything recorded so far, oldest first.
pub fn seen() -> Vec<Obs> {
    SEEN.lock().clone()
}

/// Drop the record. Test setup only.
pub fn clear() {
    SEEN.lock().clear();
}

/// The whole invariant as a pure function of the observed stream.
pub fn evaluate(stream: &[Obs]) -> Result<(), String> {
    for o in stream {
        if o.terminals == 0 {
            return Err(format!(
                "the stream for request `{}` ended with NO terminal chunk; a consumer would wait \
                 forever for an answer that already stopped arriving",
                o.request
            ));
        }
        if o.terminals > 1 {
            return Err(format!(
                "the stream for request `{}` carried {} terminal chunks; exactly one ends a \
                 stream (§12)",
                o.request, o.terminals
            ));
        }
        if o.after_terminal > 0 {
            return Err(format!(
                "the stream for request `{}` yielded {} chunk(s) AFTER its terminal chunk",
                o.request, o.after_terminal
            ));
        }
    }
    Ok(())
}

/// The spec `LlmPlugin::invariants` returns.
pub fn every_stream_ends_once() -> InvariantSpec {
    InvariantSpec {
        name: "every_stream_ends_with_exactly_one_terminal_chunk",
        plugin: PLUGIN,
        cadence: Cadence::OnQuiesce,
        check: |ctx| Box::pin(check(ctx)),
    }
}

const PLUGIN: &str = crate::PLUGIN_NAME;

async fn check(ctx: Context) -> Result<(), InvariantViolation> {
    evaluate(&seen()).map_err(|detail| InvariantViolation {
        invariant: "every_stream_ends_with_exactly_one_terminal_chunk",
        plugin: PLUGIN,
        entry: ctx.entry_id().clone(),
        detail,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs(terminals: u32, after_terminal: u32) -> Obs {
        Obs {
            fiber: FiberUid(1),
            request: "deadbeef".into(),
            terminals,
            after_terminal,
        }
    }

    #[test]
    fn exactly_one_terminal_chunk_is_clean() {
        assert_eq!(evaluate(&[obs(1, 0), obs(1, 0)]), Ok(()));
        assert_eq!(evaluate(&[]), Ok(()), "an idle session is vacuously clean");
    }

    #[test]
    fn a_stream_with_no_terminal_chunk_is_a_violation() {
        let detail = evaluate(&[obs(0, 0)]).expect_err("a hung stream must be reported");
        assert!(detail.contains("NO terminal chunk"), "{detail}");
    }

    #[test]
    fn a_second_terminal_chunk_is_a_violation() {
        let detail = evaluate(&[obs(2, 1)]).expect_err("two terminals must be reported");
        assert!(detail.contains("2 terminal chunks"), "{detail}");
    }

    #[test]
    fn a_chunk_after_the_terminal_one_is_a_violation() {
        let detail = evaluate(&[obs(1, 3)]).expect_err("trailing chunks must be reported");
        assert!(detail.contains("AFTER its terminal chunk"), "{detail}");
    }

    #[test]
    fn forgetting_a_fiber_drops_only_its_records() {
        clear();
        record(Obs {
            fiber: FiberUid(1),
            ..obs(1, 0)
        });
        record(Obs {
            fiber: FiberUid(2),
            ..obs(1, 0)
        });
        forget(FiberUid(1));
        assert_eq!(seen().len(), 1);
        assert_eq!(seen()[0].fiber, FiberUid(2));
        clear();
    }
}
