//! Invariant: `run` is an ORDINARY tool. It is registered through `ToolsHandle::register` and
//! guarded by the same pipeline as any other, and every call it makes from inside the sandbox
//! goes through that same pipeline too — there is no back door around the seam.

use std::sync::Arc;

use bough_kernel::{Context, FiberUid};
use bough_plugin_js::{Caps, JsError, JsHandle, Program};
use bough_plugin_ledger::{Append, Class, LedgerHandle, StepType, TrajId};
use bough_plugin_tools::{
    FailureClass, RenderIntent, Tool, ToolCall, ToolCx, ToolFailure, ToolName, ToolOutcome,
    ToolScope, ToolSpec, ToolsHandle,
};

use crate::bind::{self, ProgramCx};
use crate::conceal::Concealment;
use crate::console::ConsoleTee;
use crate::vocabulary::ProgramErrorBody;
use crate::{CodemodeConfig, RUN_TOOL};

/// The ONE API tool.
pub struct Run {
    pub cfg: Arc<CodemodeConfig>,
    /// The plugin's own context: concealment effects belong to THIS fiber, not the caller's.
    pub ctx: Context,
    pub fiber: FiberUid,
    pub js: JsHandle,
    pub tools: ToolsHandle,
    pub ledger: LedgerHandle,
    pub conceal: Arc<Concealment>,
}

/// The deterministic id of the `n`-th inner call of the program `run` call `program`.
/// Deterministic ids are what make a replayed program reproduce the ledger it recorded.
pub fn inner_call_id(program: &str, n: u32) -> String {
    format!("{program}.{n}")
}

impl Run {
    fn caps(&self) -> Caps {
        self.cfg.caps.unwrap_or_else(|| self.js.default_caps())
    }

    /// Append the one terminal error a program can end with.
    async fn append_error(
        &self,
        traj: &TrajId,
        call: &ToolCall,
        error: JsError,
        ops: u64,
        ms: u64,
    ) {
        let body = ProgramErrorBody {
            program: call.id.clone(),
            error,
            ops,
            ms,
        };
        let Ok(body) = serde_json::to_value(&body) else {
            return;
        };
        let _ = self
            .ledger
            .0
            .append(Append {
                traj: traj.clone(),
                wake: call.wake.clone(),
                kind: StepType::new("program/error"),
                class: Class::Thought,
                body,
                cites: vec![],
                at: chrono::Utc::now(),
                id: None,
            })
            .await;
    }

    async fn traj_of(&self, call: &ToolCall) -> Result<TrajId, ToolFailure> {
        match self.ledger.0.agent(&call.agent).await {
            Ok(Some(row)) => Ok(row.traj),
            Ok(None) => Err(ToolFailure {
                kind: FailureClass::Error,
                message: format!(
                    "agent `{}` has no trajectory to write the program to",
                    call.agent
                ),
            }),
            Err(e) => Err(ToolFailure {
                kind: FailureClass::Error,
                message: format!("the ledger could not be read: {e}"),
            }),
        }
    }
}

#[async_trait::async_trait]
impl Tool for Run {
    /// Always exclusive: a program is a barrier by construction.
    fn is_concurrency_safe(&self, _args: &serde_json::Value) -> bool {
        false
    }

    /// 1. preflight `js.check` (syntax lands as `program/error` + a failed tool/result);
    /// 2. snapshot the agent's tools and build the mirror;
    /// 3. build the `HostFn`s (aliases, namespaces, the read/write concurrency lock);
    /// 4. `js.run`;
    /// 5. map the single terminal outcome.
    async fn call(&self, call: Arc<ToolCall>, cx: ToolCx) -> Result<ToolOutcome, ToolFailure> {
        let source = call
            .args
            .get("program")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolFailure {
                kind: FailureClass::Error,
                message: "`run` takes one string field, `program`".to_string(),
            })?
            .to_string();

        let traj = self.traj_of(&call).await?;
        let caps = self.caps();

        // 1. Preflight. A program that cannot parse never reaches the sandbox, and the model is
        //    told why in the same place every other program error lands.
        if let Err(error) = self.js.check(&source).await {
            let message = error.to_string();
            self.append_error(&traj, &call, error, 0, 0).await;
            return Err(ToolFailure {
                kind: FailureClass::Error,
                message,
            });
        }

        // 2. The snapshot, and the mirror it executes against.
        let mirror = self
            .conceal
            .snapshot(&self.ctx, &self.tools, &call.agent, caps.wall_ms)
            .await
            .map_err(|e| ToolFailure {
                kind: FailureClass::Error,
                message: format!("the tool snapshot could not be taken: {e}"),
            })?;

        // 3. The globals. A binding error is a MISCONFIGURATION and fails loud rather than
        //    silently shipping a smaller surface than the section documents.
        let bindings = bind::bindings(&mirror.specs, &self.cfg.aliases, &self.cfg.namespaces)
            .map_err(|e| ToolFailure {
                kind: FailureClass::Error,
                message: format!("the sandbox surface could not be built: {e}"),
            })?;
        let pcx = ProgramCx::new(
            cx.ctx.clone(),
            self.ledger.clone(),
            traj.clone(),
            call.wake.clone(),
            call.agent.clone(),
            call.step_index,
            call.id.clone(),
            mirror.tools.clone(),
            cx.cancel.clone(),
            self.cfg.max_calls_per_program,
            self.cfg.tags_required,
        );
        let by_name: std::collections::BTreeMap<String, &ToolSpec> = mirror
            .specs
            .iter()
            .map(|s| (s.name.to_string(), s))
            .collect();
        let host = bindings
            .iter()
            .filter_map(|b| {
                by_name
                    .get(&b.tool)
                    .map(|s| bind::host_fn(b, s, pcx.clone()))
            })
            .collect();

