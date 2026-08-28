//! Invariant: the four built-ins are ordinary rows on `ctx.commands`, registered as EFFECTS — the
//! shell has no private command path. Unloading the `tui` row removes them, and nothing they do is
//! model-visible: they render locally and append no step (P3-D8).

use std::sync::Arc;

use bough_kernel::{Context, PluginError};
use bough_plugin_commands::{
    Command, CommandCx, CommandError, CommandName, CommandOutput, CommandScope, CommandSpec,
    Commands, Invocation, OutputRender,
};
use bough_plugin_ledger::AgentName;

use crate::TuiHandle;

/// Register `/help`, `/quit`, `/focus` and `/agents`. Each is its own effect, so a later phase can
/// replace one by registering a scoped twin rather than by editing this file.
pub async fn register(ctx: &Context, tui: &TuiHandle) -> Result<(), PluginError> {
    let commands = ctx
        .get::<Commands>()
        .map_err(|e| PluginError::new(ctx.entry_id().clone(), e))?;

    for spec in specs(tui) {
        commands.register(ctx, spec).await?;
    }
    Ok(())
}

/// The plain-language summary of each built-in, as `/help` prints it (phase ux1 §2.8, M16).
///
/// A TABLE rather than four string literals inside `specs`, because the lint below has to read
/// exactly the strings that are registered: a summary the lint cannot see is a summary the lint
/// does not check.
pub fn summaries() -> Vec<(&'static str, &'static str)> {
    vec![
        ("help", "list the commands and keys this window understands"),
        ("quit", "close bough"),
        ("focus", "show one agent's conversation"),
        (
            "agents",
            "list the agents, what each is doing, and how many messages are waiting",
        ),
    ]
}

/// The summary for one built-in.
fn summary(name: &str) -> String {
    summaries()
        .into_iter()
        .find(|(n, _)| *n == name)
        .map(|(_, s)| s.to_string())
        .expect("every built-in has a summary")
}

/// The four specs. Separated from registration so a test can read them without a registry.
pub fn specs(tui: &TuiHandle) -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            name: CommandName::new("help"),
            summary: summary("help"),
            usage: "/help".to_string(),
            args: no_args(),
            scope: CommandScope::Global,
            run: Arc::new(Help(tui.clone())),
        },
        CommandSpec {
            name: CommandName::new("quit"),
            summary: summary("quit"),
            usage: "/quit".to_string(),
            args: no_args(),
            scope: CommandScope::Global,
            run: Arc::new(Quit(tui.clone())),
        },
        CommandSpec {
            name: CommandName::new("focus"),
            summary: summary("focus"),
            usage: "/focus <agent>".to_string(),
            args: one_string("agent", "the agent's name"),
            scope: CommandScope::Global,
            run: Arc::new(Focus(tui.clone())),
        },
        CommandSpec {
            name: CommandName::new("agents"),
            summary: summary("agents"),
            usage: "/agents".to_string(),
            args: no_args(),
            scope: CommandScope::Global,
            run: Arc::new(Roster(tui.clone())),
        },
    ]
}

/// A command that takes nothing.
fn no_args() -> schemars::Schema {
    schemars::json_schema!({
        "type": "object",
        "properties": {},
        "additionalProperties": false
    })
}

/// A command that takes one required string.
fn one_string(name: &str, description: &str) -> schemars::Schema {
    schemars::json_schema!({
        "type": "object",
        "properties": { name: { "type": "string", "description": description } },
        "required": [name],
        "additionalProperties": false
    })
}

struct Help(TuiHandle);

