//! Invariant: validate BEFORE the transaction, write `steps` + `step_refs` (derived, never
//! caller-supplied) in ONE commit, and emit `ledger/step` only AFTER the commit returns — the
//! event is durable, so the row is readable when a listener sees it (§0.2, V7).

use std::sync::Arc;

use bough_plugin_ledger::{refs, Append, LedgerError, LedgerStep, Seq, Step, StepId, StepTypeDef};
use rusqlite::Transaction;

use crate::schema::store_err;
use crate::store::SqliteStore;

/// A validated append, ready for the transaction: nothing here can be refused any more.
pub(crate) struct Ready {
    pub(crate) req: Append,
    pub(crate) def: StepTypeDef,
    pub(crate) id: StepId,
}

/// Everything that can refuse an append happens here, OUTSIDE the transaction: the class rule,
/// the cite requirement of evidence, and the body schema (§3).
pub(crate) fn prepare(store: &SqliteStore, req: Append) -> Result<Ready, LedgerError> {
    let def = store.types.validate_append(&req)?;
    let id = req
        .id
        .clone()
        .unwrap_or_else(|| StepId::new(uuid::Uuid::now_v7().to_string()));
    Ok(Ready { req, def, id })
}

/// Insert one step and its DERIVED refs inside `tx`, allocating `seq` as `MAX(seq)+1` for the
/// trajectory in this same transaction.
pub(crate) fn insert_step(tx: &Transaction<'_>, ready: &Ready) -> Result<Step, LedgerError> {
    let Ready { req, def, id } = ready;
    let seq: i64 = tx
        .query_row(
            "SELECT COALESCE(MAX(seq), 0) + 1 FROM steps WHERE traj_id = ?1",
            rusqlite::params![req.traj.as_str()],
            |r| r.get(0),
        )
        .map_err(store_err)?;

    let body = serde_json::to_string(&req.body).map_err(json_err)?;
    let cites = serde_json::to_string(&req.cites).map_err(json_err)?;
    let step_refs = refs::derive_step_refs(&req.cites, &req.body);

    tx.execute(
        "INSERT INTO steps (id, traj_id, seq, at, wake_id, type, class, body, cites, ignorable) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        rusqlite::params![
            id.as_str(),
            req.traj.as_str(),
            seq,
            req.at.to_rfc3339(),
            req.wake.as_str(),
            req.kind.as_str(),
            req.class.as_str(),
            body,
            cites,
            def.ignorable as i64,
        ],
    )
    .map_err(store_err)?;

    {
        let mut stmt = tx
            .prepare("INSERT OR IGNORE INTO step_refs (step_id, ref) VALUES (?1, ?2)")
            .map_err(store_err)?;
        for r in &step_refs {
            stmt.execute(rusqlite::params![id.as_str(), r.as_str()])
                .map_err(store_err)?;
        }
    }

    Ok(Step {
        id: id.clone(),
        traj: req.traj.clone(),
        seq: Seq(seq as u64),
        at: req.at,
        wake: req.wake.clone(),
        kind: req.kind.clone(),
        class: req.class,
        body: Arc::new(req.body.clone()),
        cites: Arc::new(req.cites.clone()),
        refs: Arc::new(step_refs),
        ignorable: def.ignorable,
    })
}

/// The whole append path for one step.
pub async fn append(store: &SqliteStore, req: Append) -> Result<Step, LedgerError> {
    let ready = prepare(store, req)?;
    let step = store
        .with_conn(move |conn| {
            let tx = conn.transaction().map_err(store_err)?;
            let step = insert_step(&tx, &ready)?;
            tx.commit().map_err(store_err)?;
            Ok(step)
        })
        .await?;
    // POST-COMMIT: the row is readable before any listener can see the event (V7).
    store.ctx.emit::<LedgerStep>(Arc::new(step.clone()));
    Ok(step)
}

/// One transaction, one contiguous seq run, one `ledger/step` per step, in seq order.
pub async fn append_batch(
    store: &SqliteStore,
    reqs: Vec<Append>,
) -> Result<Vec<Step>, LedgerError> {
    // Every refusal lands before anything is written, so a bad member of the batch leaves no
    // partial run behind.
    let ready: Vec<Ready> = reqs
        .into_iter()
        .map(|r| prepare(store, r))
        .collect::<Result<_, _>>()?;

    let steps = store
        .with_conn(move |conn| {
            let tx = conn.transaction().map_err(store_err)?;
            let mut out = Vec::with_capacity(ready.len());
            for r in &ready {
                out.push(insert_step(&tx, r)?);
            }
            tx.commit().map_err(store_err)?;
            Ok(out)
        })
        .await?;

    for step in &steps {
        store.ctx.emit::<LedgerStep>(Arc::new(step.clone()));
    }
    Ok(steps)
}

pub(crate) fn json_err(e: serde_json::Error) -> LedgerError {
    LedgerError::Store(anyhow::Error::new(e))
}
