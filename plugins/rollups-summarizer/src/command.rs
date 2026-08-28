//! Invariant (P3-D8): `/seal` runs a governance pass and RENDERS the report; it dispatches no
//! model turn on the agent's behalf and appends nothing but the pass's own steps. `--plan` writes
//! nothing at all.

use std::sync::Arc;

use bough_kernel::{Context, PluginError};
use bough_plugin_commands::{
    Command, CommandCx, CommandError, CommandName, CommandOutput, CommandScope, CommandSpec,
    Commands, Invocation, OutputRender,
};
use bough_plugin_ledger::AgentName;
use bough_plugin_rollups::{Attribution, SealPlan, SealReport, SealRequest, SkipReason, Stop};

use crate::RecapSummarizer;

/// Register `/seal`, if a `commands` registry is bound (P4-D8).
///
/// ```text
/// /seal [agent] [--plan]      run (or, with --plan, only report) a seal pass for the agent
/// ```
pub async fn register(ctx: &Context, summarizer: &RecapSummarizer) -> Result<(), PluginError> {
    // ABSENT is headless — the seam works with no surface at all, and the schedule hook is the
    // seam method, not the command (P4-D8, P4-D14). An ERROR is the kernel refusing the read
    // (an undeclared key, an access fault) and is a boot failure, never a row with no commands.
    let commands = match ctx.try_get::<Commands>() {
        Ok(Some(c)) => c,
        Ok(None) => return Ok(()),
        Err(e) => return Err(PluginError::new(ctx.entry_id().clone(), e)),
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
                run: Arc::new(SealCommand {
                    summarizer: summarizer.clone(),
                }),
            },
        )
        .await?;
    Ok(())
}

struct SealCommand {
    summarizer: RecapSummarizer,
}

#[async_trait::async_trait]
impl Command for SealCommand {
    async fn run(&self, inv: Invocation, cx: CommandCx) -> Result<CommandOutput, CommandError> {
        let dry = inv.args.iter().any(|a| a == "--plan");
        let named = inv.args.iter().find(|a| !a.starts_with("--")).cloned();
        let agent = match (named, cx.agent.as_ref()) {
            (Some(n), _) => AgentName::new(n),
            (None, Some(a)) => a.name().clone(),
            // Never guess whose memory to seal.
            (None, None) => {
                return Err(CommandError::BadArgs {
                    usage: "/seal [agent] [--plan]".to_string(),
                    detail: "no agent is focused, so `/seal` needs one by name".to_string(),
                })
            }
        };
        let inner = &self.summarizer.0;
        let row = inner
            .ledger
            .0
            .agent(&agent)
            .await
            .map_err(|e| CommandError::Failed(e.to_string()))?
            .ok_or_else(|| CommandError::Failed(format!("no agent named `{agent}`")))?;
        let req = SealRequest {
            agent: agent.clone(),
            traj: row.traj.clone(),
            at: cx.at,
            upto: None,
            max_calls: None,
            // Phase 4 always System; Phase 5's leader is a value change here and nowhere else.
            attribution: Attribution::System,
        };
        let text = if dry {
            let plan = crate::seal::plan(inner, &req)
                .await
                .map_err(|e| CommandError::Failed(e.to_string()))?;
            render_plan(&agent, &plan)
        } else {
            let report = crate::seal::run(inner, &req)
                .await
                .map_err(|e| CommandError::Failed(e.to_string()))?;
            render_report(&agent, &report)
        };
        Ok(CommandOutput {
            text,
            render: OutputRender::Plain,
            cites: Vec::new(),
        })
    }
}

/// The word a skip reason is rendered as. Every reason is nameable; nothing is skipped silently.
pub fn why(r: SkipReason) -> &'static str {
    match r {
        SkipReason::AlreadySealed => "already sealed",
        SkipReason::TooCloseToHead => "too close to the head",
        SkipReason::TooShort => "too short",
        SkipReason::NotEnoughChildren => "not enough children",
        SkipReason::CallBudget => "call budget",
        SkipReason::Refused => "provider seals nothing",
    }
}

