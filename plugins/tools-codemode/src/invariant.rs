//! §0.2 runtime invariant for `bough-plugin-tools-codemode`:
//!
//! **Model-visible ⟺ ledgered, under code mode too.** For every `program` id, the ordered
//! concatenation of its `program/console` chunks equals the `tool/result` content of the `run`
//! call (modulo the truncation notice), and every `program/call` has exactly one
//! `program/result` with the same `index` before that `tool/result`.
//!
//! Without this check, a program could do work the ledger never saw — which is exactly the
//! failure mode a single `run(program)` tool invites. WP-2 owns the recorder and the wiring.

use bough_kernel::{Cadence, Context, FiberUid, InvariantSpec, InvariantViolation};

/// One finished program, as the consumer saw it.
#[derive(Clone, Debug, PartialEq)]
pub struct Obs {
    pub fiber: FiberUid,
    /// The `run` call id.
    pub program: String,
    /// The `index` of every `program/call` appended, in issue order.
    pub calls: Vec<u32>,
    /// The `index` of every `program/result` appended, in append order.
    pub results: Vec<u32>,
    /// The concatenation of the `program/console` chunks.
    pub console: String,
    /// The `run` call's `tool/result` content.
    pub result_content: String,
}

static SEEN: parking_lot::Mutex<Vec<Obs>> = parking_lot::Mutex::new(Vec::new());

/// Record one finished program.
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

/// The whole invariant as a pure function of the observed programs.
pub fn evaluate(programs: &[Obs]) -> Result<(), String> {
    for o in programs {
        for c in &o.calls {
            let n = o.results.iter().filter(|r| *r == c).count();
            if n != 1 {
                return Err(format!(
                    "program `{}`: the call at index {c} has {n} program/result step(s); exactly \
                     one answers each call",
                    o.program
                ));
            }
        }
        for r in &o.results {
            if !o.calls.contains(r) {
                return Err(format!(
                    "program `{}`: a program/result at index {r} answers no program/call",
                    o.program
                ));
            }
        }
        if o.console != o.result_content {
            return Err(format!(
                "program `{}`: the console chunks ({} bytes) do not reconstruct the tool/result \
                 content ({} bytes); the model saw something the ledger does not hold",
                o.program,
                o.console.len(),
                o.result_content.len()
            ));
        }
    }
    Ok(())
}

/// The spec `CodemodePlugin::invariants` returns.
pub fn every_program_call_is_ledgered() -> InvariantSpec {
    InvariantSpec {
        name: "every_program_call_is_ledgered_and_console_reconstructs_the_result",
        plugin: PLUGIN,
        cadence: Cadence::OnQuiesce,
        check: |ctx| Box::pin(check(ctx)),
    }
}

const PLUGIN: &str = crate::PLUGIN_NAME;

async fn check(ctx: Context) -> Result<(), InvariantViolation> {
    evaluate(&seen()).map_err(|detail| InvariantViolation {
        invariant: "every_program_call_is_ledgered_and_console_reconstructs_the_result",
        plugin: PLUGIN,
        entry: ctx.entry_id().clone(),
        detail,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs(calls: Vec<u32>, results: Vec<u32>, console: &str, content: &str) -> Obs {
        Obs {
            fiber: FiberUid(1),
            program: "call_1".into(),
            calls,
            results,
            console: console.into(),
            result_content: content.into(),
        }
    }

    #[test]
    fn a_paired_program_is_clean() {
        assert_eq!(
            evaluate(&[obs(vec![0, 1], vec![0, 1], "hi\n", "hi\n")]),
            Ok(())
        );
        assert_eq!(evaluate(&[]), Ok(()), "an idle session is vacuously clean");
    }

    #[test]
    fn an_unanswered_call_is_a_violation() {
        let d = evaluate(&[obs(vec![0, 1], vec![0], "", "")])
            .expect_err("an unanswered inner call must be reported");
        assert!(d.contains("index 1"), "{d}");
    }

    #[test]
    fn an_orphan_result_is_a_violation() {
        let d = evaluate(&[obs(vec![0], vec![0, 2], "", "")])
            .expect_err("an orphan result must be reported");
        assert!(d.contains("answers no program/call"), "{d}");
    }

    #[test]
    fn console_that_does_not_reconstruct_the_result_is_a_violation() {
        let d = evaluate(&[obs(vec![], vec![], "a", "b")])
            .expect_err("a divergent console must be reported");
        assert!(d.contains("do not reconstruct"), "{d}");
    }
}
