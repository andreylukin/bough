//! Invariant: the live arm changes ONE thing — the provider — and prints the same table. It is
//! `#[ignore]`d and gated on `BOUGH_LIVE=1` (AGENTS.md): the default suite never touches the
//! network.

use bough_bench_tools::bank;
use bough_bench_tools::run::{Arm, Runner};

#[test]
#[ignore = "live: haiku, both arms, the whole bank (BOUGH_LIVE=1)"]
fn bench_tools_live_haiku_bank() {
    if std::env::var("BOUGH_LIVE").as_deref() != Ok("1") {
        eprintln!("BOUGH_LIVE is not 1: skipping the live bench");
        return;
    }
    let tasks = bank::load(&bank::bench_dir().join("bank")).expect("the bank loads");
    let runner = Runner::new(true).expect("a bough binary and a price table");
    let report = runner.run_bank(&tasks).expect("the bank runs");
    println!("\n{}", report.render());
    for arm in Arm::BOTH {
        assert_eq!(report.summary(arm).tasks, tasks.len());
    }
}

/// A SINGLE round: the whole bank once, ONE arm (default codemode, the shipped consumer), on
/// whatever model `BOUGH_BENCH_MODEL` names — e.g. a Luna round:
///
/// `set -a; . ~/.bough/env; set +a; BOUGH_LIVE=1 BOUGH_BENCH_MODEL=openai:gpt-5.6-luna \
///  cargo test -p bough-bench-tools -- --ignored --nocapture single_arm`
#[test]
#[ignore = "live: one arm, the whole bank once (BOUGH_LIVE=1; BOUGH_BENCH_MODEL to pick the model)"]
fn bench_tools_live_single_arm() {
    if std::env::var("BOUGH_LIVE").as_deref() != Ok("1") {
        eprintln!("BOUGH_LIVE is not 1: skipping the live bench");
        return;
    }
    let arm = match std::env::var("BOUGH_BENCH_ARM").as_deref() {
        Ok("typed") => Arm::Typed,
        _ => Arm::Codemode,
    };
    // BOUGH_BENCH_TASK narrows the round to ONE task (by id prefix, e.g. `07`), and
    // BOUGH_BENCH_REPEAT runs the selection N times — a stability probe, not a comparison.
    let mut tasks = bank::load(&bank::bench_dir().join("bank")).expect("the bank loads");
    if let Ok(wanted) = std::env::var("BOUGH_BENCH_TASK") {
        tasks.retain(|t| t.id.starts_with(&wanted));
        assert!(!tasks.is_empty(), "no bank task matches `{wanted}`");
    }
    let repeat: usize = std::env::var("BOUGH_BENCH_REPEAT")
        .ok()
        .and_then(|r| r.parse().ok())
        .unwrap_or(1);
    let runner = Runner::new(true).expect("a bough binary and a price table");
    let mut rows = Vec::new();
    for round in 1..=repeat {
        for task in &tasks {
            let row = runner.run_one(task, arm).expect("the task runs");
            eprintln!(
                "round {round} · {} · {} · steps {} · in {} out {} · {}",
                task.id,
                if row.passed { "pass" } else { "FAIL" },
                row.steps,
                row.input_tokens,
                row.output_tokens,
                row.note.clone().unwrap_or_default()
            );
            rows.push(row);
        }
    }
    let passed = rows.iter().filter(|r| r.passed).count();
    let in_tokens: u64 = rows.iter().map(|r| r.input_tokens).sum();
    let out_tokens: u64 = rows.iter().map(|r| r.output_tokens).sum();
    println!(
        "\nsingle {} round · arm {} · {passed}/{} passed · in {in_tokens} · out {out_tokens}",
        bough_bench_tools::run::bench_model().unwrap_or_else(|| "haiku".to_string()),
        arm.label(),
        rows.len(),
    );
}
