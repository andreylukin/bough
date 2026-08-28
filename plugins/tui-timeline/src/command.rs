//! Invariant: the command FOCUSES the pane and sets its filter; it registers no second way to read
//! the ledger (D-C3 — the pane owns no write path and no query of its own). Everything `/timeline`
//! can do, a person can do by typing in the pane's own field, which is why the command needs no
//! error path of its own beyond the parse.

use std::sync::Arc;

use bough_kernel::{Context, PluginError};
use bough_plugin_commands::{
    positional_rest, Command, CommandCx, CommandError, CommandName, CommandOutput, CommandScope,
    CommandSpec, Commands, Invocation, OutputRender,
};
use bough_plugin_tui_shell::pane::PaneId;
use bough_plugin_tui_shell::{Tui, TuiHandle};

use crate::filter::{parse_filter, render_filter};
use crate::pane::TimelinePane;

/// The command this row registers: `/timeline [filter…]`.
pub const NAME: &str = "timeline";

/// The plain-language summary `/help` lists it under (phase ux1 §2.8).
pub const SUMMARY: &str = "show what every agent did, newest last, with filters";

/// Register `/timeline`, if a `commands` registry is bound.
///
/// ABSENT is headless: the row works with no command surface. An ERROR is the kernel refusing the
/// read and is a boot failure, never a row that silently registered nothing (§0.2).
pub async fn register(ctx: &Context, pane: Arc<TimelinePane>) -> Result<(), PluginError> {
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
                usage: "/timeline [filter…]".to_string(),
                // Every word is one filter term, and there may be any number of them.
                args: positional_rest(&["filter"], 0),
                scope: CommandScope::Global,
                run: Arc::new(TimelineCommand {
                    pane,
                    tui: (*tui).clone(),
                }),
            },
        )
        .await?;
    Ok(())
}

struct TimelineCommand {
    pane: Arc<TimelinePane>,
    tui: TuiHandle,
}

#[async_trait::async_trait]
impl Command for TimelineCommand {
    async fn run(&self, inv: Invocation, cx: CommandCx) -> Result<CommandOutput, CommandError> {
        let text = inv.args.join(" ");
        // The parse happens BEFORE the pane is focused: `/timeline wombat:7` names the bad word
        // where the person typed it rather than opening a pane with an error in its header.
        let filter = parse_filter(&text, cx.at).map_err(|e| CommandError::BadArgs {
            usage: "/timeline [filter…]".to_string(),
            detail: e.to_string(),
        })?;
        let described = filter.describe(cx.at);
        self.pane.state.lock().set_filter(filter.clone(), cx.at);
        self.tui.focus_pane(PaneId::new(crate::PANE_ID)).await;
        self.pane.clone().reload(self.tui.clone());
        Ok(CommandOutput {
            text: render(&described, &render_filter(&filter, cx.at)),
            render: OutputRender::Plain,
            cites: Vec::new(),
        })
    }
}

/// PURE: what `/timeline` says it did.
pub fn render(described: &str, spelled: &str) -> String {
    if spelled.is_empty() {
        return "timeline: showing every agent, newest last".to_string();
    }
    format!("timeline: {described}")
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
    fn an_empty_filter_says_it_is_showing_everything() {
        assert!(render("everything", "").contains("every agent"));
        assert_eq!(render("agent:sol", "agent:sol"), "timeline: agent:sol");
    }
}
