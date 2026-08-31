//! Invariant: a task's pass predicate is data. ≥12 tasks over a fixed fixture repo, and their
//! declared coverage names every entry of the sandbox surface.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// One bench task, loaded from `bench/tools/bank/*.yml`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Coverage {
    Bash,
    Bg,
    View,
    Patch,
    Write,
    Ledger,
    Inbox,
    Act,
    Agent,
    Ask,
    Fork,
    Schedule,
}

impl Coverage {
    /// Every entry of the §3 surface table. The bank test asserts the union of the tasks'
    /// `covers` equals this set — a surface entry nobody benches is a surface entry nobody knows
    /// the cost of.
    /// `sh` is deliberately ABSENT: no row in the tree registers a tool by that name, so a task
    /// claiming it could only ever fail, and the surface no longer documents it either (the
    /// prose is gated on the verb actually being injected). It comes back here the day a
    /// concurrent-shell Provider does.
    pub const ALL: [Coverage; 12] = [
        Coverage::Bash,
        Coverage::Bg,
        Coverage::View,
        Coverage::Patch,
        Coverage::Write,
        Coverage::Ledger,
        Coverage::Inbox,
        Coverage::Act,
        Coverage::Agent,
        Coverage::Ask,
        Coverage::Fork,
        Coverage::Schedule,
    ];
}

/// One data predicate. No model judgement appears here.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "pass", rename_all = "snake_case")]
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

/// Load the bank from `dir`, in file-name order so a run's row order is stable.
pub fn load(dir: &Path) -> anyhow::Result<Vec<Task>> {
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| anyhow::anyhow!("reading the bank at {}: {e}", dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "yml"))
        .collect();
    files.sort();

    let mut tasks = Vec::with_capacity(files.len());
    for path in files {
        let text = std::fs::read_to_string(&path)
            .map_err(|e| anyhow::anyhow!("reading {}: {e}", path.display()))?;
        let task: Task = serde_yaml::from_str(&text)
            .map_err(|e| anyhow::anyhow!("{} does not parse as a task: {e}", path.display()))?;
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_string();
        anyhow::ensure!(
            task.id == stem,
            "{}: task id `{}` must equal its file stem `{stem}` (the transcript fixtures are \
             looked up by id)",
            path.display(),
            task.id
        );
        anyhow::ensure!(
            !task.pass.is_empty(),
            "{}: a task with no pass predicate is a task nothing can fail",
            path.display()
        );
        tasks.push(task);
    }
    Ok(tasks)
}

/// The repo's `bench/tools` directory, however the bench is invoked from.
pub fn bench_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}
