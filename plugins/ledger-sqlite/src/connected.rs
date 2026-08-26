//! Invariant: `connected(agent)` is `own_chain ∪ ancestry ∪ ref_matches`, computed AT NEED and
//! WRITING NOTHING (§3). A ref linked late therefore includes its history retroactively, with
//! nothing written onto the entries themselves (V6).

use bough_plugin_ledger::{AgentName, Connected, LedgerError};

use crate::store::SqliteStore;

/// Three indexed queries and no writes.
pub async fn connected(store: &SqliteStore, agent: &AgentName) -> Result<Connected, LedgerError> {
    let name = agent.clone();
    let row = store
        .with_conn({
            let name = name.clone();
            move |conn| crate::read::read_agent(conn, &name)
        })
        .await?
        .ok_or_else(|| LedgerError::Store(anyhow::anyhow!("no such agent `{name}`")))?;

    let ancestry = crate::read::ancestry(store, &row.traj).await?;

    let own = row.traj.clone();
    let refs = row.routing_refs.clone();
    let matching = store
        .with_conn({
            let refs = refs.clone();
            move |conn| crate::read::trajs_matching_refs(conn, &refs)
        })
        .await?;
    // The agent's own trajectory is `own`, not a ref match; ancestry is reported on its own axis.
    let ref_matches = matching.into_iter().filter(|t| *t != own).collect();

    Ok(Connected {
        own,
        ancestry,
        ref_matches,
        refs,
    })
}
