//! Invariant (P3-D8): these three commands report, reset or supersede — none of them dispatches a
//! model turn on the agent's behalf. `/supersede` is a thin call to `ctx.rollups.supersede`: it
//! lives here, not on the summarizer, because §8 puts "if a tier block itself is suspected bad"
//! inside the drift-watch paragraph and the suspicion is what drift-watch surfaces.

use std::sync::Arc;

use bough_kernel::{Context, PluginError};
use bough_plugin_commands::{
    positional, Command, CommandCx, CommandError, CommandName, CommandOutput, CommandScope,
    CommandSpec, Commands, Invocation, OutputRender,
};
use bough_plugin_ledger::{AgentName, RollupId};
use bough_plugin_rollups::{Attribution, SupersedeReport, SupersedeRequest};

use crate::{DriftHandle, ResetReport, ResetRequest, SignalState, Signals};

/// Register `/drift`, `/reset` and `/supersede`, if a `commands` registry is bound.
///
/// ```text
/// /drift [agent]                      render the signals and any flags
/// /reset <agent>                      §8's one-command reset
/// /supersede <rollup-id> <reason>     supersede a suspected-bad tier block
/// ```
///
/// The key is OPTIONAL injection: a headless profile mounts this row with no surface at all and
/// still computes signals through `ctx.drift`.
pub async fn register(ctx: &Context, drift: &DriftHandle) -> Result<(), PluginError> {
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
                name: CommandName::new("drift"),
                summary: "per-agent stability signals from the ledger".to_string(),
                usage: "/drift [agent]".to_string(),
                args: positional(&["agent"], 0),
                scope: CommandScope::Global,
                run: Arc::new(DriftCommand {
                    drift: drift.clone(),
                }),
            },
        )
        .await?;
    commands
        .register(
            ctx,
            CommandSpec {
                name: CommandName::new("reset"),
                summary: "rebuild an agent's identity from raw evidence".to_string(),
                usage: "/reset <agent>".to_string(),
                args: positional(&["agent"], 1),
                scope: CommandScope::Global,
                run: Arc::new(ResetCommand {
                    drift: drift.clone(),
                }),
            },
        )
        .await?;
    commands
        .register(
            ctx,
            CommandSpec {
                name: CommandName::new("supersede"),
                summary: "supersede a suspected-bad tier block".to_string(),
                usage: "/supersede <rollup-id> <reason>".to_string(),
                args: positional(&["rollup", "reason"], 2),
                scope: CommandScope::Global,
                run: Arc::new(SupersedeCommand {
                    drift: drift.clone(),
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

struct DriftCommand {
    drift: DriftHandle,
}

#[async_trait::async_trait]
impl Command for DriftCommand {
    async fn run(&self, inv: Invocation, cx: CommandCx) -> Result<CommandOutput, CommandError> {
        let agent = target(&inv, &cx, "/drift [agent]")?;
        let signals = self
            .drift
            .signals(&agent, cx.at)
            .await
            .map_err(|e| CommandError::Failed(e.to_string()))?;
        Ok(CommandOutput {
            text: render_signals(&signals),
            render: OutputRender::KeyValue,
            cites: Vec::new(),
        })
    }
}

struct ResetCommand {
    drift: DriftHandle,
}

#[async_trait::async_trait]
impl Command for ResetCommand {
    async fn run(&self, inv: Invocation, cx: CommandCx) -> Result<CommandOutput, CommandError> {
        let agent = target(&inv, &cx, "/reset <agent>")?;
        let traj = self
            .drift
            .trajectory(&agent)
            .await
            .map_err(|e| CommandError::Failed(e.to_string()))?;
        let report = self
            .drift
            .reset(&ResetRequest {
                agent: agent.clone(),
                traj,
                at: cx.at,
                // Phase 4 attributes governance to the system; Phase 5's leader writes
                // `Attribution::Agent` with no shape change (§8).
                attribution: Attribution::System,
            })
            .await
            .map_err(|e| CommandError::Failed(e.to_string()))?;
        Ok(CommandOutput {
            text: render_reset(&agent, &report),
            render: OutputRender::KeyValue,
            cites: Vec::new(),
        })
    }
}

struct SupersedeCommand {
    drift: DriftHandle,
}

#[async_trait::async_trait]
impl Command for SupersedeCommand {
    async fn run(&self, inv: Invocation, cx: CommandCx) -> Result<CommandOutput, CommandError> {
        let usage = "/supersede <rollup-id> <reason>";
        let (block, reason) = match inv.args.split_first() {
            Some((block, rest)) if !rest.is_empty() => (block.clone(), rest.join(" ")),
            _ => {
                return Err(CommandError::BadArgs {
                    usage: usage.to_string(),
                    // A supersession with no reason is an unexplained edit to memory; §3's relief
                    // valve is a NEW block plus a note, and a note with nothing in it is not one.
                    detail: "a rollup id and a reason are both required".to_string(),
                });
            }
        };
        let report = self
            .drift
            .supersede(&SupersedeRequest {
                block: RollupId::new(block),
                reason,
                at: cx.at,
                attribution: Attribution::System,
            })
            .await
            .map_err(|e| CommandError::Failed(e.to_string()))?;
        Ok(CommandOutput {
            text: render_supersede(&report),
            render: OutputRender::KeyValue,
            cites: Vec::new(),
        })
    }
}

/// PURE: what `/drift` shows.
pub fn render_signals(s: &Signals) -> String {
    let mut out = String::new();
    out.push_str(&format!("agent: {}\n", s.agent));
    out.push_str(&format!(
        "window: {}..{} ({} samples)\n",
        s.window.from.0, s.window.to.0, s.samples
    ));
    out.push_str(&format!(
        "thought length: n={} mean={:.1} var={:.1} cv={:.2} p50={:.0} p95={:.0}\n",
        s.thought_len.n,
        s.thought_len.mean,
        s.thought_len.variance,
        s.thought_len.cv,
        s.thought_len.p50,
        s.thought_len.p95
    ));
    if s.tool_use.is_empty() {
        out.push_str("tool use: no calls in the window\n");
    } else {
        let shares: Vec<String> = s
            .tool_use
            .iter()
            .map(|t| format!("{} {:.0}%", t.tool, t.share * 100.0))
            .collect();
        out.push_str(&format!("tool use: {}\n", shares.join(", ")));
        out.push_str(&format!("tool entropy: {:.2}\n", s.tool_entropy));
    }
    match &s.claim_rejection {
        // §16: uncertainty never becomes assertion. A rate nobody can compute is reported as
        // inactive, with what is missing, rather than as 0%.
        SignalState::Inactive { since } => {
            out.push_str(&format!("claim rejection: inactive — {since}\n"))
        }
        SignalState::Active { value, n } => {
            out.push_str(&format!("claim rejection: {:.0}% of {n}\n", value * 100.0))
        }
    }
    if s.flags.is_empty() {
        out.push_str("flags: none\n");
    } else {
        let flags: Vec<String> = s
            .flags
            .iter()
            .map(|f| {
                serde_json::to_value(f)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_string))
                    .unwrap_or_else(|| format!("{f:?}"))
            })
            .collect();
        out.push_str(&format!("flags: {}\n", flags.join(", ")));
    }
    out
}

/// PURE: what `/reset` shows. The tier count is REPORTED, both halves, because "sealed tiers
/// untouched" is a claim the reader gets to check.
pub fn render_reset(agent: &AgentName, r: &ResetReport) -> String {
    let mut out = String::new();
    out.push_str(&format!("reset: {agent}\n"));
    out.push_str(&format!("digest: {} (from raw evidence)\n", r.digest));
    match &r.replaced_digest {
        Some(old) => out.push_str(&format!("replaced: {old}\n")),
        None => out.push_str("replaced: nothing — this agent had no digest\n"),
    }
    out.push_str(&format!(
        "about-line: {} (intent half empty)\n",
        r.about_line
    ));
    out.push_str(&format!("step: {}\n", r.reset_step));
    out.push_str(&format!(
        "sealed tiers: {} before, {} after — untouched\n",
        r.tiers_before, r.tiers_after
    ));
    out
}

/// PURE: what `/supersede` shows.
pub fn render_supersede(r: &SupersedeReport) -> String {
    format!(
        "superseded: {}\nby: {}\nexpiry note: {}\nthe old block is immutable and stays in the \
         ledger; only `superseded_by` was set\n",
        r.old, r.new, r.note
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DriftFlag, Stat, ToolShare};
    use bough_plugin_ledger::{Seq, SeqRange, StepId};

    fn signals() -> Signals {
        Signals {
            agent: AgentName::new("scout"),
            window: SeqRange {
                from: Seq(1),
                to: Seq(50),
            },
            samples: 6,
            thought_len: Stat {
                n: 4,
                mean: 12.0,
                variance: 4.0,
                cv: 0.17,
                p50: 12.0,
                p95: 14.0,
            },
            tool_use: vec![ToolShare {
                tool: "bash".to_string(),
                calls: 2,
                share: 1.0,
            }],
            tool_entropy: 0.0,
            claim_rejection: SignalState::Inactive {
                since: crate::signals::CLAIM_REJECTION_SINCE.to_string(),
            },
            flags: vec![DriftFlag::ToolUseCollapsed],
        }
    }

    #[test]
    fn drift_renders_the_signals_and_says_claim_rejection_is_inactive() {
        let text = render_signals(&signals());
        assert!(text.contains("agent: scout"), "{text}");
        assert!(text.contains("window: 1..50 (6 samples)"), "{text}");
        assert!(text.contains("cv=0.17"), "{text}");
        assert!(text.contains("bash 100%"), "{text}");
        assert!(text.contains("claim rejection: inactive"), "{text}");
        assert!(
            text.contains("no claim in the window has been decided"),
            "{text}"
        );
        assert!(text.contains("flags: tool_use_collapsed"), "{text}");
    }

    #[test]
    fn a_clean_agent_renders_no_flags_and_no_tool_line() {
        let mut s = signals();
        s.flags.clear();
        s.tool_use.clear();
        let text = render_signals(&s);
        assert!(text.contains("flags: none"), "{text}");
        assert!(text.contains("tool use: no calls in the window"), "{text}");
    }

    #[test]
    fn reset_reports_the_tier_count_on_both_sides() {
        let text = render_reset(
            &AgentName::new("scout"),
            &ResetReport {
                digest: bough_plugin_ledger::RollupId::new("d2"),
                replaced_digest: Some(bough_plugin_ledger::RollupId::new("d1")),
                about_line: StepId::new("a1"),
                reset_step: StepId::new("r1"),
                tiers_before: 3,
                tiers_after: 3,
            },
        );
        assert!(text.contains("digest: d2 (from raw evidence)"), "{text}");
        assert!(text.contains("replaced: d1"), "{text}");
        assert!(text.contains("intent half empty"), "{text}");
        assert!(text.contains("3 before, 3 after"), "{text}");
    }

    #[test]
    fn supersede_says_the_old_block_was_not_edited() {
        let text = render_supersede(&SupersedeReport {
            old: bough_plugin_ledger::RollupId::new("t1"),
            new: bough_plugin_ledger::RollupId::new("t2"),
            note: StepId::new("n1"),
        });
        assert!(text.contains("superseded: t1"), "{text}");
        assert!(text.contains("by: t2"), "{text}");
        assert!(text.contains("immutable"), "{text}");
    }
}
