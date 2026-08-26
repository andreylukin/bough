//! Invariant (§16): the stub is REACHABLE. `/seal` is registered by whichever provider holds the
//! `rollups` key, so swapping the summarizer out must not make the command vanish — a surface that
//! answers "unknown command `seal`" tells the operator nothing about why nothing is being sealed,
//! where "0 sealed … (nothing to do): this provider seals nothing" tells them exactly.
//!
//! The registration is deliberately a near-copy of the summarizer's rather than a shared helper:
//! the seam crate owns the vocabulary, not the surface, and a provider that renders its own report
//! is what lets the two answers differ honestly.

use std::sync::Arc;

use bough_kernel::{Context, PluginError};
use bough_plugin_commands::{
    Command, CommandCx, CommandError, CommandName, CommandOutput, CommandScope, CommandSpec,
    Commands, Invocation, OutputRender,
};
use bough_plugin_ledger::AgentName;
use bough_plugin_rollups::{Attribution, SealRequest, SkipReason, Stop, Summarizer};

use crate::NoneSummarizer;

/// Register `/seal`, if a `commands` registry is bound (P4-D8). Headless binds none, and the row
/// activates anyway.
pub async fn register(ctx: &Context, stub: &NoneSummarizer) -> Result<(), PluginError> {
    let Ok(Some(commands)) = ctx.try_get::<Commands>() else {
        return Ok(());
    };
    commands
        .register(
            ctx,
            CommandSpec {
                name: CommandName::new("seal"),
                summary: "run a memory seal pass for an agent".to_string(),
                usage: "/seal [agent] [--plan]".to_string(),
                args: schemars::json_schema!({ "type": "array", "items": { "type": "string" } }),
                scope: CommandScope::Global,
                run: Arc::new(SealCommand { stub: stub.clone() }),
            },
        )
        .await?;
    Ok(())
}

struct SealCommand {
    stub: NoneSummarizer,
}

#[async_trait::async_trait]
impl Command for SealCommand {
    async fn run(&self, inv: Invocation, cx: CommandCx) -> Result<CommandOutput, CommandError> {
        let named = inv.args.iter().find(|a| !a.starts_with("--")).cloned();
        let agent = match (named, cx.agent.as_ref()) {
            (Some(n), _) => AgentName::new(n),
            (None, Some(a)) => a.name().clone(),
            (None, None) => {
                return Err(CommandError::BadArgs {
                    usage: "/seal [agent] [--plan]".to_string(),
                    detail: "no agent is focused, so `/seal` needs one by name".to_string(),
                })
            }
        };
        let row = self
            .stub
            .ledger
            .0
            .agent(&agent)
            .await
            .map_err(|e| CommandError::Failed(e.to_string()))?
            .ok_or_else(|| CommandError::Failed(format!("no agent named `{agent}`")))?;
        let report = self
            .stub
            .seal(&SealRequest {
                agent: agent.clone(),
                traj: row.traj.clone(),
                at: cx.at,
                upto: None,
                max_calls: None,
                attribution: Attribution::System,
            })
            .await
            .map_err(|e| CommandError::Failed(e.to_string()))?;
        Ok(CommandOutput {
            text: render(&agent, &report),
            render: OutputRender::Plain,
            cites: Vec::new(),
        })
    }
}

/// PURE: what `/seal` shows under the stub. The same first line the summarizer writes, so the two
/// reports are comparable, plus the sentence that names the reason.
pub fn render(agent: &AgentName, report: &bough_plugin_rollups::SealReport) -> String {
    let mut out = format!(
        "{agent}: {} sealed, {} call(s), {} in / {} out ({})\n",
        report.sealed.len(),
        report.calls,
        report.tokens_in,
        report.tokens_out,
        match report.stop {
            Stop::Complete => "complete",
            Stop::CallBudget => "stopped at the call budget",
            Stop::NothingToDo => "nothing to do",
        }
    );
    for s in &report.skipped {
        if s.why == SkipReason::Refused {
            out.push_str(&format!(
                "  skipped tier {} {}..{}: this provider seals nothing\n",
                s.tier, s.from_seq.0, s.to_seq.0
            ));
        }
    }
    out.push_str("the `rollups` row is bound to `rollups-none`, which seals nothing, ever\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use bough_plugin_ledger::Seq;
    use bough_plugin_rollups::{PassId, SealReport, Skip};

    #[test]
    fn the_report_says_nothing_to_do_and_why() {
        let text = render(
            &AgentName::new("sol"),
            &SealReport {
                pass: PassId::new("pass:none"),
                planned: 0,
                sealed: Vec::new(),
                skipped: vec![Skip {
                    tier: 1,
                    from_seq: Seq(1),
                    to_seq: Seq(60),
                    why: SkipReason::Refused,
                }],
                calls: 0,
                tokens_in: 0,
                tokens_out: 0,
                stop: Stop::NothingToDo,
            },
        );
        assert!(text.contains("nothing to do"), "{text}");
        assert!(text.contains("seals nothing"), "{text}");
        assert!(text.contains("0 sealed"), "{text}");
    }
}
