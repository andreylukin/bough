//! The cheap tier (port of `src/worker/`): auto titles, composer ghost text,
//! live activity blurbs over `complete_text`. Not an agent. Each method
//! resolves `None` and never errors; one in-flight blurb per session, drop
//! don't queue (the ledgers live in the watchers). Every reader degrades on
//! absence by contract (ARCHITECTURE.md §4.3) — a ctx built without the tier
//! is a working server, which is what keeps every turn test hermetic with no
//! stub to remember.
//!
//! Module layering, same as TS: `titles` is the BASE (it owns the shared
//! [`titles::cheap_text`] primitive and the model resolution); `ghost` and
//! `activity` reach down to it and never across to each other.

pub mod activity;
pub mod ghost;
pub mod titles;

use std::sync::Arc;

use crate::types::CheapTier;

pub use titles::{cheap_model, cheap_model_with, CHEAP_MODEL_ENV, DEFAULT_CHEAP_MODEL};

/// The production tier: three thin adapters over the module fns, each of
/// which resolves `None` for every failure there is (no key, provider error,
/// refusal, empty answer, deadline). The model is read from
/// `BOUGH_CHEAP_MODEL` per call — never `ctx.model`: a user pinned to Opus
/// for the coding work must not pay Opus rates to put five words in a
/// sidebar.
pub struct CheapTierImpl;

#[async_trait::async_trait]
impl CheapTier for CheapTierImpl {
    async fn title(&self, first_message: &str) -> Option<String> {
        titles::cheap_title(first_message, &Default::default()).await
    }
    async fn ghost_text(&self, prompt: &str) -> Option<String> {
        ghost::cheap_ghost(prompt, &Default::default()).await
    }
    async fn activity(&self, recent: &str) -> Option<String> {
        activity::cheap_activity(recent, &Default::default()).await
    }
}

/// The tier boot installs (wave 2 flips this from the wave-1 `None`). Always
/// present: the gate is per call — a missing key or an unreachable provider
/// is a silent `None`, and `BOUGH_CHEAP_MODEL` is re-read on every call so a
/// picker change needs no restart.
pub fn create_cheap_tier() -> Option<Arc<dyn CheapTier>> {
    Some(Arc::new(CheapTierImpl))
}

// ---------------------------------------------------------------------------
// Shared test plumbing for the three feature modules' suites
// ---------------------------------------------------------------------------

