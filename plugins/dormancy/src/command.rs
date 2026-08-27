//! Invariant (§16): dormancy is REACHABLE from the surface. `/sleep`, `/resume` and `/paused` are
//! registered only when a `commands` registry is bound — headless binds none and the row activates
//! anyway (the P4-D8 precedent). None of them dispatches a model turn (P3-D8): `/resume` arms a
//! drain, and the loop decides whether there is anything to drain.
//!
//! WP-5 rename: `/wake` and `/dormant` became `/resume` and `/paused`. `wake` is this tree's
//! internal word for one unit of work and the surface does not get to use it (phase ux1 §2.8,
//! M16); the STEP TYPES and the `agent/wake-request` event keep their names, which is exactly the
//! line the vocabulary rule draws.

use std::sync::Arc;

use bough_kernel::{Context, PluginError};
use bough_plugin_commands::{
    positional, positional_rest, Command, CommandCx, CommandError, CommandName, CommandOutput,
    CommandScope, CommandSpec, Commands, Invocation, OutputRender,
};
use bough_plugin_ledger::AgentName;
use bough_plugin_rollups::Attribution;

use crate::{DormancyChange, DormancyHandle, ReactivateCause, SleepRequest, WakeUpRequest};

/// Register `/sleep <agent> [reason]`, `/resume <agent>` and `/paused`, if `commands` is bound.
pub async fn register(ctx: &Context, dormancy: &DormancyHandle) -> Result<(), PluginError> {
    // ABSENT is headless: the row works with no surface at all. An ERROR is the kernel refusing
    // the read and is a boot failure, never a row that silently registered nothing (§0.2).
    let commands = match ctx.try_get::<Commands>() {
        Ok(Some(c)) => c,
        Ok(None) => return Ok(()),
        Err(e) => return Err(PluginError::new(ctx.entry_id().clone(), e)),
    };
    commands
        .register(
            ctx,
            CommandSpec {
                name: CommandName::new("sleep"),
                summary: SUMMARY_SLEEP.to_string(),
                usage: "/sleep <agent> [reason…]".to_string(),
                args: positional_rest(&["agent", "reason"], 1),
                scope: CommandScope::Global,
                run: Arc::new(SleepCommand {
                    dormancy: dormancy.clone(),
                }),
            },
        )
        .await?;
    commands
        .register(
            ctx,
            CommandSpec {
                name: CommandName::new("resume"),
                summary: SUMMARY_RESUME.to_string(),
                usage: "/resume <agent>".to_string(),
                args: positional(&["agent"], 1),
                scope: CommandScope::Global,
                run: Arc::new(WakeCommand {
                    dormancy: dormancy.clone(),
                }),
            },
        )
        .await?;
    commands
        .register(
            ctx,
            CommandSpec {
                name: CommandName::new("paused"),
                summary: SUMMARY_PAUSED.to_string(),
                usage: "/paused".to_string(),
                args: positional(&[], 0),
                scope: CommandScope::Global,
                run: Arc::new(DormantCommand {
                    dormancy: dormancy.clone(),
                }),
            },
        )
        .await?;
    Ok(())
}

/// The plain-language summaries these three commands are listed under (phase ux1 §2.8, M16).
/// The ledger's step types keep the house words; a sentence shown to a human does not.
pub const SUMMARY_SLEEP: &str =
    "pause an agent: no new turns, and anything sent to it keeps queuing";
pub const SUMMARY_RESUME: &str = "restart a paused agent and let it work through its backlog";
pub const SUMMARY_PAUSED: &str = "list the agents that are paused";

/// The agent a command acts on: the argument, else the focused agent.
fn target(inv: &Invocation, cx: &CommandCx, usage: &str) -> Result<AgentName, CommandError> {
    match inv.args.first() {
        Some(name) => Ok(AgentName::new(name)),
        None => cx
            .agent
            .as_ref()
            .map(|a| a.name().clone())
            .ok_or_else(|| CommandError::BadArgs {
                usage: usage.to_string(),
                detail: "no agent named and none focused".to_string(),
            }),
    }
}

struct SleepCommand {
    dormancy: DormancyHandle,
}

