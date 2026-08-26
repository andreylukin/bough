//! Invariant (§2, §16): drafting a requirement from Andrey's words produces a CLAIM, never a pin.
//! The leader may write down what it heard; only Andrey can make it binding. A leader that could
//! pin its own reading of a conversation would be able to rewrite the requirements by paraphrase.

use bough_plugin_ledger::{Cite, StepId, TrajId};
use chrono::{DateTime, Utc};

/// A requirement draft.
#[derive(Clone, Debug)]
pub struct DraftRequest {
    /// The trajectory the words were said on, so the claim cites them.
    pub traj: TrajId,
    pub title: String,
    pub body: String,
    /// Andrey's own words. Cited, because the claim renders as a reading OF something.
    pub from: Vec<Cite>,
    /// Pins this requirement would supersede if accepted (§3's relief valve).
    pub supersedes: Vec<StepId>,
    pub at: DateTime<Utc>,
}
