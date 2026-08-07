//! `logs` — the clean-room log-compression pipeline behind `bough patterns`
//! (port of `src/logs/`, spec `specs/small.md` §1).
//!
//! A streaming pipeline that compresses an arbitrarily large log into a
//! fixed-size `Analysis`: distinct statement templates, per-variable-slot
//! statistics, anomalies, correlations.
//!
//! Nothing else in bough-core calls it — `bough patterns` is its only consumer,
//! deliberately: "a host function is a permanent widening of every program's API
//! and of the system prompt that must describe it, whereas a subcommand costs
//! nothing until something runs it."
//!
//! The whole subsystem is synchronous and dependency-free apart from serde: no
//! tokio, no clock, no filesystem, no randomness. The CLI feeds it lines from a
//! `BufReader`.

pub mod analyze;
pub mod anomaly;
pub mod correlation;
pub mod drain;
pub mod format;
pub mod mask;
pub mod sketch;
pub mod stats;
pub mod timestamp;
pub mod types;

pub use analyze::{analyze, AnalyzeOptions, Analyzer};
pub use format::{to_human, to_json, to_llm};
pub use types::Analysis;
