//! The tag-memory's vector layer, and the pump that fills it (port of
//! `src/history/embed.ts` plus the drain ticker at the bottom of
//! `src/server/main.ts`).
//!
//! The layer itself lives in [`crate::db::embed`] — it is the only extension
//! consumer in the tree and half its contract is db-layer design (`specs/db.md`
//! §8), so the code sits with the other SQLite code and the history module
//! re-exports it under the name its callers know. This file adds the one thing
//! the history side owns: **the drain ticker**, which is what makes the layer
//! visible to anything at all. A layer nobody drains embeds nothing, and
//! `bough tags similar` then searches an empty index — the failure mode is
//! silent, so the pump is wired at boot rather than left to a caller.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

pub use crate::db::embed::{
    create_embed_layer, EmbedLayer, EmbedLayerOptions, SimilarRow, EMBED_MODEL_ENV,
};

/// One tick a minute: the first tick after a busy session catches up gradually
/// (64 commands at a time) instead of embedding the whole backlog at once.
pub const DRAIN_INTERVAL: Duration = Duration::from_secs(60);

/// What the server prints when the layer exists, so a machine without it is
/// obviously — not silently — without it.
pub const DRAIN_READY_LINE: &str =
    "history embeddings: drain ticker running, `bough tags similar` enabled";

/// Start the drain pump over the live databases. `None` when there is no layer
/// to pump (no extension capability, `BOUGH_NO_EMBED`, or no sqlite-lembed
/// installed) — the everyday answer on a machine that never set embeddings up,
/// and nothing else in the system changes.
///
/// Ticks immediately, then every [`DRAIN_INTERVAL`]. **Drop-if-busy**, like
/// every cheap-tier consumer: a drain that outruns its interval must not stack
/// ticks up behind it. Each drain runs on `spawn_blocking` because embedding is
/// CPU-bound and synchronous inside SQLite.
///
/// Requires a tokio runtime. The returned handle is not needed — the pump lives
/// for the life of the process — but is returned so a test can drive one.
pub fn start_drain_ticker() -> Option<tokio::task::JoinHandle<()>> {
    let layer = Arc::new(create_embed_layer(None)?);
    Some(spawn_drain_ticker(layer, DRAIN_INTERVAL))
}

/// The ticker body, over an injected layer and interval so a test can drive it
/// without the live `~/.bough`.
pub fn spawn_drain_ticker(
    layer: Arc<EmbedLayer>,
    interval: Duration,
) -> tokio::task::JoinHandle<()> {
    let draining = Arc::new(AtomicBool::new(false));
    tokio::spawn(async move {
        // `tokio::time::interval` fires its first tick immediately — which is
        // the TS `tick(); setInterval(tick, 60_000)` shape exactly.
        let mut ticker = tokio::time::interval(interval);
        // Missed ticks are DELAYED, not replayed: after a long drain, catching
        // up on skipped ticks would run the batches back-to-back, which is the
        // exact burst the small batch exists to avoid.
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            if draining.swap(true, Ordering::SeqCst) {
                continue;
            }
            let layer = layer.clone();
            let flag = draining.clone();
            let done = tokio::task::spawn_blocking(move || layer.drain());
            let result = done.await;
            // Cleared before the result is inspected, so a panic inside the
            // extension costs one tick rather than wedging the pump forever.
            flag.store(false, Ordering::SeqCst);
            if let Ok(Ok(n)) = result {
                if n > 0 {
                    tracing::debug!("history embeddings: drained {n} command(s)");
                }
            }
        }
    })
}

/// The layer as the PUSHED recall sees it: the meaning half of
/// `history/tags/stats.rs`'s query hints, behind that module's own trait so it
/// keeps knowing nothing about extensions or models.
pub struct EmbedRecall(Arc<EmbedLayer>);

impl super::stats::SemanticRecall for EmbedRecall {
    fn related(&self, text: &str) -> Vec<super::stats::SemanticHit> {
        // Silence on failure, by contract: this runs on the turn path, where
        // a missing model, a locked store or a half-downloaded GGUF must cost
        // a hint and nothing else.
        self.0
            .related(text)
            .unwrap_or_default()
            .into_iter()
            .map(|r| super::stats::SemanticHit {
                cmd: r.cmd,
                tags: r.tags,
                repo: r.repo,
                exit_code: r.exit_code,
                ts: r.ts,
            })
            .collect()
    }
}

