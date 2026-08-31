//! WP-2, the crate's invariant end to end: model-visible ⟺ ledgered under code mode.
//!
//! For every `program` id the ordered concatenation of its `program/console` chunks equals the
//! `tool/result` content of the `run` call (modulo the truncation notice), and every
//! `program/call` has exactly one `program/result` with the same `index`. The checks below read
//! the LEDGER — not the consumer's own bookkeeping — so a step that was never appended fails
//! them.

use crate::support;

use std::sync::Arc;

use bough_plugin_tools_codemode::invariant::{evaluate, Obs};
use support::{config, harness, spec, Echo};

fn echo() -> Arc<dyn bough_plugin_tools::Tool> {
    Arc::new(Echo { concludes: false })
}

/// The four fields the invariant needs, read back out of the ledger.
async fn observed(h: &support::Harness, content: &str) -> Obs {
    let calls = h.steps("program/call").await;
    let results = h.steps("program/result").await;
    let console: String = h
        .steps("program/console")
        .await
        .iter()
        .map(|s| s.body["text"].as_str().unwrap_or_default().to_string())
        .collect();
    Obs {
        fiber: bough_kernel::FiberUid(1),
        program: "call_1".to_string(),
        calls: calls
            .iter()
            .map(|s| s.body["index"].as_u64().unwrap() as u32)
            .collect(),
        results: results
            .iter()
            .map(|s| s.body["index"].as_u64().unwrap() as u32)
            .collect(),
        console,
        error: None,
        result_content: content.to_string(),
    }
}

#[tokio::test]
async fn the_console_chunks_reconstruct_the_tool_result_and_every_call_is_answered() {
    let h = harness(vec![spec("echo", echo())], config()).await;
    let out = h
        .program("log first\ncall echo [{\"n\":1}]\nlog between\ncall echo [{\"n\":2}]\nlog last")
        .await
        .unwrap();

    let obs = observed(&h, &out.content).await;
    assert_eq!(obs.calls, vec![0, 1], "ids are minted in issue order");
    assert_eq!(obs.results, vec![0, 1]);
    assert_eq!(evaluate(std::slice::from_ref(&obs)), Ok(()));
    assert!(obs.console.starts_with("first\n"), "{:?}", obs.console);
    assert!(obs.console.ends_with("last\n"), "{:?}", obs.console);
}

