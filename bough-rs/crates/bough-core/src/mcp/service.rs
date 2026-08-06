//! MCP as a SERVICE: the process connects granted remote servers and keeps them
//! connected, independently of any conversation (port of `src/mcp/service.ts`).
//!
//! WHY THIS EXISTS. Every connection used to be made on demand, by a turn, in a
//! conversation's name — so "is Slack connected?" had no process-level answer, only
//! a per-conversation one, and the honest answer in a fresh conversation was always
//! "no". Three symptoms, one cause: a new conversation showed every server
//! disconnected; the panel could do nothing before the first message was sent; and
//! proving a server worked meant spending a turn on a tool call. Registering a
//! server is a statement about this machine, and so is granting it — the connection
//! had no business being narrower than either.
//!
//! WHAT IT OWNS, and the boundary is deliberate: **remote servers only.** A stdio
//! server is a subprocess whose cwd is the conversation's checkout, so it cannot be
//! shared and must not be started before someone asks — starting every registered
//! command at boot would spawn processes for conversations that may never happen, in
//! a directory that is not theirs. `manager.rs`'s `scope_for` draws the same line for
//! the same reason.
//!
//! WHAT IT IS NOT: a cache. It holds live connections, never answers. The status
//! builder still re-reads the registry, the grant and the connection table on every
//! call, and this module only changes WHEN a connection is opened, never what is
//! reported about one.
//!
//! FAILURE IS NORMAL AND SILENT HERE. A server that is down, unauthorized or
//! misconfigured must not delay start-up or print a stack: the failure is already
//! recorded by the manager and surfaces as a `failed` row in the panel and in
//! `bough mcp`, with the reason. Reconciling is best-effort by construction.

use std::sync::Arc;

use crate::mcp::config::{activations_for, is_stdio, load_registry, McpConfigOptions};
use crate::mcp::manager::{mcp_manager, McpManager, SpawnCtx, SHARED_SCOPE};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ReconcileResult {
    /// Servers connected (or already connected) after this pass.
    pub connected: Vec<String>,
    /// Servers that were tried and failed, with the reason the panel will show.
    pub failed: Vec<(String, String)>,
    /// Live connections closed because the grant went away.
    pub closed: Vec<String>,
}

#[derive(Clone, Default)]
pub struct ServiceDeps {
    pub manager: Option<Arc<McpManager>>,
    pub config: Option<McpConfigOptions>,
    /// Where a connection is attempted from. Unused by remote servers; here for
    /// tests.
    pub workspace: Option<String>,
}

/// Bring the process's connections in line with the registry and the global grant.
///
/// Idempotent, and safe to call as often as something changes: an already-live
/// connection is reused rather than reopened, and a server whose grant was withdrawn
/// is dropped so it stops answering — a revoked server that kept serving from an open
/// connection would be a permission that outlived its revocation, which is the one
/// thing this layer must never do.
pub async fn reconcile_mcp(deps: &ServiceDeps) -> ReconcileResult {
    let manager = deps.manager.clone().unwrap_or_else(mcp_manager);
    let config = deps.config.clone().unwrap_or_default();
    let registry = load_registry(&config).servers;
    // The GLOBAL grant, which is what a human's ⏎ in the panel writes. A
    // session-scoped grant is a skill's or a TTL's, and belongs to the turn that has
    // it — not to a process-wide connection.
    let granted = activations_for(None, &config);

    let wanted: Vec<String> = registry
        .iter()
        .filter(|(name, cfg)| granted.iter().any(|g| g == *name) && !is_stdio(cfg))
        .map(|(name, _)| name.clone())
        .collect();

    // Drop first: a connection whose grant is gone must stop being usable before
    // anything else happens, and closing it cannot fail in a way worth reporting.
    let mut closed: Vec<String> = Vec::new();
    for conn in manager.statuses(Some(SHARED_SCOPE)) {
        if !wanted.contains(&conn.server) {
            closed.push(conn.server.clone());
            // `drop_conn`, not `drop`: an inherent `drop` method is shadowed by
            // `Drop::drop` at every call site (row 3.3).
            manager.drop_conn(SHARED_SCOPE, &conn.server).await;
        }
    }

    if wanted.is_empty() {
        return ReconcileResult {
            connected: vec![],
            failed: vec![],
            closed,
        };
    }

    let workspace = deps.workspace.clone().unwrap_or_else(|| {
        std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_default()
    });
    let catalogs = manager
        .ensure(SHARED_SCOPE, &wanted, &SpawnCtx::new(workspace))
        .await;
    let mut connected = Vec::new();
    let mut failed = Vec::new();
    for c in catalogs {
        match c.error {
            Some(error) => failed.push((c.name, error)),
            None => connected.push(c.name),
        }
    }
    ReconcileResult {
        connected,
        failed,
        closed,
    }
}

