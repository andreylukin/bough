//! Invariant: an assembly refusal names the section or the budget rule that caused it.

use crate::section::SectionId;

/// Everything projection refuses.
#[derive(Debug, thiserror::Error)]
pub enum ProjectionError {
    #[error("section `{id}` is already registered at this scope")]
    DuplicateSection { id: SectionId },
    #[error("section `{id}` is a built-in band id and is reserved by the assembler")]
    ReservedSection { id: SectionId },
    #[error("section `{id}` declares agent scope but names no agent")]
    AgentScopeWithoutAgent { id: SectionId },
    #[error("section `{id}` failed to render: {detail}")]
    SectionRender { id: SectionId, detail: String },
    #[error("no such agent `{0}`")]
    NoSuchAgent(String),
    #[error("writing the file view to `{path}` failed: {detail}")]
    FileView { path: String, detail: String },
    #[error(transparent)]
    Ledger(#[from] bough_plugin_ledger::LedgerError),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}
