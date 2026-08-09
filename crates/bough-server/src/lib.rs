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
pub mod questions;
pub mod schedules;
pub mod search;
pub mod sessions;
pub mod skills;
pub mod theme;
pub mod turns;
pub mod workflows;
