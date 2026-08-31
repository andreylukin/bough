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
                summary: SUMMARY_OLDFEED.to_string(),
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
                summary: SUMMARY_PRIME.to_string(),
                usage: "/prime [text]".to_string(),
                args: schemars::json_schema!({ "type": "array", "items": { "type": "string" } }),
                scope: CommandScope::Global,
                run: Arc::new(PrimeCommand { feed: feed.clone() }),
            },
        )
        .await?;
    Ok(())
}

/// The plain-language summaries these two commands are listed under (phase ux1 §2.8, M16).
pub const SUMMARY_OLDFEED: &str = "show what the old bough feed last imported";
pub const SUMMARY_PRIME: &str = "load past shell history for a topic into the agent's context";

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
    out.push_str(
        "this is context for you to read: past commands are never delivered to an agent as a \
         message\n",
    );
    out
}

struct OldFeedCommand {
    feed: OldFeedHandle,
}

#[async_trait::async_trait]
impl Command for OldFeedCommand {
    async fn run(&self, _inv: Invocation, _cx: CommandCx) -> Result<CommandOutput, CommandError> {
        // M27: a bridge with no live source rendered `last sweep: never` and looked like a
        // working command that found nothing. It is OFF, and the reason is a missing FILE the
        // reader can go and look at, so it is an error with that path in it.
        if let Some(why) = off_reason(self.feed.0.enabled.is_empty(), &self.feed.0.cfg.jungler_db) {
            return Err(CommandError::Failed(why));
        }
        Ok(CommandOutput {
            text: render(&self.feed.status()),
            render: OutputRender::KeyValue,
            cites: Vec::new(),
        })
    }
}

/// PURE: why `/oldfeed` has nothing to show, when it has nothing to show. `None` means at least
/// one source is live and the command renders the sweep instead.
///
/// The PATH is the whole point: "the bridge is off" is not actionable and "no jungler.db at
/// ~/.jungler/jungler.db" is.
pub fn off_reason(no_live_source: bool, jungler_db: &std::path::Path) -> Option<String> {
    if !no_live_source {
        return None;
    }
    Some(format!(
        "the old-feed bridge is off (no jungler.db at {})",
        jungler_db.display()
    ))
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

    /// M16: no summary this row registers may use the tree's internal vocabulary.
    #[test]
    fn every_summary_is_plain_language() {
        for s in [SUMMARY_OLDFEED, SUMMARY_PRIME] {
            assert_eq!(bough_plugin_commands::palette::house_word(s), None, "{s}");
        }
    }

    /// M27: no `jungler.db` is a stated reason naming the file, not an empty answer.
    #[test]
    fn an_off_bridge_names_the_file_it_wanted() {
        let why = off_reason(true, std::path::Path::new("/home/a/.jungler/jungler.db"))
            .expect("no live source is a reason");
        assert!(why.contains("jungler.db"), "{why}");
        assert!(why.contains("/home/a/.jungler/jungler.db"), "{why}");
        // A live source renders the sweep instead.
        assert_eq!(
            off_reason(false, std::path::Path::new("/x/jungler.db")),
            None
        );
    }

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
