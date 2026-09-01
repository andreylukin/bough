//! §5: `tell` — mail a SIBLING lane.
//!
//! The gap this closes (Andrey, 2026-09-01): `inbox()` READ mail and nothing wrote it. Lanes could
//! not address each other at all — `schedule` mails your own future self and `spawn_worker` makes
//! a child, neither of which is a peer. So the curator lane shipped with two personas instructing
//! trunk to mail it lessons, and trunk spent an afternoon unable to obey them.
//!
//! Two decisions, both Andrey's (2026-09-01):
//! - ANY lane may tell ANY lane. Lanes are peers; a routing table here would be a second, weaker
//!   spelling of the leader's own hand.
//! - QUEUED by default (`Ordinary` mail, no wake): the recipient reads it on its next wake, so a
//!   chatty lane costs nothing until the reader was going to run anyway. `wake: true` is the
//!   explicit exception for something that cannot wait, and it says so in its own word.

use std::sync::Arc;

use bough_kernel::{Context, Plugin, PluginError};
use bough_plugin_agents::{Agents, MailClass, Message, Sender, Target};
use bough_plugin_ledger::AgentName;
use bough_plugin_tools::{
    FailureClass, RenderIntent, Tool, ToolCall, ToolCx, ToolFailure, ToolName, ToolOutcome,
    ToolScope, ToolSpec, Tools,
};

pub const PLUGIN_NAME: &str = "tool-tell";

/// What `tell` takes from the model.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct TellArgs {
    /// The lane to mail, by name.
    pub lane: String,
    /// What to say. The recipient sees this and nothing of your conversation, so it carries its
    /// own context — step ids included when the point is evidence.
    pub message: String,
    /// A one-line subject; the rail and the mail band show it.
    #[serde(default)]
    pub subject: Option<String>,
    /// Wake the lane NOW instead of leaving the mail for its next wake. The exception, not the
    /// habit: an immediate wake spends a turn's worth of model on the recipient's behalf.
    #[serde(default)]
    pub wake: bool,
    /// The documented third positional (`tell(lane, message, opts)`), carrying the same two
    /// fields. Code mode binds positionals by NAME (`tell#lane,message,opts`), so the object the
    /// surface promises has to exist in the schema — the lesson `agent(task, opts)` taught on
    /// 2026-09-01, applied before it could bite twice.
    #[serde(default)]
    pub opts: Option<TellOpts>,
}

/// `tell(lane, message, opts)`'s options object.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct TellOpts {
    #[serde(default)]
    pub subject: Option<String>,
    #[serde(default)]
    pub wake: Option<bool>,
}

/// PURE: the subject a call carries, or one derived from the message's first line. A blank
/// subject renders as an empty row in the mail band, which is how mail becomes unreadable.
pub fn subject_of(args: &TellArgs) -> String {
    if let Some(s) = args.subject.as_ref() {
        let s = s.trim();
        if !s.is_empty() {
            return s.chars().take(80).collect();
        }
    }
    let line = args.message.trim().lines().next().unwrap_or("").trim();
    if line.is_empty() {
        return "(no subject)".to_string();
    }
    line.chars().take(80).collect()
}

struct TellTool;

impl TellTool {
    pub fn spec() -> ToolSpec {
        ToolSpec {
            name: ToolName::new("tell"),
            description: "Mail another lane. It reads this on its NEXT wake (pass `wake: true` \
                          only for something that cannot wait). The lane sees your message and \
                          nothing of this conversation, so say everything it needs — cite step \
                          ids when the point is evidence."
                .to_string(),
            input_schema: schemars::SchemaGenerator::default().into_root_schema_for::<TellArgs>(),
            render: RenderIntent::Generic,
            scope: ToolScope::Global,
            tool: Arc::new(TellTool),
        }
    }
}

#[async_trait::async_trait]
impl Tool for TellTool {
    fn is_concurrency_safe(&self, _args: &serde_json::Value) -> bool {
        true
    }

