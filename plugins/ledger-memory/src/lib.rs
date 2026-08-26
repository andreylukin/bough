//! Invariant: this is the TEST ledger Provider (§3). It is a behavioural TWIN of `ledger-sqlite`,
//! not an approximation: same seq allocation under concurrency, same derived `step_refs` (the
//! Definition's function), same class and schema refusals, same unknown-type read rule, same fork
//! validation, same `connected`, same deterministic search ordering. Its bundle row is
//! `ledger-memory`, and it is in NO bundle: the swap patch names it.

pub mod invariant;
pub mod search;
pub mod store;

use std::collections::BTreeSet;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use bough_kernel::{Context, InvariantSpec, Plugin, PluginError};
use bough_plugin_ledger::*;

use crate::store::MemoryStore;

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "ledger-memory";

/// No configuration at all — an empty struct, so the swap patch can write `config: {}`.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MemoryConfig {}

/// The provider plugin.
pub struct MemoryLedgerPlugin;

#[async_trait::async_trait]
impl Plugin for MemoryLedgerPlugin {
    const NAME: &'static str = "ledger-memory";
    type Config = MemoryConfig;

    async fn apply(ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let entry = ctx.entry_id().clone();
        let store = MemoryStore::new(ctx.clone());
        ctx.provide::<Ledger>(LedgerHandle(store))
            .await
            .map_err(|e| PluginError::new(entry, e))?;

        // The recorded stream the four ledger invariants read. It is per fiber LIFE: a reload
        // keeps the `FiberUid`, so this fiber's observations are forgotten when it unloads or the
        // invariants would flag the reload itself (§0.3).
        let mine = ctx.fiber_uid();
        ctx.effect(move |e| async move {
            e.defer_sync(move || bough_plugin_ledger::invariant::forget(mine));
            Ok(())
        })
        .await?;
        ctx.on::<LedgerStep, _, _>(move |step| async move {
            bough_plugin_ledger::invariant::record(bough_plugin_ledger::invariant::Obs {
                fiber: mine,
                traj: step.traj.clone(),
                seq: step.seq,
                wake: step.wake.clone(),
                kind: step.kind.clone(),
            });
        })
        .await?;
        Ok(())
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

bough_kernel::register_plugin!(MemoryLedgerPlugin);

#[async_trait::async_trait]
impl LedgerStore for MemoryStore {
    fn provider(&self) -> &'static str {
        MemoryLedgerPlugin::NAME
    }
    fn format_version(&self) -> u32 {
        LEDGER_FORMAT_VERSION
    }

    fn register_step_type(&self, def: StepTypeDef) -> Result<StepTypeToken, LedgerError> {
        self.types.register(def)
    }
    fn step_types(&self) -> Vec<StepTypeDef> {
        self.types.all()
    }
    fn skipped_ignorable(&self) -> u64 {
        self.skipped.load(Ordering::SeqCst)
    }

    async fn append(&self, req: Append) -> Result<Step, LedgerError> {
        let mut out = self.commit(vec![req])?;
        // POST-COMMIT: the row is already readable when `ledger/step` fires (§0.2).
        self.announce(&out);
        Ok(out.remove(0))
    }
    async fn append_batch(&self, reqs: Vec<Append>) -> Result<Vec<Step>, LedgerError> {
        let out = self.commit(reqs)?;
        self.announce(&out);
        Ok(out)
    }

    async fn step(&self, id: &StepId) -> Result<Option<Step>, LedgerError> {
        let inner = self.inner.read();
        let Some((traj, seq)) = inner.by_id.get(id) else {
            return Ok(None);
        };
        let Some(step) = inner
            .steps
            .get(traj)
            .and_then(|c| c.iter().find(|s| s.seq == *seq))
        else {
            return Ok(None);
        };
        if self.admit(step)? {
            Ok(Some(step.clone()))
        } else {
            Ok(None)
        }
    }

    async fn steps(&self, q: &StepQuery) -> Result<Vec<Step>, LedgerError> {
        let inner = self.inner.read();
        let mut out: Vec<Step> = self
            .readable_many(&inner, &q.trajs)?
            .into_iter()
            .filter(|s| q.kinds.is_empty() || q.kinds.contains(&s.kind))
            .filter(|s| q.class.map(|c| c == s.class).unwrap_or(true))
            .filter(|s| q.wake.as_ref().map(|w| w == &s.wake).unwrap_or(true))
            .filter(|s| q.after.map(|a| s.seq > a).unwrap_or(true))
            .filter(|s| q.before.map(|b| s.seq < b).unwrap_or(true))
            .filter(|s| q.refs.is_empty() || q.refs.iter().any(|r| s.refs.contains(r)))
            .collect();
        // Deterministic on both providers: seq, then trajectory as the tiebreak.
        out.sort_by(|a, b| a.seq.cmp(&b.seq).then_with(|| a.traj.cmp(&b.traj)));
        if q.order == Order::SeqDesc {
            out.reverse();
        }
        if let Some(n) = q.limit {
            out.truncate(n);
        }
        Ok(out)
    }

    async fn tail(&self, traj: &TrajId, n: usize) -> Result<Vec<Step>, LedgerError> {
        let inner = self.inner.read();
        let all = self.readable(&inner, traj)?;
        let from = all.len().saturating_sub(n);
        Ok(all[from..].to_vec())
    }

    async fn head_seq(&self, traj: &TrajId) -> Result<Option<Seq>, LedgerError> {
        let inner = self.inner.read();
        // The head is the last COMMITTED seq, whether or not this binary can read the row: it is
        // what the next append allocates from.
        Ok(inner.steps.get(traj).and_then(|c| c.last()).map(|s| s.seq))
    }

    async fn search(&self, q: &SearchQuery) -> Result<Vec<SearchHit>, LedgerError> {
        crate::search::search(self, q)
    }

    async fn live_pins(&self, trajs: &[TrajId]) -> Result<Vec<Pin>, LedgerError> {
        let inner = self.inner.read();
        let steps = self.readable_many(&inner, trajs)?;
        Ok(store::live_pins_from(&steps))
    }

    async fn unconsumed_mail(&self, traj: &TrajId) -> Result<Vec<Step>, LedgerError> {
        let inner = self.inner.read();
        let steps = self.readable(&inner, traj)?;
        Ok(store::unconsumed_mail_from(&steps))
    }

    async fn add_edge(&self, e: Edge) -> Result<(), LedgerError> {
        let mut inner = self.inner.write();
        // `edges` has PRIMARY KEY (child, parent, kind): the same edge twice is one row.
        if !inner
            .edges
            .iter()
            .any(|x| x.child == e.child && x.parent == e.parent && x.kind == e.kind)
        {
            inner.edges.push(e);
        }
        Ok(())
    }

    async fn edges(&self, traj: &TrajId) -> Result<Vec<Edge>, LedgerError> {
        let inner = self.inner.read();
        Ok(inner
            .edges
            .iter()
            .filter(|e| &e.child == traj || &e.parent == traj)
            .cloned()
            .collect())
    }

    async fn ancestry(&self, traj: &TrajId) -> Result<Vec<TrajId>, LedgerError> {
        let inner = self.inner.read();
        Ok(store::ancestry_from(&inner.edges, traj))
    }

    async fn fork(&self, req: Fork) -> Result<ForkOutcome, LedgerError> {
        // The prefix check and the two writes are ONE critical section: a refused fork writes
        // nothing, and a granted one cannot interleave with an append to the parent.
        let seed = store::end_seed_append(&req);
        let def = self.types.validate_append(&seed)?;
        let mut inner = self.inner.write();
        let parent_chain = inner
            .steps
            .get(&req.parent)
            .ok_or_else(|| LedgerError::NoSuchTrajectory(req.parent.clone()))?;
        if let Some((wake, opened_at)) = store::open_wake_at(parent_chain, req.at_seq) {
            // REFUSED, never clipped (§3).
            return Err(LedgerError::ForkInsideOpenWake {
                parent: req.parent.clone(),
                at_seq: req.at_seq,
                wake,
                opened_at,
            });
        }
        let edge = Edge {
            child: req.child.clone(),
            parent: req.parent.clone(),
            at_seq: req.at_seq,
            kind: EdgeKind::Ancestor,
            at: req.at,
        };
        inner.edges.push(edge.clone());
        let end_seed = store::insert_for_fork(&mut inner, seed, def.ignorable);
        drop(inner);
        self.announce(std::slice::from_ref(&end_seed));
        Ok(ForkOutcome { edge, end_seed })
    }

    async fn connected(&self, agent: &AgentName) -> Result<Connected, LedgerError> {
        // Derived AT NEED and writes nothing (§3): three scans over the committed rows.
        let inner = self.inner.read();
        let row = inner
            .agents
            .get(agent)
            .ok_or_else(|| LedgerError::Store(anyhow::anyhow!("no such agent `{agent}`")))?
            .clone();
        let ancestry = store::ancestry_from(&inner.edges, &row.traj);
        let mut ref_matches: BTreeSet<TrajId> = BTreeSet::new();
        if !row.routing_refs.is_empty() {
            for (traj, chain) in inner.steps.iter() {
                if traj == &row.traj {
                    continue;
                }
                if chain
                    .iter()
                    .any(|s| store::matches_any(s, &row.routing_refs))
                {
                    ref_matches.insert(traj.clone());
                }
            }
        }
        Ok(Connected {
            own: row.traj,
            ancestry,
            ref_matches: ref_matches.into_iter().collect(),
            refs: row.routing_refs,
        })
    }

    async fn seal_rollup(&self, r: NewRollup) -> Result<Rollup, LedgerError> {
        let rollup = store::sealed(r);
        let mut inner = self.inner.write();
        inner.rollups.insert(rollup.id.clone(), rollup.clone());
        Ok(rollup)
    }

    async fn supersede_rollup(&self, old: &RollupId, new: &RollupId) -> Result<(), LedgerError> {
        let mut inner = self.inner.write();
        let row = inner
            .rollups
            .get_mut(old)
            .ok_or_else(|| LedgerError::Store(anyhow::anyhow!("no such rollup `{old}`")))?;
        // The ONE permitted write to a sealed row, and it is set once (§3).
        if let Some(existing) = &row.superseded_by {
            return Err(LedgerError::AlreadySuperseded(
                old.clone(),
                existing.clone(),
            ));
        }
        row.superseded_by = Some(new.clone());
        Ok(())
    }

    async fn rollups(&self, q: &RollupQuery) -> Result<Vec<Rollup>, LedgerError> {
        let inner = self.inner.read();
        let mut out: Vec<Rollup> = inner
            .rollups
            .values()
            .filter(|r| q.trajs.is_empty() || q.trajs.contains(&r.traj))
            .filter(|r| q.kind.map(|k| k == r.kind).unwrap_or(true))
            .filter(|r| q.max_tier.map(|t| r.tier <= t).unwrap_or(true))
            .filter(|r| q.include_superseded || r.superseded_by.is_none())
            .cloned()
            .collect();
        out.sort_by(|a, b| {
            a.traj
                .cmp(&b.traj)
                .then_with(|| a.tier.cmp(&b.tier))
                .then_with(|| a.from_seq.cmp(&b.from_seq))
                .then_with(|| a.id.cmp(&b.id))
        });
        if let Some(n) = q.limit {
            out.truncate(n);
        }
        Ok(out)
    }

    async fn put_agent(&self, a: AgentRow) -> Result<(), LedgerError> {
        // The one mutable map: §3 exempts `agents` from append-only.
        self.inner.write().agents.insert(a.name.clone(), a);
        Ok(())
    }
    async fn agent(&self, name: &AgentName) -> Result<Option<AgentRow>, LedgerError> {
        Ok(self.inner.read().agents.get(name).cloned())
    }
    async fn agents(&self) -> Result<Vec<AgentRow>, LedgerError> {
        Ok(self.inner.read().agents.values().cloned().collect())
    }
    async fn delete_agent(&self, name: &AgentName) -> Result<(), LedgerError> {
        self.inner.write().agents.remove(name);
        Ok(())
    }

    async fn action_intent(&self, a: NewAction) -> Result<ActionRow, LedgerError> {
        let row = store::action_row(a);
        self.inner
            .write()
            .actions
            .insert(row.id.clone(), row.clone());
        Ok(row)
    }
    async fn action_done(
        &self,
        id: &ActionId,
        status: ActionStatus,
        result: serde_json::Value,
    ) -> Result<(), LedgerError> {
        let mut inner = self.inner.write();
        let row = inner
            .actions
            .get_mut(id)
            .ok_or_else(|| LedgerError::Store(anyhow::anyhow!("no such action `{id}`")))?;
        row.status = status;
        row.result = Some(result);
        row.done_at = Some(chrono::Utc::now());
        Ok(())
    }
    async fn actions(&self, q: &ActionQuery) -> Result<Vec<ActionRow>, LedgerError> {
        let inner = self.inner.read();
        let mut out: Vec<ActionRow> = inner
            .actions
            .values()
            .filter(|a| q.ids.is_empty() || q.ids.contains(&a.id))
            .filter(|a| q.wake.as_ref().map(|w| w == &a.wake).unwrap_or(true))
            .filter(|a| q.status.map(|s| s == a.status).unwrap_or(true))
            .cloned()
            .collect();
        out.sort_by(|a, b| a.at.cmp(&b.at).then_with(|| a.id.cmp(&b.id)));
        if let Some(n) = q.limit {
            out.truncate(n);
        }
        Ok(out)
    }

    async fn row_hashes(&self, scope: HashScope) -> Result<Vec<RowHash>, LedgerError> {
        let inner = self.inner.read();
        let mut out = Vec::new();
        if matches!(scope, HashScope::All | HashScope::Steps) {
            for chain in inner.steps.values() {
                for s in chain {
                    out.push(store::row_hash(
                        "steps",
                        s.id.as_str().to_string(),
                        store::hash_step(s),
                    ));
                }
            }
        }
        if matches!(scope, HashScope::All | HashScope::Edges) {
            for e in &inner.edges {
                let (id, hash) = store::hash_edge(e);
                out.push(store::row_hash("edges", id, hash));
            }
        }
        if matches!(scope, HashScope::All | HashScope::Rollups) {
            for r in inner.rollups.values() {
                out.push(RowHash {
                    table: "rollups",
                    id: r.id.as_str().to_string(),
                    hash: store::hash_rollup(r),
                    // Reported BESIDE the hash, never inside it, so a legal set-once write is not
                    // a row change.
                    superseded_by: r.superseded_by.as_ref().map(|s| s.as_str().to_string()),
                });
            }
        }
        out.sort_by(|a, b| a.table.cmp(b.table).then_with(|| a.id.cmp(&b.id)));
        Ok(out)
    }

    async fn trajectory_view(&self, traj: &TrajId) -> Result<TrajectoryView, LedgerError> {
        let inner = self.inner.read();
        let steps = self.readable(&inner, traj)?;
        let edges: Vec<Edge> = inner
            .edges
            .iter()
            .filter(|e| &e.child == traj || &e.parent == traj)
            .cloned()
            .collect();
        let mut rollups: Vec<Rollup> = inner
            .rollups
            .values()
            .filter(|r| &r.traj == traj)
            .cloned()
            .collect();
        rollups.sort_by(|a, b| {
            a.tier
                .cmp(&b.tier)
                .then_with(|| a.from_seq.cmp(&b.from_seq))
                .then_with(|| a.id.cmp(&b.id))
        });
        let agent = inner.agents.values().find(|a| &a.traj == traj).cloned();
        Ok(TrajectoryView {
            traj: traj.clone(),
            steps,
            edges,
            rollups,
            agent,
        })
    }
}
