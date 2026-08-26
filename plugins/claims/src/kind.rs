//! Invariant: an UNKNOWN claim kind is [`ClaimKind::Other`] and stays accept/rejectable while
//! doing nothing structural. The ledger's `ClaimProposed.kind` is a free string on purpose (§3), so
//! a claim written by an older or newer binary must still render and still be decidable — a parse
//! failure that swallowed the claim would lose a proposal Andrey never saw.

use std::collections::BTreeSet;

use bough_plugin_ledger::{AgentName, Ref, Seq, StepId};

use crate::{BudProposal, MergeProposal, SplitProposal};

/// What a claim is ABOUT: the parsed form of the ledger's free-string kind.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ClaimKind {
    /// Accepted ⇒ a pin (§3: "accepted requirements are pins").
    Requirement {
        supersedes: Vec<StepId>,
    },
    /// Accepted ⇒ an `agents` row is born through `ctx.graph` (a bud from the proposing lane).
    Lane {
        name: AgentName,
        from_seq: Option<Seq>,
        routing_refs: BTreeSet<Ref>,
        wake_classes: BTreeSet<String>,
    },
    Split(SplitProposal),
    Merge(MergeProposal),
    Bud(BudProposal),
    Contradiction {
        between: Vec<StepId>,
    },
    /// Unknown, or deliberately unstructured. Decidable; does nothing.
    Other,
}

impl ClaimKind {
    /// Whether this kind CHANGES STRUCTURE, and so may only be proposed by the leader (§2).
    pub fn is_structural(&self) -> bool {
        matches!(
            self,
            ClaimKind::Lane { .. } | ClaimKind::Split(_) | ClaimKind::Merge(_) | ClaimKind::Bud(_)
        )
    }

    /// The free string the ledger stores.
    pub fn as_str(&self) -> &'static str {
        todo!("WP-4: the wire spelling of each kind")
    }
}

/// PURE: parse the ledger's `(kind, body)` pair. An unrecognised kind is [`ClaimKind::Other`].
pub fn parse(_kind: &str, _body: &serde_json::Value) -> ClaimKind {
    todo!("WP-4: parse, and never fail: an unknown kind is Other")
}