#[async_trait::async_trait]
impl Command for SleepCommand {
    async fn run(&self, inv: Invocation, cx: CommandCx) -> Result<CommandOutput, CommandError> {
        let agent = target(&inv, &cx, "/sleep <agent> [reason]")?;
        let reason = match inv.args.len() {
            0 | 1 => "asked to".to_string(),
            _ => inv.args[1..].join(" "),
        };
        let change = self
            .dormancy
            .sleep(SleepRequest {
                agent,
                reason,
                by: Attribution::Andrey,
                cites: Vec::new(),
                at: cx.at,
            })
            .await
            .map_err(|e| CommandError::Failed(e.to_string()))?;
        Ok(CommandOutput {
            text: render_change(&change),
            render: OutputRender::KeyValue,
            cites: Vec::new(),
        })
    }
}

struct WakeCommand {
    dormancy: DormancyHandle,
}

#[async_trait::async_trait]
impl Command for WakeCommand {
    async fn run(&self, inv: Invocation, cx: CommandCx) -> Result<CommandOutput, CommandError> {
        let agent = target(&inv, &cx, "/resume <agent>")?;
        let change = self
            .dormancy
            .wake_up(WakeUpRequest {
                agent,
                cause: ReactivateCause::Command,
                cites: Vec::new(),
                at: cx.at,
            })
            .await
            .map_err(|e| CommandError::Failed(e.to_string()))?;
        Ok(CommandOutput {
            text: render_change(&change),
            render: OutputRender::KeyValue,
            cites: Vec::new(),
        })
    }
}

struct DormantCommand {
    dormancy: DormancyHandle,
}

#[async_trait::async_trait]
impl Command for DormantCommand {
    async fn run(&self, _inv: Invocation, _cx: CommandCx) -> Result<CommandOutput, CommandError> {
        Ok(CommandOutput {
            text: render_list(&self.dormancy.dormant()),
            render: OutputRender::Plain,
            cites: Vec::new(),
        })
    }
}

/// PURE: what `/sleep` and `/resume` show.
pub fn render_change(c: &DormancyChange) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "{}: {}\n",
        c.agent,
        if c.dormant { "dormant" } else { "awake" }
    ));
    out.push_str(&format!("step: {}\n", c.step));
    match &c.drain {
        Some(w) => out.push_str(&format!("drain: {w}\n")),
        None if c.dormant => out.push_str(
            "messages keep arriving and keep queuing; the agent takes no turns until it is\n restarted\n",
        ),
        None => out.push_str("drain: nothing was queued\n"),
    }
    out
}

/// PURE: what `/paused` shows.
pub fn render_list(dormant: &[AgentName]) -> String {
    if dormant.is_empty() {
        return "no agent is paused\n".to_string();
    }
    let mut out = String::new();
    for a in dormant {
        out.push_str(&format!("{a}\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use bough_plugin_ledger::{StepId, WakeId};

    /// M16: no summary this row registers may use the tree's internal vocabulary.
    #[test]
    fn every_summary_is_plain_language() {
        for s in [SUMMARY_SLEEP, SUMMARY_RESUME, SUMMARY_PAUSED] {
            assert_eq!(bough_plugin_commands::palette::house_word(s), None, "{s}");
        }
    }

    #[test]
    fn sleeping_says_the_queue_survives() {
        let text = render_change(&DormancyChange {
            agent: AgentName::new("terra"),
            dormant: true,
            step: StepId::new("s1"),
            drain: None,
        });
        assert!(text.contains("terra: dormant"), "{text}");
        assert!(text.contains("keep queuing"), "{text}");
    }

    #[test]
    fn waking_names_the_drain_it_armed() {
        let text = render_change(&DormancyChange {
            agent: AgentName::new("terra"),
            dormant: false,
            step: StepId::new("s2"),
            drain: Some(WakeId::new("w9")),
        });
        assert!(text.contains("terra: awake"), "{text}");
        assert!(text.contains("drain: w9"), "{text}");
    }

    #[test]
    fn the_list_says_so_when_nobody_is_paused() {
        assert!(render_list(&[]).contains("no agent is paused"));
        assert_eq!(render_list(&[AgentName::new("terra")]), "terra\n");
    }
}
