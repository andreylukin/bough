//! Invariant (§16): the keyboard path and the click path decide through the SAME seam. `/claims`,
//! `/accept`, `/edit` and `/reject` call [`crate::ClaimsHandle::decide`] with [`crate::Actor::Andrey`]
//! exactly as a click on a claim card does, so the two surfaces cannot drift apart.

use std::sync::Arc;

use bough_kernel::{Context, PluginError};
use bough_plugin_commands::{
    positional, Command, CommandCx, CommandError, CommandName, CommandOutput, CommandScope,
    CommandSpec, Commands, Invocation, OutputRender,
};

use crate::{
    Actor, ClaimId, ClaimQuery, ClaimsHandle, DecideOutcome, DecideRequest, Decision, OpenClaim,
};

/// Register `/claims`, `/accept <claim>`, `/edit <claim> <text…>` and `/reject <claim> <reason…>`,
/// if `commands` is bound.
pub async fn register(ctx: &Context, claims: &ClaimsHandle) -> Result<(), PluginError> {
    let commands = match ctx.try_get::<Commands>() {
        Ok(Some(c)) => c,
        Ok(None) => return Ok(()),
        Err(e) => return Err(PluginError::new(ctx.entry_id().clone(), e)),
    };
    commands
        .register(
            ctx,
            CommandSpec {
                name: CommandName::new("claims"),
                summary: "open claims awaiting a decision".to_string(),
                usage: "/claims".to_string(),
                args: positional(&[], 0),
                scope: CommandScope::Global,
                run: Arc::new(ListCommand(claims.clone())),
            },
        )
        .await?;
    for (name, summary, usage, min, shape) in [
        (
            "accept",
            "accept a claim as it stands",
            "/accept <claim>",
            1usize,
            Shape::Accept,
        ),
        (
            "edit",
            "accept a claim with edited text",
            "/edit <claim> <text…>",
            2,
            Shape::Edit,
        ),
        (
            "reject",
            "reject a claim, with a reason",
            "/reject <claim> <reason…>",
            2,
            Shape::Reject,
        ),
    ] {
        commands
            .register(
                ctx,
                CommandSpec {
                    name: CommandName::new(name),
                    summary: summary.to_string(),
                    usage: usage.to_string(),
                    args: positional(&["claim", "text"], min),
                    scope: CommandScope::Global,
                    run: Arc::new(DecideCommand {
                        claims: claims.clone(),
                        shape,
                        usage: usage.to_string(),
                    }),
                },
            )
            .await?;
    }
    Ok(())
}

/// Which decision a command makes.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Shape {
    Accept,
    Edit,
    Reject,
}

struct ListCommand(ClaimsHandle);

#[async_trait::async_trait]
impl Command for ListCommand {
    async fn run(&self, _inv: Invocation, _cx: CommandCx) -> Result<CommandOutput, CommandError> {
        let open = self
            .0
            .open(&ClaimQuery::default())
            .await
            .map_err(|e| CommandError::Failed(e.to_string()))?;
        Ok(CommandOutput {
            text: render_open(&open),
            render: OutputRender::KeyValue,
            cites: Vec::new(),
        })
    }
}

struct DecideCommand {
    claims: ClaimsHandle,
    shape: Shape,
    usage: String,
}

#[async_trait::async_trait]
impl Command for DecideCommand {
    async fn run(&self, inv: Invocation, cx: CommandCx) -> Result<CommandOutput, CommandError> {
        let (claim, rest) = inv
            .args
            .split_first()
            .ok_or_else(|| CommandError::BadArgs {
                usage: self.usage.clone(),
                detail: "a claim id is required".to_string(),
            })?;
        let text = rest.join(" ");
        let decision = decision_for(self.shape, &text).ok_or_else(|| CommandError::BadArgs {
            usage: self.usage.clone(),
            // A rejection with no reason is an unexplained refusal, and an edit with no text is
            // not an edit.
            detail: "the text is required".to_string(),
        })?;
        let outcome = self
            .claims
            .decide(DecideRequest {
                claim: ClaimId::new(claim),
                decision,
                actor: Actor::Andrey,
                at: cx.at,
            })
            .await
            .map_err(|e| CommandError::Failed(e.to_string()))?;
        Ok(CommandOutput {
            text: render_outcome(&outcome),
            render: OutputRender::KeyValue,
            cites: Vec::new(),
        })
    }
}

/// PURE: the decision a shape and its text make. `None` when the shape needs text and has none.
///
/// An `/edit` takes ONE text: its first line is the title, the rest is the body, so the pin the
/// acceptance sets carries both halves without a second argument.
pub fn decision_for(shape: Shape, text: &str) -> Option<Decision> {
    match shape {
        Shape::Accept => Some(Decision::Accept),
        Shape::Reject if !text.trim().is_empty() => Some(Decision::Reject {
            reason: text.to_string(),
        }),
        Shape::Edit if !text.trim().is_empty() => {
            let (title, body) = text.split_once('\n').unwrap_or((text, text));
            Some(Decision::Edit {
                title: title.trim().to_string(),
                body: body.trim().to_string(),
            })
        }
        _ => None,
    }
}

/// PURE: what `/claims` shows.
pub fn render_open(open: &[OpenClaim]) -> String {
    if open.is_empty() {
        return "no open claims\n".to_string();
    }
    let mut out = String::new();
    for c in open {
        out.push_str(&format!(
            "{} [{}] {} — {}\n",
            c.claim,
            c.kind.as_str(),
            c.by,
            c.title
        ));
    }
    out
}

/// PURE: what a decision reports back.
pub fn render_outcome(o: &DecideOutcome) -> String {
    let mut out = format!("claim {}: {}\n", o.claim, o.step);
    if let Some(pin) = &o.pin {
        out.push_str(&format!("pinned: {pin}\n"));
    }
    if let Some(born) = &o.born {
        out.push_str(&format!("lane born: {born}\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rejection_without_a_reason_is_refused_before_the_seam() {
        assert!(decision_for(Shape::Reject, "   ").is_none());
        assert!(matches!(
            decision_for(Shape::Reject, "out of scope"),
            Some(Decision::Reject { .. })
        ));
        // An accept needs nothing but the id.
        assert!(matches!(
            decision_for(Shape::Accept, ""),
            Some(Decision::Accept)
        ));
    }

    #[test]
    fn an_edit_splits_its_first_line_off_as_the_title() {
        match decision_for(Shape::Edit, "shorter title\nthe body, rewritten") {
            Some(Decision::Edit { title, body }) => {
                assert_eq!(title, "shorter title");
                assert_eq!(body, "the body, rewritten");
            }
            other => panic!("{other:?}"),
        }
        assert!(decision_for(Shape::Edit, "").is_none());
    }
}