/// The process-wide recall handle, or `None` where the layer does not exist —
/// the everyday answer on a machine without sqlite-lembed.
///
/// Built ONCE and memoized including the `None`: opening the layer reads a
/// 25MB model, so a per-turn open would put that on the turn path, and a
/// machine without the dylib would retry the same failed lookup every turn
/// forever. The drain ticker keeps the index behind it fresh.
pub fn recall_layer() -> Option<&'static EmbedRecall> {
    static LAYER: OnceLock<Option<EmbedRecall>> = OnceLock::new();
    LAYER
        .get_or_init(|| create_embed_layer(None).map(|l| EmbedRecall(Arc::new(l))))
        .as_ref()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pump must answer `None` — not panic, not block — on a machine with
    /// no layer. This is the everyday path, and the one boot depends on.
    #[test]
    fn no_ticker_without_a_layer() {
        // Decide the once-per-process capability FIRST, so this check cannot
        // race a sibling test into a different answer between the two calls.
        crate::db::extensions::enable_sqlite_extensions();
        if create_embed_layer(None).is_some() {
            // A layer exists on this machine; starting it here would pump the
            // LIVE `~/.bough` from a unit test. The absent path is what this
            // test is about, and it is unobservable here.
            return;
        }
        assert!(
            start_drain_ticker().is_none(),
            "no layer → no pump, and no runtime needed"
        );
    }

    /// The pump ticks immediately (not one interval later) and keeps ticking.
    /// Driven over TEMP databases — never the live `~/.bough` — so the proof is
    /// that a drain really ran: embeddings.db does not exist until one does.
    /// Skipped on a machine with no layer, like the fixture test in `db::embed`.
    #[tokio::test]
    async fn ticker_drains_on_the_first_tick_and_keeps_running() {
        use crate::db::open_db;
        use crate::schema::parts::{Session, SessionKind};
        use crate::types::{CommandRecord, Db};

        crate::db::extensions::enable_sqlite_extensions();
        let dir = std::env::temp_dir().join(format!("bough-embed-pump-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let bough_db = dir.join("bough.db");
        let embed_db = dir.join("embeddings.db");
        {
            let db = open_db(Some(bough_db.to_str().unwrap()), Default::default()).unwrap();
            db.create_session(Session {
                id: "s1".into(),
                title: "s1".into(),
                kind: SessionKind::Root,
                created_at: 1,
                parent_id: None,
                origin_id: None,
                origin_message_id: None,
                workspace: None,
                origin_dir: None,
                base: None,
                model: None,
                effort: None,
                draft: None,
                context_tokens: None,
                cached_tokens: None,
                last_llm_at: None,
                outcome_ok: None,
            })
            .unwrap();
            db.record_command(&CommandRecord {
                session_id: "s1".into(),
                ts: 1_000,
                repo: "repo".into(),
                cmd: "docker exec -it myapp-dev-1 bash".into(),
                tags: "docker:exec".into(),
                tag_list: vec![],
                dirs: vec![],
                exit_code: Some(0),
                duration_ms: Some(1),
                output_head: String::new(),
                spill_path: None,
                source: "live".into(),
                message_id: None,
            })
            .unwrap();
        }
        let Some(layer) = create_embed_layer(Some(EmbedLayerOptions {
            bough_db: Some(bough_db.to_string_lossy().into_owned()),
            embed_db: Some(embed_db.to_string_lossy().into_owned()),
            model_path: None,
        })) else {
            let _ = std::fs::remove_dir_all(&dir);
            return; // no extension support on this machine — nothing to pump
        };

        let handle = spawn_drain_ticker(Arc::new(layer), Duration::from_millis(50));
        // Poll rather than sleep-a-fixed-amount: the first tick is immediate, so
        // this settles fast, and a slow machine does not turn into a flake. The
        // first tick may also be fetching the model, so the window is generous.
        let mut drained = 0i64;
        for _ in 0..200 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            if let Ok(conn) = rusqlite::Connection::open(&embed_db) {
                if let Ok(n) =
                    conn.query_row("SELECT count(*) FROM vec_index", [], |r| r.get::<_, i64>(0))
                {
                    drained = n;
                    if n > 0 {
                        break;
                    }
                }
            }
        }
        assert_eq!(
            drained, 1,
            "the pump embedded the pending command without being asked"
        );
        assert!(
            !handle.is_finished(),
            "the pump runs for the life of the process"
        );
        handle.abort();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
