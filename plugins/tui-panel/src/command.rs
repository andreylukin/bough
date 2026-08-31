//! Invariant: a command OPENS the panel on its tab and focuses it; it never grows a second way
//! to read the tree or write the ui layer (the pane's `refresh` and `toggle` stay the only two).

use std::sync::Arc;

use bough_kernel::{Context, PluginError};
use bough_plugin_commands::{
    no_args, Command, CommandCx, CommandError, CommandName, CommandOutput, CommandScope,
    CommandSpec, Commands, Invocation, OutputRender,
};
use bough_plugin_tui_shell::pane::PaneId;
use bough_plugin_tui_shell::{Tui, TuiHandle};

use crate::pane::PanelPane;
use crate::state::Tab;

/// The three commands, one per tab. The tab table is the one authority on what exists; a slash
/// name and its tab cannot disagree because both come from the same row here.
pub const COMMANDS: [(&str, Tab, &str); 3] = [
    (
        "config",
        Tab::Config,
        "show every configured row, who set it, and switch rows on or off",
    ),
    (
        "connectors",
        Tab::Connectors,
        "show the MCP servers and collectors this bough is wired to",
    ),
    (
        "model",
        Tab::Model,
        "show which model each agent runs on and what the last request used",
    ),
];

/// Register `/config`, `/connectors`, `/model`, if a command registry is bound.
///
/// ABSENT is headless: the row works with no command surface. An ERROR is the kernel refusing
/// the read and is a boot failure, never a row that silently registered nothing (§0.2).
pub async fn register(ctx: &Context, pane: Arc<PanelPane>) -> Result<(), PluginError> {
    let commands = match ctx.try_get::<Commands>() {
        Ok(Some(c)) => c,
        Ok(None) => return Ok(()),
        Err(e) => return Err(PluginError::new(ctx.entry_id().clone(), e)),
    };
    let tui = ctx
        .get::<Tui>()
        .map_err(|e| PluginError::new(ctx.entry_id().clone(), e))?;
    for (name, tab, summary) in COMMANDS {
        commands
            .register(
                ctx,
                CommandSpec {
                    name: CommandName::new(name),
                    summary: summary.to_string(),
                    usage: format!("/{name}"),
                    args: no_args(),
                    scope: CommandScope::Global,
                    run: Arc::new(PanelCommand {
                        tab,
                        pane: Arc::clone(&pane),
                        tui: (*tui).clone(),
                    }),
                },
            )
            .await?;
    }
    Ok(())
}

struct PanelCommand {
    tab: Tab,
    pane: Arc<PanelPane>,
    tui: TuiHandle,
}

#[async_trait::async_trait]
impl Command for PanelCommand {
    async fn run(&self, _inv: Invocation, _cx: CommandCx) -> Result<CommandOutput, CommandError> {
        self.pane.open(self.tui.clone(), self.tab);
        self.tui.focus_pane(PaneId::new(crate::PANE_ID)).await;
        Ok(CommandOutput {
            text: render(self.tab),
            render: OutputRender::Plain,
            cites: Vec::new(),
        })
    }
}

/// PURE: what the command says it did.
pub fn render(tab: Tab) -> String {
    format!("panel: {}", tab.title())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// M16: no summary this row registers may use the tree's internal vocabulary.
    #[test]
    fn every_summary_is_plain_language() {
        for (name, _, summary) in COMMANDS {
            assert_eq!(
                bough_plugin_commands::palette::house_word(summary),
                None,
                "/{name}"
            );
        }
    }

    #[test]
    fn the_command_names_the_tab_it_opened() {
        assert_eq!(render(Tab::Connectors), "panel: connectors");
    }
}
