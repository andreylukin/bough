//! `GET /models` — the picker's catalog, static rows plus whatever the key
//! can reach (port of `src/server/models.ts`).
//!
//! THE BUG THIS EXISTS FOR: `discoverOpenAIModels` and `mergeModels` were
//! written, tested, and called by nothing — a user with a working key still
//! saw only the compiled-in ids. The catalog is answered SERVER-SIDE because
//! the server is the process that holds the credential (`~/.bough/env`); a
//! TUI that discovered with its own environment would offer rows the server
//! cannot bill.
//!
//! NEVER SLOWER THAN THE TERMINAL IT BLOCKS. The TUI awaits this before its
//! first frame, and a provider allows itself ten seconds, so a hung provider
//! would be ten seconds of blank terminal. A caller waits `deadline_ms` and
//! no longer: past it the request is answered from the static table (plus any
//! stale cache), the discovery it started keeps running, and the NEXT ask is
//! served from the warm cache. The list arriving one launch late is a cost
//! nobody notices; a boot that hangs is.

use std::panic::AssertUnwindSafe;
use std::sync::{Arc, LazyLock, Mutex};

use futures::future::{BoxFuture, FutureExt, Shared};
use serde_json::json;

use bough_core::llm::discovery::{discover_models, merge_models};
use bough_core::llm::routing::{ModelRow, ProviderOpts, MODELS};
use bough_core::types::{system_clock, Clock};

use crate::http::{handler, json as json_res, Handler};

/// How long a discovered list is trusted. Model lists change on the
/// providers' schedule, not ours.
pub const TTL_MS: i64 = 10 * 60_000;

/// How long a request waits on a cold discovery before answering without it.
pub const DEADLINE_MS: u64 = 2_500;

/// The injected discovery — a fresh future per ask, so tests script it.
pub type Discover = Arc<dyn Fn() -> BoxFuture<'static, Vec<ModelRow>> + Send + Sync>;

struct CatalogState {
    /// The last discovery's rows and when they landed.
    cached: Option<(i64, Vec<ModelRow>)>,
    /// One discovery in flight at a time. Without it a cold server answering
    /// three simultaneous asks would open three requests to `/v1/models` and
    /// keep the last one's answer — a rate limit waiting for a busy morning.
    inflight: Option<Shared<BoxFuture<'static, Vec<ModelRow>>>>,
}

/// The per-process catalog cache. The handler uses one process-level
/// instance; tests construct their own so one test's stub does not answer the
/// next one's.
pub struct ModelCatalog {
    state: Arc<Mutex<CatalogState>>,
}

/// The injectable seams, for the same reason `loadDefaults` takes a path: a
/// test that waited two and a half real seconds on a real socket would be a
/// test nobody runs.
#[derive(Default)]
pub struct CatalogOpts {
    pub now: Option<Clock>,
    pub deadline_ms: Option<u64>,
    pub discover: Option<Discover>,
}

impl Default for ModelCatalog {
    fn default() -> Self {
        Self::new()
    }
}

impl ModelCatalog {
    pub fn new() -> ModelCatalog {
        ModelCatalog { state: Arc::new(Mutex::new(CatalogState { cached: None, inflight: None })) }
    }

