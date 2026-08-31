//! Invariant: `/pin` and `/unpin` are ANDREY'S HANDS on the pin stream, and the only writers of
//! `pin/set` / `pin/retire` in the tree since the claims demolition (2026-08-30) removed the
//! accepted-requirement path. A pin is a standing instruction that rides every projection of its
//! lane verbatim (§3, §5); withdrawing one is a `pin/retire` naming it, never a deletion — the
//! ledger stays append-only and `live_pins` folds the two.
//!
//! Both commands are appends under a synthetic wake (the `decide:` precedent): a pin is nobody's
//! wake output. `pin/set` is class Thought here — the ledger declares the type `Either`, and a
//! hand-typed instruction is a statement of intent, not evidence of anything.

use std::sync::Arc;

use bough_kernel::{Context, Inject, InvariantSpec, Plugin, PluginError};
use bough_plugin_commands::{
    positional_rest, Command, CommandCx, CommandError, CommandName, CommandOutput, CommandScope,
    CommandSpec, Commands, Invocation, OutputRender,
};
use bough_plugin_ledger::{
    AgentName, Append, Class, Ledger, LedgerHandle, StepId, StepType, TrajId, WakeId,
};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "pins";

/// The row's config. Empty: the commands need nothing configurable.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PinsConfig {}

/// The synthetic wake a hand-set pin is appended under.
pub fn pin_wake(agent: &AgentName) -> WakeId {
    WakeId::new(format!("pin:{agent}"))
}

/// PURE: `/pin`'s one text argument split the way `/edit` split its own: the first line is the
/// title, the rest (or the whole, single-line) is the text.
pub fn split_pin_text(text: &str) -> Option<(String, String)> {
    if text.trim().is_empty() {
        return None;
    }
    let (title, body) = text.split_once('\n').unwrap_or((text, text));
    Some((title.trim().to_string(), body.trim().to_string()))
}

/// The `pins` row.
pub struct PinsPlugin;

#[async_trait::async_trait]
impl Plugin for PinsPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = PinsConfig;

    fn inject() -> Inject {
        Inject::required(["ledger"]).union(&Inject::optional(["commands"]))
    }

    async fn apply(ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let entry = ctx.entry_id().clone();
        let ledger = (*ctx
            .get::<Ledger>()
            .map_err(|e| PluginError::new(entry.clone(), e))?)
        .clone();
        // ABSENT is headless: the row is inert with no surface at all (the dormancy precedent).
        let commands = match ctx.try_get::<Commands>() {
            Ok(Some(c)) => c,
            Ok(None) => return Ok(()),
            Err(e) => return Err(PluginError::new(entry, e)),
        };
        commands
            .register(
                &ctx,
                CommandSpec {
                    name: CommandName::new("pin"),
                    summary: "pin a standing instruction to an agent".to_string(),
                    usage: "/pin <agent> <text…>".to_string(),
                    args: positional_rest(&["agent", "text"], 2),
                    scope: CommandScope::Global,
                    run: Arc::new(PinCommand(ledger.clone())),
                },
            )
            .await?;
        commands
            .register(
                &ctx,
                CommandSpec {
                    name: CommandName::new("unpin"),
                    summary: "retire a pin, with a reason".to_string(),
                    usage: "/unpin <pin-step> <reason…>".to_string(),
                    args: positional_rest(&["pin", "reason"], 2),
                    scope: CommandScope::Global,
                    run: Arc::new(UnpinCommand(ledger)),
                },
            )
            .await?;
        Ok(())
    }

    fn invariants() -> Vec<InvariantSpec> {
        // §0.2: no runtime invariant. The one relation here — live pins are `pin/set` minus what
        // later steps name — is the LEDGER's (`live_pins`), owned and checked there.
        Vec::new()
    }
}

bough_kernel::register_plugin!(PinsPlugin);

struct PinCommand(LedgerHandle);

