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