    /// The merged catalog: static table first (keeping its ids), discovered
    /// rows after, within `deadline_ms` no matter what the providers do.
    pub async fn catalog(&self, opts: CatalogOpts) -> Vec<ModelRow> {
        let now = opts.now.unwrap_or_else(system_clock);
        let discover: Discover = opts
            .discover
            .unwrap_or_else(|| Arc::new(|| discover_models(ProviderOpts::default()).boxed()));
        let deadline_ms = opts.deadline_ms.unwrap_or(DEADLINE_MS);

        let shared = {
            let mut st = self.state.lock().unwrap();
            if let Some((at, rows)) = &st.cached {
                if now() - at < TTL_MS {
                    return merge_models(&MODELS, rows);
                }
            }
            if st.inflight.is_none() {
                // The discovery runs on its OWN task so that a caller whose
                // deadline gave up on it cannot stall it by not polling — the
                // abandoned discovery still warms the cache for the next ask.
                let (tx, rx) = tokio::sync::oneshot::channel::<Vec<ModelRow>>();
                let state = self.state.clone();
                let stamp = now.clone();
                tokio::spawn(async move {
                    // `discover_models` documents that it never fails; the
                    // guard is here because a panic on a background task
                    // would poison the single-flight slot forever, which is
                    // the one outcome worse than an incomplete picker.
                    let rows = AssertUnwindSafe(discover())
                        .catch_unwind()
                        .await
                        .unwrap_or_default();
                    let mut st = state.lock().unwrap();
                    st.cached = Some((stamp(), rows.clone()));
                    st.inflight = None;
                    drop(st);
                    let _ = tx.send(rows);
                });
                let fut: BoxFuture<'static, Vec<ModelRow>> =
                    async move { rx.await.unwrap_or_default() }.boxed();
                st.inflight = Some(fut.shared());
            }
            st.inflight.clone().expect("just ensured")
        };

        let rows = tokio::select! {
            rows = shared => rows,
            _ = tokio::time::sleep(std::time::Duration::from_millis(deadline_ms)) => {
                // The stale rows, not an empty list: a cache past its TTL is
                // still a better answer than the static table alone, and it
                // is what the user saw a minute ago.
                let st = self.state.lock().unwrap();
                st.cached.as_ref().map(|(_, r)| r.clone()).unwrap_or_default()
            }
        };
        merge_models(&MODELS, &rows)
    }
}

/// The process-level cache the route answers from — per PROCESS, the same
/// scope the theme file and model defaults already use.
static CATALOG: LazyLock<ModelCatalog> = LazyLock::new(ModelCatalog::new);