#[async_trait::async_trait]
impl Command for Help {
    async fn run(&self, _inv: Invocation, cx: CommandCx) -> Result<CommandOutput, CommandError> {
        // KEYS FIRST, deliberately. The help band is as tall as the rows above the composer and
        // no taller, and the command list grows with every row a tree loads — so the half that
        // gets cut must be the half a reader can also get from the `/` palette. The keys have no
        // other home (M18: `/help` listed commands and not one binding).
        let mut lines = Vec::new();
        lines.push("keys".to_string());
        // ONE table (M16): the status line's hints and this list are the same function, so they
        // cannot drift apart or disagree with what `action_for` actually does.
        for (keys, what) in keymap_hints() {
            lines.push(format!("  {keys:<22} {what}"));
        }
        lines.push(String::new());
        lines.push("commands  (or press / for the same list, filtered as you type)".to_string());
        // The shell's OWN registry handle, not a fresh `ctx.get`: `/help` lists what THIS surface
        // dispatches through, and a command must not depend on being run from a row context.
        match self.0.commands() {
            Some(commands) => {
                let scope = cx.agent.as_ref().map(|a| a.name().clone());
                let listed = commands.list(scope.as_ref());
                if listed.is_empty() {
                    lines.push("  (none registered)".to_string());
                }
                for info in listed {
                    lines.push(format!("  {:<22} {}", info.usage, info.summary));
                }
            }
            // A reason, never a silent gap (M27): every section of `/help` says something.
            None => lines.push("  (this surface has no command registry)".to_string()),
        }
        // Each pane's keys under the pane's own name (visual audit): "(trajectory)" after six
        // rows in a row was a column of noise, and the reader's question is "what does THIS pane
        // take", which a heading answers and a suffix does not. LAST, after the commands: the
        // band is capped, and a pane's keys are the least urgent third — the pane teaches them
        // itself once the keyboard is in it.
        for pane in self.0.entries() {
            let hints = pane.pane.key_hints();
            if hints.is_empty() {
                continue;
            }
            lines.push(String::new());
            lines.push(format!("{} pane", pane.info.title));
            for (keys, what) in hints {
                lines.push(format!("  {keys:<22} {what}"));
            }
        }
        Ok(CommandOutput {
            text: lines.join("\n"),
            render: OutputRender::KeyValue,
            cites: Vec::new(),
        })
    }
}

/// The fixed keymap, as `/help` and the status line render it (P3-D18, M16).
///
/// Phase ux1: the table itself lives in [`crate::keymap::hints`], which is what
/// [`crate::keymap::action_for`] is written against. This name stays for the callers that already
/// have it, and is now a pure forward — there is exactly ONE table, so `/help` cannot advertise a
/// binding the keymap does not implement.
pub fn keymap_hints() -> Vec<(&'static str, &'static str)> {
    crate::keymap::hints()
}

struct Quit(TuiHandle);

#[async_trait::async_trait]
impl Command for Quit {
    async fn run(&self, _inv: Invocation, _cx: CommandCx) -> Result<CommandOutput, CommandError> {
        // B8: the farewell is stored and printed AFTER the terminal is restored, so `/quit`
        // can never leave a black rectangle behind it.
        self.0.quit_with(0, crate::run::farewell());
        Ok(CommandOutput {
            text: "leaving".to_string(),
            render: OutputRender::Plain,
            cites: Vec::new(),
        })
    }
}

struct Focus(TuiHandle);

#[async_trait::async_trait]
impl Command for Focus {
    async fn run(&self, inv: Invocation, _cx: CommandCx) -> Result<CommandOutput, CommandError> {
        let Some(want) = inv.args.first() else {
            return Err(CommandError::BadArgs {
                usage: "/focus <agent>".to_string(),
                detail: "no agent named".to_string(),
            });
        };
        let name = AgentName::new(want);
        let Some(agents) = self.0 .0.agents.clone() else {
            return Err(CommandError::Failed("no agent roster".to_string()));
        };
        let Some(agent) = agents.by_name(&name) else {
            return Err(CommandError::Failed(format!("no agent `{want}`")));
        };
        self.0.focus(crate::run::to_agent(agent.id().clone())).await;
        Ok(CommandOutput {
            text: format!("focused {want}"),
            render: OutputRender::Plain,
            cites: Vec::new(),
        })
    }
}

struct Roster(TuiHandle);

/// The header row `/agents` prints, and the widths every row is laid out on (M24: a table of
/// bare words is not a table).
pub const ROSTER_HEADER: &str = "agent        status    doing                          waiting";

/// The step type dormancy is folded from, by NAME (P3-D11): `dormancy` owns the fact, this
/// row only reads it.
const DORMANCY_STEP: &str = "agent/dormancy";

