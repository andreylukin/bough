//! Invariant: reads are queries over committed rows and write nothing — `connected()` included
//! (§3: membership is derived at need).

use std::collections::BTreeSet;

use crate::id::{ActionId, RollupId, Seq, StepId, StepType, TrajId, WakeId};
use crate::rows::{ActionStatus, AgentRow, RollupKind};
use crate::step::{Class, Step};

/// Result order of a step query.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Order {
    #[default]
    SeqAsc,
    SeqDesc,
}

/// A step query. Every empty/`None` field means "no filter".
#[derive(Clone, Debug, Default)]
pub struct StepQuery {
    pub trajs: Vec<TrajId>,
    pub kinds: Vec<StepType>,
    pub class: Option<Class>,
    pub wake: Option<WakeId>,
    pub after: Option<Seq>,
    pub before: Option<Seq>,
    /// Any-match against the derived `step_refs`.
    pub refs: Vec<crate::id::Ref>,
    pub order: Order,
    pub limit: Option<usize>,
}

/// Full-text search across trajectories.
#[derive(Clone, Debug)]
pub struct SearchQuery {
    pub text: String,
    /// Empty ⇒ every trajectory.
    pub trajs: Vec<TrajId>,
    pub limit: usize,
}

/// One search result. Ordered `seq DESC, traj ASC` on both providers (P1-D19).
#[derive(Clone, Debug)]
pub struct SearchHit {
    pub step: Step,
    pub snippet: String,
}

/// A live pin, as the projection's pins band renders it.
#[derive(Clone, Debug)]
pub struct Pin {
    pub step: StepId,
    pub traj: TrajId,
    pub seq: Seq,
    pub class: Class,
    pub title: String,
    pub text: String,
}

/// A rollup query.
#[derive(Clone, Debug, Default)]
pub struct RollupQuery {
    pub trajs: Vec<TrajId>,
    pub kind: Option<RollupKind>,
    pub max_tier: Option<u8>,
    /// Superseded rollups are excluded by default.
    pub include_superseded: bool,
    pub limit: Option<usize>,
}

/// An actions-journal query.
#[derive(Clone, Debug, Default)]
pub struct ActionQuery {
    pub ids: Vec<ActionId>,
    pub wake: Option<WakeId>,
    pub status: Option<ActionStatus>,
    pub limit: Option<usize>,
}

/// A fork request: `child` branches off `parent` after `at_seq`.
#[derive(Clone, Debug)]
pub struct Fork {
    pub parent: TrajId,
    pub child: TrajId,
    pub at_seq: Seq,
    pub at: chrono::DateTime<chrono::Utc>,
}

/// What a successful fork wrote: the edge and the child's first live step.
#[derive(Clone, Debug)]
pub struct ForkOutcome {
    pub edge: crate::rows::Edge,
    /// The `fork/end-seed` marker at the child's seq 1.
    pub end_seed: Step,
}

/// `connected(agent) = own_chain ∪ ancestry ∪ ref_matches`, computed AT NEED (§3).
#[derive(Clone, Debug)]
pub struct Connected {
    pub own: TrajId,
    pub ancestry: Vec<TrajId>,
    pub ref_matches: Vec<TrajId>,
    /// The agent's routing refs, read from the `agents` row at call time.
    pub refs: BTreeSet<crate::id::Ref>,
}

impl Connected {
    /// The membership of an agent with NO `agents` row.
    ///
    /// `agents` is mutable config a merge may delete (§3), and "an answer wake must always be
    /// buildable" (§5): a projection for such an agent degrades to identity-only rather than
    /// refusing. There is no head pointer, so `own` is the empty trajectory id, which matches no
    /// row in any query.
    pub fn rowless() -> Self {
        Connected {
            own: TrajId::new(""),
            ancestry: Vec::new(),
            ref_matches: Vec::new(),
            refs: BTreeSet::new(),
        }
    }

    /// Whether this membership was built for an agent with no row.
    pub fn is_rowless(&self) -> bool {
        self.own.as_str().is_empty()
    }

    /// Every trajectory in the membership, deduplicated and sorted.
    pub fn trajectories(&self) -> BTreeSet<TrajId> {
        let mut out = BTreeSet::new();
        if !self.is_rowless() {
            out.insert(self.own.clone());
        }
        out.extend(self.ancestry.iter().cloned());
        out.extend(self.ref_matches.iter().cloned());
        out
    }
}

/// Which tables [`crate::LedgerStore::row_hashes`] covers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HashScope {
    /// `steps`, `edges` and `rollups`.
    All,
    Steps,
    Edges,
    Rollups,
}

/// A stable content hash for one append-only row.
#[derive(Clone, Debug)]
pub struct RowHash {
    pub table: &'static str,
    pub id: String,
    /// For a rollup the hash EXCLUDES `superseded_by`, so a legal set-once write is not reported
    /// as a row change.
    pub hash: String,
    pub superseded_by: Option<String>,
}

/// A whole trajectory as plain data: the pure input of the file-view renderer.
#[derive(Clone, Debug)]
pub struct TrajectoryView {
    pub traj: TrajId,
    pub steps: Vec<Step>,
    pub edges: Vec<crate::rows::Edge>,
    pub rollups: Vec<crate::rows::Rollup>,
    pub agent: Option<AgentRow>,
}

/// Handed to `row_hashes` callers that want to name a specific rollup.
pub type RollupRef = RollupId;
