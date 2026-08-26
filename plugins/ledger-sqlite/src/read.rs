//! Invariant: a row whose type is unknown to this binary is REFUSED on read
//! ([`LedgerError::UnknownStepTypeOnRead`]) unless the row's stored `ignorable` flag is set, in
//! which case it is skipped and COUNTED — a skip nobody can see is indistinguishable from data
//! loss (§3, P1-D7).

use std::collections::{BTreeSet, HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use bough_plugin_ledger::vocabulary::{PinRetire, PinSet, WakeEnd};
use bough_plugin_ledger::{
    refs, ActionId, ActionQuery, ActionRow, ActionStatus, AgentName, AgentRow, Cite, Class, Edge,
    EdgeKind, HashScope, LedgerError, NewAction, NewRollup, Order, Pin, Rollup, RollupId,
    RollupKind, RollupQuery, RowHash, Seq, SeqRange, Step, StepId, StepQuery, StepType,
    StepTypeMap, TrajId, TrajectoryView, WakeId,
};
use chrono::{DateTime, Utc};
use rusqlite::Connection;
use sha2::{Digest, Sha256};

use crate::append::json_err;
use crate::schema::store_err;
use crate::store::SqliteStore;

/// Every column of `steps`, in envelope order. Spelled once so the row mapper cannot drift.
pub(crate) const STEP_COLS: &str =
    "id, traj_id, seq, at, wake_id, type, class, body, cites, ignorable";

/// Materialize one row, applying the unknown-type rule.
///
/// Deviation from the plan's signature (§2.8 named `(&SqliteStore, &Row)`): the mapper runs inside
/// a `spawn_blocking` closure that cannot borrow the store, so it takes the two pieces of the
/// store it actually needs.
pub fn row_to_step(
    types: &StepTypeMap,
    skipped: &AtomicU64,
    row: &rusqlite::Row<'_>,
) -> Result<Option<Step>, LedgerError> {
    let id = StepId::new(row.get::<_, String>(0).map_err(store_err)?);
    let traj = TrajId::new(row.get::<_, String>(1).map_err(store_err)?);
    let seq = Seq(row.get::<_, i64>(2).map_err(store_err)? as u64);
    let at = parse_time(&row.get::<_, String>(3).map_err(store_err)?)?;
    let wake = WakeId::new(row.get::<_, String>(4).map_err(store_err)?);
    let kind = StepType::new(row.get::<_, String>(5).map_err(store_err)?);
    let class = parse_class(&row.get::<_, String>(6).map_err(store_err)?)?;
    let body: serde_json::Value =
        serde_json::from_str(&row.get::<_, String>(7).map_err(store_err)?).map_err(json_err)?;
    let cites: Vec<Cite> =
        serde_json::from_str(&row.get::<_, String>(8).map_err(store_err)?).map_err(json_err)?;
    let ignorable = row.get::<_, i64>(9).map_err(store_err)? != 0;

    if types.get(&kind).is_none() {
        if ignorable {
            skipped.fetch_add(1, Ordering::Relaxed);
            return Ok(None);
        }
        return Err(LedgerError::UnknownStepTypeOnRead {
            step: id,
            traj,
            kind,
        });
    }

    let step_refs = refs::derive_step_refs(&cites, &body);
    Ok(Some(Step {
        id,
        traj,
        seq,
        at,
        wake,
        kind,
        class,
        body: Arc::new(body),
        cites: Arc::new(cites),
        refs: Arc::new(step_refs),
        ignorable,
    }))
}

fn parse_class(s: &str) -> Result<Class, LedgerError> {
    match s {
        "evidence" => Ok(Class::Evidence),
        "thought" => Ok(Class::Thought),
        other => Err(LedgerError::Store(anyhow::anyhow!(
            "`{other}` is not a step class"
        ))),
    }
}

pub(crate) fn parse_time(s: &str) -> Result<DateTime<Utc>, LedgerError> {
    DateTime::parse_from_rfc3339(s)
        .map(|t| t.with_timezone(&Utc))
        .map_err(|e| LedgerError::Store(anyhow::Error::new(e)))
}

/// Run `sql` (whose parameters are `params`) and map every surviving row to a [`Step`].
pub(crate) fn query_steps(
    conn: &Connection,
    types: &StepTypeMap,
    skipped: &AtomicU64,
    sql: &str,
    params: &[&dyn rusqlite::ToSql],
) -> Result<Vec<Step>, LedgerError> {
    let mut stmt = conn.prepare(sql).map_err(store_err)?;
    let mut rows = stmt.query(params).map_err(store_err)?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().map_err(store_err)? {
        if let Some(step) = row_to_step(types, skipped, row)? {
            out.push(step);
        }
    }
    Ok(out)
}

/// A `?n` placeholder list for an `IN` clause, e.g. `?3, ?4, ?5`.
fn placeholders(start: usize, n: usize) -> String {
    (start..start + n)
        .map(|i| format!("?{i}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// [`bough_plugin_ledger::LedgerStore::steps`].
pub async fn steps(store: &SqliteStore, q: &StepQuery) -> Result<Vec<Step>, LedgerError> {
    let (types, skipped) = (store.types.clone(), store.skipped.clone());
    let q = q.clone();
    store
        .with_conn(move |conn| {
            let mut sql = format!("SELECT DISTINCT {} FROM steps s", prefixed(STEP_COLS, "s"));
            let mut args: Vec<String> = Vec::new();
            let mut clauses: Vec<String> = Vec::new();

            if !q.refs.is_empty() {
                let n = args.len() + 1;
                sql.push_str(" JOIN step_refs r ON r.step_id = s.id");
                clauses.push(format!("r.ref IN ({})", placeholders(n, q.refs.len())));
                args.extend(q.refs.iter().map(|r| r.as_str().to_string()));
            }
            if !q.trajs.is_empty() {
                let n = args.len() + 1;
                clauses.push(format!("s.traj_id IN ({})", placeholders(n, q.trajs.len())));
                args.extend(q.trajs.iter().map(|t| t.as_str().to_string()));
            }
            if !q.kinds.is_empty() {
                let n = args.len() + 1;
                clauses.push(format!("s.type IN ({})", placeholders(n, q.kinds.len())));
                args.extend(q.kinds.iter().map(|k| k.as_str().to_string()));
            }
            if let Some(class) = q.class {
                args.push(class.as_str().to_string());
                clauses.push(format!("s.class = ?{}", args.len()));
            }
            if let Some(wake) = &q.wake {
                args.push(wake.as_str().to_string());
                clauses.push(format!("s.wake_id = ?{}", args.len()));
            }
            if let Some(after) = q.after {
                args.push(after.0.to_string());
                clauses.push(format!("s.seq > CAST(?{} AS INTEGER)", args.len()));
            }
            if let Some(before) = q.before {
                args.push(before.0.to_string());
                clauses.push(format!("s.seq < CAST(?{} AS INTEGER)", args.len()));
            }
            if !clauses.is_empty() {
                sql.push_str(" WHERE ");
                sql.push_str(&clauses.join(" AND "));
            }
            sql.push_str(match q.order {
                Order::SeqAsc => " ORDER BY s.seq ASC, s.traj_id ASC",
                Order::SeqDesc => " ORDER BY s.seq DESC, s.traj_id ASC",
            });
            if let Some(limit) = q.limit {
                sql.push_str(&format!(" LIMIT {limit}"));
            }

            let bound: Vec<&dyn rusqlite::ToSql> =
                args.iter().map(|a| a as &dyn rusqlite::ToSql).collect();
            query_steps(conn, &types, &skipped, &sql, &bound)
        })
        .await
}

/// `id, traj_id, ...` → `s.id, s.traj_id, ...`.
fn prefixed(cols: &str, alias: &str) -> String {
    cols.split(", ")
        .map(|c| format!("{alias}.{c}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// One step by id.
pub async fn step(store: &SqliteStore, id: &StepId) -> Result<Option<Step>, LedgerError> {
    let (types, skipped) = (store.types.clone(), store.skipped.clone());
    let id = id.clone();
    store
        .with_conn(move |conn| {
            let sql = format!("SELECT {STEP_COLS} FROM steps WHERE id = ?1");
            let found = query_steps(conn, &types, &skipped, &sql, &[&id.as_str()])?;
            Ok(found.into_iter().next())
        })
        .await
}

/// The newest `n` steps of `traj`, returned OLDEST FIRST — the order the verbatim tail renders in.
pub async fn tail(store: &SqliteStore, traj: &TrajId, n: usize) -> Result<Vec<Step>, LedgerError> {
    let (types, skipped) = (store.types.clone(), store.skipped.clone());
    let traj = traj.clone();
    store
        .with_conn(move |conn| {
            let mut out = query_steps(
                conn,
                &types,
                &skipped,
                crate::store::TAIL_SQL,
                &[&traj.as_str(), &(n as i64)],
            )?;
            out.reverse();
            Ok(out)
        })
        .await
}

/// The last allocated seq of `traj`, or `None` when the trajectory has no steps.
pub async fn head_seq(store: &SqliteStore, traj: &TrajId) -> Result<Option<Seq>, LedgerError> {
    let traj = traj.clone();
    store
        .with_conn(move |conn| {
            let head: Option<i64> = conn
                .query_row(
                    "SELECT MAX(seq) FROM steps WHERE traj_id = ?1",
                    rusqlite::params![traj.as_str()],
                    |r| r.get(0),
                )
                .map_err(store_err)?;
            Ok(head.map(|s| Seq(s as u64)))
        })
        .await
}

/// Live pins: every `pin/set` minus every id a later `pin/set.supersedes` or `pin/retire.retires`
/// names. Age is never a criterion (§3).
pub async fn live_pins(store: &SqliteStore, trajs: &[TrajId]) -> Result<Vec<Pin>, LedgerError> {
    let q = StepQuery {
        trajs: trajs.to_vec(),
        kinds: vec![StepType::new("pin/set"), StepType::new("pin/retire")],
        order: Order::SeqAsc,
        ..Default::default()
    };
    let rows = steps(store, &q).await?;

    let mut dead: HashSet<String> = HashSet::new();
    for s in &rows {
        match s.kind.as_str() {
            "pin/set" => {
                let set: PinSet = serde_json::from_value((*s.body).clone()).map_err(json_err)?;
                dead.extend(set.supersedes.iter().map(|i| i.as_str().to_string()));
            }
            "pin/retire" => {
                let ret: PinRetire = serde_json::from_value((*s.body).clone()).map_err(json_err)?;
                dead.extend(ret.retires.iter().map(|i| i.as_str().to_string()));
            }
            _ => {}
        }
    }

    let mut out = Vec::new();
    for s in rows {
        if s.kind.as_str() != "pin/set" || dead.contains(s.id.as_str()) {
            continue;
        }
        let set: PinSet = serde_json::from_value((*s.body).clone()).map_err(json_err)?;
        out.push(Pin {
            step: s.id.clone(),
            traj: s.traj.clone(),
            seq: s.seq,
            class: s.class,
            title: set.title,
            text: set.text,
        });
    }
    Ok(out)
}

/// Delivered mail not named by any `wake/end.consumed` set. Union, order-independent (§5).
pub async fn unconsumed_mail(store: &SqliteStore, traj: &TrajId) -> Result<Vec<Step>, LedgerError> {
    let ends = steps(
        store,
        &StepQuery {
            trajs: vec![traj.clone()],
            kinds: vec![StepType::new("wake/end")],
            ..Default::default()
        },
    )
    .await?;
    let mut ranges: Vec<SeqRange> = Vec::new();
    for e in &ends {
        let end: WakeEnd = serde_json::from_value((*e.body).clone()).map_err(json_err)?;
        ranges.extend(end.consumed);
    }
    let consumed = SeqRange::union(&ranges);

    let mail = steps(
        store,
        &StepQuery {
            trajs: vec![traj.clone()],
            kinds: vec![StepType::new("mail/delivered")],
            ..Default::default()
        },
    )
    .await?;
    Ok(mail
        .into_iter()
        .filter(|m| !consumed.iter().any(|r| r.contains(m.seq)))
        .collect())
}

// ---- edges -----------------------------------------------------------------

/// Every edge `traj` takes part in, as child or as parent.
pub async fn edges(store: &SqliteStore, traj: &TrajId) -> Result<Vec<Edge>, LedgerError> {
    let traj = traj.clone();
    store
        .with_conn(move |conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT child_traj, parent_traj, at_seq, kind, at FROM edges \
                     WHERE child_traj = ?1 OR parent_traj = ?1 \
                     ORDER BY child_traj, parent_traj, kind",
                )
                .map_err(store_err)?;
            let mut rows = stmt.query([traj.as_str()]).map_err(store_err)?;
            let mut out = Vec::new();
            while let Some(r) = rows.next().map_err(store_err)? {
                out.push(row_to_edge(r)?);
            }
            Ok(out)
        })
        .await
}

pub(crate) fn row_to_edge(r: &rusqlite::Row<'_>) -> Result<Edge, LedgerError> {
    let kind = match r.get::<_, String>(3).map_err(store_err)?.as_str() {
        "ancestor" => EdgeKind::Ancestor,
        "merge" => EdgeKind::Merge,
        other => {
            return Err(LedgerError::Store(anyhow::anyhow!(
                "`{other}` is not an edge kind"
            )))
        }
    };
    Ok(Edge {
        child: TrajId::new(r.get::<_, String>(0).map_err(store_err)?),
        parent: TrajId::new(r.get::<_, String>(1).map_err(store_err)?),
        at_seq: Seq(r.get::<_, i64>(2).map_err(store_err)? as u64),
        kind,
        at: parse_time(&r.get::<_, String>(4).map_err(store_err)?)?,
    })
}

/// Transitive parents of `traj`, nearest first, never including `traj` itself.
pub async fn ancestry(store: &SqliteStore, traj: &TrajId) -> Result<Vec<TrajId>, LedgerError> {
    let traj = traj.clone();
    store
        .with_conn(move |conn| {
            let mut stmt = conn
                .prepare("SELECT parent_traj FROM edges WHERE child_traj = ?1 ORDER BY parent_traj")
                .map_err(store_err)?;
            let mut seen: HashSet<String> = HashSet::from([traj.as_str().to_string()]);
            let mut queue: VecDeque<String> = VecDeque::from([traj.as_str().to_string()]);
            let mut out = Vec::new();
            while let Some(next) = queue.pop_front() {
                let parents: Vec<String> = stmt
                    .query_map([next.as_str()], |r| r.get::<_, String>(0))
                    .map_err(store_err)?
                    .collect::<Result<_, _>>()
                    .map_err(store_err)?;
                for p in parents {
                    if seen.insert(p.clone()) {
                        out.push(TrajId::new(&p));
                        queue.push_back(p);
                    }
                }
            }
            Ok(out)
        })
        .await
}

/// Write one edge. `INSERT OR IGNORE`: the primary key already makes an edge idempotent, and an
/// `UPDATE` would abort at the trigger.
pub async fn add_edge(store: &SqliteStore, e: Edge) -> Result<(), LedgerError> {
    store
        .with_conn(move |conn| {
            insert_edge(conn, &e)?;
            Ok(())
        })
        .await
}

pub(crate) fn insert_edge(conn: &Connection, e: &Edge) -> Result<(), LedgerError> {
    conn.execute(
        "INSERT OR IGNORE INTO edges (child_traj, parent_traj, at_seq, kind, at) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![
            e.child.as_str(),
            e.parent.as_str(),
            e.at_seq.0 as i64,
            match e.kind {
                EdgeKind::Ancestor => "ancestor",
                EdgeKind::Merge => "merge",
            },
            e.at.to_rfc3339(),
        ],
    )
    .map_err(store_err)?;
    Ok(())
}

// ---- rollups ---------------------------------------------------------------

const ROLLUP_COLS: &str = "id, traj_id, kind, tier, from_seq, to_seq, src_trajs, body, \
                           notable_refs, prompt_ver, sealed_at, superseded_by";

fn row_to_rollup(r: &rusqlite::Row<'_>) -> Result<Rollup, LedgerError> {
    let kind: RollupKind = serde_json::from_value(serde_json::Value::String(
        r.get::<_, String>(2).map_err(store_err)?,
    ))
    .map_err(json_err)?;
    let src: Vec<String> =
        serde_json::from_str(&r.get::<_, String>(6).map_err(store_err)?).map_err(json_err)?;
    let notable: Vec<String> =
        serde_json::from_str(&r.get::<_, String>(8).map_err(store_err)?).map_err(json_err)?;
    Ok(Rollup {
        id: RollupId::new(r.get::<_, String>(0).map_err(store_err)?),
        traj: TrajId::new(r.get::<_, String>(1).map_err(store_err)?),
        kind,
        tier: r.get::<_, i64>(3).map_err(store_err)? as u8,
        from_seq: Seq(r.get::<_, i64>(4).map_err(store_err)? as u64),
        to_seq: Seq(r.get::<_, i64>(5).map_err(store_err)? as u64),
        src_trajs: src.into_iter().map(TrajId::new).collect(),
        body: serde_json::from_str(&r.get::<_, String>(7).map_err(store_err)?).map_err(json_err)?,
        notable_refs: notable
            .into_iter()
            .map(bough_plugin_ledger::Ref::new)
            .collect(),
        prompt_ver: r.get::<_, String>(9).map_err(store_err)?,
        sealed_at: parse_time(&r.get::<_, String>(10).map_err(store_err)?)?,
        superseded_by: r
            .get::<_, Option<String>>(11)
            .map_err(store_err)?
            .map(RollupId::new),
    })
}

/// Seal a rollup. Immutable from here on, bar the one set-once `superseded_by` write.
pub async fn seal_rollup(store: &SqliteStore, r: NewRollup) -> Result<Rollup, LedgerError> {
    let id =
        r.id.clone()
            .unwrap_or_else(|| RollupId::new(uuid::Uuid::now_v7().to_string()));
    let rollup = Rollup {
        id,
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
    };
    let written = rollup.clone();
    store
        .with_conn(move |conn| {
            let kind = serde_json::to_value(written.kind).map_err(json_err)?;
            conn.execute(
                &format!(
                    "INSERT INTO rollups ({ROLLUP_COLS}) VALUES \
                     (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, NULL)"
                ),
                rusqlite::params![
                    written.id.as_str(),
                    written.traj.as_str(),
                    kind.as_str().unwrap_or_default(),
                    written.tier as i64,
                    written.from_seq.0 as i64,
                    written.to_seq.0 as i64,
                    serde_json::to_string(
                        &written
                            .src_trajs
                            .iter()
                            .map(|t| t.as_str())
                            .collect::<Vec<_>>()
                    )
                    .map_err(json_err)?,
                    serde_json::to_string(&written.body).map_err(json_err)?,
                    serde_json::to_string(
                        &written
                            .notable_refs
                            .iter()
                            .map(|r| r.as_str())
                            .collect::<Vec<_>>()
                    )
                    .map_err(json_err)?,
                    written.prompt_ver,
                    written.sealed_at.to_rfc3339(),
                ],
            )
            .map_err(store_err)?;
            Ok(())
        })
        .await?;
    Ok(rollup)
}

/// The ONE permitted write to a sealed row (§3), refused a second time by BOTH this check and the
/// `rollups_seal_once` trigger beneath it.
pub async fn supersede_rollup(
    store: &SqliteStore,
    old: &RollupId,
    new: &RollupId,
) -> Result<(), LedgerError> {
    let (owned_old, owned_new) = (old.clone(), new.clone());
    let (old, new) = (owned_old.clone(), owned_new.clone());
    store
        .with_conn(move |conn| {
            let tx = conn.transaction().map_err(store_err)?;
            let current: Option<Option<String>> = tx
                .query_row(
                    "SELECT superseded_by FROM rollups WHERE id = ?1",
                    rusqlite::params![old.as_str()],
                    |r| r.get(0),
                )
                .map(Some)
                .or_else(|e| match e {
                    rusqlite::Error::QueryReturnedNoRows => Ok(None),
                    other => Err(store_err(other)),
                })?;
            match current {
                None => {
                    return Err(LedgerError::Store(anyhow::anyhow!(
                        "no such rollup `{old}`"
                    )))
                }
                Some(Some(existing)) => {
                    return Err(LedgerError::AlreadySuperseded(
                        old.clone(),
                        RollupId::new(existing),
                    ))
                }
                Some(None) => {}
            }
            tx.execute(
                "UPDATE rollups SET superseded_by = ?2 WHERE id = ?1",
                rusqlite::params![old.as_str(), new.as_str()],
            )
            .map_err(store_err)?;
            tx.commit().map_err(store_err)?;
            Ok(())
        })
        .await?;
    // The transition is now committed and can never happen again for this rollup, so the
    // `seal_once` invariant's record is written here — the one place it can be written from.
    bough_plugin_ledger::invariant::record_supersession(
        store.ctx.fiber_uid(),
        &owned_old,
        &owned_new,
    );
    Ok(())
}

/// [`bough_plugin_ledger::LedgerStore::rollups`].
pub async fn rollups(store: &SqliteStore, q: &RollupQuery) -> Result<Vec<Rollup>, LedgerError> {
    let q = q.clone();
    store
        .with_conn(move |conn| {
            let mut sql = format!("SELECT {ROLLUP_COLS} FROM rollups");
            let mut args: Vec<String> = Vec::new();
            let mut clauses: Vec<String> = Vec::new();
            if !q.trajs.is_empty() {
                clauses.push(format!("traj_id IN ({})", placeholders(1, q.trajs.len())));
                args.extend(q.trajs.iter().map(|t| t.as_str().to_string()));
            }
            if let Some(kind) = q.kind {
                let k = serde_json::to_value(kind).map_err(json_err)?;
                args.push(k.as_str().unwrap_or_default().to_string());
                clauses.push(format!("kind = ?{}", args.len()));
            }
            if let Some(max) = q.max_tier {
                args.push(max.to_string());
                clauses.push(format!("tier <= CAST(?{} AS INTEGER)", args.len()));
            }
            if !q.include_superseded {
                clauses.push("superseded_by IS NULL".into());
            }
            if !clauses.is_empty() {
                sql.push_str(" WHERE ");
                sql.push_str(&clauses.join(" AND "));
            }
            sql.push_str(" ORDER BY traj_id ASC, tier ASC, from_seq ASC, id ASC");
            if let Some(limit) = q.limit {
                sql.push_str(&format!(" LIMIT {limit}"));
            }
            let bound: Vec<&dyn rusqlite::ToSql> =
                args.iter().map(|a| a as &dyn rusqlite::ToSql).collect();
            let mut stmt = conn.prepare(&sql).map_err(store_err)?;
            let mut rows = stmt.query(bound.as_slice()).map_err(store_err)?;
            let mut out = Vec::new();
            while let Some(r) = rows.next().map_err(store_err)? {
                out.push(row_to_rollup(r)?);
            }
            Ok(out)
        })
        .await
}

// ---- agents (MUTABLE config) -----------------------------------------------

fn row_to_agent(r: &rusqlite::Row<'_>) -> Result<AgentRow, LedgerError> {
    let routing: Vec<String> =
        serde_json::from_str(&r.get::<_, String>(2).map_err(store_err)?).map_err(json_err)?;
    let classes: Vec<String> =
        serde_json::from_str(&r.get::<_, String>(3).map_err(store_err)?).map_err(json_err)?;
    Ok(AgentRow {
        name: AgentName::new(r.get::<_, String>(0).map_err(store_err)?),
        traj: TrajId::new(r.get::<_, String>(1).map_err(store_err)?),
        routing_refs: routing
            .into_iter()
            .map(bough_plugin_ledger::Ref::new)
            .collect(),
        wake_classes: classes.into_iter().collect(),
        model_override: r.get::<_, Option<String>>(4).map_err(store_err)?,
        tick_floor: r
            .get::<_, Option<i64>>(5)
            .map_err(store_err)?
            .map(|ms| std::time::Duration::from_millis(ms as u64)),
        digest_rollup: r
            .get::<_, Option<String>>(6)
            .map_err(store_err)?
            .map(RollupId::new),
    })
}

const AGENT_COLS: &str =
    "name, traj_id, routing_refs, wake_classes, model_override, tick_floor, digest_rollup_id";

/// Upsert one agent row. `agents` is mutable config, so this is a plain replace (§3).
pub async fn put_agent(store: &SqliteStore, a: AgentRow) -> Result<(), LedgerError> {
    store
        .with_conn(move |conn| {
            conn.execute(
                &format!(
                    "INSERT OR REPLACE INTO agents ({AGENT_COLS}) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)"
                ),
                rusqlite::params![
                    a.name.as_str(),
                    a.traj.as_str(),
                    serde_json::to_string(
                        &a.routing_refs
                            .iter()
                            .map(|r| r.as_str())
                            .collect::<Vec<_>>()
                    )
                    .map_err(json_err)?,
                    serde_json::to_string(&a.wake_classes).map_err(json_err)?,
                    a.model_override,
                    a.tick_floor.map(|d| d.as_millis() as i64),
                    a.digest_rollup.as_ref().map(|r| r.as_str()),
                ],
            )
            .map_err(store_err)?;
            Ok(())
        })
        .await
}

pub async fn agent(store: &SqliteStore, name: &AgentName) -> Result<Option<AgentRow>, LedgerError> {
    let name = name.clone();
    store.with_conn(move |conn| read_agent(conn, &name)).await
}

pub(crate) fn read_agent(
    conn: &Connection,
    name: &AgentName,
) -> Result<Option<AgentRow>, LedgerError> {
    let mut stmt = conn
        .prepare(&format!("SELECT {AGENT_COLS} FROM agents WHERE name = ?1"))
        .map_err(store_err)?;
    let mut rows = stmt.query([name.as_str()]).map_err(store_err)?;
    match rows.next().map_err(store_err)? {
        Some(r) => Ok(Some(row_to_agent(r)?)),
        None => Ok(None),
    }
}

pub async fn agents(store: &SqliteStore) -> Result<Vec<AgentRow>, LedgerError> {
    store
        .with_conn(move |conn| {
            let mut stmt = conn
                .prepare(&format!("SELECT {AGENT_COLS} FROM agents ORDER BY name"))
                .map_err(store_err)?;
            let mut rows = stmt.query([]).map_err(store_err)?;
            let mut out = Vec::new();
            while let Some(r) = rows.next().map_err(store_err)? {
                out.push(row_to_agent(r)?);
            }
            Ok(out)
        })
        .await
}

pub async fn delete_agent(store: &SqliteStore, name: &AgentName) -> Result<(), LedgerError> {
    let name = name.clone();
    store
        .with_conn(move |conn| {
            conn.execute("DELETE FROM agents WHERE name = ?1", [name.as_str()])
                .map_err(store_err)?;
            Ok(())
        })
        .await
}

// ---- actions journal --------------------------------------------------------

const ACTION_COLS: &str = "id, wake_id, idem_key, kind, payload, status, result, at, done_at";

fn action_status(s: &str) -> Result<ActionStatus, LedgerError> {
    match s {
        "intent" => Ok(ActionStatus::Intent),
        "done" => Ok(ActionStatus::Done),
        "failed" => Ok(ActionStatus::Failed),
        other => Err(LedgerError::Store(anyhow::anyhow!(
            "`{other}` is not an action status"
        ))),
    }
}

pub(crate) fn status_str(s: ActionStatus) -> &'static str {
    match s {
        ActionStatus::Intent => "intent",
        ActionStatus::Done => "done",
        ActionStatus::Failed => "failed",
    }
}

fn row_to_action(r: &rusqlite::Row<'_>) -> Result<ActionRow, LedgerError> {
    Ok(ActionRow {
        id: ActionId::new(r.get::<_, String>(0).map_err(store_err)?),
        wake: WakeId::new(r.get::<_, String>(1).map_err(store_err)?),
        idem_key: bough_plugin_ledger::IdemKey::new(r.get::<_, String>(2).map_err(store_err)?),
        kind: r.get::<_, String>(3).map_err(store_err)?,
        payload: serde_json::from_str(&r.get::<_, String>(4).map_err(store_err)?)
            .map_err(json_err)?,
        status: action_status(&r.get::<_, String>(5).map_err(store_err)?)?,
        result: match r.get::<_, Option<String>>(6).map_err(store_err)? {
            Some(s) => Some(serde_json::from_str(&s).map_err(json_err)?),
            None => None,
        },
        at: parse_time(&r.get::<_, String>(7).map_err(store_err)?)?,
        done_at: match r.get::<_, Option<String>>(8).map_err(store_err)? {
            Some(s) => Some(parse_time(&s)?),
            None => None,
        },
    })
}

pub async fn action_intent(store: &SqliteStore, a: NewAction) -> Result<ActionRow, LedgerError> {
    let row = ActionRow {
        id: a
            .id
            .clone()
            .unwrap_or_else(|| ActionId::new(uuid::Uuid::now_v7().to_string())),
        wake: a.wake,
        idem_key: a.idem_key,
        kind: a.kind,
        payload: a.payload,
        status: ActionStatus::Intent,
        result: None,
        at: a.at,
        done_at: None,
    };
    let written = row.clone();
    store
        .with_conn(move |conn| {
            conn.execute(
                &format!(
                    "INSERT INTO actions ({ACTION_COLS}) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, ?7, NULL)"
                ),
                rusqlite::params![
                    written.id.as_str(),
                    written.wake.as_str(),
                    written.idem_key.as_str(),
                    written.kind,
                    serde_json::to_string(&written.payload).map_err(json_err)?,
                    status_str(written.status),
                    written.at.to_rfc3339(),
                ],
            )
            .map_err(store_err)?;
            Ok(())
        })
        .await?;
    Ok(row)
}

pub async fn action_done(
    store: &SqliteStore,
    id: &ActionId,
    status: ActionStatus,
    result: serde_json::Value,
    now: DateTime<Utc>,
) -> Result<(), LedgerError> {
    let id = id.clone();
    store
        .with_conn(move |conn| {
            let n = conn
                .execute(
                    "UPDATE actions SET status = ?2, result = ?3, done_at = ?4 WHERE id = ?1",
                    rusqlite::params![
                        id.as_str(),
                        status_str(status),
                        serde_json::to_string(&result).map_err(json_err)?,
                        now.to_rfc3339(),
                    ],
                )
                .map_err(store_err)?;
            if n == 0 {
                return Err(LedgerError::Store(anyhow::anyhow!("no such action `{id}`")));
            }
            Ok(())
        })
        .await
}

pub async fn actions(store: &SqliteStore, q: &ActionQuery) -> Result<Vec<ActionRow>, LedgerError> {
    let q = q.clone();
    store
        .with_conn(move |conn| {
            let mut sql = format!("SELECT {ACTION_COLS} FROM actions");
            let mut args: Vec<String> = Vec::new();
            let mut clauses: Vec<String> = Vec::new();
            if !q.ids.is_empty() {
                clauses.push(format!("id IN ({})", placeholders(1, q.ids.len())));
                args.extend(q.ids.iter().map(|i| i.as_str().to_string()));
            }
            if let Some(wake) = &q.wake {
                args.push(wake.as_str().to_string());
                clauses.push(format!("wake_id = ?{}", args.len()));
            }
            if let Some(status) = q.status {
                args.push(status_str(status).to_string());
                clauses.push(format!("status = ?{}", args.len()));
            }
            if !clauses.is_empty() {
                sql.push_str(" WHERE ");
                sql.push_str(&clauses.join(" AND "));
            }
            sql.push_str(" ORDER BY at ASC, id ASC");
            if let Some(limit) = q.limit {
                sql.push_str(&format!(" LIMIT {limit}"));
            }
            let bound: Vec<&dyn rusqlite::ToSql> =
                args.iter().map(|a| a as &dyn rusqlite::ToSql).collect();
            let mut stmt = conn.prepare(&sql).map_err(store_err)?;
            let mut rows = stmt.query(bound.as_slice()).map_err(store_err)?;
            let mut out = Vec::new();
            while let Some(r) = rows.next().map_err(store_err)? {
                out.push(row_to_action(r)?);
            }
            Ok(out)
        })
        .await
}

// ---- integrity --------------------------------------------------------------

fn hash(parts: &[&str]) -> String {
    let mut h = Sha256::new();
    for p in parts {
        h.update(p.as_bytes());
        h.update([0u8]);
    }
    format!("{:x}", h.finalize())
}

/// Stable per-row content hashes; for rollups the hash EXCLUDES `superseded_by`.
pub async fn row_hashes(
    store: &SqliteStore,
    scope: HashScope,
) -> Result<Vec<RowHash>, LedgerError> {
    store
        .with_conn(move |conn| {
            let mut out = Vec::new();
            let want = |t| matches!(scope, HashScope::All) || scope == t;

            if want(HashScope::Steps) {
                let mut stmt = conn
                    .prepare(&format!("SELECT {STEP_COLS} FROM steps ORDER BY id"))
                    .map_err(store_err)?;
                let mut rows = stmt.query([]).map_err(store_err)?;
                while let Some(r) = rows.next().map_err(store_err)? {
                    let cols: Vec<String> = (0..10)
                        .map(|i| {
                            r.get::<_, rusqlite::types::Value>(i)
                                .map(|v| format!("{v:?}"))
                                .map_err(store_err)
                        })
                        .collect::<Result<_, _>>()?;
                    let parts: Vec<&str> = cols.iter().map(|s| s.as_str()).collect();
                    out.push(RowHash {
                        table: "steps",
                        id: r.get::<_, String>(0).map_err(store_err)?,
                        hash: hash(&parts),
                        superseded_by: None,
                    });
                }
            }
            if want(HashScope::Edges) {
                let mut stmt = conn
                    .prepare(
                        "SELECT child_traj, parent_traj, at_seq, kind, at FROM edges \
                         ORDER BY child_traj, parent_traj, kind",
                    )
                    .map_err(store_err)?;
                let mut rows = stmt.query([]).map_err(store_err)?;
                while let Some(r) = rows.next().map_err(store_err)? {
                    let cols: Vec<String> = (0..5)
                        .map(|i| {
                            r.get::<_, rusqlite::types::Value>(i)
                                .map(|v| format!("{v:?}"))
                                .map_err(store_err)
                        })
                        .collect::<Result<_, _>>()?;
                    let parts: Vec<&str> = cols.iter().map(|s| s.as_str()).collect();
                    out.push(RowHash {
                        table: "edges",
                        id: format!(
                            "{}|{}|{}",
                            r.get::<_, String>(0).map_err(store_err)?,
                            r.get::<_, String>(1).map_err(store_err)?,
                            r.get::<_, String>(3).map_err(store_err)?
                        ),
                        hash: hash(&parts),
                        superseded_by: None,
                    });
                }
            }
            if want(HashScope::Rollups) {
                let mut stmt = conn
                    .prepare(&format!("SELECT {ROLLUP_COLS} FROM rollups ORDER BY id"))
                    .map_err(store_err)?;
                let mut rows = stmt.query([]).map_err(store_err)?;
                while let Some(r) = rows.next().map_err(store_err)? {
                    // 0..11 — column 11 is `superseded_by`, deliberately EXCLUDED: a legal
                    // set-once write must not read as a row change (§3).
                    let cols: Vec<String> = (0..11)
                        .map(|i| {
                            r.get::<_, rusqlite::types::Value>(i)
                                .map(|v| format!("{v:?}"))
                                .map_err(store_err)
                        })
                        .collect::<Result<_, _>>()?;
                    let parts: Vec<&str> = cols.iter().map(|s| s.as_str()).collect();
                    out.push(RowHash {
                        table: "rollups",
                        id: r.get::<_, String>(0).map_err(store_err)?,
                        hash: hash(&parts),
                        superseded_by: r.get::<_, Option<String>>(11).map_err(store_err)?,
                    });
                }
            }
            Ok(out)
        })
        .await
}

/// A whole trajectory as plain data, for the file view.
pub async fn trajectory_view(
    store: &SqliteStore,
    traj: &TrajId,
) -> Result<TrajectoryView, LedgerError> {
    let steps = steps(
        store,
        &StepQuery {
            trajs: vec![traj.clone()],
            ..Default::default()
        },
    )
    .await?;
    let edges = edges(store, traj).await?;
    let rollups = rollups(
        store,
        &RollupQuery {
            trajs: vec![traj.clone()],
            include_superseded: true,
            ..Default::default()
        },
    )
    .await?;
    let traj_owned = traj.clone();
    let agent = store
        .with_conn(move |conn| {
            let mut stmt = conn
                .prepare(&format!(
                    "SELECT {AGENT_COLS} FROM agents WHERE traj_id = ?1 ORDER BY name LIMIT 1"
                ))
                .map_err(store_err)?;
            let mut rows = stmt.query([traj_owned.as_str()]).map_err(store_err)?;
            match rows.next().map_err(store_err)? {
                Some(r) => Ok(Some(row_to_agent(r)?)),
                None => Ok(None),
            }
        })
        .await?;
    Ok(TrajectoryView {
        traj: traj.clone(),
        steps,
        edges,
        rollups,
        agent,
    })
}

/// Every trajectory carrying a step that matches one of `refs`. The `ref_matches` half of
/// `connected` (§3), and a pure index read.
pub(crate) fn trajs_matching_refs(
    conn: &Connection,
    refs: &BTreeSet<bough_plugin_ledger::Ref>,
) -> Result<Vec<TrajId>, LedgerError> {
    if refs.is_empty() {
        return Ok(Vec::new());
    }
    let sql = format!(
        "SELECT DISTINCT s.traj_id FROM step_refs r JOIN steps s ON s.id = r.step_id \
         WHERE r.ref IN ({}) ORDER BY s.traj_id",
        placeholders(1, refs.len())
    );
    let args: Vec<String> = refs.iter().map(|r| r.as_str().to_string()).collect();
    let bound: Vec<&dyn rusqlite::ToSql> = args.iter().map(|a| a as &dyn rusqlite::ToSql).collect();
    let mut stmt = conn.prepare(&sql).map_err(store_err)?;
    let found: Vec<String> = stmt
        .query_map(bound.as_slice(), |r| r.get::<_, String>(0))
        .map_err(store_err)?
        .collect::<Result<_, _>>()
        .map_err(store_err)?;
    Ok(found.into_iter().map(TrajId::new).collect())
}