#[tokio::test]
async fn the_inner_call_ids_are_the_deterministic_program_dot_n() {
    let h = harness(vec![spec("echo", echo())], config()).await;
    h.program("call echo []\ncall echo []").await.unwrap();
    let ids: Vec<String> = h
        .steps("program/call")
        .await
        .iter()
        .map(|s| s.body["call"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids, vec!["call_1.0".to_string(), "call_1.1".to_string()]);
    let answered: Vec<String> = h
        .steps("program/result")
        .await
        .iter()
        .map(|s| s.body["call"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(answered, ids, "one result per call, same id");
}

#[tokio::test]
async fn a_truncated_program_still_reconstructs_from_its_chunks() {
    let mut cfg = config();
    cfg.max_console_bytes = 40;
    let h = harness(vec![spec("echo", echo())], cfg).await;
    let mut src = String::new();
    for i in 0..40 {
        src.push_str(&format!("log line-{i:03}\n"));
    }
    let out = h.program(&src).await.unwrap();

    assert!(out.content.contains("bytes elided"), "{:?}", out.content);
    let obs = observed(&h, &out.content).await;
    assert_eq!(
        evaluate(std::slice::from_ref(&obs)),
        Ok(()),
        "the notice is a CHUNK, so the concatenation still equals the result"
    );
    assert!(obs.console.starts_with("line-000\n"), "{:?}", obs.console);
    assert!(obs.console.ends_with("line-039\n"), "{:?}", obs.console);
    let notices = h
        .steps("program/console")
        .await
        .into_iter()
        .filter(|s| s.body["dropped_bytes"].as_u64().unwrap_or(0) > 0)
        .count();
    assert_eq!(notices, 1, "exactly one chunk names the dropped bytes");
}

#[tokio::test]
async fn a_program_that_throws_still_ledgers_what_it_did() {
    let h = harness(vec![spec("echo", echo())], config()).await;
    let failure = h
        .program("call echo []\nlog printed\nthrow boom")
        .await
        .expect_err("an uncaught throw ends the round");
    assert!(failure.message.contains("printed"), "{failure:?}");
    let obs = observed(&h, &failure.message).await;
    assert_eq!(obs.calls, vec![0]);
    assert_eq!(obs.results, vec![0]);
    assert_eq!(h.steps("program/error").await.len(), 1);
}

/// A SUCCESSFUL program reports its cost in the `tool/result`'s `value`.
///
/// The TUI's program row reads `ms` from exactly that field (`tui-focus::rows`), and the success
/// arm used to return `value: None` — so every real code-mode program rendered with no duration
/// while `tui-focus/tests/program.rs` passed against a fixture the product never wrote. The
/// model is unaffected either way: `run` answers with the console.
#[tokio::test]
async fn a_successful_program_reports_its_ms_and_ops_in_the_result_value() {
    let h = harness(vec![spec("echo", echo())], config()).await;
    let out = h
        .program("took 1200 4200\ncall echo []\nlog done")
        .await
        .expect("the program succeeds");
    let value = out
        .value
        .clone()
        .expect("a successful program carries its cost");
    assert_eq!(
        value["ms"], 1200,
        "the program row folds `ms` off this: {value}"
    );
    assert_eq!(value["ops"], 4200, "{value}");
    assert!(
        out.content.contains("done"),
        "and the console is still what the model gets: {:?}",
        out.content
    );
}

/// The invariant's own OBSERVATION must come from the ledger, not from the buffer that produced
/// the tool result.
///
/// `run` used to record `console: console.clone(), result_content: console.clone()` — the same
/// String on both sides — so the crate's headline clause (`o.console != o.result_content`) could
/// never fire and proved nothing; and it was knowingly wrong on the two failing paths, where the
/// model receives the console PLUS a terminal message. This pins both halves: the recorded
/// console is the one the ledger holds, the recorded result is the one the model got, and they
/// are genuinely different strings on the throwing path.
#[tokio::test]
async fn the_recorded_observation_is_read_from_the_ledger_and_not_from_the_result_buffer() {
    // The record is process-global and the cases in this binary run in parallel, so this program
    // is selected by the bytes it produced rather than by clearing the stream.
    let h = harness(vec![spec("echo", echo())], config()).await;
    let failure = h
        .program("call echo []\nlog printed\nthrow boom")
        .await
        .expect_err("an uncaught throw ends the round");

    let recorded = bough_plugin_tools_codemode::invariant::seen();
    let obs = recorded
        .iter()
        .find(|o| o.result_content == failure.message)
        .expect("this program's observation is on the record");

    // The two sides are NOT the same string: the comparison has something to fail on.
    assert_ne!(
        obs.console, obs.result_content,
        "the failing path hands the model more than the console"
    );
    assert_eq!(
        obs.result_content, failure.message,
        "the recorded result must be the bytes the model received"
    );
    assert_eq!(
        obs.error.as_deref().map(|e| !e.is_empty()),
        Some(true),
        "the terminal message is recorded as the difference it is"
    );

    // …and the console half is the LEDGER's, chunk for chunk.
    let ledgered: String = h
        .steps("program/console")
        .await
        .iter()
        .map(|s| s.body["text"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(obs.console, ledgered);
    assert!(obs.console.contains("printed"), "{:?}", obs.console);
    assert_eq!(obs.calls, vec![0], "read back off `program/call`");
    assert_eq!(obs.results, vec![0]);
    assert_eq!(evaluate(std::slice::from_ref(obs)), Ok(()));
}

/// The preflight is given the roster it is about to inject.
///
/// `Run::call` used to preflight with an EMPTY bound list, before the snapshot existed — so the
/// engine's shadowed-binding diagnostic (`preflight::syntax_error_message`'s `bound` branch,
/// the plan's §9 deviation) could never fire in production, whatever a program declared. Its
/// tests passed a roster to the pure function by hand and proved the message, never the wiring.
#[tokio::test]
async fn the_preflight_is_given_the_names_the_sandbox_will_inject() {
    let h = harness(vec![spec("echo", echo())], config()).await;
    h.program("log hello").await.unwrap();
    assert_eq!(
        support::preflighted_with("log hello"),
        vec!["echo".to_string()],
        "the preflight must see the globals, not an empty list"
    );

    // …and a program that shadows one of them is refused, naming it.
    let failure = h
        .program("!!shadow echo\nlog never")
        .await
        .expect_err("a shadowed binding does not parse");
    assert!(
        failure.message.contains("`echo` is already bound"),
        "{}",
        failure.message
    );
}

/// A host call still in flight when the round closes is answered BY THE ROUND, and its own late
/// append is refused.
///
/// `js-quickjs`'s `run_one` drops the program future when the wall clock or a cancel wins its
/// `select!`, but each host call was handed to the CALLER's runtime and is not cancelled by that
/// drop. It kept `Arc<ProgramCx>` and appended its `program/result` after `Run::call` had already
/// read the state, disposed the mirror and returned its `tool/result` — a sub-step outside its
/// call, and an observation whose call had no result, which the crate's invariant reports as a
/// product violation for what is a race.
#[tokio::test]
async fn a_call_still_in_flight_when_the_round_closes_is_settled_not_left_dangling() {
    struct Slow;

    #[async_trait::async_trait]
    impl bough_plugin_tools::Tool for Slow {
        async fn call(
            &self,
            _call: std::sync::Arc<bough_plugin_tools::ToolCall>,
            _cx: bough_plugin_tools::ToolCx,
        ) -> Result<bough_plugin_tools::ToolOutcome, bough_plugin_tools::ToolFailure> {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            Ok(bough_plugin_tools::ToolOutcome {
                content: "too late".to_string(),
                value: None,
                cites: vec![],
                concludes_wake: false,
            })
        }
    }

    let h = harness(vec![spec("slow", std::sync::Arc::new(Slow))], config()).await;
    h.program("detach slow []\nlog done").await.unwrap();

    let calls = h.steps("program/call").await;
    let results = h.steps("program/result").await;
    assert_eq!(calls.len(), 1, "the detached call was recorded");
    assert_eq!(
        results.len(),
        1,
        "and answered by the round, not left dangling"
    );
    assert_eq!(
        results[0].body["outcome"], "error",
        "the settled answer says the round ended first: {}",
        results[0].body
    );
    // The invariant sees a paired program, not a violation manufactured by the race.
    let obs = observed(&h, "done\n").await;
    assert_eq!(
        bough_plugin_tools_codemode::invariant::evaluate(&[obs]),
        Ok(())
    );

    // And when the detached call finally finishes, its own step is REFUSED: nothing lands after
    // the round's `tool/result`.
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    assert_eq!(h.steps("program/result").await.len(), 1, "no late sub-step");
    assert_eq!(h.steps("program/call").await.len(), 1);
}
