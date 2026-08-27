//! Invariant: append-only is STRUCTURAL here — there is no mutation method to call. One write
//! lock allocates seq exactly as sqlite's transaction does, `step_refs` come from the
//! Definition's `derive_step_refs` (never a re-implementation), and `agents` is the one mutable
//! map. Everything is dropped when the fiber unloads: no persistence, no file, no config.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use bough_kernel::Context;
use bough_plugin_ledger::{
    ActionId, ActionRow, ActionStatus, AgentName, AgentRow, Append, Cite, Class, Edge, EdgeKind,
    Fork, LedgerError, LedgerStep, NewAction, NewRollup, Pin, Ref, Rollup, RollupId, RollupKind,
    RowHash, Seq, SeqRange, Step, StepId, StepType, StepTypeMap, TrajId, WakeId,
};
use parking_lot::RwLock;
use sha2::{Digest, Sha256};

/// Everything the memory provider holds, behind one lock — the lock IS the single writer.
#[derive(Default)]
pub struct Inner {
    /// Steps by trajectory, in seq order.
    pub steps: BTreeMap<TrajId, Vec<Step>>,
    /// Every step by id, for the point lookup.
    pub by_id: BTreeMap<StepId, (TrajId, Seq)>,
    pub edges: Vec<Edge>,
    pub rollups: BTreeMap<RollupId, Rollup>,
    pub actions: BTreeMap<ActionId, ActionRow>,
    /// The one mutable map (§3 exempts `agents` from append-only).
    pub agents: BTreeMap<AgentName, AgentRow>,
}

/// The store behind the `ledger` binding.
pub struct MemoryStore {
    pub(crate) inner: RwLock<Inner>,
    pub(crate) types: Arc<StepTypeMap>,
    /// The provider's captured context: `ledger/step` is emitted from it, post-commit.
    pub(crate) ctx: Context,
    pub(crate) skipped: Arc<AtomicU64>,
}

impl MemoryStore {
    /// An empty store with the sixteen builtin step types installed.
    pub fn new(ctx: Context) -> Arc<MemoryStore> {
        Arc::new(MemoryStore {
            inner: RwLock::new(Inner::default()),
            types: Arc::new(StepTypeMap::with_builtins()),
            ctx,
            skipped: Arc::new(AtomicU64::new(0)),
        })
    }

    /// Validate, take the write lock, allocate `MAX(seq)+1`, insert. The sqlite transaction's twin.
    pub(crate) fn commit(&self, reqs: Vec<Append>) -> Result<Vec<Step>, LedgerError> {
        // Validation happens BEFORE the lock, exactly as sqlite validates before it opens the
        // transaction: a refusal must write nothing and must not serialise behind other writers.
        let mut checked = Vec::with_capacity(reqs.len());
        for req in &reqs {
            let def = self.types.validate_append(req)?;
            checked.push(def.ignorable);
        }

        let mut inner = self.inner.write();
        let mut out = Vec::with_capacity(reqs.len());
        for (req, ignorable) in reqs.into_iter().zip(checked) {
            let step = insert_locked(&mut inner, req, ignorable);
            out.push(step);
        }
        Ok(out)
    }

    /// Emit `ledger/step` for each committed row, in seq order. Called only AFTER the write lock
    /// has been released and the rows are readable (durability, §0.2).
    pub(crate) fn announce(&self, steps: &[Step]) {
        for s in steps {
            self.ctx.emit::<LedgerStep>(Arc::new(s.clone()));
        }
    }

    /// Apply the unknown-type read rule to one row: a type this binary does not know is refused,
    /// unless the row's stored `ignorable` flag is set, in which case it is skipped and counted.
    pub(crate) fn admit(&self, s: &Step) -> Result<bool, LedgerError> {
        if self.types.get(&s.kind).is_some() {
            return Ok(true);
        }
        if s.ignorable {
            self.skipped.fetch_add(1, Ordering::SeqCst);
            return Ok(false);
        }
        Err(LedgerError::UnknownStepTypeOnRead {
            step: s.id.clone(),
            traj: s.traj.clone(),
            kind: s.kind.clone(),
        })
    }

    /// Every readable row of `traj`, oldest first.
    pub(crate) fn readable(&self, inner: &Inner, traj: &TrajId) -> Result<Vec<Step>, LedgerError> {
        self.readable_of_kinds(inner, traj, &[])
    }