#[async_trait::async_trait]
impl Command for PinCommand {
    async fn run(&self, inv: Invocation, cx: CommandCx) -> Result<CommandOutput, CommandError> {
        let (agent, rest) = inv
            .args
            .split_first()
            .ok_or_else(|| CommandError::BadArgs {
                usage: "/pin <agent> <text…>".to_string(),
                detail: "an agent is required".to_string(),
            })?;
        let agent = AgentName::new(agent);
        let (title, text) =
            split_pin_text(&rest.join(" ")).ok_or_else(|| CommandError::BadArgs {
                usage: "/pin <agent> <text…>".to_string(),
                detail: "the text is required".to_string(),
            })?;
        let traj = traj_of(&self.0, &agent).await?;
        let step = self
            .0
             .0
            .append(Append {
                traj,
                wake: pin_wake(&agent),
                kind: StepType::new("pin/set"),
                class: Class::Thought,
                body: serde_json::json!({ "title": title, "text": text, "supersedes": [] }),
                cites: Vec::new(),
                at: cx.at,
                id: None,
            })
            .await
            .map_err(|e| CommandError::Failed(e.to_string()))?;
        Ok(CommandOutput {
            text: format!("pinned to {agent}: {}\n", step.id),
            render: OutputRender::KeyValue,
            cites: Vec::new(),
        })
    }
}

struct UnpinCommand(LedgerHandle);

#[async_trait::async_trait]
impl Command for UnpinCommand {
    async fn run(&self, inv: Invocation, cx: CommandCx) -> Result<CommandOutput, CommandError> {
        let (pin, rest) = inv
            .args
            .split_first()
            .ok_or_else(|| CommandError::BadArgs {
                usage: "/unpin <pin-step> <reason…>".to_string(),
                detail: "a pin step id is required".to_string(),
            })?;
        let reason = rest.join(" ");
        if reason.trim().is_empty() {
            return Err(CommandError::BadArgs {
                usage: "/unpin <pin-step> <reason…>".to_string(),
                detail: "the reason is required: an unexplained withdrawal is unreadable later"
                    .to_string(),
            });
        }
        let pin_id = StepId::new(pin);
        // The retire lands on the PIN'S OWN trajectory, and a `/unpin` of something that is not
        // a pin is refused rather than appended: `live_pins` would ignore it, and a step that
        // does nothing but exist is exactly what §0.2 refuses to write.
        let step = self
            .0
             .0
            .step(&pin_id)
            .await
            .map_err(|e| CommandError::Failed(e.to_string()))?
            .ok_or_else(|| CommandError::Failed(format!("no step `{pin_id}`")))?;
        if step.kind.as_str() != "pin/set" {
            return Err(CommandError::Failed(format!(
                "`{pin_id}` is a `{}` step, not a pin",
                step.kind
            )));
        }
        let retired = self
            .0
             .0
            .append(Append {
                traj: step.traj.clone(),
                wake: WakeId::new(format!("unpin:{pin_id}")),
                kind: StepType::new("pin/retire"),
                class: Class::Thought,
                body: serde_json::json!({ "retires": [pin_id.as_str()], "reason": reason }),
                cites: Vec::new(),
                at: cx.at,
                id: None,
            })
            .await
            .map_err(|e| CommandError::Failed(e.to_string()))?;
        Ok(CommandOutput {
            text: format!("retired {pin_id}: {}\n", retired.id),
            render: OutputRender::KeyValue,
            cites: Vec::new(),
        })
    }
}

/// The lane's trajectory, or a failure that names the agent.
async fn traj_of(ledger: &LedgerHandle, agent: &AgentName) -> Result<TrajId, CommandError> {
    ledger
        .0
        .agent(agent)
        .await
        .map_err(|e| CommandError::Failed(e.to_string()))?
        .map(|row| row.traj)
        .ok_or_else(|| CommandError::Failed(format!("no agent named `{agent}`")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_line_is_the_title_and_blank_text_is_refused() {
        assert_eq!(
            split_pin_text("cite everything\nevery claim of fact carries a step id"),
            Some((
                "cite everything".to_string(),
                "every claim of fact carries a step id".to_string()
            ))
        );
        // A single line is both halves, the `/edit` precedent.
        assert_eq!(
            split_pin_text("cite everything"),
            Some(("cite everything".to_string(), "cite everything".to_string()))
        );
        assert_eq!(split_pin_text("   "), None);
    }
}
