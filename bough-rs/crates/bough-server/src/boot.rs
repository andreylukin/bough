//! The composed main wiring (port of `src/server/main.ts`). **Process wiring
//! lives here and only here** — everything else receives what it needs as a
//! parameter, and a test builds its own ctx over an in-memory database and
//! never runs this file at all.
//!
//! Boot order is load-bearing (ARCHITECTURE.md §6), wave-1 subset:
//!
//! 1. decide sqlite extension capability — before the first open
//! 2. open db (migrate)
//! 3. build Bus, HostState, AppCtx
//! 4. recover orphaned turns — BEFORE the listener binds (a client that
//!    connected first would fetch a session that looks busy forever); orphaned
//!    WORKFLOW recovery arrives with the workflow subsystem (wave 2, row 2.x)
//! 5. install the `SearchSafeDb` wrapper on ctx.db — AFTER recovery used the
//!    raw handle; everything served goes through the wrapper, which is what
//!    keeps an FTS failure from failing a message insert
//! 6. wire the ONE composed turn starter (runner production defaults; wave 2
//!    layers skills/grants/notes); without it a posted message
//!    lands + announces without starting a turn (the documented M1 shape)
//! 7. `sweep_scratch` best-effort (v1 stub: sweeps nothing)
//! 8. start the schedule ticker (v1 stub: never fires) — after the starter
//! 9. bind `127.0.0.1:$BOUGH_PORT` — **loopback only, no override**: there is
//!    no auth layer and none is planned; binding anywhere else would silently
//!    publish an unauthenticated API that runs arbitrary programs as the user
//! 10. SIGINT/SIGTERM → `jobs.kill_all()` (MCP child kill arrives with wave 3)
//!
//! **Coexistence:** `BOUGH_PORT` moves the listener and `BOUGH_HOME` relocates
//! the data root, which is what lets this run beside the live install.

use std::sync::{Arc, Mutex, RwLock};

use bough_core::bus::Bus;
use bough_core::db::extensions::enable_sqlite_extensions;
use bough_core::db::sqlite_db::{open_db, DbOptions};
use bough_core::errors::BoughError;
use bough_core::paths::db_path;
use bough_core::scratch::{sweep_scratch, SweepOptions};
use bough_core::turn::queue::TurnRegistry;
use bough_core::turn::state::{recover_orphaned_turns, OrphanedTurn, RecoverOptions};
use bough_core::types::{system_clock, AppCtx, HostState, SharedDb};

use crate::app::build_router;
use crate::search::{index_recovered_messages, SearchSafeDb, SearchSafeOptions};

/// The default port, shared with the TS server so cutover changes nothing.
pub const DEFAULT_PORT: u16 = 4321;

/// What booting the process produced, short of the socket: the one ctx and
/// the recovery report. Separated from [`start`] so a test can drive the whole
/// boot order against its own database file and never bind.
pub struct Boot {
    pub ctx: AppCtx,
    /// The turns the previous process left `running`, now `orphaned`.
    pub recovered: Vec<OrphanedTurn>,
}

