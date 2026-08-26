//! Invariant (§7, P2-D22): there are FOUR outward acts and the set is CLOSED. A kind not in this
//! enum cannot be spelled at all, and a kind no Provider registered does not exist as far as the
//! executor is concerned — "Slack send is not a kind" is a compile-time fact, not a lookup.

use crate::error::ActionError;

/// §7's four sanctioned outward acts.
#[derive(
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Debug,
    serde::Serialize,
    serde::Deserialize,
    schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    OpenPr,
    PushToPr,
    BotThreadOp,
    LinearWrite,
}

impl ActionKind {
    /// The spelling used in the journal, in error messages and in the idem key.
    pub fn as_str(&self) -> &'static str {
        match self {
            ActionKind::OpenPr => "open_pr",
            ActionKind::PushToPr => "push_to_pr",
            ActionKind::BotThreadOp => "bot_thread_op",
            ActionKind::LinearWrite => "linear_write",
        }
    }

    /// Every kind, for `--dump-config` and for the tool row's registrations.
    pub fn all() -> &'static [ActionKind] {
        &[
            ActionKind::OpenPr,
            ActionKind::PushToPr,
            ActionKind::BotThreadOp,
            ActionKind::LinearWrite,
        ]
    }
}

/// What an action acts on, as the caller spelled it.
#[derive(Clone, Debug, PartialEq)]
pub struct ActionTarget {
    pub raw: String,
}

impl ActionTarget {
    /// Canonical form per kind (lowercased host, no trailing slash, `owner/repo#number`,
    /// `TEAM-123`). The idem key hashes THIS, so two spellings of one target collide (§7).
    ///
    /// WP-7.
    pub fn canonical(&self, _kind: ActionKind) -> Result<String, ActionError> {
        todo!("WP-7: per-kind canonicalisation; a bad target is BadTarget, never a silent pass")
    }
}
