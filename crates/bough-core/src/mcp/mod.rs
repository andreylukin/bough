//! MCP (port of `src/mcp/`). The no-hang contract: every wait is bounded; a
//! 401 surfaces as "not authorized — open the mcp panel (^p) and press a",
//! never a hang. Grants are never cached (re-read per call); a spawner's Live
//! grant becomes Inherited at spawn.

pub mod catalog;
pub mod client;
pub mod config;
pub mod keychain;
pub mod manager;
pub mod oauth;
pub mod remote;
pub mod service;
pub mod status;
