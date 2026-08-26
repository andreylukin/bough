//! Invariant: the four built-ins are ordinary rows on `ctx.commands`, registered as EFFECTS — the
//! shell has no private command path. Unloading the `tui` row removes them, and nothing they do is
//! model-visible: they render locally and append no step (P3-D8).

use std::sync::Arc;

use bough_kernel::{Context, PluginError};
use bough_plugin_commands::{
    Command, CommandCx, CommandError, CommandName, CommandOutput, CommandScope, CommandSpec,
    Commands, Invocation, OutputRender,
};
use bough_plugin_ledger::AgentName;

use crate::TuiHandle;

/// Register `/help`, `/quit`, `/focus` and `/agents`. Each is its own effect, so a later phase can
/// replace one by registering a scoped twin rather than by editing this file.
pub async fn register(ctx: &Context, tui: &TuiHandle) -> Result<(), PluginError> {
    let commands = ctx
        .get::<Commands>()
        .map_err(|e| PluginError::new(ctx.entry_id().clone(), e))?;

    for spec in specs(tui) {
        commands.register(ctx, spec).await?;
    }
    Ok(())
}

/// The four specs. Separated from registration so a test can read them without a registry.
pub fn specs(tui: &TuiHandle) -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            name: CommandName::new("help"),
            summary: "the commands and key hints this surface has".to_string(),
            usage: "/help".to_string(),
            args: no_args(),
            scope: CommandScope::Global,
            run: Arc::new(Help(tui.clone())),
        },
        CommandSpec {
            name: CommandName::new("quit"),
            summary: "tear the tree down and leave".to_string(),
            usage: "/quit".to_string(),
            args: no_args(),
            scope: CommandScope::Global,
            run: Arc::new(Quit(tui.clone())),
        },
        CommandSpec {
            name: CommandName::new("focus"),
            summary: "show one agent in the main pane".to_string(),
            usage: "/focus <agent>".to_string(),
            args: one_string("agent", "the agent's name"),
            scope: CommandScope::Global,
            run: Arc::new(Focus(tui.clone())),
        },
        CommandSpec {
            name: CommandName::new("agents"),
            summary: "the roster: status, trajectory, unconsumed mail".to_string(),
            usage: "/agents".to_string(),
            args: no_args(),
            scope: CommandScope::Global,
            run: Arc::new(Roster(tui.clone())),
        },
    ]
}

/// A command that takes nothing.
fn no_args() -> schemars::Schema {
    schemars::json_schema!({
        "type": "object",
        "properties": {},
        "additionalProperties": false
    })
}

/// A command that takes one required string.
fn one_string(name: &str, description: &str) -> schemars::Schema {
    schemars::json_schema!({
        "type": "object",
        "properties": { name: { "type": "string", "description": description } },
        "required": [name],
        "additionalProperties": false
    })
}

struct Help(TuiHandle);

#[async_trait::async_trait]
impl Command for Help {
    async fn run(&self, _inv: Invocation, cx: CommandCx) -> Result<CommandOutput, CommandError> {
        let mut lines = Vec::new();
        // The shell's OWN registry handle, not a fresh `ctx.get`: `/help` lists what THIS surface
        // dispatches through, and a command must not depend on being run from a row context.
        if let Some(commands) = self.0.commands() {
            let scope = cx.agent.as_ref().map(|a| a.name().clone());
            for info in commands.list(scope.as_ref()) {
                lines.push(format!("{:<16} {}", info.usage, info.summary));
            }
        }
        for (keys, what) in keymap_hints() {
            lines.push(format!("{keys:<16} {what}"));
        }
        for pane in self.0.entries() {
            for (keys, what) in pane.pane.key_hints() {
                lines.push(format!("{keys:<16} {what} ({})", pane.info.title));
            }
        }
        Ok(CommandOutput {
            text: lines.join("\n"),
            render: OutputRender::KeyValue,
            cites: Vec::new(),
        })
    }
}

/// The fixed keymap, as `/help` and the status line render it (P3-D18).
pub fn keymap_hints() -> Vec<(&'static str, &'static str)> {
    vec![
        ("Enter", "send, or dispatch a / line"),
        ("Alt+Enter", "newline"),
        ("Esc", "clear the composer"),
        ("Tab", "cycle focus"),
        ("↑ ↓ PgUp PgDn", "scroll the focused pane"),
        ("Ctrl+F", "the search pane"),
        ("Ctrl+C", "cancel the running wake, else quit"),
        ("Ctrl+L", "redraw"),
    ]
}

struct Quit(TuiHandle);

#[async_trait::async_trait]
impl Command for Quit {
    async fn run(&self, _inv: Invocation, _cx: CommandCx) -> Result<CommandOutput, CommandError> {
        self.0.quit(0);
        Ok(CommandOutput {
            text: "leaving".to_string(),
            render: OutputRender::Plain,
            cites: Vec::new(),
        })
    }
}

struct Focus(TuiHandle);

#[async_trait::async_trait]
impl Command for Focus {
    async fn run(&self, inv: Invocation, _cx: CommandCx) -> Result<CommandOutput, CommandError> {
        let Some(want) = inv.args.first() else {
            return Err(CommandError::BadArgs {
                usage: "/focus <agent>".to_string(),
                detail: "no agent named".to_string(),
            });
        };
        let name = AgentName::new(want);
        let Some(agents) = self.0 .0.agents.clone() else {
            return Err(CommandError::Failed("no agent roster".to_string()));
        };
        let Some(agent) = agents.by_name(&name) else {
            return Err(CommandError::Failed(format!("no agent `{want}`")));
        };
        self.0.focus(crate::run::to_agent(agent.id().clone())).await;
        Ok(CommandOutput {
            text: format!("focused {want}"),
            render: OutputRender::Plain,
            cites: Vec::new(),
        })
    }
}

struct Roster(TuiHandle);

#[async_trait::async_trait]
impl Command for Roster {
    async fn run(&self, _inv: Invocation, _cx: CommandCx) -> Result<CommandOutput, CommandError> {
        let Some(agents) = self.0 .0.agents.clone() else {
            return Err(CommandError::Failed("no agent roster".to_string()));
        };
        let mut lines = Vec::new();
        for a in agents.list() {
            lines.push(format!(
                "{:<12} {:<9} {:<20} {} queued",
                a.name(),
                format!("{:?}", a.status()).to_lowercase(),
                a.traj(),
                a.inbox().len()
            ));
        }
        Ok(CommandOutput {
            text: lines.join("\n"),
            render: OutputRender::KeyValue,
            cites: Vec::new(),
        })
    }
}
