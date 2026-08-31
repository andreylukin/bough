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
    /// The row's wake classes AFTER the change, when this step changed them. `None` ⇒ the change
    /// was about routing refs alone. §5 makes wake classes MUTABLE CONFIG, so they need the same
    /// evidence trail the refs have — a class that appeared without a step would explain nothing.
    #[serde(default)]
    pub wake_classes: Option<Vec<String>>,
    pub by: Attribution,
}

/// This crate's four step types, for `LedgerHandle::declare_step_types`.
///
/// Three are Evidence, so the ledger itself refuses one without cites: an unrouted item that
/// cannot say where it came from, or an adoption nobody can attribute, is not appendable.
/// `leader/question` is the Thought, because a question is not truth (§16).
pub fn step_types() -> Vec<bough_plugin_ledger::StepTypeDef> {
    use bough_plugin_ledger::{ClassRule, StepTypeDef};
    vec![
        StepTypeDef::of::<MailUnrouted>("mail/unrouted", OWNER).class_rule(ClassRule::Evidence),
        StepTypeDef::of::<MailAdopted>("mail/adopted", OWNER).class_rule(ClassRule::Evidence),
        StepTypeDef::of::<LeaderQuestion>("leader/question", OWNER).class_rule(ClassRule::Thought),
        StepTypeDef::of::<AgentRouting>("agent/routing", OWNER).class_rule(ClassRule::Evidence),
    ]
}
