//! Invariant: the command FOCUSES the pane and takes a fresh snapshot; it never registers a second
//! way to assemble a projection (D-C3 — the pane owns no write path and no second implementation).

use std::sync::Arc;

use bough_kernel::{Context, PluginError};
use bough_plugin_commands::{
    positional, Command, CommandCx, CommandError, CommandName, CommandOutput, CommandScope,
    CommandSpec, Commands, Invocation, OutputRender,
};
use bough_plugin_ledger::AgentName;
use bough_plugin_tui_shell::pane::PaneId;
use bough_plugin_tui_shell::{Tui, TuiHandle};

use crate::pane::PreviewPane;

/// The command this row registers: `/preview [agent]`.
pub const NAME: &str = "preview";

/// The plain-language summary `/help` lists it under (phase ux1 §2.8).
///
/// It says "if it ran right now" and not the tree's own word for that, because the palette refuses
/// a house word in a summary and a reader outside this codebase does not have one either.
pub const SUMMARY: &str = "show the exact text this agent would be given if it ran right now";

/// Register `/preview`, if a `commands` registry is bound.
///
/// ABSENT is headless: the row works with no command surface. An ERROR is the kernel refusing the
/// read and is a boot failure, never a row that silently registered nothing (§0.2).
pub async fn register(ctx: &Context, pane: Arc<PreviewPane>) -> Result<(), PluginError> {
    let commands = match ctx.try_get::<Commands>() {
        Ok(Some(c)) => c,
        Ok(None) => return Ok(()),
        Err(e) => return Err(PluginError::new(ctx.entry_id().clone(), e)),
    };
    let tui = ctx
        .get::<Tui>()
        .map_err(|e| PluginError::new(ctx.entry_id().clone(), e))?;
    commands
        .register(
            ctx,
            CommandSpec {
                name: CommandName::new(NAME),
                summary: SUMMARY.to_string(),
                usage: "/preview [agent]".to_string(),
                args: positional(&["agent"], 0),
                scope: CommandScope::Global,
                run: Arc::new(PreviewCommand {
                    pane,
                    tui: (*tui).clone(),
                }),
            },
        )
        .await?;
    Ok(())
}

struct PreviewCommand {
    pane: Arc<PreviewPane>,
    tui: TuiHandle,
}

#[async_trait::async_trait]
impl Command for PreviewCommand {
    async fn run(&self, inv: Invocation, _cx: CommandCx) -> Result<CommandOutput, CommandError> {
        // No argument means the agent the screen is already about: the pane is a lens on the
        // focused agent, and inventing a default agent here would answer a different question.
        let agent = match inv.args.first() {
            Some(name) => Some(AgentName::new(name.as_str())),
            None => self.tui.agent().map(|a| a.name().clone()),
        };
        self.tui.focus_pane(PaneId::new(crate::PANE_ID)).await;
        if let Some(agent) = agent.clone() {
            self.pane.clone().refresh(self.tui.clone(), agent);
        }
        Ok(CommandOutput {
            text: render(agent.as_ref().map(|a| a.to_string()).as_deref()),
            render: OutputRender::Plain,
            cites: Vec::new(),
        })
    }
}

/// PURE: what `/preview` says it did.
pub fn render(agent: Option<&str>) -> String {
    match agent {
        Some(a) => format!("preview: {a}"),
        None => "preview: no agent is in focus".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// M16: no summary this row registers may use the tree's internal vocabulary.
    #[test]
    fn the_summary_is_plain_language() {
        assert_eq!(bough_plugin_commands::palette::house_word(SUMMARY), None);
    }

    #[test]
    fn with_no_agent_in_focus_the_command_says_so() {
        assert!(render(None).contains("no agent"));
        assert_eq!(render(Some("sol")), "preview: sol");
    }
}
