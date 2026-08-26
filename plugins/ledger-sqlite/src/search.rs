//! Invariant: FTS5 is external-content over `steps` with an insert-only trigger, and hits are
//! ordered `seq DESC, traj ASC` — NOT bm25 rank, which the memory provider cannot reproduce
//! (P1-D19). The conformance suite's whole value is that the two providers answer identically.

use bough_plugin_ledger::{LedgerError, SearchHit, SearchQuery};

use crate::read::{query_steps, STEP_COLS};
use crate::store::SqliteStore;

/// Turn free text into an FTS5 MATCH expression: alphanumeric tokens, each quoted, ANDed.
///
/// Quoting is not decoration — an unescaped `:` or `#` from a ref is FTS5 syntax and would make
/// the query an error rather than a miss. The token split is the same one the memory provider
/// does by hand, which is why the two agree.
pub(crate) fn match_expr(text: &str) -> Option<String> {
    let tokens: Vec<String> = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| format!("\"{}\"", t.to_lowercase()))
        .collect();
    if tokens.is_empty() {
        None
    } else {
        Some(tokens.join(" "))
    }
}

/// The deterministic snippet BOTH providers produce: the head of the body as stored.
pub(crate) fn snippet_of(body: &serde_json::Value) -> String {
    let text = body.to_string();
    match text.char_indices().nth(160) {
        Some((cut, _)) => text[..cut].to_string(),
        None => text,
    }
}

/// [`bough_plugin_ledger::LedgerStore::search`].
pub async fn search(store: &SqliteStore, q: &SearchQuery) -> Result<Vec<SearchHit>, LedgerError> {
    let Some(expr) = match_expr(&q.text) else {
        return Ok(Vec::new());
    };
    let (types, skipped) = (store.types.clone(), store.skipped.clone());
    let q = q.clone();
    store
        .with_conn(move |conn| {
            let mut sql = format!(
                "SELECT {} FROM steps_fts JOIN steps s ON s.rowid = steps_fts.rowid \
                 WHERE steps_fts MATCH ?1",
                STEP_COLS
                    .split(", ")
                    .map(|c| format!("s.{c}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            let mut args: Vec<String> = vec![expr];
            if !q.trajs.is_empty() {
                let list = (2..2 + q.trajs.len())
                    .map(|i| format!("?{i}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                sql.push_str(&format!(" AND s.traj_id IN ({list})"));
                args.extend(q.trajs.iter().map(|t| t.as_str().to_string()));
            }
            sql.push_str(&format!(
                " ORDER BY s.seq DESC, s.traj_id ASC LIMIT {}",
                q.limit
            ));
            let bound: Vec<&dyn rusqlite::ToSql> =
                args.iter().map(|a| a as &dyn rusqlite::ToSql).collect();
            let steps = query_steps(conn, &types, &skipped, &sql, &bound)?;
            Ok(steps
                .into_iter()
                .map(|step| SearchHit {
                    snippet: snippet_of(&step.body),
                    step,
                })
                .collect())
        })
        .await
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use bough_kernel::{Context, KernelCore};
    use bough_plugin_ledger::{
        Append, Cite, Class, LedgerStore, Ref, SearchQuery, StepType, TrajId, WakeId,
    };
    use chrono::Utc;

    use super::*;
    use crate::SqliteConfig;

    fn store() -> Arc<SqliteStore> {
        SqliteStore::open(
            &SqliteConfig {
                path: ":memory:".into(),
                busy_timeout_ms: 5000,
            },
            Context::root(KernelCore::new()),
        )
        .expect("in-memory ledger opens")
    }

    fn pin(traj: &str, title: &str, cites: Vec<Cite>) -> Append {
        Append {
            traj: TrajId::new(traj),
            wake: WakeId::new("w1"),
            kind: StepType::new("pin/set"),
            class: if cites.is_empty() {
                Class::Thought
            } else {
                Class::Evidence
            },
            body: serde_json::json!({ "title": title, "text": "body text", "supersedes": [] }),
            cites,
            at: Utc::now(),
            id: None,
        }
    }

    #[tokio::test]
    async fn fts_matches_body_text() {
        let s = store();
        s.append(pin("t", "kingfisher", vec![])).await.unwrap();
        s.append(pin("t", "heron", vec![])).await.unwrap();
        let hits = s
            .search(&SearchQuery {
                text: "kingfisher".into(),
                trajs: vec![],
                limit: 10,
            })
            .await
            .expect("search runs");
        assert_eq!(hits.len(), 1);
        assert!(hits[0].snippet.contains("kingfisher"));
    }

    /// The `cites` column is indexed too, so a ref is findable even when the body never spells it.
    #[tokio::test]
    async fn fts_matches_cite_text() {
        let s = store();
        s.append(pin(
            "t",
            "heron",
            vec![Cite {
                r#ref: Ref::new("gh:owner/repo#4242"),
                url: None,
            }],
        ))
        .await
        .unwrap();
        let hits = s
            .search(&SearchQuery {
                text: "4242".into(),
                trajs: vec![],
                limit: 10,
            })
            .await
            .expect("search runs");
        assert_eq!(hits.len(), 1, "the cite text is not indexed");
        assert_eq!(hits[0].step.cites.len(), 1);
    }

    #[tokio::test]
    async fn hits_are_ordered_deterministically() {
        let s = store();
        // Two trajectories, interleaved seqs, all matching the same token.
        for traj in ["b", "a"] {
            for _ in 0..3 {
                s.append(pin(traj, "heron", vec![])).await.unwrap();
            }
        }
        let hits = s
            .search(&SearchQuery {
                text: "heron".into(),
                trajs: vec![],
                limit: 10,
            })
            .await
            .expect("search runs");
        let order: Vec<(u64, String)> = hits
            .iter()
            .map(|h| (h.step.seq.0, h.step.traj.as_str().to_string()))
            .collect();
        assert_eq!(
            order,
            vec![
                (3, "a".to_string()),
                (3, "b".to_string()),
                (2, "a".to_string()),
                (2, "b".to_string()),
                (1, "a".to_string()),
                (1, "b".to_string()),
            ],
            "hits must be seq DESC, traj ASC"
        );
    }
}
