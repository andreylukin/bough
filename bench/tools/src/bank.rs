//! Invariant: a task's pass predicate is data. ≥12 tasks over a fixed fixture repo, and their
//! declared coverage names every entry of the sandbox surface.

use serde::{Deserialize, Serialize};

/// One bench task, loaded from `bench/tools/bank/*.yml`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    /// What the user asks for.
    pub prompt: String,
    /// Which surface entries this task exercises: the coverage claim the bank test checks.
    pub covers: Vec<Coverage>,
    /// What must be true afterwards.
    pub pass: Vec<Pass>,
}

/// A surface entry a task claims to exercise.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Coverage {
    Bash,
    Sh,
    Bg,
    View,
    Patch,
    Write,
    Ledger,
    Inbox,
    Claim,
    Act,
    Agent,
    Ask,
    Fork,
    Schedule,
}

/// One data predicate. No model judgement appears here.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Pass {
    /// A file under the fixture repo equals this text exactly.
    FileEquals { path: String, text: String },
    /// A file contains this substring.
    FileContains { path: String, needle: String },
    /// A file does not exist.
    FileAbsent { path: String },
    /// The ledger holds at least `count` steps of this kind.
    StepAppended { kind: String, count: usize },
    /// The actions journal holds a row of this kind.
    JournalRow { kind: String },
}

/// Load the bank from `dir`.
///
/// WP-8 owns the body.
pub fn load(_dir: &std::path::Path) -> anyhow::Result<Vec<Task>> {
    todo!("WP-8: read bank/*.yml into Tasks")
}
