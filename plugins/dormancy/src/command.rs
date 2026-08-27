//! Invariant (§16): dormancy is REACHABLE from the surface. `/sleep`, `/wake` and `/dormant` are
//! registered only when a `commands` registry is bound — headless binds none and the row activates
//! anyway (the P4-D8 precedent). None of them dispatches a model turn (P3-D8): `/wake` arms a
//! drain, and the loop decides whether there is anything to drain.

use std::sync::Arc;

use bough_kernel::{Context, PluginError};
use bough_plugin_commands::{
    positional, positional_rest, Command, CommandCx, CommandError, CommandName, CommandOutput,
    CommandScope, CommandSpec, Commands, Invocation, OutputRender,
};
use bough_plugin_ledger::AgentName;
use bough_plugin_rollups::Attribution;

use crate::{DormancyChange, DormancyHandle, ReactivateCause, SleepRequest, WakeUpRequest};

/// Register `/sleep <agent> [reason]`, `/wake <agent>` and `/dormant`, if `commands` is bound.
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
                summary: "put a lane to sleep: no ticks, no wakes, mail keeps queuing".to_string(),
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
                name: CommandName::new("wake"),
                summary: "reactivate a sleeping lane and drain its backlog".to_string(),
                usage: "/wake <agent>".to_string(),
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
                name: CommandName::new("dormant"),
                summary: "which lanes are asleep".to_string(),
                usage: "/dormant".to_string(),
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
        let agent = target(&inv, &cx, "/wake <agent>")?;
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

/// PURE: what `/sleep` and `/wake` show.
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
            "mail keeps arriving and keeps queuing; no ticks and no wakes until reactivation\n",
        ),
        None => out.push_str("drain: nothing was queued\n"),
    }
    out
}

/// PURE: what `/dormant` shows.
pub fn render_list(dormant: &[AgentName]) -> String {
    if dormant.is_empty() {
        return "no lane is asleep\n".to_string();
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

    #[test]
    fn sleeping_says_the_mail_keeps_queuing() {
        let text = render_change(&DormancyChange {
            agent: AgentName::new("terra"),
            dormant: true,
            step: StepId::new("s1"),
            drain: None,
        });
        assert!(text.contains("terra: dormant"), "{text}");
        assert!(text.contains("keeps queuing"), "{text}");
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
    fn the_list_says_so_when_nobody_is_asleep() {
        assert!(render_list(&[]).contains("no lane is asleep"));
        assert_eq!(render_list(&[AgentName::new("terra")]), "terra\n");
    }
}
