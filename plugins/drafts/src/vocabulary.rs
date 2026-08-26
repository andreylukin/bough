//! Invariant: the two step types are `ClassRule::Thought` and `ignorable: false` (P6-D4). A draft
//! is the agent's own composition, so it is never evidence; and it is never skippable, because a
//! binary that cannot read a draft must not silently pretend the agent sent nothing.

use bough_plugin_ledger::Ref;

use crate::DraftId;

/// `draft/message`.
pub const DRAFT_MESSAGE: &str = "draft/message";
/// `draft/ticket`.
pub const DRAFT_TICKET: &str = "draft/ticket";

/// The `draft/message` body.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct DraftMessage {
    pub draft: DraftId,
    pub audience: String,
    pub subject: String,
    pub body: String,
    #[serde(default)]
    pub refs: Vec<Ref>,
}

/// The `draft/ticket` body.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct DraftTicket {
    pub draft: DraftId,
    pub audience: String,
    pub title: String,
    pub body: String,
    #[serde(default)]
    pub refs: Vec<Ref>,
}