    /// Every readable row of `traj` whose type is in `kinds` (empty ⇒ every type), oldest first.
    ///
    /// The kind filter is applied BEFORE the unknown-type rule, because on the sqlite provider it
    /// is a `WHERE s.type IN (…)` and a row the query excluded is never materialized at all. A
    /// caller that names the types it reads — crash repair naming its four — therefore gets the
    /// same answer from both providers even when the chain also holds a row of a type this binary
    /// has not declared (§3, P1-D7).
    pub(crate) fn readable_of_kinds(
        &self,
        inner: &Inner,
        traj: &TrajId,
        kinds: &[bough_plugin_ledger::StepType],
    ) -> Result<Vec<Step>, LedgerError> {
        let mut out = Vec::new();
        for s in inner.steps.get(traj).map(Vec::as_slice).unwrap_or(&[]) {
            if !kinds.is_empty() && !kinds.contains(&s.kind) {
                continue;
            }
            if self.admit(s)? {
                out.push(s.clone());
            }
        }
        Ok(out)
    }

    /// Every readable row of every trajectory (or of `trajs`, when non-empty), oldest first
    /// within a trajectory.
    pub(crate) fn readable_many(
        &self,
        inner: &Inner,
        trajs: &[TrajId],
        kinds: &[bough_plugin_ledger::StepType],
    ) -> Result<Vec<Step>, LedgerError> {
        let mut out = Vec::new();
        if trajs.is_empty() {
            for traj in inner.steps.keys() {
                out.extend(self.readable_of_kinds(inner, traj, kinds)?);
            }
        } else {
            for traj in trajs {
                out.extend(self.readable_of_kinds(inner, traj, kinds)?);
            }
        }
        Ok(out)
    }
}

/// The insert half of the commit, for a caller that already holds the write lock — `fork`, whose
/// prefix check and two writes are one critical section.
pub(crate) fn insert_for_fork(inner: &mut Inner, req: Append, ignorable: bool) -> Step {
    insert_locked(inner, req, ignorable)
}

/// The insert half of the commit, under the caller's write lock. `MAX(seq)+1` is read and written
/// without releasing the lock, which is what makes two concurrent appends unable to collide.
fn insert_locked(inner: &mut Inner, req: Append, ignorable: bool) -> Step {
    let chain = inner.steps.entry(req.traj.clone()).or_default();
    let seq = Seq(chain.last().map(|s| s.seq.0).unwrap_or(0) + 1);
    // Every request-time default is resolved HERE, in one explicit step (§0.2).
    let spec = bough_plugin_ledger::resolve_append(&req);
    let (id, refs) = (spec.id, spec.refs);
    let step = Step {
        id: id.clone(),
        traj: req.traj.clone(),
        seq,
        at: req.at,
        wake: req.wake,
        kind: req.kind,
        class: req.class,
        body: Arc::new(req.body),
        cites: Arc::new(req.cites),
        refs: Arc::new(refs),
        ignorable,
    };
    chain.push(step.clone());
    inner.by_id.insert(id, (req.traj, seq));
    step
}

/// The one place a step becomes a hash. Excludes nothing: a step has no mutable column.
pub(crate) fn hash_step(s: &Step) -> String {
    let mut h = Sha256::new();
    h.update(s.id.as_str().as_bytes());
    h.update(b"\0");
    h.update(s.traj.as_str().as_bytes());
    h.update(b"\0");
    h.update(s.seq.0.to_string().as_bytes());
    h.update(b"\0");
    h.update(s.at.to_rfc3339().as_bytes());
    h.update(b"\0");
    h.update(s.wake.as_str().as_bytes());
    h.update(b"\0");
    h.update(s.kind.as_str().as_bytes());
    h.update(b"\0");
    h.update(class_str(s.class).as_bytes());
    h.update(b"\0");
    h.update(s.body.to_string().as_bytes());
    h.update(b"\0");
    h.update(
        serde_json::to_string(s.cites.as_ref())
            .unwrap_or_default()
            .as_bytes(),
    );
    h.update(b"\0");
    h.update(if s.ignorable { b"1" } else { b"0" });
    format!("{:x}", h.finalize())
}