/// The about-line's state half, by NAME (`about/line`, `body.state`) — the newest one on `traj`.
async fn about_state_now(
    ledger: &bough_plugin_ledger::LedgerHandle,
    traj: &bough_plugin_ledger::TrajId,
) -> Option<String> {
    use bough_plugin_ledger::{Order, StepQuery, StepType};
    let steps = ledger
        .0
        .steps(&StepQuery {
            trajs: vec![traj.clone()],
            kinds: vec![StepType::new("about/line")],
            order: Order::SeqDesc,
            limit: Some(1),
            ..Default::default()
        })
        .await
        .ok()?;
    let state = steps.first()?.body.get("state")?.as_str()?.trim();
    // A table cell, not markdown: the writer's backticks and stars are noise here (the rail
    // cleans its copy the same way).
    let state: String = state
        .chars()
        .filter(|c| !matches!(c, '`' | '*' | '_'))
        .collect();
    let state = state.split_whitespace().collect::<Vec<_>>().join(" ");
    (!state.is_empty()).then_some(state)
}

/// The first `max` characters, marked with `…` when cut — a table cell, not a paragraph.
fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('\u{2026}');
    out
}

/// Whether the newest `agent/dormancy` step on `traj` says the lane is asleep.
async fn dormant_now(
    ledger: &bough_plugin_ledger::LedgerHandle,
    traj: &bough_plugin_ledger::TrajId,
) -> bool {
    use bough_plugin_ledger::{Order, StepQuery, StepType};
    ledger
        .0
        .steps(&StepQuery {
            trajs: vec![traj.clone()],
            kinds: vec![StepType::new(DORMANCY_STEP)],
            order: Order::SeqDesc,
            limit: Some(1),
            ..Default::default()
        })
        .await
        .ok()
        .and_then(|steps| {
            steps
                .first()
                .and_then(|s| s.body.get("dormant").and_then(|v| v.as_bool()))
        })
        .unwrap_or(false)
}

#[async_trait::async_trait]
impl Command for Roster {
    async fn run(&self, _inv: Invocation, _cx: CommandCx) -> Result<CommandOutput, CommandError> {
        let Some(agents) = self.0 .0.agents.clone() else {
            return Err(CommandError::Failed(
                "this window has no agent roster bound".to_string(),
            ));
        };
        let roster = agents.list();
        if roster.is_empty() {
            // Output, or a reason — never an empty pane (M27).
            return Ok(CommandOutput {
                text: "no agents are running".to_string(),
                render: OutputRender::Plain,
                cites: Vec::new(),
            });
        }
        // Dormancy is not a status (§1), so the live registry cannot say it — but the rail does,
        // and two surfaces disagreeing about the one state that decides whether mail is answered
        // was visual audit F13. Read it the way the rail does: the newest `agent/dormancy` step
        // on the lane, by NAME, with no dependency on the `dormancy` row (P3-D11).
        let ledger = self.0 .0.ledger.lock().clone();
        let mut lines = vec![ROSTER_HEADER.to_string()];
        for a in roster {
            let mut status = format!("{:?}", a.status()).to_lowercase();
            // `doing` is the about-line's STATE half — what the lane last said it did — not the
            // trajectory id, which is an internal name (visual audit follow-up).
            // A lane that has never written an about-line has nothing to say yet; its trajectory
            // id is an internal name, not an answer.
            let mut doing = "nothing yet".to_string();
            if let Some(ledger) = &ledger {
                if dormant_now(ledger, a.traj()).await {
                    status = "dormant".to_string();
                }
                if let Some(state) = about_state_now(ledger, a.traj()).await {
                    doing = state;
                }
            }
            lines.push(format!(
                "{:<12} {:<9} {:<30} {}",
                a.name(),
                status,
                clip(&doing, 30),
                a.inbox().len()
            ));
        }
        Ok(CommandOutput {
            text: lines.join("\n"),
            render: OutputRender::KeyValue,
            cites: Vec::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// M16: no built-in summary may use this tree's internal vocabulary.
    #[test]
    fn every_builtin_summary_is_plain_language() {
        for (name, summary) in summaries() {
            assert_eq!(
                bough_plugin_commands::palette::house_word(summary),
                None,
                "/{name}: `{summary}`"
            );
        }
    }

    /// The header names every column the rows print.
    #[test]
    fn the_roster_header_names_its_columns() {
        for column in ["agent", "status", "doing", "waiting"] {
            assert!(ROSTER_HEADER.contains(column), "{ROSTER_HEADER}");
        }
    }
}
