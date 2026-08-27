//! Invariant: an UNKNOWN claim kind is [`ClaimKind::Other`] and stays accept/rejectable while
//! doing nothing structural. The ledger's `ClaimProposed.kind` is a free string on purpose (§3), so
//! a claim written by an older or newer binary must still render and still be decidable — a parse
//! failure that swallowed the claim would lose a proposal Andrey never saw.

use std::collections::BTreeSet;

use bough_plugin_ledger::{AgentName, Ref, Seq, StepId};

use crate::{BudProposal, ClaimsError, MergeProposal, SplitProposal};

/// Where the structured half of a claim rides inside the `claim/proposed` body.
///
/// `ClaimProposed` is the LEDGER's type and Phase 5 does not change it (§17 Phase 5 touches no
/// ledger vocabulary), so the structural payload travels as one additional property beside the
/// four the ledger declares. The schema admits it; a reader that does not know the key still sees
/// the kind, the title and the body.
pub const DETAIL_KEY: &str = "detail";

/// What a claim is ABOUT: the parsed form of the ledger's free-string kind.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ClaimKind {
    /// Accepted ⇒ a pin (§3: "accepted requirements are pins").
    Requirement {
        #[serde(default)]
        supersedes: Vec<StepId>,
    },
    /// Accepted ⇒ an `agents` row is born through `ctx.graph` (a bud from the proposing lane).
    Lane {
        name: AgentName,
        #[serde(default)]
        from_seq: Option<Seq>,
        #[serde(default)]
        routing_refs: BTreeSet<Ref>,
        #[serde(default)]
        wake_classes: BTreeSet<String>,
    },
    Split(SplitProposal),
    Merge(MergeProposal),
    Bud(BudProposal),
    Contradiction {
        #[serde(default)]
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
        match self {
            ClaimKind::Requirement { .. } => "requirement",
            ClaimKind::Lane { .. } => "lane",
            ClaimKind::Split(_) => "split",
            ClaimKind::Merge(_) => "merge",
            ClaimKind::Bud(_) => "bud",
            ClaimKind::Contradiction { .. } => "contradiction",
            ClaimKind::Other => "other",
        }
    }

    /// The structured half, as the proposal body carries it.
    pub fn detail(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }
}

/// PURE: whether a kind NAME is one of the structural four, judged from the name alone.
///
/// The name, not the parsed value: a lane claim whose detail this binary cannot read still asks
/// for structure, and refusing it as `Other` would let a lane agent smuggle one past §2.
pub fn is_structural_name(name: &str) -> bool {
    matches!(name, "lane" | "split" | "merge" | "bud")
}

/// §2's one refusal on the PROPOSE side: a lane agent may not propose structure. The global
/// `propose_claim` calls this; its leader-scoped twin in `tool-leader` does not.
pub fn refuse_structure_from_a_lane(kind: &ClaimKind) -> Result<(), ClaimsError> {
    refuse_structural_name(kind.as_str())
}

/// The same refusal, over a kind NAME the caller has not parsed.
pub fn refuse_structural_name(kind: &str) -> Result<(), ClaimsError> {
    if is_structural_name(kind) {
        return Err(ClaimsError::NotTheLeader {
            kind: kind.to_string(),
        });
    }
    Ok(())
}

/// PURE: parse the ledger's `(kind, body)` pair. An unrecognised kind is [`ClaimKind::Other`].
pub fn parse(kind: &str, body: &serde_json::Value) -> ClaimKind {
    let mut detail = match body.get(DETAIL_KEY) {
        Some(serde_json::Value::Object(o)) => serde_json::Value::Object(o.clone()),
        _ => serde_json::Value::Object(serde_json::Map::new()),
    };
    if let Some(o) = detail.as_object_mut() {
        // The tag is the ledger's `kind` column, never the detail's own copy of it: the two
        // cannot disagree if only one of them is read.
        o.insert(
            "kind".to_string(),
            serde_json::Value::String(kind.to_string()),
        );
    }
    serde_json::from_value(detail).unwrap_or(ClaimKind::Other)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ProposedChild;

    fn body(kind: &ClaimKind) -> serde_json::Value {
        serde_json::json!({
            "claim": "c1",
            "kind": kind.as_str(),
            "title": "t",
            "body": "b",
            DETAIL_KEY: kind.detail(),
        })
    }

    #[test]
    fn a_known_kind_parses() {
        for kind in [
            ClaimKind::Requirement {
                supersedes: vec![StepId::new("p1")],
            },
            ClaimKind::Lane {
                name: AgentName::new("infra"),
                from_seq: Some(Seq(7)),
                routing_refs: BTreeSet::from([Ref::new("repo:bough")]),
                wake_classes: BTreeSet::from(["ask".to_string()]),
            },
            ClaimKind::Merge(MergeProposal {
                survivor: Some(AgentName::new("a")),
                absorbed: AgentName::new("b"),
            }),
            ClaimKind::Bud(BudProposal {
                parent: AgentName::new("a"),
                at_seq: Seq(3),
                child: ProposedChild {
                    agent: Some(AgentName::new("c")),
                    routing_refs: BTreeSet::new(),
                    wake_classes: BTreeSet::new(),
                },
            }),
            ClaimKind::Contradiction {
                between: vec![StepId::new("s1"), StepId::new("s2")],
            },
            ClaimKind::Other,
        ] {
            assert_eq!(
                parse(kind.as_str(), &body(&kind)),
                kind,
                "a kind this binary wrote must read back identically"
            );
        }
    }

    #[test]
    fn an_unknown_kind_is_other_and_harmless() {
        // A kind from a newer binary, with a detail this one cannot interpret.
        let from_the_future = serde_json::json!({
            "claim": "c1",
            "kind": "quorum",
            "title": "t",
            "body": "b",
            DETAIL_KEY: { "kind": "quorum", "voters": ["a", "b"] },
        });
        let parsed = parse("quorum", &from_the_future);
        assert_eq!(parsed, ClaimKind::Other, "an unknown kind is Other");
        // Harmless: it changes no structure, so accepting it does nothing but record the act.
        assert!(!parsed.is_structural());
        refuse_structure_from_a_lane(&parsed).expect("Other is proposable by any lane agent");

        // A known kind whose detail is corrupt does not panic and does not become that kind.
        let corrupt = serde_json::json!({ "kind": "lane", DETAIL_KEY: "not an object" });
        assert_eq!(parse("lane", &corrupt), ClaimKind::Other);
        // And a body with no detail at all still parses the kinds whose fields all default.
        assert_eq!(
            parse("requirement", &serde_json::json!({})),
            ClaimKind::Requirement {
                supersedes: Vec::new()
            }
        );
    }

    #[test]
    fn a_structural_kind_from_a_lane_agent_is_refused() {
        let lane = ClaimKind::Lane {
            name: AgentName::new("infra"),
            from_seq: None,
            routing_refs: BTreeSet::new(),
            wake_classes: BTreeSet::new(),
        };
        let err = refuse_structure_from_a_lane(&lane)
            .expect_err("only the leader proposes structure (§2)");
        assert!(matches!(err, ClaimsError::NotTheLeader { .. }), "{err}");
        assert!(
            err.to_string()
                .contains("only the leader proposes structure"),
            "{err}"
        );

        // The three non-structural kinds any lane agent may propose.
        for ok in [
            ClaimKind::Requirement {
                supersedes: Vec::new(),
            },
            ClaimKind::Contradiction {
                between: Vec::new(),
            },
            ClaimKind::Other,
        ] {
            refuse_structure_from_a_lane(&ok).expect("a lane agent may propose this");
        }
    }
}
