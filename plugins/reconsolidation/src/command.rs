//! Invariant (P3-D8): `/reconsolidate` runs a pass and RENDERS the report; `--plan` writes
//! nothing at all.

use std::sync::Arc;

use bough_kernel::{Context, PluginError};
use bough_plugin_commands::{
    Command, CommandCx, CommandError, CommandName, CommandOutput, CommandScope, CommandSpec,
    Commands, Invocation, OutputRender,
};
use bough_plugin_ledger::{AgentName, Seq};

use crate::{PassPlan, PassReport, PassRequest, ReconHandle};

/// Register `/reconsolidate`, if a `commands` registry is bound.
///
/// ```text
/// /reconsolidate [agent] [--plan] [--since <seq>]
/// ```
pub async fn register(ctx: &Context, recon: &ReconHandle) -> Result<(), PluginError> {
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
                name: CommandName::new("reconsolidate"),
                summary: "distil, surface contradictions and expire stale evidence".to_string(),
                usage: "/reconsolidate [agent] [--plan] [--since <seq>]".to_string(),
                args: schemars::json_schema!({ "type": "array", "items": { "type": "string" } }),
                scope: CommandScope::Global,
                run: Arc::new(ReconsolidateCommand {
                    recon: recon.clone(),
                }),
            },
        )
        .await?;
    Ok(())
}

/// PURE: the flags a `/reconsolidate` line carries.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Args {
    pub agent: Option<String>,
    pub plan_only: bool,
    pub since: Option<u64>,
}

/// Parse the argument list. `Err` names the usage problem; the dispatcher renders it.
pub fn parse_args(args: &[String]) -> Result<Args, String> {
    let mut out = Args::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--plan" => out.plan_only = true,
            "--since" => {
                let v = it.next().ok_or("`--since` needs a seq")?;
                out.since = Some(
                    v.parse::<u64>()
                        .map_err(|_| format!("`{v}` is not a seq"))?,
                );
            }
            other if other.starts_with("--") => return Err(format!("unknown flag `{other}`")),
            other if out.agent.is_none() => out.agent = Some(other.to_string()),
            other => return Err(format!("unexpected argument `{other}`")),
        }
    }
    Ok(out)
}

/// PURE: what `--plan` renders. No write happened, and the text says so.
pub fn render_plan(plan: &PassPlan) -> String {
    format!(
        "would reconsolidate seq {}..{}\n  distil: {}\n  contradiction candidates: {}\n  \
         expiry candidates: {}\nnothing was written\n",
        plan.range.from.0,
        plan.range.to.0,
        if plan.distil {
            "yes"
        } else {
            "nothing to distil"
        },
        plan.contradiction_candidates.len(),
        plan.expiry_candidates.len(),
    )
}

/// PURE: what a completed pass renders.
pub fn render_report(report: &PassReport) -> String {
    format!(
        "pass {}\n  distilled: {}\n  contradictions proposed: {}\n  evidence expired: {}\n  \
         model calls: {} ({} in / {} out)\n",
        report.pass,
        report
            .distilled
            .as_ref()
            .map(|d| d.to_string())
            .unwrap_or_else(|| "none".to_string()),
        report.contradictions.len(),
        report.expired.len(),
        report.calls,
        report.tokens_in,
        report.tokens_out,
    )
}

struct ReconsolidateCommand {
    recon: ReconHandle,
}

#[async_trait::async_trait]
impl Command for ReconsolidateCommand {
    async fn run(&self, inv: Invocation, cx: CommandCx) -> Result<CommandOutput, CommandError> {
        let usage = "/reconsolidate [agent] [--plan] [--since <seq>]".to_string();
        let args = parse_args(&inv.args).map_err(|detail| CommandError::BadArgs {
            usage: usage.clone(),
            detail,
        })?;
        let name = match (&args.agent, &cx.agent) {
            (Some(a), _) => AgentName::new(a),
            (None, Some(a)) => a.name().clone(),
            (None, None) => {
                return Err(CommandError::BadArgs {
                    usage,
                    detail: "no agent is focused, so `/reconsolidate` needs one named".to_string(),
                })
            }
        };
        let row = self
            .recon
            .0
            .ledger
            .0
            .agent(&name)
            .await
            .map_err(|e| CommandError::Failed(e.to_string()))?
            .ok_or_else(|| CommandError::Failed(format!("no agent `{name}`")))?;

        let req = PassRequest {
            agent: name,
            traj: row.traj,
            at: cx.at,
            since: args.since.map(Seq),
            // §8: the pass is leader-attributed once a leader exists. The `leader` row installs
            // its own name through `attribute_to`; with no leader in the tree this is `System`,
            // which is what Phase 4 always wrote.
            attribution: self.recon.attribution(),
            max_calls: None,
        };
        let text = if args.plan_only {
            render_plan(
                &self
                    .recon
                    .plan(&req)
                    .await
                    .map_err(|e| CommandError::Failed(e.to_string()))?,
            )
        } else {
            render_report(
                &self
                    .recon
                    .run(&req)
                    .await
                    .map_err(|e| CommandError::Failed(e.to_string()))?,
            )
        };
        Ok(CommandOutput {
            text,
            render: OutputRender::Plain,
            cites: Vec::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_flags_parse() {
        assert_eq!(parse_args(&[]).unwrap(), Args::default());
        assert_eq!(
            parse_args(&["sol".into(), "--plan".into(), "--since".into(), "12".into()]).unwrap(),
            Args {
                agent: Some("sol".into()),
                plan_only: true,
                since: Some(12)
            }
        );
        assert!(parse_args(&["--since".into()]).is_err());
        assert!(parse_args(&["--since".into(), "x".into()]).is_err());
        assert!(parse_args(&["--nope".into()]).is_err());
    }

    /// `--plan` must SAY that it wrote nothing: a plan a reader mistakes for a report is the one
    /// way this surface could lie (§16).
    #[test]
    fn the_plan_rendering_says_nothing_was_written() {
        let plan = PassPlan {
            range: bough_plugin_ledger::SeqRange {
                from: Seq(1),
                to: Seq(9),
            },
            distil: true,
            contradiction_candidates: vec![],
            expiry_candidates: vec![],
        };
        assert!(render_plan(&plan).contains("nothing was written"));
    }
}
