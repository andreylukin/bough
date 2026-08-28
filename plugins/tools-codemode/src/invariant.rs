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
    /// The concatenation of the `program/console` chunks, READ BACK OUT OF THE LEDGER. It must
    /// come from the durable rows and never from the buffer that produced `result_content`, or
    /// the clause below is true by construction and checks nothing.
    pub console: String,
    /// The terminal message appended to the console on the two failing paths (a cap breach, a
    /// thrown/timed-out program). `None` on the clean path. It is itself ledgered, as
    /// `program/error`.
    pub error: Option<String>,
    /// The `run` call's `tool/result` content — the bytes the MODEL received.
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
        let expected = match &o.error {
            Some(message) => format!("{}{message}", o.console),
            None => o.console.clone(),
        };
        if expected != o.result_content {
            return Err(format!(
                "program `{}`: the ledgered console ({} bytes) plus the terminal message do not \
                 reconstruct the tool/result content ({} bytes); the model saw something the \
                 ledger does not hold",
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
            error: None,
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

    /// The failing paths hand the model the console PLUS a terminal message; the message is the
    /// only legal difference, and anything else is still a violation. On the code before this,
    /// `run` recorded `console` on both sides of the comparison, so nothing here could fail.
    #[test]
    fn a_terminal_message_is_the_only_legal_difference() {
        let mut o = obs(vec![], vec![], "printed\n", "printed\nboom");
        o.error = Some("boom".into());
        assert_eq!(evaluate(std::slice::from_ref(&o)), Ok(()));

        let mut wrong = o.clone();
        wrong.result_content = "printed\nboom and a line the ledger never saw".into();
        let d = evaluate(&[wrong]).expect_err("extra model-visible bytes must be reported");
        assert!(
            d.contains("do not \n                 reconstruct") || d.contains("reconstruct"),
            "{d}"
        );

        // A console the ledger is MISSING is the append-failed case, and it must fail too.
        let mut lost = o.clone();
        lost.console = String::new();
        evaluate(&[lost]).expect_err("a console chunk that never committed must be reported");
    }

    /// The clause the crate is named for, with a divergence PLANTED in it.
    ///
    /// `run` used to build the observation with `console: console.clone(), result_content:
    /// console.clone()` — one String on both sides — so this comparison could not fail whatever
    /// the ledger held. The three cases below are the three ways the two halves can part: bytes
    /// the model saw that no `program/console` row holds, a chunk that never committed, and a
    /// terminal message claimed where none was appended.
    #[test]
    fn the_invariant_catches_a_planted_console_divergence() {
        let clean = obs(vec![], vec![], "printed\n", "printed\n");
        assert_eq!(evaluate(std::slice::from_ref(&clean)), Ok(()));

        // 1. The model was shown a line the ledger does not hold.
        let mut planted = clean.clone();
        planted.result_content = "printed\nand a line the ledger never saw".into();
        let d = evaluate(&[planted]).expect_err("model-visible bytes with no step must be caught");
        assert!(d.contains("reconstruct"), "{d}");

        // 2. A `program/console` append that did not commit.
        let mut lost = clean.clone();
        lost.console = String::new();
        evaluate(&[lost]).expect_err("a console chunk that never committed must be caught");

        // 3. A terminal message on a program that ended clean.
        let mut phantom = clean.clone();
        phantom.error = Some("boom".into());
        evaluate(&[phantom]).expect_err("an unappended terminal message must be caught");
    }

    #[test]
    fn console_that_does_not_reconstruct_the_result_is_a_violation() {
        let d = evaluate(&[obs(vec![], vec![], "a", "b")])
            .expect_err("a divergent console must be reported");
        assert!(d.contains("do not reconstruct"), "{d}");
    }
}