#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use tokio_util::sync::CancellationToken;

    use crate::bus::Bus;
    use crate::db::sqlite_db::{DbOptions, SqliteDb};
    use crate::errors::BoughError;
    use crate::schema::events::BoughEvent;
    use crate::schema::parts::{Session, SessionKind};
    use crate::types::{
        system_clock, CheapTier, LlmBlock, LlmClient, LlmParams, LlmResult, OnText, SharedDb,
    };
    use crate::worker::titles::TitleCtx;

    /// An `LlmClient` that answers with one text block.
    pub fn saying_client(text: &str) -> Arc<dyn LlmClient> {
        struct Saying(String);
        #[async_trait::async_trait]
        impl LlmClient for Saying {
            async fn run(
                &self,
                _p: LlmParams,
                _t: OnText,
                _c: CancellationToken,
            ) -> Result<LlmResult, BoughError> {
                Ok(LlmResult {
                    content: vec![LlmBlock::Text { text: self.0.clone() }],
                    stop_reason: "end_turn".into(),
                    usage: None,
                })
            }
        }
        Arc::new(Saying(text.to_string()))
    }

    /// An `LlmClient` that never settles — the failure a try/catch alone
    /// does not cover.
    pub fn hanging_client() -> Arc<dyn LlmClient> {
        struct Hanging;
        #[async_trait::async_trait]
        impl LlmClient for Hanging {
            async fn run(
                &self,
                _p: LlmParams,
                _t: OnText,
                _c: CancellationToken,
            ) -> Result<LlmResult, BoughError> {
                futures::future::pending::<()>().await;
                unreachable!()
            }
        }
        Arc::new(Hanging)
    }

    /// A tier whose three methods answer canned values and count their calls.
    pub struct StubTier {
        title: Option<String>,
        ghost: Option<String>,
        activity: Option<String>,
        /// When set, the FIRST activity call answers `None` (the failure
        /// shape a Rust tier can express) and later calls answer the value.
        activity_fails_first: bool,
        pub title_calls: AtomicUsize,
        pub ghost_calls: AtomicUsize,
        pub activity_calls: AtomicUsize,
    }

    impl StubTier {
        fn make(
            title: Option<&str>,
            ghost: Option<&str>,
            activity: Option<&str>,
            activity_fails_first: bool,
        ) -> StubTier {
            StubTier {
                title: title.map(String::from),
                ghost: ghost.map(String::from),
                activity: activity.map(String::from),
                activity_fails_first,
                title_calls: AtomicUsize::new(0),
                ghost_calls: AtomicUsize::new(0),
                activity_calls: AtomicUsize::new(0),
            }
        }
        pub fn title(t: &str) -> StubTier {
            Self::make(Some(t), None, None, false)
        }
        pub fn ghost(g: &str) -> StubTier {
            Self::make(None, Some(g), None, false)
        }
        pub fn activity(a: &str) -> StubTier {
            Self::make(None, None, Some(a), false)
        }
        pub fn activity_after_none(a: &str) -> StubTier {
            Self::make(None, None, Some(a), true)
        }
        pub fn none() -> StubTier {
            Self::make(None, None, None, false)
        }
    }

    #[async_trait::async_trait]
    impl CheapTier for StubTier {
        async fn title(&self, _f: &str) -> Option<String> {
            self.title_calls.fetch_add(1, Ordering::SeqCst);
            self.title.clone()
        }
        async fn ghost_text(&self, _p: &str) -> Option<String> {
            self.ghost_calls.fetch_add(1, Ordering::SeqCst);
            self.ghost.clone()
        }
        async fn activity(&self, _r: &str) -> Option<String> {
            let n = self.activity_calls.fetch_add(1, Ordering::SeqCst);
            if self.activity_fails_first && n == 0 {
                return None;
            }
            self.activity.clone()
        }
    }

    /// A tier whose `title`/`activity` calls are held open until the test
    /// releases them — a burst against a tier that resolves immediately would
    /// find the slot free every time and pass with no drop rule at all.
    pub struct GatedTier {
        pub calls: AtomicUsize,
        value: Mutex<Option<String>>,
        notify: tokio::sync::Notify,
        /// `hang_when`: inputs containing this marker never settle; others
        /// answer `immediate` at once.
        hang_marker: Option<String>,
        immediate: Option<String>,
    }

    impl GatedTier {
        pub fn new() -> GatedTier {
            GatedTier {
                calls: AtomicUsize::new(0),
                value: Mutex::new(None),
                notify: tokio::sync::Notify::new(),
                hang_marker: None,
                immediate: None,
            }
        }
        pub fn hang_when(marker: &str, immediate: &str) -> GatedTier {
            GatedTier {
                calls: AtomicUsize::new(0),
                value: Mutex::new(None),
                notify: tokio::sync::Notify::new(),
                hang_marker: Some(marker.to_string()),
                immediate: Some(immediate.to_string()),
            }
        }
        pub fn release(&self, value: &str) {
            *self.value.lock().unwrap() = Some(value.to_string());
            self.notify.notify_waiters();
        }
        async fn gated(&self, input: &str) -> Option<String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if let Some(marker) = &self.hang_marker {
                if input.contains(marker) {
                    futures::future::pending::<()>().await;
                    unreachable!()
                }
                return self.immediate.clone();
            }
            loop {
                if let Some(v) = self.value.lock().unwrap().take() {
                    return Some(v);
                }
                self.notify.notified().await;
            }
        }
    }

    #[async_trait::async_trait]
    impl CheapTier for GatedTier {
        async fn title(&self, f: &str) -> Option<String> {
            self.gated(f).await
        }
        async fn ghost_text(&self, _p: &str) -> Option<String> {
            None
        }
        async fn activity(&self, r: &str) -> Option<String> {
            self.gated(r).await
        }
    }

    pub fn test_db() -> SharedDb {
        Arc::new(Mutex::new(SqliteDb::new(":memory:", DbOptions::default()).unwrap()))
    }

    pub fn test_title_ctx(cheap: Option<Arc<dyn CheapTier>>) -> TitleCtx {
        TitleCtx { db: test_db(), bus: Arc::new(Bus::new(system_clock())), cheap }
    }

    pub fn collect_events(bus: &Bus) -> Arc<Mutex<Vec<BoughEvent>>> {
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = events.clone();
        bus.subscribe(Arc::new(move |e: &BoughEvent| sink.lock().unwrap().push(e.clone())));
        events
    }

    pub fn seed_session(db: &SharedDb, title: &str) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        db.lock()
            .unwrap()
            .create_session(Session {
                id: id.clone(),
                title: title.into(),
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
        id
    }

    #[tokio::test]
    async fn the_production_tier_exists_and_degrades_to_none_offline() {
        // `create_cheap_tier` is Some by construction; with no key in a
        // hermetic env the call itself must still resolve (to None) rather
        // than error — driven here with an injected env so nothing reads the
        // developer's shell.
        assert!(super::create_cheap_tier().is_some());
        let opts = crate::worker::titles::CheapCallOpts {
            env: Some(Arc::new(|_| None)),
            timeout_ms: Some(200),
            llm: Some(hanging_client()),
            ..Default::default()
        };
        assert_eq!(crate::worker::titles::cheap_text("s", "p", 8, &opts).await, None);
    }
}