/// The edge's hash and its id, which is its primary key spelled out.
pub(crate) fn hash_edge(e: &Edge) -> (String, String) {
    let id = format!(
        "{}|{}|{}",
        e.child.as_str(),
        e.parent.as_str(),
        edge_kind_str(e.kind)
    );
    let mut h = Sha256::new();
    h.update(id.as_bytes());
    h.update(b"\0");
    h.update(e.at_seq.0.to_string().as_bytes());
    h.update(b"\0");
    h.update(e.at.to_rfc3339().as_bytes());
    (id, format!("{:x}", h.finalize()))
}

/// A rollup's hash EXCLUDES `superseded_by`, which is reported beside it, so the one legal
/// set-once write is not a row change (§3).
pub(crate) fn hash_rollup(r: &Rollup) -> String {
    let mut h = Sha256::new();
    h.update(r.id.as_str().as_bytes());
    h.update(b"\0");
    h.update(r.traj.as_str().as_bytes());
    h.update(b"\0");
    h.update(rollup_kind_str(r.kind).as_bytes());
    h.update(b"\0");
    h.update(r.tier.to_string().as_bytes());
    h.update(b"\0");
    h.update(r.from_seq.0.to_string().as_bytes());
    h.update(b"\0");
    h.update(r.to_seq.0.to_string().as_bytes());
    h.update(b"\0");
    for t in &r.src_trajs {
        h.update(t.as_str().as_bytes());
        h.update(b",");
    }
    h.update(b"\0");
    h.update(r.body.to_string().as_bytes());
    h.update(b"\0");
    for rf in &r.notable_refs {
        h.update(rf.as_str().as_bytes());
        h.update(b",");
    }
    h.update(b"\0");
    h.update(r.prompt_ver.as_bytes());
    h.update(b"\0");
    h.update(r.sealed_at.to_rfc3339().as_bytes());
    format!("{:x}", h.finalize())
}

pub(crate) fn class_str(c: Class) -> &'static str {
    match c {
        Class::Evidence => "evidence",
        Class::Thought => "thought",
    }
}

pub(crate) fn edge_kind_str(k: EdgeKind) -> &'static str {
    match k {
        EdgeKind::Ancestor => "ancestor",
        EdgeKind::Merge => "merge",
    }
}

pub(crate) fn rollup_kind_str(k: RollupKind) -> &'static str {
    match k {
        RollupKind::Tier => "tier",
        RollupKind::Digest => "digest",
        RollupKind::Reconciliation => "reconciliation",
    }
}

// ---- the read-side algorithms, shared with `lib.rs`'s trait impl ---------------

/// `pin/set` rows in `trajs`, minus every step id named by a `pin/set.supersedes` or a
/// `pin/retire.retires`. Age is never a criterion (§3).
pub(crate) fn live_pins_from(steps: &[Step]) -> Vec<Pin> {
    let mut dead: BTreeSet<String> = BTreeSet::new();
    for s in steps {
        match s.kind.as_str() {
            "pin/set" => {
                for id in string_array(&s.body, "supersedes") {
                    dead.insert(id);
                }
            }
            "pin/retire" => {
                for id in string_array(&s.body, "retires") {
                    dead.insert(id);
                }
            }
            _ => {}
        }
    }
    let mut out: Vec<Pin> = steps
        .iter()
        .filter(|s| s.kind.as_str() == "pin/set")
        .filter(|s| !dead.contains(s.id.as_str()))
        .map(|s| Pin {
            step: s.id.clone(),
            traj: s.traj.clone(),
            seq: s.seq,
            class: s.class,
            title: string_field(&s.body, "title"),
            text: string_field(&s.body, "text"),
        })
        .collect();
    out.sort_by(|a, b| a.seq.cmp(&b.seq).then_with(|| a.traj.cmp(&b.traj)));
    out
}

/// DELIVERED mail not covered by the union of every `wake/end.consumed` range of the trajectory.
/// The union is order-independent, so the answer does not depend on which wake ended first (§5).
pub(crate) fn unconsumed_mail_from(steps: &[Step]) -> Vec<Step> {
    let mut ranges: Vec<SeqRange> = Vec::new();
    for s in steps {
        if s.kind.as_str() == "wake/end" {
            if let Some(list) = s.body.get("consumed").and_then(|v| v.as_array()) {
                for r in list {
                    if let Ok(r) = serde_json::from_value::<SeqRange>(r.clone()) {
                        ranges.push(r);
                    }
                }
            }
        }
    }
    let consumed = SeqRange::union(&ranges);
    steps
        .iter()
        .filter(|s| s.kind.as_str() == "mail/delivered")
        .filter(|s| !consumed.iter().any(|r| r.contains(s.seq)))
        .cloned()
        .collect()
}

