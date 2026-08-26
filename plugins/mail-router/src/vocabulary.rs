//! Invariant (§0.2, "model-visible ⟺ ledgered"): every routing DECISION this crate makes that a
//! later reader could need — an unrouted item, an adoption, a routing change, a question — is a
//! step, not a side channel. The unsorted queue must survive a restart; adoption is attributable;
//! a routing change explains later deliveries; and a question is not truth, which is why
//! `leader/question` is a Thought and the other three are Evidence.

use bough_plugin_ledger::{AgentName, Ref, StepId};
use bough_plugin_rollups::Attribution;

/// The owner string every step type below is registered under.
pub const OWNER: &str = "mail-router";

/// `mail/unrouted` — Evidence. On the unsorted trajectory only.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct MailUnrouted {
    pub from: Ref,
    pub subject: String,
    pub summary: String,
    #[serde(default)]
    pub refs: Vec<Ref>,
}

/// `mail/adopted` — Evidence.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct MailAdopted {
    pub unrouted: StepId,
    pub to: AgentName,
    pub by: Attribution,
}

/// `leader/question` — Thought: a question is not truth (§16).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct LeaderQuestion {
    pub asked_by: String,
    pub about: String,
    #[serde(default)]
    pub options: Vec<String>,
}

/// `agent/routing` — Evidence.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct AgentRouting {
    pub agent: AgentName,
    #[serde(default)]
    pub added: Vec<Ref>,
    #[serde(default)]
    pub removed: Vec<Ref>,
    pub by: Attribution,
}

/// Declare this crate's four step types on the bound ledger. Called once, from `apply`.
pub fn declare(_ledger: &bough_plugin_ledger::LedgerHandle) -> Result<(), crate::MailError> {
    todo!("WP-1: declare mail/unrouted, mail/adopted, leader/question, agent/routing")
}