/// `GET /models` — `{models: [ModelRow]}`, static merged with discovered.
pub fn get_models() -> Handler {
    handler(|_req, _ctx, _params| async move {
        let models = CATALOG.catalog(CatalogOpts::default()).await;
        Ok(json_res(&json!({ "models": models }), 200))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use bough_core::llm::routing::Provider;
    use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};

    fn luna() -> ModelRow {
        ModelRow {
            id: "openai:gpt-5.6-luna".into(),
            label: "gpt-5.6-luna (OpenAI)".into(),
            provider: Provider::Openai,
        }
    }

    /// A discovery that never settles — what a hung `api.openai.com` looks
    /// like from here.
    fn hung() -> Discover {
        Arc::new(|| futures::future::pending().boxed())
    }

    fn resolved(rows: Vec<ModelRow>) -> Discover {
        Arc::new(move || {
            let rows = rows.clone();
            async move { rows }.boxed()
        })
    }

    #[tokio::test]
    async fn discovered_rows_land_after_the_static_table_which_keeps_its_ids() {
        let catalog = ModelCatalog::new();
        let rows = catalog
            .catalog(CatalogOpts { discover: Some(resolved(vec![luna()])), ..Default::default() })
            .await;
        assert_eq!(rows[..MODELS.len()], MODELS[..]);
        assert_eq!(rows.last().unwrap(), &luna());
    }

    #[tokio::test]
    async fn a_hung_provider_answers_from_the_static_table_instead_of_blocking_the_boot() {
        let catalog = ModelCatalog::new();
        let rows = catalog
            .catalog(CatalogOpts { discover: Some(hung()), deadline_ms: Some(1), ..Default::default() })
            .await;
        assert_eq!(rows, *MODELS);
    }

    #[tokio::test]
    async fn the_discovery_a_deadline_gave_up_on_still_warms_the_cache_for_the_next_ask() {
        let catalog = ModelCatalog::new();
        let (tx, rx) = tokio::sync::oneshot::channel::<Vec<ModelRow>>();
        let rx = Arc::new(Mutex::new(Some(rx)));
        let slow: Discover = Arc::new(move || {
            let rx = rx.lock().unwrap().take().expect("one discovery in flight");
            async move { rx.await.unwrap() }.boxed()
        });

        let first = catalog
            .catalog(CatalogOpts { discover: Some(slow), deadline_ms: Some(1), ..Default::default() })
            .await;
        assert_eq!(first, *MODELS, "the deadline answered without the slow rows");

        tx.send(vec![luna()]).unwrap();
        // Let the abandoned discovery settle into the cache.
        for _ in 0..500 {
            if catalog.state.lock().unwrap().cached.is_some() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
        let rows = catalog
            .catalog(CatalogOpts { discover: Some(hung()), ..Default::default() })
            .await;
        assert_eq!(rows.last().unwrap(), &luna());
    }

    #[tokio::test]
    async fn one_discovery_in_flight_however_many_callers_ask_at_once() {
        let catalog = Arc::new(ModelCatalog::new());
        let calls = Arc::new(AtomicUsize::new(0));
        let counting_calls = calls.clone();
        let counted: Discover = Arc::new(move || {
            counting_calls.fetch_add(1, Ordering::SeqCst);
            let rows = vec![luna()];
            async move { rows }.boxed()
        });
        let asks = (0..3).map(|_| {
            let catalog = catalog.clone();
            let counted = counted.clone();
            async move {
                catalog.catalog(CatalogOpts { discover: Some(counted), ..Default::default() }).await
            }
        });
        futures::future::join_all(asks).await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_fresh_cache_is_not_re_discovered_an_expired_one_is() {
        let catalog = ModelCatalog::new();
        let calls = Arc::new(AtomicUsize::new(0));
        let counting_calls = calls.clone();
        let counted: Discover = Arc::new(move || {
            counting_calls.fetch_add(1, Ordering::SeqCst);
            let rows = vec![luna()];
            async move { rows }.boxed()
        });
        let clock = Arc::new(AtomicI64::new(1_000_000));
        let reading = clock.clone();
        let now: Clock = Arc::new(move || reading.load(Ordering::SeqCst));

        let ask = |c: Discover, n: Clock| CatalogOpts { discover: Some(c), now: Some(n), ..Default::default() };
        catalog.catalog(ask(counted.clone(), now.clone())).await;
        catalog.catalog(ask(counted.clone(), now.clone())).await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        clock.fetch_add(11 * 60_000, Ordering::SeqCst); // past the TTL
        catalog.catalog(ask(counted, now)).await;
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_discovery_that_panics_degrades_to_the_static_table() {
        // `discover_models` documents that it never fails. If that ever stops
        // being true, the picker loses rows — it does not take the server
        // down with it.
        let catalog = ModelCatalog::new();
        let boom: Discover = Arc::new(|| async { panic!("boom") }.boxed());
        let rows = catalog.catalog(CatalogOpts { discover: Some(boom), ..Default::default() }).await;
        assert_eq!(rows, *MODELS);
    }

    #[tokio::test]
    async fn get_models_serves_the_catalog_with_ts_field_names() {
        use crate::app::{create_handler, CreateHandlerOptions};
        use crate::http::testutil;
        let fx = testutil::fixture();
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        let res = call.call(testutil::get("/models")).await;
        assert_eq!(res.status(), 200);
        let body = testutil::body_json(res).await;
        let models = body["models"].as_array().unwrap();
        assert!(!models.is_empty());
        assert_eq!(models[0]["id"], "claude-opus-4-8");
        assert_eq!(models[0]["label"], "Opus 4.8");
        assert_eq!(models[0]["provider"], "anthropic");
        // The Workers AI rows survive with their `@cf/` ids intact.
        assert!(models.iter().any(|m| m["id"] == "@cf/zai-org/glm-5.2"));
    }
}