/// The wake left open at `at_seq`, if any: the fork rule's whole content (§3).
pub(crate) fn open_wake_at(steps: &[Step], at_seq: Seq) -> Option<(WakeId, Seq)> {
    let mut open: Vec<(WakeId, Seq)> = Vec::new();
    for s in steps.iter().filter(|s| s.seq <= at_seq) {
        match s.kind.as_str() {
            "wake/start" => open.push((s.wake.clone(), s.seq)),
            "wake/end" => open.retain(|(w, _)| w != &s.wake),
            _ => {}
        }
    }
    open.into_iter().min_by_key(|(_, seq)| *seq)
}

fn string_field(body: &serde_json::Value, key: &str) -> String {
    body.get(key)
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string()
}

fn string_array(body: &serde_json::Value, key: &str) -> Vec<String> {
    body.get(key)
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// The `Ref` set a step matches on, borrowed for the membership scan.
pub(crate) fn matches_any(step: &Step, refs: &BTreeSet<Ref>) -> bool {
    step.refs.iter().any(|r| refs.contains(r))
}

/// Every ancestor of `traj`, nearest first, following `ancestor` edges transitively.
pub(crate) fn ancestry_from(edges: &[Edge], traj: &TrajId) -> Vec<TrajId> {
    let mut out: Vec<TrajId> = Vec::new();
    let mut frontier = vec![traj.clone()];
    let mut seen: BTreeSet<TrajId> = BTreeSet::new();
    seen.insert(traj.clone());
    while let Some(cur) = frontier.pop() {
        let mut parents: Vec<&Edge> = edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Ancestor && e.child == cur)
            .collect();
        parents.sort_by(|a, b| a.parent.cmp(&b.parent));
        for e in parents {
            if seen.insert(e.parent.clone()) {
                out.push(e.parent.clone());
                frontier.push(e.parent.clone());
            }
        }
    }
    out
}

/// The `fork/end-seed` body: the child's first live step names the prefix it inherits (§3).
pub(crate) fn end_seed_append(req: &Fork) -> Append {
    Append {
        traj: req.child.clone(),
        wake: WakeId::seed(&req.child),
        kind: StepType::new("fork/end-seed"),
        class: Class::Thought,
        body: serde_json::json!({ "parent": req.parent.as_str(), "at_seq": req.at_seq.0 }),
        cites: Vec::<Cite>::new(),
        at: req.at,
        id: None,
    }
}

/// A journal row as `action_intent` writes it.
pub(crate) fn action_row(a: NewAction) -> ActionRow {
    ActionRow {
        id: a
            .id
            .unwrap_or_else(|| ActionId::new(uuid::Uuid::now_v7().to_string())),
        wake: a.wake,
        idem_key: a.idem_key,
        kind: a.kind,
        payload: a.payload,
        status: ActionStatus::Intent,
        result: None,
        at: a.at,
        done_at: None,
    }
}

/// A sealed rollup as `seal_rollup` writes it.
pub(crate) fn sealed(r: NewRollup) -> Rollup {
    Rollup {
        id: r
            .id
            .unwrap_or_else(|| RollupId::new(uuid::Uuid::now_v7().to_string())),
        traj: r.traj,
        kind: r.kind,
        tier: r.tier,
        from_seq: r.from_seq,
        to_seq: r.to_seq,
        src_trajs: r.src_trajs,
        body: r.body,
        notable_refs: r.notable_refs,
        prompt_ver: r.prompt_ver,
        sealed_at: r.sealed_at,
        superseded_by: None,
    }
}

