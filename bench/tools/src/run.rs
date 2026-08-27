//! Invariant: the two arms differ by ONE patch — the consumer row — and by nothing else. The same
//! bank, the same fixture repo, the same headless profile, the same model.

use crate::bank::Task;
use crate::report::Row;

/// Which surface an arm measures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arm {
    /// `tools-baseline` + `tools-operator`, typed calls.
    Typed,
    /// One `run(program)` over the sandbox.
    Codemode,
}

impl Arm {
    /// The patch file that selects this arm.
    pub fn patch(&self) -> &'static str {
        match self {
            Arm::Typed => "bench/tools/arms/typed.yml",
            Arm::Codemode => "bench/tools/arms/codemode.yml",
        }
    }
}

/// Boots the headless profile once per (task, arm) and scores the result.
pub struct Runner {
    /// `false` ⇒ `llm-replay` against the recorded transcripts; `true` (BOUGH_LIVE=1) ⇒
    /// `llm-anthropic` on haiku.
    pub live: bool,
}

impl Runner {
    /// Run one task under one arm and score it against its data predicates.
    ///
    /// WP-8 owns the body.
    pub async fn run_one(&self, _task: &Task, _arm: Arm) -> anyhow::Result<Row> {
        todo!("WP-8: boot headless with the arm's patch, run the wake, score the predicates")
    }

    /// The whole bank, both arms.
    ///
    /// WP-8 owns the body.
    pub async fn run_bank(&self, _tasks: &[Task]) -> anyhow::Result<Vec<Row>> {
        todo!("WP-8: run every task under both arms")
    }
}
