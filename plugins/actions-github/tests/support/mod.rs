#![allow(dead_code)]
//! A recording `gh`: it answers reads from a scripted table and records EVERY call, so a test can
//! assert both what was written and — for reconciliation — that nothing was.

use std::sync::Arc;

use bough_kernel::{Context, KernelCore};
use bough_plugin_actions::{
    ActionKind, ActionRequest, ActionTarget, ActionsHandle, ExecuteRequest,
};
use bough_plugin_actions_github::{GhActionError, GhRunner, GithubActions, GithubActionsConfig};
use bough_plugin_gh_cli::{GhError, GhOutput};
use bough_plugin_ledger::{AgentName, AgentRow, LedgerHandle, StepId, TrajId, WakeId};
use bough_plugin_ledger_memory::store::MemoryStore;
use chrono::{DateTime, TimeZone, Utc};
use parking_lot::Mutex;

pub const AGENT: &str = "sol";
pub const TRAJ: &str = "t1";

pub fn at() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, 1, 9, 0, 0).unwrap()
}

/// One recorded call.
#[derive(Clone, Debug, PartialEq)]
pub struct Call {
    pub argv: Vec<String>,
    pub write: bool,
}

#[derive(Default)]
pub struct FakeGh {
    pub calls: Arc<Mutex<Vec<Call>>>,
    /// `(substring of the argv, JSON to answer with)`, first match wins.
    pub reads: Vec<(String, serde_json::Value)>,
    /// What a write prints on stdout.
    pub write_stdout: String,
    pub me: String,
}

impl FakeGh {
    pub fn new(me: &str) -> FakeGh {
        FakeGh {
            me: me.into(),
            ..Default::default()
        }
    }
    pub fn read(mut self, matches: &str, body: serde_json::Value) -> FakeGh {
        self.reads.push((matches.to_string(), body));
        self
    }
    pub fn stdout(mut self, s: &str) -> FakeGh {
        self.write_stdout = s.into();
        self
    }
    pub fn log(&self) -> Vec<Call> {
        self.calls.lock().clone()
    }
}

#[async_trait::async_trait]
impl GhRunner for FakeGh {
    async fn json(&self, args: &[&str]) -> Result<serde_json::Value, GhError> {
        let argv: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        self.calls.lock().push(Call {
            argv: argv.clone(),
            write: false,
        });
        let joined = argv.join(" ");
        for (m, body) in &self.reads {
            if joined.contains(m.as_str()) {
                return Ok(body.clone());
            }
        }
        Err(GhError::BadJson {
            args: joined,
            detail: "the fake was not told how to answer this read".into(),
        })
    }
    async fn run(&self, args: &[&str], _stdin: Option<&str>) -> Result<GhOutput, GhError> {
        self.calls.lock().push(Call {
            argv: args.iter().map(|s| s.to_string()).collect(),
            write: true,
        });
        Ok(GhOutput {
            stdout: self.write_stdout.clone(),
            stderr: String::new(),
            code: 0,
        })
    }
    async fn whoami(&self) -> Result<String, GhError> {
        Ok(self.me.clone())
    }
}

pub fn cfg() -> Arc<GithubActionsConfig> {
    Arc::new(GithubActionsConfig {
        gh_bin: "gh".into(),
        known_bots: vec!["dependabot[bot]".into()],
        timeout_ms: 1000,
    })
}

pub fn provider(gh: &Arc<FakeGh>) -> Arc<GithubActions> {
    GithubActions::with_runner(cfg(), gh.clone() as Arc<dyn GhRunner>)
}

/// A mounted memory ledger with one agent row, and the actions seam over it.
pub async fn fixture() -> (Context, LedgerHandle, ActionsHandle) {
    let ctx = Context::root(KernelCore::new());
    let ledger = LedgerHandle(MemoryStore::new(ctx.clone()) as Arc<_>);
    ledger
        .0
        .put_agent(AgentRow {
            name: AgentName::new(AGENT),
            traj: TrajId::new(TRAJ),
            routing_refs: Default::default(),
            wake_classes: Default::default(),
            model_override: None,
            tick_floor: None,
            digest_rollup: None,
        })
        .await
        .expect("the agent row goes in");
    let actions = ActionsHandle::new(ledger.clone());
    (ctx, ledger, actions)
}

pub fn request(kind: ActionKind, target: &str, payload: serde_json::Value) -> ActionRequest {
    ActionRequest {
        kind,
        target: ActionTarget::new(target),
        payload,
        agent: AgentName::new(AGENT),
        wake: WakeId::new("w1"),
        step: StepId::new("s1"),
        at: at(),
    }
}

/// An `ExecuteRequest` built the way the seam builds one, for a direct Provider call.
pub fn exec(kind: ActionKind, target: &str, payload: serde_json::Value) -> ExecuteRequest {
    let req = request(kind, target, payload);
    let canonical = req
        .target
        .canonical(kind)
        .expect("the target canonicalises");
    let idem = bough_plugin_actions::idem_key(kind, &canonical, &req.step);
    ExecuteRequest {
        marker: bough_plugin_actions::marker_for(&idem),
        idem_key: idem.clone(),
        action: bough_plugin_ledger::ActionId::new("a1"),
        canonical_target: canonical,
        request: Arc::new(req),
    }
}

/// The last write the fake saw, or a panic naming what it did see.
pub fn last_write(gh: &Arc<FakeGh>) -> Call {
    gh.log()
        .into_iter()
        .rfind(|c| c.write)
        .unwrap_or_else(|| panic!("no write happened; the log was {:?}", gh.log()))
}

/// The `GhActionError` inside an `ActionError::Provider`.
pub fn refusal(e: bough_plugin_actions::ActionError) -> String {
    match e {
        bough_plugin_actions::ActionError::Provider { source, .. } => {
            let _ = source.downcast_ref::<GhActionError>();
            source.to_string()
        }
        other => panic!("expected a provider refusal, got {other}"),
    }
}