/// A row hash as the invariant module reads it.
pub(crate) fn row_hash(table: &'static str, id: String, hash: String) -> RowHash {
    RowHash {
        table,
        id,
        hash,
        superseded_by: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bough_kernel::{Context, KernelCore};
    use bough_plugin_ledger::{HashScope, LedgerStore};
    use chrono::{TimeZone, Utc};

    fn ctx() -> Context {
        Context::root(KernelCore::new())
    }

    fn store() -> Arc<MemoryStore> {
        MemoryStore::new(ctx())
    }

    fn note(traj: &str, i: u32) -> Append {
        Append {
            traj: TrajId::new(traj),
            wake: WakeId::new("w1"),
            kind: StepType::new("step/start"),
            class: Class::Thought,
            body: serde_json::json!({ "index": i }),
            cites: vec![],
            at: Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
            id: None,
        }
    }

    /// P1-D9's claim, on this provider: the write lock allocates seq the way sqlite's transaction
    /// does, so 32 racing appends produce 1..=32 with no collision and no gap.
    #[tokio::test]
    async fn concurrent_appends_produce_a_contiguous_seq_run() {
        let s = store();
        let mut set = tokio::task::JoinSet::new();
        for i in 0..32u32 {
            let s = s.clone();
            set.spawn(async move { s.append(note("t", i)).await.unwrap().seq });
        }
        let mut seqs: Vec<u64> = Vec::new();
        while let Some(r) = set.join_next().await {
            seqs.push(r.unwrap().0);
        }
        seqs.sort_unstable();
        assert_eq!(seqs, (1..=32).collect::<Vec<u64>>());
    }

    /// `superseded_by` is set once: the second write is refused, naming the id already there.
    #[tokio::test]
    async fn supersede_twice_is_refused() {
        let s = store();
        let mk = |id: &str| NewRollup {
            id: Some(RollupId::new(id)),
            traj: TrajId::new("t"),
            kind: RollupKind::Tier,
            tier: 1,
            from_seq: Seq(1),
            to_seq: Seq(2),
            src_trajs: vec![],
            body: serde_json::json!({}),
            notable_refs: BTreeSet::new(),
            prompt_ver: "p1".into(),
            sealed_at: Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
        };
        for id in ["old", "new", "newer"] {
            s.seal_rollup(mk(id)).await.unwrap();
        }
        let old = RollupId::new("old");
        s.supersede_rollup(&old, &RollupId::new("new"))
            .await
            .unwrap();
        let err = s
            .supersede_rollup(&old, &RollupId::new("newer"))
            .await
            .unwrap_err();
        assert!(
            matches!(err, LedgerError::AlreadySuperseded(ref o, ref n)
                     if o.as_str() == "old" && n.as_str() == "new"),
            "expected AlreadySuperseded(old, new), got {err}"
        );
    }

    /// Append-only is structural: there is no method that writes a committed step, and everything
    /// the API does offer leaves every existing row hash exactly where it was.
    #[tokio::test]
    async fn no_api_mutates_a_committed_step() {
        let s = store();
        let first = s.append(note("t", 0)).await.unwrap();
        let before = s.row_hashes(HashScope::Steps).await.unwrap();

        // Everything the trait offers that could plausibly touch a row.
        s.append(note("t", 1)).await.unwrap();
        s.append(note("u", 0)).await.unwrap();
        s.add_edge(Edge {
            child: TrajId::new("u"),
            parent: TrajId::new("t"),
            at_seq: Seq(1),
            kind: EdgeKind::Ancestor,
            at: Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
        })
        .await
        .unwrap();
        s.put_agent(AgentRow {
            name: AgentName::new("sol"),
            traj: TrajId::new("t"),
            routing_refs: BTreeSet::new(),
            wake_classes: BTreeSet::new(),
            model_override: None,
            tick_floor: None,
            digest_rollup: None,
        })
        .await
        .unwrap();

        let after = s.row_hashes(HashScope::Steps).await.unwrap();
        let mine = |v: &Vec<RowHash>| {
            v.iter()
                .find(|h| h.id == first.id.as_str())
                .expect("the first step still has a row hash")
                .hash
                .clone()
        };
        assert_eq!(
            mine(&before),
            mine(&after),
            "a committed step never changes"
        );
        assert_eq!(
            s.step(&first.id).await.unwrap().unwrap(),
            first,
            "and reads back byte-identical"
        );
    }

    /// No persistence, no file, no config: when the fiber unloads the store goes with it, and a
    /// fresh store on the same context sees nothing of what the old one held.
    #[tokio::test]
    async fn an_unloaded_fiber_leaves_no_state() {
        let ctx = ctx();
        let first = MemoryStore::new(ctx.clone());
        first.append(note("t", 0)).await.unwrap();
        assert_eq!(
            first.head_seq(&TrajId::new("t")).await.unwrap(),
            Some(Seq(1))
        );
        drop(first);

        let second = MemoryStore::new(ctx);
        assert_eq!(
            second.head_seq(&TrajId::new("t")).await.unwrap(),
            None,
            "the new store starts empty; nothing outlives the old one"
        );
        assert!(second.agents().await.unwrap().is_empty());
    }
}
