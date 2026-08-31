//! Invariant: the offline arm is DETERMINISTIC. A transcript answers the same request sequence the
//! same way in every process and on every run, so two bench runs of the same fixture differ only if
//! the SURFACE differs — which is the only difference the bench is allowed to report.
//!
//! `bench_tools_runs_the_bank_through_both_consumers_offline` is `make bench-tools`. It is
//! `#[ignore]`d for the same reason `make bench` is: it drives the release binary, it takes
//! minutes, and it is a measurement rather than a regression test.

use std::path::Path;

use bough_bench_tools::bank;
use bough_bench_tools::run::{Arm, Runner};
use bough_plugin_llm::{CallConfig, LlmContentBlock, LlmMessage, LlmRequest, LlmRole};
use bough_plugin_llm_replay::{ReplayAdapter, ReplayConfig, Transcript};

/// The rounds a bench fixture (a patch layer) carries.
fn transcript_of(path: &Path) -> Transcript {
    let text = std::fs::read_to_string(path).expect("the fixture is readable");
    let doc: serde_yaml::Value = serde_yaml::from_str(&text).expect("the fixture parses as YAML");
    let rounds = doc
        .get("entries")
        .and_then(|e| e.get("llm.anthropic"))
        .and_then(|e| e.get("config"))
        .and_then(|c| c.get("rounds"))
        .cloned()
        .unwrap_or_else(|| panic!("{} has no llm.anthropic rounds", path.display()));
    Transcript::from_value(rounds).expect("the rounds parse")
}

fn request(n: usize) -> LlmRequest {
    LlmRequest {
        projection_digest: None,
        model: "claude-haiku-4-5-20251001".into(),
        system: Some("the bench".into()),
        system_volatile: None,
        messages: vec![LlmMessage {
            role: LlmRole::User,
            content: vec![LlmContentBlock::Text {
                text: format!("round {n}"),
            }],
        }],
        tools: vec![],
        call: CallConfig {
            model: "claude-haiku-4-5-20251001".into(),
            max_tokens: 8192,
            effort: None,
            tool_choice_none: false,
            meta: Default::default(),
        },
    }
}

/// Everything a fresh adapter over `t` yields for `n` successive requests, as text.
fn play(t: &Transcript) -> String {
    let cfg = std::sync::Arc::new(ReplayConfig {
        transcript: None,
        rounds: None,
        strict: true,
        models: "*".into(),
        delay_ms: 0,
    });
    let adapter = ReplayAdapter::new(cfg, t.clone());
    (0..t.rounds.len())
        .map(|i| format!("{:?}", adapter.answer(&request(i))))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn each_fixture_transcript_replays_deterministically_twice_with_identical_results() {
    let dir = bank::bench_dir();
    let tasks = bank::load(&dir.join("bank")).expect("the bank loads");
    assert!(!tasks.is_empty());
    for task in &tasks {
        for arm in Arm::BOTH {
            let path = dir.join(arm.fixtures()).join(format!("{}.yml", task.id));
            let t = transcript_of(&path);
            assert!(
                !t.rounds.is_empty(),
                "{} records no round at all",
                path.display()
            );
            assert_eq!(
                play(&t),
                play(&t),
                "{} does not replay identically twice",
                path.display()
            );
        }
    }
}

/// The recorded rounds must be TERMINAL and PRICED: a round with no usage chunk would silently
/// contribute zero to the $ column, which is the one thing `report.rs` refuses to do.
#[test]
fn every_recorded_round_reports_its_usage() {
    let dir = bank::bench_dir();
    for task in bank::load(&dir.join("bank")).expect("the bank loads") {
        for arm in Arm::BOTH {
            let path = dir.join(arm.fixtures()).join(format!("{}.yml", task.id));
            let t = transcript_of(&path);
            for (i, round) in t.rounds.iter().enumerate() {
                assert!(
                    round
                        .chunks
                        .iter()
                        .any(|c| matches!(c, bough_plugin_llm_replay::RecordedChunk::Usage { .. })),
                    "{} round {i} reports no usage",
                    path.display()
                );
            }
        }
    }
}

/// `make bench-tools`. Prints the table §8 of `docs/phase-codemode-plan.md` records.
#[test]
#[ignore = "the bench: drives the release binary over the whole bank, both arms"]
fn bench_tools_runs_the_bank_through_both_consumers_offline() {
    let tasks = bank::load(&bank::bench_dir().join("bank")).expect("the bank loads");
    let runner = Runner::new(false).expect("a bough binary and a price table");
    let report = runner.run_bank(&tasks).expect("the bank runs");
    println!("\n{}", report.render());
    for arm in Arm::BOTH {
        let s = report.summary(arm);
        assert_eq!(s.tasks, tasks.len(), "{} ran a partial bank", arm.label());
    }
}