    async fn call(&self, call: Arc<ToolCall>, cx: ToolCx) -> Result<ToolOutcome, ToolFailure> {
        let args: TellArgs = serde_json::from_value(call.args.clone()).map_err(|e| ToolFailure {
            kind: FailureClass::Denied,
            message: format!("bad arguments for `tell`: {e}"),
        })?;
        // The opts spelling wins over the flat one when both are given.
        let mut args = args;
        if let Some(opts) = args.opts.take() {
            if opts.subject.is_some() {
                args.subject = opts.subject;
            }
            if let Some(wake) = opts.wake {
                args.wake = wake;
            }
        }
        let lane = args.lane.trim().to_string();
        if lane.is_empty() {
            return Err(ToolFailure {
                kind: FailureClass::Denied,
                message: "`tell` needs a lane to address".to_string(),
            });
        }
        if args.message.trim().is_empty() {
            return Err(ToolFailure {
                kind: FailureClass::Denied,
                message: "`tell` needs something to say".to_string(),
            });
        }
        let agents = cx.ctx.get::<Agents>().map_err(|e| ToolFailure {
            kind: FailureClass::NotFound,
            message: format!("agents registry unavailable: {e}"),
        })?;
        let name = AgentName::new(&lane);
        // A lane that is not live is a REFUSAL with the roster in it, never a silent drop: mail
        // addressed to nobody would look sent.
        let Some(target) = agents.by_name(&name) else {
            let live: Vec<String> = agents
                .list()
                .iter()
                .map(|a| a.name().to_string())
                .collect();
            return Err(ToolFailure {
                kind: FailureClass::NotFound,
                message: format!("no lane named `{lane}`; live lanes: {}", live.join(", ")),
            });
        };
        // The CALLER is on the call (`ToolCall::agent`), not inferred from an ambient initiator
        // — which resolved to `None` here and made every lesson look like Andrey wrote it, the
        // one thing the curator must not believe (found live, 2026-09-01).
        let me = Some(call.agent.clone());
        if me.as_ref() == Some(&name) {
            return Err(ToolFailure {
                kind: FailureClass::Denied,
                message: "`tell` addresses another lane; use `schedule` to leave yourself a note"
                    .to_string(),
            });
        }
        let subject = subject_of(&args);
        let msg = Message {
            id: bough_plugin_agents::MessageId::new(uuid::Uuid::now_v7().to_string()),
            from: match me.clone() {
                Some(name) => Sender::Agent(name),
                None => Sender::Andrey,
            },
            class: if args.wake {
                MailClass::Wake
            } else {
                MailClass::Ordinary
            },
            text: args.message.clone(),
            subject: subject.clone(),
            cites: Vec::new(),
            refs: Default::default(),
            mail_seq: None,
            at: chrono::Utc::now(),
        };
        // Both land at the START of the next wake; the flag decides whether one is OPENED for it.
        // Queued mail costs the recipient nothing until it was going to run anyway.
        let sent = target.send(msg, Target::NextWake, args.wake).await;
        sent.map_err(|e| ToolFailure {
            kind: FailureClass::Error,
            message: format!("`tell` could not reach `{lane}`: {e}"),
        })?;
        Ok(ToolOutcome {
            content: if args.wake {
                format!("woke `{lane}`: {subject}")
            } else {
                format!("mailed `{lane}` (reads it on its next wake): {subject}")
            },
            value: Some(serde_json::json!({ "lane": lane, "subject": subject, "woke": args.wake })),
            cites: Vec::new(),
            concludes_wake: false,
        })
    }
}

/// The row.
pub struct TellToolPlugin;

#[async_trait::async_trait]
impl Plugin for TellToolPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = TellConfig;

    fn inject() -> bough_kernel::Inject {
        bough_kernel::Inject::required(["tools", "agents"])
    }

    fn validate(_cfg: &Self::Config) -> Result<(), bough_kernel::ConfigError> {
        Ok(())
    }

    async fn apply(ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let entry = ctx.entry_id().clone();
        let tools = ctx.get::<Tools>().map_err(|e| PluginError::new(entry, e))?;
        tools.register(&ctx, TellTool::spec()).await?;
        Ok(())
    }
}

/// No configuration: who may address whom is §5's, not a deployment's.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TellConfig {}

bough_kernel::register_plugin!(TellToolPlugin);

#[cfg(test)]
mod tests {
    use super::*;

    fn args(subject: Option<&str>, message: &str) -> TellArgs {
        TellArgs {
            lane: "cambium".to_string(),
            message: message.to_string(),
            subject: subject.map(str::to_string),
            wake: false,
            opts: None,
        }
    }

    #[test]
    fn a_missing_subject_comes_from_the_first_line() {
        assert_eq!(
            subject_of(&args(None, "gh 401s from an ssh resident\nkeychain is the cause")),
            "gh 401s from an ssh resident"
        );
        assert_eq!(subject_of(&args(Some("  "), "body")), "body");
        assert_eq!(subject_of(&args(None, "   ")), "(no subject)");
    }

    #[test]
    fn a_given_subject_wins_and_is_bounded() {
        assert_eq!(subject_of(&args(Some("a lesson"), "body")), "a lesson");
        let long = "x".repeat(200);
        assert_eq!(subject_of(&args(Some(&long), "body")).chars().count(), 80);
    }
}
