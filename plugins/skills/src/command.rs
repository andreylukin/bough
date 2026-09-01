//! Invariant: `/skill-name` INJECTS, it never runs the skill itself. The command asks the
//! focused agent — durably, through `Agent::followup` — to load the skill with the `skill` tool,
//! which is the ledgered act; a palette row that could execute a skill without a model in the
//! loop would be a second, unledgered path to the same behavior (P3-D8's spirit).

use std::sync::Arc;

use bough_plugin_agents::{MailClass, Message, MessageId, Sender};
use bough_plugin_commands::{
    Command, CommandCx, CommandError, CommandName, CommandOutput, CommandScope, CommandSpec,
    Invocation, OutputRender,
};

use crate::parse::Skill;

/// PURE: the message a `/name [task]` sends on the user's behalf.
pub fn instruction(name: &str, task: &str) -> String {
    if task.trim().is_empty() {
        format!("Load the skill `{name}` with the `skill` tool and follow it.")
    } else {
        format!(
            "Load the skill `{name}` with the `skill` tool, then apply it to: {}",
            task.trim()
        )
    }
}

/// PURE: the palette row's one-line summary — the skill's description, first sentence, capped.
pub fn summary_of(skill: &Skill) -> String {
    let d = skill.description.trim();
    if d.is_empty() {
        return "load this skill".to_string();
    }
    let first = d.split_once(". ").map(|(s, _)| s).unwrap_or(d);
    let mut out: String = first.chars().take(80).collect();
    if first.chars().count() > 80 {
        out.push('\u{2026}');
    }
    out
}

/// The registration for one skill.
pub fn spec(skill: Arc<Skill>) -> CommandSpec {
    CommandSpec {
        name: CommandName::new(skill.name.clone()),
        summary: summary_of(&skill),
        // Just the name: `[task]` repeated on every row was palette noise, and the free-text
        // argument is obvious from use.
        usage: format!("/{}", skill.name),
        args: schemars::SchemaGenerator::default().into_root_schema_for::<SkillCommandArgs>(),
        scope: CommandScope::Global,
        run: Arc::new(SkillCommand { skill }),
    }
}

/// What `/name` takes after the name.
#[derive(serde::Deserialize, schemars::JsonSchema)]
struct SkillCommandArgs {
    /// What to apply the skill to; empty = just load and follow it.
    #[serde(default)]
    #[allow(dead_code)]
    task: String,
}

struct SkillCommand {
    skill: Arc<Skill>,
}

#[async_trait::async_trait]
impl Command for SkillCommand {
    async fn run(&self, inv: Invocation, cx: CommandCx) -> Result<CommandOutput, CommandError> {
        let name = self.skill.name.as_str();
        let Some(agent) = cx.agent else {
            return Err(CommandError::Failed(format!(
                "/{name} needs a focused agent to hand the skill to"
            )));
        };
        let text = instruction(name, &inv.args.join(" "));
        agent
            .followup(Message {
                id: MessageId::new(uuid::Uuid::now_v7().to_string()),
                from: Sender::Andrey,
                class: MailClass::Wake,
                text,
                subject: format!("/{name}"),
                cites: Vec::new(),
                refs: Default::default(),
                mail_seq: None,
                at: cx.at,
            })
            .await
            .map_err(|e| CommandError::Failed(e.to_string()))?;
        Ok(CommandOutput {
            text: format!("asked the agent to load skill `{name}`"),
            render: OutputRender::Plain,
            cites: Vec::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bough_plugin_projection::SectionId;

    fn skill(name: &str, description: &str) -> Arc<Skill> {
        Arc::new(Skill {
            id: SectionId::new(format!("skill:{name}")),
            name: name.to_string(),
            description: description.to_string(),
            triggers: vec![],
            body: "b".into(),
        })
    }

    #[test]
    fn the_instruction_names_the_skill_and_carries_the_task() {
        assert_eq!(
            instruction("monarch", ""),
            "Load the skill `monarch` with the `skill` tool and follow it."
        );
        assert_eq!(
            instruction("monarch", " list accounts "),
            "Load the skill `monarch` with the `skill` tool, then apply it to: list accounts"
        );
    }

    #[test]
    fn the_spec_reads_as_a_palette_row() {
        let s = spec(skill(
            "monarch",
            "Query Monarch Money. Use when budgets come up.",
        ));
        assert_eq!(s.name.as_str(), "monarch");
        assert_eq!(s.usage, "/monarch");
        assert_eq!(s.summary, "Query Monarch Money");
        assert_eq!(spec(skill("x", "")).summary, "load this skill");
    }
}
