//! The workflow engine (port of `src/workflow/`). The Rust engine owns the
//! journal, keys, semaphore, pause gate and prefix replay; the script runs in
//! the JS sidecar (`harness::wf`). Scripts are deterministic; every `agent()`
//! call carries a structural coordinate. The prefix decision + journal-row
//! insert happen in one non-await section on the run's message-loop task.

pub mod control;
pub mod engine;
pub mod journal_fs;
pub mod key;
pub mod meta;
pub mod pos;
pub mod relaunch;
pub mod replay;
pub mod report;
pub mod runner;
pub mod saved;
pub mod structured;