/// PURE: what `/seal --plan` shows.
pub fn render_plan(agent: &AgentName, plan: &SealPlan) -> String {
    let mut out = format!(
        "{agent}: head {}, sealing up to {} ({} block(s) planned)\n",
        plan.head.0,
        plan.upto.0,
        plan.blocks.len()
    );
    for b in &plan.blocks {
        out.push_str(&format!(
            "  tier {} {}..{} → {}\n",
            b.tier, b.from_seq.0, b.to_seq.0, b.id
        ));
    }
    for s in &plan.skipped {
        out.push_str(&format!(
            "  skipped tier {} {}..{}: {}\n",
            s.tier,
            s.from_seq.0,
            s.to_seq.0,
            why(s.why)
        ));
    }
    if plan.blocks.is_empty() && plan.skipped.is_empty() {
        out.push_str("  nothing to seal\n");
    }
    out.push_str("nothing was written: --plan reads only\n");
    out
}

/// PURE: what `/seal` shows.
pub fn render_report(agent: &AgentName, report: &SealReport) -> String {
    // A sentence first (ux-visual): `0 sealed, 0 call(s), 0 in / 0 out (nothing to do)` was a
    // row of counters. The block and skip lines below keep the ledger's names.
    let mut out = match report.stop {
        Stop::NothingToDo if report.sealed.is_empty() => {
            format!("{agent}: nothing to seal yet.\n")
        }
        stop => format!(
            "{agent}: sealed {} block{} in {} model call{} ({} tokens in / {} out){}.\n",
            report.sealed.len(),
            if report.sealed.len() == 1 { "" } else { "s" },
            report.calls,
            if report.calls == 1 { "" } else { "s" },
            report.tokens_in,
            report.tokens_out,
            match stop {
                Stop::CallBudget => " — stopped at the call budget",
                _ => "",
            }
        ),
    };
    for id in &report.sealed {
        out.push_str(&format!("  sealed {id}\n"));
    }
    for s in &report.skipped {
        out.push_str(&format!(
            "  skipped tier {} {}..{}: {}\n",
            s.tier,
            s.from_seq.0,
            s.to_seq.0,
            why(s.why)
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use bough_plugin_rollups::{PassId, Skip};

    #[test]
    fn a_pass_with_nothing_to_do_says_so_rather_than_reporting_success() {
        let text = render_report(
            &AgentName::new("sol"),
            &SealReport {
                pass: PassId::new("pass:1"),
                planned: 0,
                sealed: vec![],
                skipped: vec![],
                calls: 0,
                tokens_in: 0,
                tokens_out: 0,
                stop: Stop::NothingToDo,
            },
        );
        assert!(text.contains("nothing to seal yet"), "{text}");
    }

    /// Never skip silently (§0.2): every reason has a word, and the render prints it.
    #[test]
    fn every_skip_reason_is_named_in_the_render() {
        let skipped = vec![
            SkipReason::AlreadySealed,
            SkipReason::TooCloseToHead,
            SkipReason::TooShort,
            SkipReason::NotEnoughChildren,
            SkipReason::CallBudget,
            SkipReason::Refused,
        ];
        let plan = SealPlan {
            traj: bough_plugin_ledger::TrajId::new("t"),
            head: bough_plugin_ledger::Seq(100),
            upto: bough_plugin_ledger::Seq(80),
            blocks: vec![],
            skipped: skipped
                .iter()
                .map(|w| Skip {
                    tier: 1,
                    from_seq: bough_plugin_ledger::Seq(1),
                    to_seq: bough_plugin_ledger::Seq(10),
                    why: *w,
                })
                .collect(),
        };
        let text = render_plan(&AgentName::new("sol"), &plan);
        for w in skipped {
            assert!(text.contains(why(w)), "`{}` is missing from {text}", why(w));
        }
        assert!(text.contains("nothing was written"), "{text}");
    }
}