/// One line for the boot log. Says nothing at all when there is nothing to say.
pub fn reconcile_summary(r: &ReconcileResult) -> Option<String> {
    let mut bits: Vec<String> = Vec::new();
    if !r.connected.is_empty() {
        bits.push(format!("connected {}", r.connected.join(", ")));
    }
    // The reason, not just the count: a server that fails at boot is exactly the one
    // whose reason nobody will go looking for.
    for (name, error) in &r.failed {
        bits.push(format!("{name} failed ({error})"));
    }
    if !r.closed.is_empty() {
        bits.push(format!("closed {}", r.closed.join(", ")));
    }
    (!bits.is_empty()).then(|| format!("MCP: {}", bits.join(" · ")))
}

#[cfg(test)]
mod tests {
    //! Hermetic — a temp registry file and an injected connector, so nothing spawns,
    //! no socket is opened, and the assertions are about WHICH servers the process
    //! holds open rather than about any real endpoint.

    use super::*;
    use crate::errors::BoughError;
    use crate::mcp::client::{McpCallResult, McpConnection, McpToolInfo};
    use crate::mcp::config::{save_registry, set_activation};
    use crate::mcp::manager::{Connector, McpManagerOptions};
    use serde_json::{json, Value};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    fn tmp_registry() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "bough-mcp-service-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("mcp.json")
    }

    struct FakeConnection {
        name: String,
        alive: AtomicBool,
    }

    #[async_trait::async_trait]
    impl McpConnection for FakeConnection {
        fn name(&self) -> &str {
            &self.name
        }
        async fn list_tools(&self) -> Result<Vec<McpToolInfo>, BoughError> {
            Ok(vec![McpToolInfo {
                name: "echo".into(),
                ..Default::default()
            }])
        }
        async fn call_tool(&self, _n: &str, _a: Value) -> Result<McpCallResult, BoughError> {
            Ok(McpCallResult::default())
        }
        async fn close(&self) {
            self.alive.store(false, Ordering::SeqCst);
        }
        fn alive(&self) -> bool {
            self.alive.load(Ordering::SeqCst)
        }
        fn stderr_tail(&self) -> String {
            String::new()
        }
    }

    fn counting_connector(count: Arc<AtomicUsize>) -> Connector {
        Arc::new(move |spec| {
            let count = count.clone();
            Box::pin(async move {
                count.fetch_add(1, Ordering::SeqCst);
                Ok(Arc::new(FakeConnection {
                    name: spec.name,
                    alive: AtomicBool::new(true),
                }) as Arc<dyn McpConnection>)
            })
        })
    }

    fn refusing_connector(reason: &'static str) -> Connector {
        Arc::new(move |_spec| {
            Box::pin(
                async move { Err(BoughError::http(502, crate::errors::ErrorKind::Mcp, reason)) },
            )
        })
    }

    fn grant(file: &std::path::Path, name: &str, on: bool) {
        set_activation(None, name, on, None, &McpConfigOptions::with_file(file)).unwrap();
    }

    #[tokio::test]
    async fn granted_remote_servers_connect_with_no_conversation_in_existence() {
        // The whole point: "is Slack connected?" must have a process-level answer.
        let file = tmp_registry();
        let opts = McpConfigOptions::with_file(&file);
        save_registry(
            &json!({ "servers": {
                "remote": { "url": "https://a.example/mcp" },
                "ungranted": { "url": "https://b.example/mcp" },
                "local": { "command": "echo", "args": [] },
            }}),
            &opts,
        )
        .unwrap();
        grant(&file, "remote", true);
        grant(&file, "local", true); // granted, but a subprocess

        let connects = Arc::new(AtomicUsize::new(0));
        let mgr = Arc::new(McpManager::new(McpManagerOptions {
            config: Some(opts.clone()),
            connect: Some(counting_connector(connects.clone())),
            ..Default::default()
        }));
        let deps = ServiceDeps {
            manager: Some(mgr.clone()),
            config: Some(opts.clone()),
            workspace: Some("/tmp".into()),
        };

        let r = reconcile_mcp(&deps).await;
        assert_eq!(r.connected, vec!["remote".to_string()]);
        assert!(r.failed.is_empty());
        // A stdio server is NOT started: its cwd is a conversation's checkout, so
        // connecting one at boot would spawn a process for a conversation that may
        // never happen, in a directory that is not its own.
        assert_eq!(connects.load(Ordering::SeqCst), 1);
        assert_eq!(
            mgr.statuses(Some(SHARED_SCOPE))
                .iter()
                .map(|c| c.server.clone())
                .collect::<Vec<_>>(),
            vec!["remote".to_string()]
        );

        // Idempotent: a second pass reuses the live connection rather than reopening.
        reconcile_mcp(&deps).await;
        assert_eq!(connects.load(Ordering::SeqCst), 1);
        mgr.drop_all().await;
    }

    #[tokio::test]
    async fn a_revoked_server_is_disconnected_not_left_serving() {
        // A permission that outlives its revocation is the one thing this layer must
        // never produce: the grant is gone from the file, so the connection has to go.
        let file = tmp_registry();
        let opts = McpConfigOptions::with_file(&file);
        save_registry(
            &json!({ "servers": { "remote": { "url": "https://a.example/mcp" } } }),
            &opts,
        )
        .unwrap();
        grant(&file, "remote", true);
        let mgr = Arc::new(McpManager::new(McpManagerOptions {
            config: Some(opts.clone()),
            connect: Some(counting_connector(Arc::new(AtomicUsize::new(0)))),
            ..Default::default()
        }));
        let deps = ServiceDeps {
            manager: Some(mgr.clone()),
            config: Some(opts.clone()),
            workspace: Some("/tmp".into()),
        };

        reconcile_mcp(&deps).await;
        assert_eq!(mgr.statuses(Some(SHARED_SCOPE)).len(), 1);

        grant(&file, "remote", false);
        let r = reconcile_mcp(&deps).await;
        assert_eq!(r.closed, vec!["remote".to_string()]);
        assert!(r.connected.is_empty());
        assert_eq!(mgr.statuses(Some(SHARED_SCOPE)).len(), 0);
        mgr.drop_all().await;
    }

    #[tokio::test]
    async fn a_server_that_will_not_connect_is_a_reported_failure_never_a_throw() {
        // Boot must not depend on a third party being up: the reason belongs on a row
        // in the panel, which is where someone can act on it.
        let file = tmp_registry();
        let opts = McpConfigOptions::with_file(&file);
        save_registry(
            &json!({ "servers": { "remote": { "url": "https://a.example/mcp" } } }),
            &opts,
        )
        .unwrap();
        grant(&file, "remote", true);
        let mgr = Arc::new(McpManager::new(McpManagerOptions {
            config: Some(opts.clone()),
            connect: Some(refusing_connector("connection refused")),
            ..Default::default()
        }));
        let r = reconcile_mcp(&ServiceDeps {
            manager: Some(mgr.clone()),
            config: Some(opts),
            workspace: Some("/tmp".into()),
        })
        .await;
        assert!(r.connected.is_empty());
        assert_eq!(r.failed.len(), 1);
        assert!(
            r.failed[0].1.contains("connection refused"),
            "{:?}",
            r.failed
        );
        let summary = reconcile_summary(&r).unwrap_or_default();
        assert!(
            summary.contains("remote failed (") && summary.contains("connection refused"),
            "{summary}"
        );
        mgr.drop_all().await;
    }

    #[tokio::test]
    async fn nothing_granted_is_a_quiet_no_op_and_says_nothing_in_the_boot_log() {
        let file = tmp_registry();
        let opts = McpConfigOptions::with_file(&file);
        save_registry(
            &json!({ "servers": { "remote": { "url": "https://a.example/mcp" } } }),
            &opts,
        )
        .unwrap();
        let mgr = Arc::new(McpManager::new(McpManagerOptions {
            config: Some(opts.clone()),
            connect: Some(refusing_connector("x")),
            ..Default::default()
        }));
        let r = reconcile_mcp(&ServiceDeps {
            manager: Some(mgr),
            config: Some(opts),
            workspace: Some("/tmp".into()),
        })
        .await;
        assert_eq!(r, ReconcileResult::default());
        assert_eq!(reconcile_summary(&r), None);
    }
}
