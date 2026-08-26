//! Invariant: RESTARTING IS INDEPENDENT. One process crashing never touches another child entry or
//! the parent. While a process is down its client answers `is_ready() == false`, so its tools STAY
//! REGISTERED and a call fails with `McpError::Unavailable` instead of the tool vanishing mid-wake.
//!
//! A JSON-RPC NOTIFICATION named `bough/actions` whose params are `{ actions: [RuntimeAction] }` is
//! journaled through `runtime_actions::execute_all` — §9's "actions they emit THROUGH the plugin
//! API are code-enforced and journaled like ward actions".
//!
//! What a process does DIRECTLY, as a process running as Andrey, is trusted config and outside the
//! boundary's scope. §9 flags this, and so does this comment.

pub mod invariant;
pub mod jsonrpc;
pub mod process;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use bough_kernel::{ConfigError, Context, InvariantSpec, Plugin, PluginError};
use bough_plugin_runtime_actions::RuntimeLimits;

/// The catalog name of the parent row.
pub const PLUGIN_NAME: &str = "mcp-subprocess";
/// The catalog name of the per-process CHILD row.
pub const PROCESS_PLUGIN_NAME: &str = "mcp-process";

/// The JSON-RPC notification a resident plugin emits actions through.
pub const ACTIONS_NOTIFICATION: &str = "bough/actions";

/// The parent row's config.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct McpSubprocessConfig {
    pub processes: Vec<ProcessRow>,
    pub limits: RuntimeLimits,
}

/// One resident process.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProcessRow {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub cwd: Option<PathBuf>,
    /// Restart policy. Backoff is jittered (backon), capped, and a process that dies faster than
    /// `min_uptime_ms` `max_restarts` times in a row is QUARANTINED and reported.
    pub max_restarts: u32,
    pub min_uptime_ms: u64,
    pub restart_delay_ms: u64,
}

/// PURE: what makes one [`ProcessRow`] impossible. Shared by the parent and the child so a row
/// cannot pass one and fail the other.
pub fn validate_row(row: &ProcessRow) -> Result<(), ConfigError> {
    let bad = |detail: String| ConfigError::Rejected { detail };
    if row.name.trim().is_empty() {
        return Err(bad("name: a resident process needs a name".into()));
    }
    if row.command.trim().is_empty() {
        return Err(bad(format!("`{}`: command is empty", row.name)));
    }
    if row.max_restarts == 0 {
        return Err(bad(format!(
            "`{}`: max_restarts must be greater than zero; a process that may never restart is a              process that disappears on its first crash",
            row.name
        )));
    }
    if row.min_uptime_ms == 0 {
        return Err(bad(format!(
            "`{}`: min_uptime_ms must be greater than zero, or no death is ever `too fast` and a              crash loop never quarantines",
            row.name
        )));
    }
    if row.restart_delay_ms == 0 {
        return Err(bad(format!(
            "`{}`: restart_delay_ms must be greater than zero; a zero backoff is a spin",
            row.name
        )));
    }
    Ok(())
}

/// PURE: the child entry one process row mounts as. `id` is `<parent>.<name>`.
pub fn child_entry(parent: &str, row: &ProcessRow, limits: &RuntimeLimits) -> bough_kernel::Entry {
    bough_kernel::Entry {
        id: bough_kernel::EntryId::new(format!("{parent}.{}", row.name)),
        plugin: Some(PROCESS_PLUGIN_NAME.to_string()),
        config: serde_yaml::to_value(process::McpProcessConfig {
            row: row.clone(),
            limits: limits.clone(),
        })
        .expect("McpProcessConfig serializes"),
        disabled: Default::default(),
        isolate: Default::default(),
        inject: Default::default(),
        group: Vec::new(),
        include: None,
    }
}

/// The [`RuntimeCx`] a resident process's `bough/actions` notification is executed through.
pub fn runtime_cx(
    ctx: &Context,
    entry: &bough_kernel::EntryId,
    name: &str,
) -> Result<bough_plugin_runtime_actions::RuntimeCx, PluginError> {
    use bough_plugin_runtime_actions::{RuntimeCx, RuntimeSource, Trigger};
    let e = |err: bough_kernel::KernelError| PluginError::new(entry.clone(), err);
    let source = RuntimeSource::Process(name.to_string());
    Ok(RuntimeCx {
        ctx: ctx.clone(),
        agents: bough_plugin_agents::AgentsHandle(
            ctx.get::<bough_plugin_agents::Agents>()
                .map_err(e)?
                .0
                .clone(),
        ),
        ledger: bough_plugin_ledger::LedgerHandle(
            ctx.get::<bough_plugin_ledger::Ledger>()
                .map_err(e)?
                .0
                .clone(),
        ),
        workers: bough_plugin_workers::WorkersHandle(
            ctx.get::<bough_plugin_workers::Workers>()
                .map_err(e)?
                .0
                .clone(),
        ),
        actions: bough_plugin_actions::ActionsHandle(
            ctx.get::<bough_plugin_actions::Actions>()
                .map_err(e)?
                .0
                .clone(),
        ),
        schedule: bough_plugin_schedule::ScheduleHandle(
            ctx.get::<bough_plugin_schedule::Schedule>()
                .map_err(e)?
                .0
                .clone(),
        ),
        trigger: Trigger::synthetic(&source),
        source,
        at: chrono::Utc::now(),
    })
}

/// The parent row.
pub struct McpSubprocessPlugin;

#[async_trait::async_trait]
impl Plugin for McpSubprocessPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = McpSubprocessConfig;

    fn inject() -> bough_kernel::Inject {
        bough_kernel::Inject::required([
            "mcp", "ledger", "agents", "actions", "workers", "schedule",
        ])
    }

    fn validate(cfg: &Self::Config) -> Result<(), ConfigError> {
        let mut seen: Vec<&str> = Vec::new();
        for row in &cfg.processes {
            validate_row(row)?;
            if seen.contains(&row.name.as_str()) {
                return Err(ConfigError::Rejected {
                    detail: format!(
                        "two processes named `{}`; a server name is its identity on `ctx.mcp`",
                        row.name
                    ),
                });
            }
            seen.push(&row.name);
        }
        Ok(())
    }

    /// ONE CHILD ENTRY PER PROCESS. Children are effects of the parent, so unloading the parent
    /// cascades — and one child failing never touches its siblings.
    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let entry = ctx.entry_id().clone();
        for row in &cfg.processes {
            ctx.mount(child_entry(entry.as_str(), row, &cfg.limits))
                .await
                .map_err(|e| PluginError::new(entry.clone(), e))?;
        }
        Ok(())
    }

    /// The parent holds no runtime invariant of its own: what must hold is per PROCESS, and the
    /// child row carries it.
    fn invariants() -> Vec<InvariantSpec> {
        Vec::new()
    }
}

bough_kernel::register_plugin!(McpSubprocessPlugin);
bough_kernel::register_plugin!(process::McpProcessPlugin);