/// Steps 1–7 of the boot order. `db_file` is injected by tests; `None` opens
/// the real `~/.bough/bough.db` (under `BOUGH_HOME` if set).
pub fn boot_ctx(db_file: Option<&str>) -> Result<Boot, BoughError> {
    // 1. FIRST, before any connection exists: the once-per-process extension
    // decision (`BOUGH_NO_EMBED` gate lives inside).
    enable_sqlite_extensions();

    // 2. Open + migrate. Refuses a newer `user_version` rather than guessing.
    let raw = open_db(db_file, DbOptions::default())?;

    // 3. The one bus (the ctx comes after recovery, over the wrapped handle).
    let bus = Arc::new(Bus::new(system_clock()));

    // 4. Before the listener binds, never after: a client that connected first
    // would fetch a session that still looks busy and render a turn that died
    // with the previous process. Uses the RAW db handle by design — the
    // search-safe wrapper (step 5) is installed after recovery.
    let recovered = recover_orphaned_turns(&raw, &*bus, RecoverOptions::default())?;
    if !recovered.is_empty() {
        println!(
            "recovered {} turn(s) orphaned by the previous process: {}",
            recovered.len(),
            recovered
                .iter()
                .map(|o| format!("{}@{}", o.turn_id, o.step))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    // Orphaned WORKFLOW recovery belongs here too (same reason, same timing);
    // it arrives with the workflow subsystem (wave 2).

    // 5. The `SearchSafeDb` wrapper — everything downstream serves with this
    // handle, which is the whole write path the wrapper protects: an FTS
    // failure must never fail a message insert. Mandatory now that FTS
    // indexing rides the message write path over HTTP.
    let db: SharedDb =
        Arc::new(Mutex::new(SearchSafeDb::new(raw, SearchSafeOptions::default())));

    // Then the messages boot recovery just closed. A turn that died
    // mid-stream never reached the finish path that indexes its message, so
    // everything the supervisor had already said in it would be unsearchable
    // forever — and this is the one moment those messages are known, closed
    // and enumerated. Idempotent, like every other index write.
    if !recovered.is_empty() {
        let ids: Vec<String> = recovered.iter().map(|o| o.message_id.clone()).collect();
        let reindexed = {
            let db = db.lock().unwrap();
            index_recovered_messages(&*db, &ids)
        };
        if reindexed > 0 {
            println!("re-indexed {reindexed} message(s) closed by turn recovery");
        }
    }

    let host = Arc::new(HostState::new());
    // Background shells publish `job.spawned`/`job.exited` through the bus.
    // The registry outlives any turn, so it is wired here, not at construction.
    host.jobs.attach_bus(bus.clone());
    let ctx = AppCtx {
        db,
        bus,
        llm: None,
        model: std::env::var("BOUGH_MODEL").ok().filter(|m| !m.is_empty()),
        effort: None,
        now: system_clock(),
        // The cheap tier (wave 2, row 2.19): auto titles, composer ghost
        // text, live activity blurbs. Always installed — the gate is per
        // call: a missing key, a provider error and a hung connection are
        // the same silent `None`, and `BOUGH_CHEAP_MODEL` is re-read on
        // every call so a picker change needs no restart. Every reader
        // still degrades when a test ctx leaves it absent.
        cheap: bough_core::worker::create_cheap_tier(),
        host,
        starter: Arc::new(RwLock::new(None)),
        turn_registry: Arc::new(TurnRegistry::new()),
        model_defaults_path: None, // None = the real ~/.bough/model.json
    };

    // 6. The ONE composed turn starter — the FINAL composition, not the base
    // one. Port of the last `ctx.startTurn = createDelegatingTurnStarter(…)`
    // in `server/main.ts`; the TS file rebuilds that starter seven times as
    // each milestone lands, and only the last one is the product.
    //
    // BOTH HALVES ARE REQUIRED AND NEITHER IS SUFFICIENT (spec §6): `extend`
    // bridges the verb host functions into the turn, and `granted` is what
    // makes `prompt/assemble` include their sections. A turn told about
    // `ask()` that cannot call it wastes a round; a turn that can call one it
    // was never told about will not call it at all. Wiring only `extend` is
    // therefore indistinguishable from wiring nothing.
    //
    // `workflow` is tier-gated (top-level turns only — a subagent that could
    // start a workflow could fan out past every cap); the rest are granted at
    // every tier, because a subagent that renders a comparison should be able
    // to publish it and the artifact store is per-session anyway.
    {
        use bough_core::hostfn::delegate::{create_delegating_turn_starter, DelegationWiring};
        use bough_core::harness::protocol::HostFnName;
        use bough_core::turn::runner::{TurnDeps, BASE_HOST_FNS};
        use bough_core::types::HostFns;

        // NOT `workflow`: its engine is wave 3, and granting a verb whose
        // host function is absent is the exact failure this comment block
        // warns about — the prompt would teach a capability every call to
        // which fails.
        let mut granted = BASE_HOST_FNS.to_vec();
        granted.extend_from_slice(&[
            HostFnName::Schedule,
            HostFnName::Ask,
            HostFnName::State,
            HostFnName::Artifact,
        ]);

        let jobs = ctx.host.jobs.clone();
        *ctx.starter.write().unwrap() = Some(create_delegating_turn_starter(DelegationWiring {
            base: TurnDeps {
                granted: Some(granted),
                // Background shells outlive an interrupt on purpose, so the
                // stop note can name them instead of implying there were none.
                surviving_jobs: Some(Arc::new(move |session_id: &str| {
                    jobs.running_ids(session_id)
                })),
                ..Default::default()
            },
            deliver: Some(bough_core::agents::notes::create_note_deliverer(Default::default())),
            extend: Some(Arc::new(|turn_ctx: &bough_core::types::TurnCtx| {
                let mut fns = HostFns::default();
                fns.schedule = Some(bough_core::hostfn::schedule::create_schedule_host_fn(
                    turn_ctx,
                    Default::default(),
                ));
                fns.ask = Some(
                    bough_core::hostfn::ask::create_ask_host_fn(turn_ctx, Default::default())
                        .into_host_fn(),
                );
                fns.state = Some(
                    bough_core::hostfn::state::create_state_host_fn(turn_ctx, Default::default())
                        .into_host_fn(),
                );
                fns.artifact = Some(bough_core::hostfn::artifact::create_artifact_host_fn(
                    turn_ctx,
                    Default::default(),
                ));
                fns
            })),
            ..Default::default()
        }));
    }

    // 6b. The cheap tier's two watchers (T10.1): auto titles and activity
    // blurbs, both bus listeners that start a task nobody holds. The
    // unsubscribes are discarded deliberately — both watchers live for the
    // life of the process, and holding a thunk nobody calls would only imply
    // otherwise. Ghost text is its own HTTP request and needs no watcher.
    let _ = bough_core::worker::titles::watch_titles(&bough_core::worker::titles::TitleCtx {
        db: ctx.db.clone(),
        bus: ctx.bus.clone(),
        cheap: ctx.cheap.clone(),
    });
    let _ = bough_core::worker::activity::watch_activity(
        &bough_core::worker::activity::ActivityCtx {
            bus: ctx.bus.clone(),
            cheap: ctx.cheap.clone(),
        },
    );
    println!(
        "cheap tier: {} ({}) — auto titles, composer ghost text (POST /sessions/:id/ghost), \
         live activity blurbs. Fire-and-forget: every failure is silent and none of them \
         can delay a turn.",
        bough_core::worker::cheap_model(),
        bough_core::worker::CHEAP_MODEL_ENV,
    );

    // 7. The scratchpad sweep, best-effort: a scratch root that cannot be
    // read is not a reason to refuse to start. (v1 stub: sweeps nothing.)
    let swept = sweep_scratch(SweepOptions::default());
    if !swept.is_empty() {
        println!(
            "swept {} scratch director{}",
            swept.len(),
            if swept.len() == 1 { "y" } else { "ies" }
        );
    }

    Ok(Boot { ctx, recovered })
}

/// Step 9: **loopback only, no override.** The hostname is a constant, not a
/// parameter — there is no auth layer, and none is planned.
pub async fn bind_loopback(port: u16) -> Result<tokio::net::TcpListener, BoughError> {
    tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .map_err(|e| BoughError::bad_request(format!("cannot bind 127.0.0.1:{port}: {e}")))
}

/// Step 10: SIGINT/SIGTERM. Background shells are children of THIS process,
/// so an unkilled one survives as an orphan with no reader for its output —
/// kill children before the process goes.
async fn shutdown_signal(ctx: AppCtx) {
    let signal_name: &str;
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        // Not every platform exposes every signal; teardown is best-effort,
        // and failing to register one must not stop the server from starting.
        let mut term = signal(SignalKind::terminate()).ok();
        let term_wait = async {
            match term.as_mut() {
                Some(t) => {
                    t.recv().await;
                }
                None => futures::future::pending::<()>().await,
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => { signal_name = "SIGINT"; }
            _ = term_wait => { signal_name = "SIGTERM"; }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
        signal_name = "SIGINT";
    }
    let killed = ctx.host.jobs.kill_all();
    println!("{signal_name}: killed {killed} background shell(s), closing db");
}

/// `bough start` — the whole boot order, then serve until a signal.
pub async fn start() -> Result<(), BoughError> {
    let port: u16 = std::env::var("BOUGH_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(DEFAULT_PORT);

    let boot = boot_ctx(None)?;

    // 8. The schedule ticker, after the starter exists (v1 stub: no-op). The
    // stopper is held for the life of the serve loop.
    let stop_ticker = bough_core::schedules::start_schedule_ticker(&boot.ctx);

    // 9. Bind, THEN report. No read/idle timeout middleware anywhere: the
    // `/events` SSE stream is idle by design between turns, and one
    // `bough exec` request is held open for a whole turn.
    let listener = bind_loopback(port).await?;
    println!("bough listening on 127.0.0.1:{port} — db {}", db_path().display());

    let router = build_router(boot.ctx.clone());
    let ctx = boot.ctx.clone();
    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal(ctx))
        .await
        .map_err(|e| BoughError::bad_request(format!("server error: {e}")))?;

    stop_ticker();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use bough_core::db::sqlite_db::SqliteDb;
    use bough_core::schema::parts::{
        Message, Part, Role, Session, SessionKind, Turn, TurnStatus,
    };
    use bough_core::types::Db;

    /// A database file holding one session whose turn was left `running` — the
    /// state a crashed server leaves behind.
    fn crashed_db() -> (std::path::PathBuf, String) {
        let dir = std::env::temp_dir().join(format!("bough-boot-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bough.db");
        let db = SqliteDb::new(path.to_str().unwrap(), Default::default()).unwrap();
        let session_id = uuid::Uuid::new_v4().to_string();
        db.create_session(Session {
            id: session_id.clone(),
            title: "wedged".into(),
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
        let m = db
            .create_message(Message {
                id: uuid::Uuid::new_v4().to_string(),
                session_id: session_id.clone(),
                role: Role::Supervisor,
                parts: vec![Part::Text { text: "partial".into() }],
                pending: true,
                created_at: 2,
            })
            .unwrap();
        db.create_turn(Turn {
            id: uuid::Uuid::new_v4().to_string(),
            session_id: session_id.clone(),
            message_id: m.id,
            status: TurnStatus::Running,
            step: "model".into(),
            created_at: 2,
            updated_at: 2,
            error: None,
            usage: None,
        })
        .unwrap();
        db.close();
        (path, session_id)
    }

    #[test]
    fn boot_recovers_orphaned_turns_before_any_listener_exists() {
        let (path, session_id) = crashed_db();
        let boot = boot_ctx(path.to_str()).unwrap();

        // The recovery happened inside boot_ctx — no socket was ever bound.
        assert_eq!(boot.recovered.len(), 1);
        assert_eq!(boot.recovered[0].session_id, session_id);
        assert!(boot.recovered[0].closed_message);

        // The session is no longer busy, so a first client sees the truth.
        let db = boot.ctx.db.lock().unwrap();
        assert!(!db.busy_session_ids().unwrap().contains(&session_id));
        assert!(db.turns_by_status(TurnStatus::Running).unwrap().is_empty());
        assert_eq!(db.turns_by_status(TurnStatus::Orphaned).unwrap().len(), 1);
    }

    #[test]
    fn a_second_boot_finds_nothing_recovery_is_idempotent() {
        let (path, _) = crashed_db();
        let first = boot_ctx(path.to_str()).unwrap();
        assert_eq!(first.recovered.len(), 1);
        drop(first);
        let second = boot_ctx(path.to_str()).unwrap();
        assert!(second.recovered.is_empty());
    }

    #[test]
    fn boot_installs_the_search_safe_wrapper_on_the_served_handle() {
        let (path, _) = crashed_db();
        let boot = boot_ctx(path.to_str()).unwrap();
        // The raw handle answers None; only the wrapper carries a health
        // record. Recovery above ran on the raw handle by construction (the
        // wrapper is built after it), and everything served goes through the
        // wrapped one.
        assert!(boot.ctx.db.lock().unwrap().index_health().is_some());
    }

    #[test]
    fn boot_wires_the_starter_and_the_cheap_tier() {
        let (path, _) = crashed_db();
        let boot = boot_ctx(path.to_str()).unwrap();
        // A posted message must START a turn — the wave-1 exit criterion is a
        // live end-to-end turn, so a boot without a starter is a dead loop.
        assert!(boot.ctx.turn_starter().is_some());
        // Wave 2 (row 2.19) flips the wave-1 `cheap: None` to the real tier.
        // Installing it is safe offline: every call degrades to a silent
        // `None`, and nothing in boot itself ever invokes it.
        assert!(boot.ctx.cheap.is_some());
        // The real model.json path (None sentinel), not a test seam.
        assert!(boot.ctx.model_defaults_path.is_none());
    }

    /// The row-1.30 gate: the LIVE `~/.bough/bough.db` must boot under the
    /// Rust migrate. Ignored by default — it reads the developer's machine —
    /// and run manually with `cargo test -p bough-server -- --ignored`.
    /// A COPY is booted, never the original; skips silently when absent.
    #[test]
    #[ignore = "reads a copy of the live ~/.bough/bough.db; manual gate"]
    fn boot_accepts_a_copy_of_the_live_database() {
        let live = db_path();
        if !live.exists() {
            return; // no live install on this machine — nothing to gate
        }
        let dir = std::env::temp_dir().join(format!("bough-live-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let copy = dir.join("bough.db");
        std::fs::copy(&live, &copy).unwrap();
        let boot = boot_ctx(copy.to_str()).unwrap();
        // Whatever was running when the live server last died is recovered;
        // the point is that migrate + recovery accept the real schema.
        let db = boot.ctx.db.lock().unwrap();
        assert!(db.turns_by_status(TurnStatus::Running).unwrap().is_empty());
        drop(db);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn the_listener_binds_loopback_only() {
        // Port 0: the OS picks a free one, so the test never collides.
        let listener = bind_loopback(0).await.unwrap();
        let addr = listener.local_addr().unwrap();
        assert_eq!(addr.ip().to_string(), "127.0.0.1");
    }

    #[tokio::test]
    async fn a_booted_ctx_serves_the_route_table() {
        use tower::ServiceExt;
        let (path, session_id) = crashed_db();
        let boot = boot_ctx(path.to_str()).unwrap();
        let router = build_router(boot.ctx.clone());

        let res = router
            .clone()
            .oneshot(crate::http::testutil::get("/sessions"))
            .await
            .unwrap();
        assert_eq!(res.status(), 200);
        let rows = crate::http::testutil::body_json(res).await;
        let row = rows
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == session_id.as_str())
            .cloned()
            .unwrap();
        // The recovered session lists as idle with the orphaned status —
        // proof the recovery ran before anything could be served.
        assert_eq!(row["busy"], false);
        assert_eq!(row["lastTurnStatus"], "orphaned");
    }
}
