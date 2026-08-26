//! Invariant: tiers are an INDEX, never a replacement (§3). Every block body names the layer
//! beneath it and a bounded set of RAW step ids its claims rest on, so a projected coarse block
//! resolves to raw evidence in one hop (P4-D5). The evidence comes from the covered steps, never
//! from the model: the index must not depend on the model's discipline.

use std::collections::BTreeSet;

use bough_plugin_ledger::{Ref, RollupId, Seq, Step, StepId};

use crate::window::Cut;

/// The body of a `tier` rollup.
///
/// The assembler's `rollup_text` reads an object's `text` field, so `text` is the rendered
/// surface and everything else is structure the index needs.
#[derive(Clone, PartialEq, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct TierBlock {
    /// The recap prose. What the projection renders.
    pub text: String,
    pub themes: Vec<Theme>,
    /// Refs INTO THE LAYER BENEATH (§3).
    pub beneath: Beneath,
    /// A bounded set of RAW step ids the block's claims rest on (P4-D5).
    pub evidence: Vec<StepId>,
    pub windows: Vec<WindowRef>,
    pub tier: u8,
    pub prompt_ver: String,
}

/// The layer a block reduces.
#[derive(Clone, PartialEq, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(tag = "layer", rename_all = "lowercase")]
pub enum Beneath {
    Raw { steps: Vec<StepId> },
    Blocks { rollups: Vec<RollupId> },
}

/// One theme the reduce produced.
#[derive(Clone, PartialEq, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct Theme {
    pub title: String,
    pub text: String,
    pub refs: Vec<Ref>,
    pub evidence: Vec<StepId>,
}

/// One episode window, as a block records it.
#[derive(Clone, PartialEq, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct WindowRef {
    pub from_seq: Seq,
    pub to_seq: Seq,
    pub cut: Cut,
}

/// The body of a `digest` rollup this crate seals.
#[derive(Clone, PartialEq, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct DigestBlock {
    pub text: String,
    pub standing: Vec<Standing>,
    pub evidence: Vec<StepId>,
    pub from_blocks: Vec<RollupId>,
    pub replaces: Option<RollupId>,
    pub prompt_ver: String,
}

/// One standing fact in a digest, with the raw steps behind it.
#[derive(Clone, PartialEq, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct Standing {
    pub text: String,
    pub evidence: Vec<StepId>,
}

/// Every ref a block names, as the index check reads them. Total over both [`Beneath`] shapes.
pub fn refs_of(block: &TierBlock) -> (Vec<StepId>, Vec<RollupId>) {
    let mut steps: Vec<StepId> = Vec::new();
    let mut rollups: Vec<RollupId> = Vec::new();
    match &block.beneath {
        Beneath::Raw { steps: beneath } => steps.extend(beneath.iter().cloned()),
        Beneath::Blocks { rollups: beneath } => rollups.extend(beneath.iter().cloned()),
    }
    // `evidence` is RAW at every tier (P4-D5), and a theme may cite raw steps of its own.
    steps.extend(block.evidence.iter().cloned());
    for t in &block.themes {
        steps.extend(t.evidence.iter().cloned());
    }
    dedup(&mut steps);
    dedup(&mut rollups);
    (steps, rollups)
}

/// Drop repeats, keeping first-seen order: the index check reports an id once.
fn dedup<T: Clone + Ord>(v: &mut Vec<T>) {
    let mut seen: BTreeSet<T> = BTreeSet::new();
    v.retain(|x| seen.insert(x.clone()));
}

/// The `notable_refs` column for a block: the domain refs of the covered steps, most frequent
/// first, capped at `max`. EMPTY when the covered steps carry none (P1-D13).
pub fn notable_refs(steps: &[Step], max: usize) -> BTreeSet<Ref> {
    let mut counts: std::collections::BTreeMap<Ref, usize> = Default::default();
    for s in steps {
        for r in s.refs.iter() {
            // An intra-ledger citation (`step:` / `rollup:`, P1-D5) is a pointer, not a subject:
            // routing a block by it would make every block notable to the step it quotes.
            if is_domain_ref(r) {
                *counts.entry(r.clone()).or_default() += 1;
            }
        }
    }
    let mut ranked: Vec<(Ref, usize)> = counts.into_iter().collect();
    // Most frequent first; ties broken by the ref itself, so the set is deterministic.
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    ranked.into_iter().take(max).map(|(r, _)| r).collect()
}

/// `true` for a ref that names something in the world rather than a row of this ledger.
pub fn is_domain_ref(r: &Ref) -> bool {
    !r.as_str().starts_with("step:") && !r.as_str().starts_with("rollup:")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::window::fixture::step_with;

    fn block(beneath: Beneath, evidence: &[&str]) -> TierBlock {
        TierBlock {
            text: "a recap".into(),
            themes: vec![],
            beneath,
            evidence: evidence.iter().map(StepId::new).collect(),
            windows: vec![],
            tier: 1,
            prompt_ver: "recap-1".into(),
        }
    }

    #[test]
    fn refs_of_is_total_over_both_beneath_shapes() {
        let raw = block(
            Beneath::Raw {
                steps: vec![StepId::new("s1"), StepId::new("s2")],
            },
            &["s2", "s3"],
        );
        let (steps, rollups) = refs_of(&raw);
        assert_eq!(
            steps.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
            vec!["s1", "s2", "s3"],
            "beneath ∪ evidence, deduped, first-seen order"
        );
        assert!(rollups.is_empty());

        let mut over = block(
            Beneath::Blocks {
                rollups: vec![RollupId::new("tier:t:1:1-4")],
            },
            &["s9"],
        );
        over.tier = 2;
        over.themes = vec![Theme {
            title: "t".into(),
            text: "x".into(),
            refs: vec![],
            evidence: vec![StepId::new("s10")],
        }];
        let (steps, rollups) = refs_of(&over);
        assert_eq!(
            steps.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
            vec!["s9", "s10"],
            "a coarse block still names raw evidence in one hop (P4-D5)"
        );
        assert_eq!(rollups.len(), 1);
    }

    #[test]
    fn notable_refs_caps_by_frequency() {
        let steps = vec![
            step_with(1, 0, "probe/note", &["gh:o/r#1", "gh:o/r#2"]),
            step_with(2, 1, "probe/note", &["gh:o/r#1", "gh:o/r#3"]),
            step_with(3, 2, "probe/note", &["gh:o/r#1", "gh:o/r#2"]),
        ];
        let got = notable_refs(&steps, 2);
        assert_eq!(
            got.iter().map(|r| r.as_str()).collect::<Vec<_>>(),
            vec!["gh:o/r#1", "gh:o/r#2"],
            "the two most frequent refs, and the third is dropped"
        );
    }

    #[test]
    fn notable_refs_is_empty_when_the_covered_steps_carry_none() {
        let steps = vec![
            step_with(1, 0, "probe/note", &[]),
            // An intra-ledger citation is not a subject the block is notable FOR.
            step_with(2, 1, "probe/note", &["step:s1", "rollup:tier:t:1:1-4"]),
        ];
        assert!(
            notable_refs(&steps, 8).is_empty(),
            "P1-D13: empty means notable to everyone, and it must be earned"
        );
    }
}
