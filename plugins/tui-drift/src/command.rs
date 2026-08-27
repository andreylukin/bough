//! Invariant (D-C10): this row registers `/driftboard`, NOT `/drift`. `drift-watch` already owns
//! `/drift`, and a pane does not shadow a registered command. The dashboard's reset is
//! `drift-watch`'s `/reset`, reached through `PaneOutcome::Command` (D-C3).

use std::sync::Arc;

use bough_kernel::{Context, PluginError};
use bough_plugin_commands::{
    positional, Command, CommandCx, CommandError, CommandName, CommandOutput, CommandScope,
    CommandSpec, Commands, Invocation, OutputRender,
};
use bough_plugin_tui_shell::pane::PaneId;
use bough_plugin_tui_shell::{Tui, TuiHandle};

/// The command this row registers: `/driftboard [agent]`.
pub const NAME: &str = "driftboard";

/// The plain-language summary `/help` lists it under (phase ux1 §2.8).
pub const SUMMARY: &str = "show how steady every agent has been lately";

/// Register `/driftboard`, if a `commands` registry is bound.
///
/// ABSENT is headless: the row works with no command surface. An ERROR is the kernel refusing the
/// read and is a boot failure, never a row that silently registered nothing (§0.2).
pub async fn register(ctx: &Context) -> Result<(), PluginError> {
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
                usage: "/driftboard [agent]".to_string(),
                args: positional(&["agent"], 0),
                scope: CommandScope::Global,
                run: Arc::new(DriftBoardCommand {
                    tui: (*tui).clone(),
                }),
            },
        )
        .await?;
    Ok(())
}

/// `/driftboard [agent]` — focus the pane. It computes nothing: the pane's own refresh tick is
/// what fills it, and a command that recomputed would be a second poll path.
struct DriftBoardCommand {
    tui: TuiHandle,
}

#[async_trait::async_trait]
impl Command for DriftBoardCommand {
    async fn run(&self, inv: Invocation, _cx: CommandCx) -> Result<CommandOutput, CommandError> {
        self.tui.focus_pane(PaneId::new(crate::PANE_ID)).await;
        let text = match inv.args.first() {
            // The argument SELECTS, it does not filter: the dashboard is cross-agent by
            // definition, and hiding the other rows would answer a different question.
            Some(agent) => format!("drift dashboard \u{b7} {agent} selected"),
            None => "drift dashboard".to_string(),
        };
        Ok(CommandOutput {
            text,
            render: OutputRender::Plain,
            cites: Vec::new(),
        })
    }
}
