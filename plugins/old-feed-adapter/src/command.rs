//! Invariant: `/oldfeed` REPORTS and never sweeps on demand. A human command dispatches without a
//! model turn and appends no step (P3-D8), so the one thing this surface may do is render the last
//! sweep — the bridge's own state, not the ledger's.

use std::sync::Arc;

use bough_kernel::{Context, PluginError};
use bough_plugin_commands::{
    Command, CommandCx, CommandError, CommandName, CommandOutput, CommandScope, CommandSpec,
    Commands, Invocation, OutputRender,
};

use crate::{CommandMemory, FeedStatus, NoteEvidence, NoteQuery, OldFeedHandle, PrimingQuery};

/// Register `/oldfeed`, if a `commands` registry is bound. The key is OPTIONAL injection: a
/// headless profile mounts this row with no surface at all and still sweeps.
pub async fn register(ctx: &Context, feed: &OldFeedHandle) -> Result<(), PluginError> {
    let Ok(Some(commands)) = ctx.try_get::<Commands>() else {
        return Ok(());
    };
    commands
        .register(
            ctx,
            CommandSpec {
                name: CommandName::new("oldfeed"),
                summary: "what the old-feed bridge last swept".to_string(),
                usage: "/oldfeed".to_string(),
                args: schemars::json_schema!({ "type": "object", "properties": {} }),
                scope: CommandScope::Global,
                run: Arc::new(OldFeedCommand { feed: feed.clone() }),
            },
        )
        .await?;

    // §14's cheap win, made REACHABLE. `prime` and `notes` were a seam with one role: the methods
    // existed, the tests called them, and nothing in the tree consumed the `old_feed` key, so the
    // priming half of §17 Phase 3 ("command_history … queried for priming") had no runtime path
    // at all. This is that path — a human command, rendered in the pane, never mail and never a
    // step, which is exactly what "competence memory, never delivered" means.
    commands
        .register(
            ctx,
            CommandSpec {
                name: CommandName::new("prime"),
                summary: "command memory and note evidence from the old bough db".to_string(),
                usage: "/prime [text]".to_string(),
                args: schemars::json_schema!({ "type": "array", "items": { "type": "string" } }),
                scope: CommandScope::Global,
                run: Arc::new(PrimeCommand { feed: feed.clone() }),
            },
        )
        .await?;
    Ok(())
}

struct PrimeCommand {
    feed: OldFeedHandle,
}

#[async_trait::async_trait]
impl Command for PrimeCommand {
    async fn run(&self, inv: Invocation, _cx: CommandCx) -> Result<CommandOutput, CommandError> {
        let contains = if inv.args.is_empty() {
            None
        } else {
            Some(inv.args.join(" "))
        };
        // `limit: 0` is the caller saying "the row decides" — resolved once, in `resolve.rs`.
        let cmds = self
            .feed
            .prime(&PrimingQuery {
                repo: None,
                tags: Vec::new(),
                contains: contains.clone(),
                limit: 0,
            })
            .await
            .map_err(|e| CommandError::Failed(e.to_string()))?;
        let notes = self
            .feed
            .notes(&NoteQuery { contains, limit: 0 })
            .await
            .map_err(|e| CommandError::Failed(e.to_string()))?;
        let cites = notes.iter().map(|n| n.cite.clone()).collect();
        Ok(CommandOutput {
            text: render_priming(&cmds, &notes),
            render: OutputRender::Plain,
            cites,
        })
    }
}

/// PURE: what `/prime` shows. Command memory is COMPETENCE MEMORY: it is rendered for Andrey and
/// never becomes a step, a message or a projection section (§17 Phase 3, and the row's own
/// `invariant::check` enforces the negative half).
pub fn render_priming(cmds: &[CommandMemory], notes: &[NoteEvidence]) -> String {
    let mut out = String::new();
    if cmds.is_empty() && notes.is_empty() {
        out.push_str("no command memory and no notes in the old bough db\n");
        return out;
    }
    if !cmds.is_empty() {
        out.push_str(&format!("command memory ({}):\n", cmds.len()));
        for c in cmds {
            let tags = if c.tags.is_empty() {
                String::new()
            } else {
                format!("  [{}]", c.tags.join(" "))
            };
            out.push_str(&format!("  {} · {}{}\n", c.repo, c.cmd, tags));
        }
    }
    if !notes.is_empty() {
        out.push_str(&format!("notes ({}):\n", notes.len()));
        for n in notes {
            out.push_str(&format!("  {} · {}\n", n.cite.r#ref, n.heading));
        }
    }
    out.push_str("not mail: command history is competence memory and is never delivered\n");
    out
}

struct OldFeedCommand {
    feed: OldFeedHandle,
}

#[async_trait::async_trait]
impl Command for OldFeedCommand {
    async fn run(&self, _inv: Invocation, _cx: CommandCx) -> Result<CommandOutput, CommandError> {
        Ok(CommandOutput {
            text: render(&self.feed.status()),
            render: OutputRender::KeyValue,
            cites: Vec::new(),
        })
    }
}

/// PURE: the last sweep as `/oldfeed` shows it.
pub fn render(status: &FeedStatus) -> String {
    let mut out = String::new();
    match status.last_sweep {
        Some(at) => out.push_str(&format!("last sweep: {}\n", at.to_rfc3339())),
        None => out.push_str("last sweep: never\n"),
    }
    if status.sources.is_empty() {
        out.push_str("sources: none live\n");
    }
    for (source, delivered, mark) in &status.sources {
        out.push_str(&format!("{source}: {delivered} new, watermark {mark}\n"));
    }
    for (source, why) in &status.disabled {
        out.push_str(&format!("{source}: disabled — {why}\n"));
    }
    out.push_str("retires in Phase 6 (`disabled: true` is the off switch)\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_never_swept_bridge_says_so() {
        let text = render(&FeedStatus::default());
        assert!(text.contains("last sweep: never"));
        assert!(text.contains("sources: none live"));
    }

    #[test]
    fn each_source_gets_its_count_and_its_watermark() {
        let text = render(&FeedStatus {
            sources: vec![("jungler.events".to_string(), 3, 12)],
            disabled: vec![("jungler.nodes".to_string(), "no `nodes` table".to_string())],
            last_sweep: None,
        });
        assert!(text.contains("jungler.events: 3 new, watermark 12"));
        assert!(text.contains("jungler.nodes: disabled — no `nodes` table"));
    }
}
