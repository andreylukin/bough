//! bough-server — axum HTTP + SSE. The ONLY crate that speaks HTTP-server.
//!
//! The parity anchor (architecture.md §0): same routes, same JSON field names,
//! same status codes (202 for postMessage, 201 for creates), same SSE framing
//! (`event:` + single `data:` line, no `id:` field, `: connected` / `: ping`
//! comments) as the TS server. `specs/server.md` §3 IS the API contract.
//! Loopback only (`127.0.0.1:$BOUGH_PORT`, default 4321), no CORS ever, no
//! read/idle timeout middleware (SSE idles between turns).

pub mod app;
pub mod artifact_lib;
pub mod artifacts;
pub mod attachments;
pub mod boot;
pub mod changes;
pub mod comments;
pub mod defaults;
pub mod events;
pub mod fs;
pub mod ghost;
pub mod history_ops;
pub mod hooks;
pub mod http;
pub mod jobs;
pub mod mcp_oauth;
pub mod mcp_routes;
pub mod models;
pub mod plugins;
pub mod questions;
pub mod schedules;
pub mod search;
pub mod sessions;
pub mod skills;
pub mod theme;
pub mod turns;
pub mod workflows;

/// Serializes the tests that swap the PROCESS-GLOBAL MCP manager.
///
/// `set_mcp_manager` replaces one static for the whole test binary, and both
/// `boot` and `mcp_routes` install a hermetic manager of their own and restore
/// the previous one at the end. Run in parallel — which is cargo's default —
/// they overwrite each other's global, and a test asserting on its own registry
/// reads somebody else's: the observed failure was `boot`'s "another
/// conversation was granted nothing" seeing a grant from an `mcp_routes` test.
/// Hermetic config files are not enough when the manager itself is shared.
///
/// Every test that calls `set_mcp_manager` takes this first and holds it until
/// it has restored the previous manager. Async so a `#[tokio::test]` can await
/// it, and unpoisoned so one failing test does not cascade into the rest.
#[cfg(test)]
pub(crate) static MCP_MANAGER_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
