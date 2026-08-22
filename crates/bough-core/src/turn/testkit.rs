//! Shared test plumbing for the turn suite: the scripted `LlmClient`, the
//! canned deps, and the ctx builder. Test-only — mirrors the fixture half of
//! `runner.test.ts`/`queue.test.ts`/`state.test.ts`.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, RwLock};

use async_trait::async_trait;
use futures::FutureExt;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::bus::Bus;
use crate::harness::protocol::ProgramResult;
use crate::prompt::assemble::AssembledPrompt;
use crate::schema::parts::Usage;
use crate::turn::queue::TurnRegistry;
use crate::turn::runner::{ProgramRunner, TurnDeps, RUN_STEPS, STOP};
use crate::types::{
    system_clock, AppCtx, HostState, LlmBlock, LlmClient, LlmParams, LlmResult, OnText, SharedDb,
};
use bough_llm::LlmError;

// ---- block builders ---------------------------------------------------------

pub fn text(t: &str) -> LlmBlock {
    LlmBlock::Text {
        text: t.to_string(),
    }
}

pub fn reasoning(t: &str, meta: Option<Value>) -> LlmBlock {
    LlmBlock::Reasoning {
        text: t.to_string(),
        meta,
    }
}

pub fn run_steps(id: &str, code: &str) -> LlmBlock {
    LlmBlock::ToolUse {
        id: id.to_string(),
        name: RUN_STEPS.to_string(),
        input: json!({ "code": code }),
    }
}

pub fn stop(id: &str) -> LlmBlock {
    LlmBlock::ToolUse {
        id: id.to_string(),
        name: STOP.to_string(),
        input: json!({}),
    }
}

// ---- the scripted client ----------------------------------------------------

/// One scripted round: what the fake model answers, or what it throws.
#[derive(Default)]
pub struct ScriptedRound {
    pub content: Vec<LlmBlock>,
    /// Streamed through `on_text` before the round resolves.
    pub deltas: Vec<String>,
    pub usage: Option<Usage>,
    pub throws: Option<LlmError>,
}

/// A scripted `LlmClient` that snapshots every call — the runner mutates its
/// `messages` between rounds, so the clone at call time is what a test can
/// assert on.
pub struct ScriptedLlm {
    rounds: Mutex<VecDeque<ScriptedRound>>,
    calls: Mutex<Vec<LlmParams>>,
}

impl ScriptedLlm {
    pub fn calls(&self) -> Vec<LlmParams> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl LlmClient for ScriptedLlm {
    async fn run(
        &self,
        params: LlmParams,
        on_text: OnText,
        _cancel: CancellationToken,
    ) -> Result<LlmResult, LlmError> {
        let n = {
            let mut calls = self.calls.lock().unwrap();
            calls.push(params);
            calls.len()
        };
        let round = self.rounds.lock().unwrap().pop_front().ok_or_else(|| {
            // Non-retryable on purpose: a script that runs dry is a broken
            // test, and retrying it three times only hides which round did it.
            LlmError::with(
                format!("the fake model ran out of script after {} round(s)", n - 1),
                400,
                None,
            )
        })?;
        if let Some(err) = round.throws {
            return Err(err);
        }
        for d in &round.deltas {
            on_text(d);
        }
        Ok(LlmResult {
            content: round.content,
            stop_reason: "end_turn".to_string(),
            usage: round.usage,
        })
    }
}

pub fn scripted_llm(rounds: Vec<ScriptedRound>) -> Arc<ScriptedLlm> {
    Arc::new(ScriptedLlm {
        rounds: Mutex::new(rounds.into()),
        calls: Mutex::new(vec![]),
    })
}

/// A model that never answers: the crash test's turn is genuinely mid-round
/// when the "process dies".
struct WedgedLlm;

#[async_trait]
impl LlmClient for WedgedLlm {
    async fn run(
        &self,
        _params: LlmParams,
        _on_text: OnText,
        _cancel: CancellationToken,
    ) -> Result<LlmResult, LlmError> {
        futures::future::pending::<Result<LlmResult, LlmError>>().await
    }
}

pub fn wedged_llm() -> Arc<dyn LlmClient> {
    Arc::new(WedgedLlm)
}

/// A model that answers every call with the same text and a `stop`.
struct AnsweringLlm {
    text: String,
}

#[async_trait]
impl LlmClient for AnsweringLlm {
    async fn run(
        &self,
        _params: LlmParams,
        _on_text: OnText,
        _cancel: CancellationToken,
    ) -> Result<LlmResult, LlmError> {
        Ok(LlmResult {
            content: vec![text(&self.text), stop("stop-1")],
            stop_reason: "end_turn".to_string(),
            usage: None,
        })
    }
}

pub fn answering_llm(reply: &str) -> Arc<dyn LlmClient> {
    Arc::new(AnsweringLlm {
        text: reply.to_string(),
    })
}

// ---- ctx and deps -----------------------------------------------------------

/// An `AppCtx` over the given db and client: fresh bus, fresh registry, no
/// cheap tier, real clock.
pub fn test_ctx(db: SharedDb, llm: Arc<dyn LlmClient>) -> AppCtx {
    AppCtx {
        db,
        bus: Arc::new(Bus::new(system_clock())),
        llm: Some(llm),
        model: Some("claude-opus-4-8".to_string()),
        effort: None,
        now: system_clock(),
        cheap: None,
        host: Arc::new(HostState::new()),
        starter: Arc::new(RwLock::new(None)),
        turn_registry: Arc::new(TurnRegistry::new()),
        model_defaults_path: None,
    }
}

/// A program runner that succeeds and prints nothing.
pub fn ok_program() -> ProgramRunner {
    Arc::new(|_run| {
        async {
            ProgramResult {
                ok: true,
                logs: vec![],
                error: None,
                interrupted: None,
            }
        }
        .boxed()
    })
}

/// The canned deps every non-runner test drives turns with: a stub prompt, a
/// fake program, a silent error collector, and no outage waits.
pub fn stub_deps() -> TurnDeps {
    TurnDeps {
        assemble: Some(Arc::new(|_input| AssembledPrompt {
            system: "SYSTEM".to_string(),
            system_volatile: String::new(),
            sections: vec![],
            shas: vec![],
        })),
        program: Some(ok_program()),
        outage_delay_ms: Some(0),
        report_error: Some(Arc::new(|_err, _sid| {})),
        ..Default::default()
    }
}
