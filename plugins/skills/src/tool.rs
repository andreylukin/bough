//! Invariant: loading a skill is a LEDGERED ACT. The `skill` tool returns exactly the pool's
//! body for the named skill, so what reached the model is reconstructible from its `tool/result`
//! step with no section machinery — model-visible ⟺ ledgered (§0.2) holds by construction.

use std::sync::Arc;

use bough_plugin_tools::{
    FailureClass, RenderIntent, Tool, ToolCall, ToolCx, ToolFailure, ToolName, ToolOutcome,
    ToolScope, ToolSpec,
};

use crate::registry::Pool;

/// The tool's name, spelled once. The catalog section tells the model to call it.
pub const SKILL_TOOL: &str = "skill";

/// What the model asks with.
#[derive(serde::Deserialize, schemars::JsonSchema)]
struct SkillArgs {
    /// The skill's name, exactly as the Skills catalog lists it.
    name: String,
}

/// The registration. The pool is CAPTURED, not resolved from `ToolCx::ctx` (the drafts rule).
pub fn spec(pool: Arc<Pool>) -> ToolSpec {
    ToolSpec {
        name: ToolName::new(SKILL_TOOL),
        description: "Load one skill from the Skills catalog by name. Returns the skill's full \
                      instructions; call it before doing the task the skill covers."
            .to_string(),
        input_schema: schemars::SchemaGenerator::default().into_root_schema_for::<SkillArgs>(),
        render: RenderIntent::Generic,
        scope: ToolScope::Global,
        tool: Arc::new(SkillTool { pool }),
    }
}

/// The loader over one pool.
struct SkillTool {
    pool: Arc<Pool>,
}

#[async_trait::async_trait]
impl Tool for SkillTool {
    /// A read of an in-memory snapshot: two loads at once cannot interfere.
    fn is_concurrency_safe(&self, _args: &serde_json::Value) -> bool {
        true
    }

    async fn call(&self, call: Arc<ToolCall>, _cx: ToolCx) -> Result<ToolOutcome, ToolFailure> {
        let args: SkillArgs =
            serde_json::from_value(call.args.clone()).map_err(|e| ToolFailure {
                kind: FailureClass::Error,
                message: format!("bad arguments for `{}`: {e}", call.name),
            })?;
        let want = args.name.trim();
        let skills = self.pool.snapshot();
        let Some(skill) = skills.iter().find(|s| s.name == want) else {
            // A miss the model can fix by calling again with a listed name is `Denied`, not
            // `NotFound` — `NotFound` means the TOOL does not exist (§9).
            let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
            return Err(ToolFailure {
                kind: FailureClass::Denied,
                message: format!(
                    "no skill named `{want}`; the catalog has: {}",
                    names.join(", ")
                ),
            });
        };
        Ok(ToolOutcome {
            content: format!("# Skill: {}\n\n{}", skill.name, skill.body.trim_end()),
            value: None,
            cites: Vec::new(),
            concludes_wake: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bough_plugin_projection::SectionId;

    fn pool_with(names: &[&str]) -> Arc<Pool> {
        let pool = Arc::new(Pool::default());
        for n in names {
            std::mem::forget(pool.insert(Arc::new(crate::parse::Skill {
                id: SectionId::new(format!("skill:{n}")),
                name: n.to_string(),
                description: format!("about {n}"),
                triggers: vec![],
                body: format!("how to {n}"),
            })));
        }
        pool
    }

    #[tokio::test]
    async fn a_named_skill_loads_and_a_miss_lists_the_catalog() {
        let tool = SkillTool {
            pool: pool_with(&["review", "deploy"]),
        };
        let call = |name: &str| {
            Arc::new(ToolCall {
                id: bough_plugin_tools::ToolCallId::new("c1"),
                name: ToolName::new(SKILL_TOOL),
                args: serde_json::json!({ "name": name }),
                agent: bough_plugin_ledger::AgentName::new("sol"),
                wake: bough_plugin_ledger::WakeId::new("w1"),
                step_index: 0,
            })
        };
        let cx = || ToolCx {
            ctx: bough_kernel::Context::root(bough_kernel::KernelCore::new()),
            cancel: tokio_util::sync::CancellationToken::new(),
            deadline: None,
            initiator: None,
        };
        let out = tool.call(call("review"), cx()).await.expect("loads");
        assert!(out.content.contains("how to review"), "{}", out.content);
        let err = tool.call(call("nope"), cx()).await.expect_err("refused");
        assert_eq!(err.kind, FailureClass::Denied);
        assert!(err.message.contains("deploy"), "{}", err.message);
    }
}
