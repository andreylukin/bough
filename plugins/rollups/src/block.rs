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
pub fn refs_of(_block: &TierBlock) -> (Vec<StepId>, Vec<RollupId>) {
    todo!("WP-1: block ref extraction")
}

/// The `notable_refs` column for a block: the domain refs of the covered steps, most frequent
/// first, capped at `max`. EMPTY when the covered steps carry none (P1-D13).
pub fn notable_refs(_steps: &[Step], _max: usize) -> BTreeSet<Ref> {
    todo!("WP-1: notable refs")
}