        // The console tee, and the task that turns its chunks into steps as they arrive.
        let (tee, mut rx) = ConsoleTee::new(call.id.clone(), self.cfg.max_console_bytes);
        let tee = Arc::new(tee);
        let drain = {
            let ledger = self.ledger.clone();
            let traj = traj.clone();
            let wake = call.wake.clone();
            tokio::spawn(async move {
                while let Some(body) = rx.recv().await {
                    let Ok(body) = serde_json::to_value(&body) else {
                        continue;
                    };
                    let _ = ledger
                        .0
                        .append(Append {
                            traj: traj.clone(),
                            wake: wake.clone(),
                            kind: StepType::new("program/console"),
                            class: Class::Thought,
                            body,
                            cites: vec![],
                            at: chrono::Utc::now(),
                            id: None,
                        })
                        .await;
                }
            })
        };

        // 4. Run it.
        let outcome = self
            .js
            .run(Program {
                source,
                caps,
                host,
                console: tee.clone(),
                cancel: cx.cancel.clone(),
            })
            .await;

        tee.finish();
        drop(tee.clone());
        // Close the channel by dropping the tee's sender: the tee is behind an Arc the drain does
        // not hold, so finishing the program ends the stream once this scope releases it.
        let console = tee.text();
        drop(tee);
        let _ = drain.await;
        mirror.dispose().await;

        let state = pcx.state();
        let ledgered = crate::invariant::Obs {
            fiber: self.fiber,
            program: call.id.to_string(),
            calls: state.calls.clone(),
            results: state.results.clone(),
            console: console.clone(),
            result_content: console.clone(),
        };

        // 5. The single terminal outcome.
        match outcome {
            Ok(_run) if state.cap_breach.is_none() => {
                crate::invariant::record(ledgered);
                // The truncation notice is already a `program/console` chunk, so the dropped
                // count needs no separate channel to the model.
                Ok(ToolOutcome {
                    content: console,
                    value: None,
                    cites: state.cites,
                    // `run` concludes the wake only because an inner result did.
                    concludes_wake: state.concludes_wake,
                })
            }
            Ok(run) => {
                // The program finished, but it had already spent its call budget: a breach is
                // terminal, so the round fails rather than reading as a clean answer.
                let message = state.cap_breach.unwrap_or_default();
                self.append_error(
                    &traj,
                    &call,
                    JsError::Thrown {
                        message: message.clone(),
                        stack: None,
                    },
                    run.ops,
                    run.ms,
                )
                .await;
                crate::invariant::record(ledgered);
                Err(ToolFailure {
                    kind: FailureClass::Blocked,
                    message: format!("{console}{message}"),
                })
            }
            Err(error) => {
                let kind = match &error {
                    JsError::TimeExceeded { .. } => FailureClass::Timeout,
                    JsError::Cancelled => FailureClass::Cancelled,
                    _ => FailureClass::Error,
                };
                let message = format!("{console}{error}");
                self.append_error(&traj, &call, error, 0, 0).await;
                crate::invariant::record(ledgered);
                Err(ToolFailure { kind, message })
            }
        }
    }
}

/// The input schema: exactly one string field. There are NO per-request schemas under code mode —
/// the surface is one projection section.
pub fn input_schema() -> schemars::Schema {
    schemars::Schema::try_from(serde_json::json!({
        "type": "object",
        "properties": {
            "program": {
                "type": "string",
                "description": "JavaScript. Every tool is a pre-injected async function; \
                                console.log is what comes back."
            }
        },
        "required": ["program"],
        "additionalProperties": false
    }))
    .expect("the `run` input schema is a literal and always parses")
}

/// The single spec this row registers.
pub fn spec(run: Arc<Run>) -> ToolSpec {
    ToolSpec {
        name: ToolName::new(RUN_TOOL),
        description: "Run one JavaScript program in the sandbox; the tools you have are \
                      pre-injected async functions and console.log output is what you get back \
                      (see the surface section for the full list)."
            .to_string(),
        input_schema: input_schema(),
        render: RenderIntent::Generic,
        scope: ToolScope::Global,
        tool: run,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inner_call_ids_are_deterministic_and_ordered() {
        assert_eq!(inner_call_id("call_7", 0), "call_7.0");
        assert_eq!(inner_call_id("call_7", 3), "call_7.3");
        // Replaying the same program mints the same ids, which is what makes a recorded
        // transcript reproduce the ledger it recorded.
        let first: Vec<String> = (0..3).map(|n| inner_call_id("c", n)).collect();
        let again: Vec<String> = (0..3).map(|n| inner_call_id("c", n)).collect();
        assert_eq!(first, again);
    }

    #[test]
    fn the_run_schema_has_exactly_one_string_field() {
        let schema = input_schema();
        let value = schema.as_value();
        let props = value["properties"].as_object().unwrap();
        assert_eq!(props.len(), 1);
        assert_eq!(props["program"]["type"], "string");
        assert_eq!(value["required"], serde_json::json!(["program"]));
    }
}
