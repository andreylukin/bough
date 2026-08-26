//! Invariant: expiry is an APPENDED marker, never an edit (§8). This module owns the SET a run of
//! markers folds down to, so the projector and the governance rows read one implementation. A
//! marker naming something unresolvable is IGNORED, never an error — a marker is data. Pins and
//! claims are absent from [`NEVER_EXPIRABLE`]'s complement by construction (§3, V7): a pin's only
//! relief valve is supersession.

use std::collections::BTreeSet;

use bough_plugin_ledger::{Ref, RollupId, Step, StepId, StepType};

/// What a run of `memory/expired` markers expires.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Expired {
    pub steps: BTreeSet<StepId>,
    pub rollups: BTreeSet<RollupId>,
}

/// PURE: fold `memory/expired` steps into the set.
///
/// A marker naming a target that is not a step or rollup ref is ignored, never an error.
pub fn parse(markers: &[Step]) -> Expired {
    let mut out = Expired::default();
    for m in markers {
        if m.kind.as_str() != EXPIRED_STEP_TYPE {
            continue;
        }
        let Ok(body) = serde_json::from_value::<ExpiredBody>((*m.body).clone()) else {
            // A marker whose body does not parse is data the projector cannot act on. It is
            // IGNORED, never an error: expiry must not be able to break a projection.
            continue;
        };
        for target in body.targets {
            match target.as_str().split_once(':') {
                Some(("step", id)) => {
                    out.steps.insert(StepId::new(id));
                }
                Some(("rollup", id)) => {
                    out.rollups.insert(RollupId::new(id));
                }
                // Any other scheme names something this seam does not expire.
                _ => {}
            }
        }
    }
    out
}

/// The step type an expiry marker is appended as. The type is REGISTERED by `reconsolidation`
/// (which owns the write); the name lives here so the projector and the governance rows agree on
/// one spelling.
pub const EXPIRED_STEP_TYPE: &str = "memory/expired";

/// The body of a `memory/expired` marker.
#[derive(
    Clone, PartialEq, Debug, Default, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct ExpiredBody {
    /// What the marker expires, in the canonical citation spelling (P1-D5): `step:<id>` or
    /// `rollup:<id>`. Any other scheme is ignored by [`parse`].
    pub targets: Vec<Ref>,
    /// Why, in one line. Rendered nowhere; read by a human grepping the ledger.
    #[serde(default)]
    pub reason: String,
    /// `expiry` (stale evidence) or `supersession` (the note a replaced block leaves). Carried as
    /// a `String` because the two governance rows spell it with two different enums; this seam
    /// only reads it back.
    #[serde(default)]
    pub kind: String,
}

/// `true` iff a step of this kind may EVER be named by an expiry marker (§3, V7).
pub fn is_expirable(kind: &StepType) -> bool {
    !NEVER_EXPIRABLE.contains(&kind.as_str())
}

/// The step kinds an expiry pass may NEVER name (§3, V7).
pub const NEVER_EXPIRABLE: &[&str] = &[
    "pin/set",
    "pin/retire",
    "claim/proposed",
    "claim/accepted",
    "claim/rejected",
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::window::fixture::step_with;
    use std::sync::Arc;

    fn marker(seq: u64, targets: &[&str]) -> Step {
        let mut s = step_with(seq, seq as i64, EXPIRED_STEP_TYPE, &[]);
        s.body = Arc::new(serde_json::json!({
            "targets": targets,
            "reason": "superseded by a later observation",
            "kind": "supersession",
        }));
        s
    }

    #[test]
    fn parse_folds_markers_into_a_set() {
        let got = parse(&[
            marker(1, &["step:s1", "rollup:tier:t:1:1-4"]),
            marker(2, &["step:s2", "step:s1"]),
        ]);
        assert_eq!(
            got.steps.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
            vec!["s1", "s2"],
            "the fold is a SET; naming a step twice expires it once"
        );
        assert_eq!(
            got.rollups.iter().map(|r| r.as_str()).collect::<Vec<_>>(),
            vec!["tier:t:1:1-4"]
        );
        // A step that is not a marker at all contributes nothing.
        assert_eq!(
            parse(&[step_with(3, 3, "probe/note", &[])]),
            Expired::default()
        );
    }

    #[test]
    fn a_marker_naming_an_unknown_scheme_is_ignored() {
        let got = parse(&[marker(1, &["gh:o/r#12", "nonsense", "step:s1"])]);
        assert_eq!(got.steps.len(), 1, "the resolvable target still lands");
        assert!(got.rollups.is_empty());
        // And a body that does not parse at all is data, not an error.
        let mut bad = marker(2, &[]);
        bad.body = Arc::new(serde_json::json!({ "nope": 1 }));
        assert_eq!(parse(&[bad]), Expired::default());
    }

    #[test]
    fn a_pin_kind_is_never_expirable() {
        for kind in [
            "pin/set",
            "pin/retire",
            "claim/proposed",
            "claim/accepted",
            "claim/rejected",
        ] {
            assert!(
                !is_expirable(&bough_plugin_ledger::StepType::new(kind)),
                "§3 V7: a pin's only relief valve is supersession, never expiry"
            );
        }
        assert!(is_expirable(&bough_plugin_ledger::StepType::new(
            "probe/note"
        )));
    }
}
