//! WP-2, the crate's invariant end to end: model-visible ⟺ ledgered under code mode.
//!
//! For every `program` id the ordered concatenation of its `program/console` chunks equals the
//! `tool/result` content of the `run` call (modulo the truncation notice), and every
//! `program/call` has exactly one `program/result` with the same `index`. The checks below read
//! the LEDGER — not the consumer's own bookkeeping — so a step that was never appended fails
//! them.

mod support;

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
