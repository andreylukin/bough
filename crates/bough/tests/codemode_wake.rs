//! V6 — a turn ends WITHOUT a stop tool.
//!
//! Main's code mode shipped two API tools, `run_steps(program)` and `stop`. This phase ships one:
//! the model ends its turn by answering in text, and the wake ends because the agent-loop's
//! wake-stopping listeners say it did (§5). A `stop` tool would be a second way to say the same
//! thing, and a model that forgot to call it would hang.
//!
//! These drive the REAL BINARY, because the thing under test is "does the turn end" — a property
//! of the process, not of a function. Each test writes its own recorded transcript, so the round
//! shapes are visible next to the assertion instead of in a shared fixture.
//!
//! The programs call `view`, not `bash`. That is not a preference: with `tags_required` on, no
//! registered tool has a `tags` property (`tools-baseline`'s `bash` is `{command, cwd}` and
//! `tools-operator` registers no `bash`/`sh`), so every shell call in the sandbox is refused
//! today. `docs/codemode-merge-notes.md` §9 records it. What these tests are about — the turn
//! ending with no `stop` tool — is independent of which host function the program called.

mod support;

use support::codemode::{answer_round, program_round, Sandbox};

use std::collections::BTreeSet;

use bough_plugin_hello::trace;
use bough_plugin_ledger::AgentName;
use bough_plugin_tools::{Tools, ToolsHandle};

// ---------------------------------------------------------------------------------------------
// A code-mode `bough exec`, out of process.

// ---------------------------------------------------------------------------------------------

/// The shape the phase is built on: program, then text, then the wake is over. No tool said so.
#[test]
fn a_program_then_text_wake_ends_by_wake_stopping() {
    let sb = Sandbox::new("ends");
    let (code, out) = sb.exec(
        "read the fixture readme",
        serde_json::json!([
            program_round("c0", "console.log(await view(\"README.md\"))"),
            answer_round("three."),
        ]),
    );
    assert_eq!(code, 0, "the turn must end cleanly:\n{out}");
    assert!(
        out.contains("three."),
        "the answer must reach stdout:\n{out}"
    );

    let kinds = sb.kinds();
    assert!(kinds.contains(&"program/call".to_string()), "{kinds:?}");
    let end = sb
        .steps()
        .into_iter()
        .find(|(k, _)| k == "wake/end")
        .expect("the wake must end");
    assert_eq!(
        end.1["reason"], "completed",
        "the wake ended because the model stopped calling tools, not because it was cut off"
    );
    // And the text round called nothing at all: exactly one `run` call in the whole wake.
    assert_eq!(
        kinds.iter().filter(|k| *k == "tool/call").count(),
        1,
        "one API call for the program and none for the answer: {kinds:?}"
    );
}

/// A program with no host call is still a complete step: the `run` tool ran, its console came
/// back, and the round closed. Nothing waits for a call that never comes.
#[test]
fn a_program_that_calls_nothing_still_ends_its_step() {
    let sb = Sandbox::new("silent");
    let (code, out) = sb.exec(
        "say something",
        serde_json::json!([
            program_round("c0", "console.log(1 + 1)"),
            answer_round("two."),
        ]),
    );
    assert_eq!(code, 0, "{out}");

    let steps = sb.steps();
    let result = steps
        .iter()
        .find(|(k, b)| k == "tool/result" && b["name"] == "run")
        .expect("the `run` call got a result");
    assert_eq!(
        result.1["outcome"], "ok",
        "a call-free program is not an error"
    );
    assert!(
        result.1["content"]
            .as_str()
            .unwrap_or_default()
            .contains('2'),
        "the console is what comes back: {}",
        result.1["content"]
    );
    assert!(
        !steps.iter().any(|(k, _)| k == "program/call"),
        "a program that calls nothing appends no `program/call`"
    );
    assert_eq!(
        steps.iter().filter(|(k, _)| k == "step/end").count(),
        2,
        "both rounds closed their step"
    );
}

/// No `stop` tool anywhere, under EITHER surface. Main shipped one; this phase deliberately does
/// not, and a `stop` that crept back in would be a second, silent way to end a turn.
#[tokio::test(flavor = "multi_thread")]
async fn no_stop_tool_is_registered_by_either_consumer() {
    let _guard = trace::test_lock();
    for profile in ["headless", "codemode"] {
        let (kernel, _dir) = support::boot_real(profile, &[]).await;
        let tools = kernel
            .root()
            .peek_live::<Tools>()
            .expect("`tools` is bound") as std::sync::Arc<ToolsHandle>;
        let all: BTreeSet<String> = tools
            .visible(&AgentName::new("sol"))
            .into_iter()
            .map(|n| n.to_string())
            .collect();
        for banned in ["stop", "run_steps", "end_turn"] {
            assert!(
                !all.contains(banned),
                "`{banned}` is registered under `{profile}`: {all:?}"
            );
        }
        kernel.shutdown().await;
    }
}

/// The failure this design has to rule out: a model that never signals the end. There is nothing
/// to signal, so the turn ends when the answer round does — and `exec` returns.
#[test]
fn a_wake_never_hangs_waiting_for_a_stop() {
    let sb = Sandbox::new("nohang");
    let started = std::time::Instant::now();
    let (code, out) = sb.exec(
        "do two programs then answer",
        serde_json::json!([
            program_round("c0", "console.log(await view(\"README.md\"))"),
            program_round("c1", "console.log(await view(\"README.md\"))"),
            answer_round("done."),
        ]),
    );
    assert_eq!(code, 0, "{out}");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(120),
        "the turn took {:?}; a turn that waits for a stop tool never returns",
        started.elapsed()
    );
    let kinds = sb.kinds();
    assert_eq!(
        kinds.iter().filter(|k| *k == "tool/call").count(),
        2,
        "two programs, then the answer: {kinds:?}"
    );
    assert_eq!(
        kinds.iter().filter(|k| *k == "wake/end").count(),
        1,
        "the wake ended exactly once: {kinds:?}"
    );
}

/// §5: "a tool result carrying `concludes_wake` ends the wake at its step." Under code mode the
/// carrier is `run`, and `run` never invents the flag — it repeats what an inner result said.
///
/// The two halves are pinned where each is decidable:
/// `plugins/tools-codemode/tests/pipeline.rs::run_never_reports_concludes_wake_unless_an_inner_result_did`
/// (the program propagates it, both ways) and
/// `plugins/agent-loop/tests/flow.rs::a_concludes_wake_tool_result_ends_the_wake_at_its_step`
/// (the loop honours it). What is decidable HERE, on the real binary, is the NEGATIVE half — and
/// the test is named for what it asserts: the plan's §5 used to list it as the proof of the
/// positive claim, which no end-to-end case in this phase drives. What is decidable here is that
/// the two compose correctly: a program whose inner results do not conclude does NOT end the wake
/// at the program's step — the loop goes round again and the model gets to answer.
#[test]
fn a_non_concluding_program_does_not_end_the_wake_at_its_step() {
    let sb = Sandbox::new("conclude");
    let (code, out) = sb.exec(
        "run and then answer",
        serde_json::json!([
            program_round("c0", "console.log(await view(\"README.md\"))"),
            answer_round("hi back."),
        ]),
    );
    assert_eq!(code, 0, "{out}");

    let steps = sb.steps();
    let run_result = steps
        .iter()
        .find(|(k, b)| k == "tool/result" && b["name"] == "run")
        .expect("the `run` call got a result");
    assert_eq!(
        run_result.1["concludes_wake"], false,
        "no inner result concluded, so `run` must not claim one did"
    );
    // Every inner result agrees, which is what `run` is repeating.
    for (_, body) in steps.iter().filter(|(k, _)| k == "program/result") {
        assert_eq!(body["concludes_wake"], false, "inner: {body}");
    }
    // And because it did not conclude, the wake continued past the program's step.
    let kinds = sb.kinds();
    let program_step = kinds.iter().position(|k| k == "tool/result").unwrap();
    let wake_end = kinds.iter().position(|k| k == "wake/end").unwrap();
    assert!(
        program_step < wake_end,
        "the wake ended before the program's result — the run did not get that far: {kinds:?}\n{out}"
    );
    assert!(
        kinds[program_step..wake_end].contains(&"thought/text".to_string()),
        "the loop must have gone round again and let the model answer: {kinds:?}"
    );
}
