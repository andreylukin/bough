//! Workflow lifecycle control, the SUBMIT boundary, and the run VIEW. Port of
//! `src/workflow/control.ts` (row 3.10).
//!
//! WHY THIS EXISTS. `workflow::engine` owns the engine — the worker, the
//! journal, the semaphore and the pause gate — and takes its [`AgentRunner`] as
//! a parameter so the whole thing is drivable offline. That parameter is a hole
//! exactly the size of this module: something has to turn `agent(prompt, opts)`
//! into a real subagent session, carry the run's abort into that session's TURN,
//! and hand the engine back a report.
//!
//! THE INVARIANT THIS HOLDS: **a control verb is not a status write.** `stop`
//! is only honest if the fan-out actually stops — the worker dies AND every
//! subagent turn the run started is interrupted, which is why
//! `engine::stop_workflow` aborts the run controller as well as terminating the
//! worker. `pause` is the mirror image: it must NOT reach a running agent,
//! because a paused run is one that stops *admitting* work, not one that
//! discards work already paid for. Both verbs live in `workflow::engine`
//! (row 3.9), where the live registry is; this module owns what a client SEES.
//!
//! WHAT IS HERE. The run view: journal rows in STRUCTURAL order with their live
//! activity, and the detail body `GET /workflows/:id` and `workflow.status`
//! both answer with.
//!
//! Two more things follow from "a run outlives the turn that started it":
//!
//!   - **The launch context is fabricated.** A REST-started run has no live turn
//!     to borrow a `TurnCtx` from, so one is built here from the owning session
//!     (its workspace, its model pin, its last message as the lineage anchor).
//!     The cancel token on it is deliberately inert: a workflow agent's
//!     interrupt travels on the RUN's token, which arrives per call.
//!   - **Subagent caps do not apply.** Every launch takes an exempt lease
//!     (spec §8): the run's own semaphore is the meter, and a 200-agent audit
//!     would not fit under a per-turn cap of 8. The NESTING rule still applies,
//!     because that one is about lifetime, not width.
//!
//! WHAT IS NOT HERE (and is NOT stubbed anywhere either): `append_workflow_part`,
//! `workflow_verb` and the bridged `workflow()` host fn. Those are the
//! PROGRAM-side surface — a model calling `workflow.start(...)` from inside a
//! turn — and they need the transcript-card sink and the confirm gate, neither
//! of which is ported. The REST surface below is complete and real.
//!
//! RUST DELTA. TS carries the injection seams on `ctx.workflowControl` because
//! `AppCtx` is frozen there. `AppCtx` is frozen here too, so the seams live in a
//! process-wide cell ([`set_workflow_control`] / [`workflow_control`]) that boot
//! fills once — the same "unwired degrades to production defaults, never to a
//! broken route" rule, spelled the way `turn::queue`'s registry was.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::agents::caps::DelegationMode;
use crate::agents::caps::{capped_launch, ReserveOptions};
use crate::agents::notes::{post_system_note, NoteDeps};
use crate::agents::subagent::{
    launch_subagent, LaunchDeps, SubagentLaunch, SubagentOptions, SubagentStatus,
};
use crate::bus::Bus;
use crate::errors::{BoughError, ErrorKind};
use crate::hostfn::ask::{AskInput, AskSettlement};
use crate::paths::workflow_script_path;
use crate::schema::events::{EventInput, EventType, MessageFinishedData, MessagePartData};
use crate::schema::parts::{Part, WorkflowAgent, WorkflowAgentStatus, WorkflowRun};
use crate::turn::queue::TurnRegistry;
use crate::turn::runner::{interrupt_turn, DEFAULT_MODEL};
use crate::types::{AppCtx, Clock, Patch, SharedDb, TurnCtx, WorkflowAgentPatch};

use super::engine::{
    is_workflow_live, pause_workflow, rerun_workflow, resume_workflow, start_workflow,
    stop_workflow, workflow_summary, RerunOpts, StartOpts, WorkflowCtx,
};
use super::journal_fs::resolve_rerun_script;
use super::key::clip;
use super::meta::{extract_meta, WorkflowMeta};
use super::pos::{compare_pos, split_journal_key};
use super::report::{run_accounting, AccountingOpts};
use super::runner::{AgentCall, AgentRunner, OnSpawned};
use super::structured::structured_workflow_ctx;

/// How many recent tool calls a row's activity trail carries.
const ACTIVITY_LINES: usize = 4;

/// First line of a tool call's input, clipped — enough to recognize, never a
/// dump. A code-mode call carries `{code}`; anything else stringifies.
fn gist(input: &Value, max: usize) -> String {
    let text = match input {
        Value::String(s) => s.clone(),
        Value::Object(o) => match o.get("code") {
            Some(Value::String(s)) => s.clone(),
            Some(other) => other.to_string(),
            None => String::new(),
        },
        Value::Null => String::new(),
        other => other.to_string(),
    };
    clip(text.trim().split('\n').next().unwrap_or(""), max)
}

/// Order journal rows by their structural coordinate, falling back to `idx` for
/// a row whose key predates coordinates. Stable: rows that compare equal keep
/// their query order, so a run written before coordinates existed still lists
/// sensibly.
///
/// STRUCTURAL order, not the `ORDER BY idx, rowid` the query returns. `idx` is
/// the order calls reached the host, which for a `pipeline()` — the one
/// combinator with no barrier — is LATENCY order and differs run to run.
/// Listing a fan-out that way gives a view that is neither the script's shape
/// nor stable across two runs of the same script, so the same workflow looks
/// different every time for no reason the reader can see.
///
/// Sorted here rather than in SQL because a coordinate is dot-separated
/// integers: lexicographic ordering puts "10" before "2".
pub fn sort_by_position(rows: Vec<WorkflowAgent>) -> Vec<WorkflowAgent> {
    let mut indexed: Vec<(usize, Option<String>, WorkflowAgent)> = rows
        .into_iter()
        .enumerate()
        .map(|(i, row)| {
            let pos = split_journal_key(&row.key).pos;
            (i, pos, row)
        })
        .collect();
    indexed.sort_by(|a, b| {
        if let (Some(pa), Some(pb)) = (&a.1, &b.1) {
            let by_pos = compare_pos(pa, pb);
            if by_pos != std::cmp::Ordering::Equal {
                return by_pos;
            }
        } else if a.2.idx != b.2.idx {
            return a.2.idx.cmp(&b.2.idx);
        }
        a.0.cmp(&b.0)
    });
    indexed.into_iter().map(|(_, _, row)| row).collect()
}

/// A journal row plus what it is currently costing and doing.
///
/// `WorkflowAgent & {tokens, toolCalls, activity, live}` in TS; flattened so
/// the wire shape is one object with the row's fields alongside these four.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowAgentView {
    #[serde(flatten)]
    pub agent: WorkflowAgent,
    pub tokens: i64,
    pub tool_calls: usize,
    /// The last few tool-call gists, `name(first line clipped to 48)`.
    pub activity: Vec<String>,
    /// Is a control handle holding this call right now, in THIS process?
    pub live: bool,
}

/// One run's journal rows, in structural order, with live activity.
///
/// `live_agent_ids` is what the control registry holds for this run. The
/// registry itself is the un-ported half of row 3.10; a caller with no registry
/// passes an empty slice, and every row reads `live: false` — which is the
/// truthful answer in a process that is holding no handles.
pub fn workflow_agent_views(
    db: &SharedDb,
    run_id: &str,
    live_agent_ids: &[String],
) -> Result<Vec<WorkflowAgentView>, BoughError> {
    let guard = db.lock().unwrap();
    let rows = sort_by_position(guard.list_workflow_agents(run_id)?);
    let mut out = Vec::with_capacity(rows.len());
    for agent in rows {
        let live = live_agent_ids.contains(&agent.id);
        let Some(session_id) = agent.session_id.clone() else {
            out.push(WorkflowAgentView {
                agent,
                tokens: 0,
                tool_calls: 0,
                activity: Vec::new(),
                live,
            });
            continue;
        };
        let usage = guard.session_usage(&session_id)?;
        let calls: Vec<(String, Value)> = guard
            .messages_for(&session_id)?
            .into_iter()
            .flat_map(|m| m.parts)
            .filter_map(|p| match p {
                Part::ToolCall { name, input, .. } => Some((name, input)),
                _ => None,
            })
            .collect();
        let activity = calls
            .iter()
            .rev()
            .take(ACTIVITY_LINES)
            .rev()
            .map(|(name, input)| format!("{name}({})", gist(input, 48)))
            .collect();
        out.push(WorkflowAgentView {
            agent,
            tokens: usage.input_tokens + usage.output_tokens,
            tool_calls: calls.len(),
            activity,
            live,
        });
    }
    Ok(out)
}

/// `GET /workflows/:id`'s body, and `workflow.status({id})`'s.
///
/// Carries the run, its journal rows with live activity, the script file,
/// whether the run is live in THIS process — and the three accounting fields
/// spec §8 requires of a run view:
///
/// - `replay` — how many calls were served from the journal and how many ran
///   live. Required, not decorative: a relaunch that replayed nothing is
///   otherwise indistinguishable from one that replayed everything.
/// - `cost` — tokens and elapsed time per agent and per phase, so an expensive
///   stage is visible while it runs rather than in the bill.
/// - `warning` — the advisory large-run flag, or `null`. Computed here, at view
///   time, so there is no path from it back into the engine.
///
/// `live` is not cosmetic: a run left `running` by a dead process is reconciled
/// to `orphaned` at boot, and a client that cannot tell the two apart shows a
/// fan-out that will never advance.
pub fn workflow_detail(
    db: &SharedDb,
    run: &WorkflowRun,
    live_agent_ids: &[String],
    at: i64,
) -> Result<Value, BoughError> {
    let accounting = run_accounting(db, run, at, AccountingOpts::default())?;
    Ok(json!({
        "workflow": run,
        "agents": workflow_agent_views(db, &run.id, live_agent_ids)?,
        "scriptFile": workflow_script_path(&run.id).to_string_lossy(),
        "live": is_workflow_live(&run.id),
        "replay": accounting.replay.to_json(),
        "cost": accounting.cost,
        "warning": accounting.warning,
        "guideline": accounting.guideline,
    }))
}

// ---------------------------------------------------------------------------
// The live-agent registry
// ---------------------------------------------------------------------------

/// One in-flight `agent()` call, as the control verbs see it.
///
/// Everything mutable is behind a lock because the handle is shared between the
/// runner attempt that owns it and whatever HTTP request presses `x` on it.
pub struct WorkflowAgentHandle {
    pub run_id: String,
    /// The journal row this call is running against.
    pub agent_id: String,
    /// The CURRENT attempt's interrupt. Replaced on a restart.
    ctrl: Mutex<CancellationToken>,
    /// Set by `restart` so the runner re-issues instead of failing the call.
    restart: Mutex<bool>,
    /// The subagent session, once it exists.
    session_id: Mutex<Option<String>>,
}

impl WorkflowAgentHandle {
    fn set_ctrl(&self, token: CancellationToken) {
        *self.ctrl.lock().unwrap_or_else(|e| e.into_inner()) = token;
    }
    fn set_restart(&self, v: bool) {
        *self.restart.lock().unwrap_or_else(|e| e.into_inner()) = v;
    }
    fn restart_wanted(&self) -> bool {
        *self.restart.lock().unwrap_or_else(|e| e.into_inner())
    }
    fn set_session(&self, id: &str) {
        *self.session_id.lock().unwrap_or_else(|e| e.into_inner()) = Some(id.to_string());
    }
    /// The subagent session this call is on right now, if it has one.
    pub fn session_id(&self) -> Option<String> {
        self.session_id
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

/// Which journal rows are live in this process, and how to reach them.
///
/// Process-wide by default, for the same reason the engine's own registry is: a
/// run outlives the request that started it, so a per-caller instance would hold
/// nothing by the time anyone pressed `x`. A test constructs its own and stays
/// isolated.
#[derive(Default)]
pub struct WorkflowAgentRegistry {
    by_run: Mutex<HashMap<String, Vec<Arc<WorkflowAgentHandle>>>>,
}

impl WorkflowAgentRegistry {
    pub fn new() -> WorkflowAgentRegistry {
        WorkflowAgentRegistry::default()
    }

    /// Bind a starting call to its journal row.
    ///
    /// The engine flips the row to `running` and calls the runner with no await
    /// between the two, so at this instant the row for THIS call is the
    /// lowest-`idx` running row nobody has claimed — every earlier one was
    /// claimed at its own start. That is why the pairing does not need (and must
    /// not use) the call key: the structured-output decorator rewrites the
    /// prompt before the runner sees it, so the key no longer matches the row
    /// the engine journaled.
    ///
    /// `None` when no unclaimed running row exists, which is not an error: the
    /// call still runs, it simply cannot be singled out.
    pub fn claim(&self, db: &SharedDb, run_id: &str) -> Option<Arc<WorkflowAgentHandle>> {
        let mut held = self.by_run.lock().unwrap_or_else(|e| e.into_inner());
        let entry = held.entry(run_id.to_string()).or_default();
        let rows = db
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .list_workflow_agents(run_id)
            .ok()?;
        let mut candidates: Vec<WorkflowAgent> = rows
            .into_iter()
            .filter(|a| {
                a.status == WorkflowAgentStatus::Running
                    && !entry.iter().any(|h| h.agent_id == a.id)
            })
            .collect();
        candidates.sort_by_key(|a| a.idx);
        let row = candidates.into_iter().next()?;
        let handle = Arc::new(WorkflowAgentHandle {
            run_id: run_id.to_string(),
            agent_id: row.id.clone(),
            ctrl: Mutex::new(CancellationToken::new()),
            restart: Mutex::new(false),
            session_id: Mutex::new(row.session_id.clone()),
        });
        entry.push(handle.clone());
        Some(handle)
    }

    /// Drop a settled call. Idempotent — the runner releases on every exit.
    pub fn release(&self, handle: &Arc<WorkflowAgentHandle>) {
        let mut held = self.by_run.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = held.get_mut(&handle.run_id) {
            entry.retain(|h| !Arc::ptr_eq(h, handle));
            if entry.is_empty() {
                held.remove(&handle.run_id);
            }
        }
    }

    pub fn get(&self, run_id: &str, agent_id: &str) -> Option<Arc<WorkflowAgentHandle>> {
        self.by_run
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(run_id)?
            .iter()
            .find(|h| h.agent_id == agent_id)
            .cloned()
    }

    /// The run's live calls. The run view's "what is actually in flight" answer.
    pub fn for_run(&self, run_id: &str) -> Vec<Arc<WorkflowAgentHandle>> {
        self.by_run
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(run_id)
            .cloned()
            .unwrap_or_default()
    }

    /// The journal-row ids [`workflow_agent_views`] marks `live`.
    pub fn live_ids(&self, run_id: &str) -> Vec<String> {
        self.for_run(run_id)
            .into_iter()
            .map(|h| h.agent_id.clone())
            .collect()
    }
}

/// The process-wide instance. Production's; a test builds its own.
pub fn workflow_agents() -> &'static Arc<WorkflowAgentRegistry> {
    static AGENTS: OnceLock<Arc<WorkflowAgentRegistry>> = OnceLock::new();
    AGENTS.get_or_init(|| Arc::new(WorkflowAgentRegistry::new()))
}

// ---------------------------------------------------------------------------
// Injection seams
// ---------------------------------------------------------------------------

/// `launch_subagent`'s shape, so a test can launch nothing.
pub type LaunchFn = Arc<
    dyn Fn(&TurnCtx, &str, &SubagentOptions, &LaunchDeps) -> Result<SubagentLaunch, BoughError>
        + Send
        + Sync,
>;

/// The seams, so every path below is drivable with no key and no subagent.
#[derive(Clone, Default)]
pub struct WorkflowControlDeps {
    /// The turn registry a child's interrupt goes through. Absent = the ctx's.
    pub registry: Option<Arc<TurnRegistry>>,
    /// Absent = the process-wide live-agent registry.
    pub agents: Option<Arc<WorkflowAgentRegistry>>,
    /// Absent = [`launch_subagent`].
    pub launch: Option<LaunchFn>,
    /// The child's launch deps — its turn deps, its wall clock, its diff seam.
    pub child: Option<Arc<dyn Fn(&TurnCtx) -> LaunchDeps + Send + Sync>>,
    /// Absent = [`post_system_note`], which wakes the owning session when a run
    /// ends.
    pub notify: Option<Arc<dyn Fn(&AppCtx, &str, &str) + Send + Sync>>,
    /// Decorate the assembled [`WorkflowCtx`]. Absent =
    /// [`structured_workflow_ctx`], the process default.
    pub decorate: Option<Arc<dyn Fn(WorkflowCtx) -> WorkflowCtx + Send + Sync>>,
    /// Injected clock. Absent = `ctx.now`.
    pub now: Option<Clock>,
    /// Where the transcript card for a launched run goes. Absent = straight
    /// onto the message, which is right for a REST launch and WRONG from
    /// inside a live turn — see [`create_workflow_host_fn`], which supplies the
    /// buffering sink.
    pub card: Option<Arc<dyn Fn(&WorkflowRun) + Send + Sync>>,
}

static CONTROL: OnceLock<WorkflowControlDeps> = OnceLock::new();

/// Boot wiring, once. A second call is ignored — the seams are process state,
/// not per-request state.
pub fn set_workflow_control(deps: WorkflowControlDeps) {
    let _ = CONTROL.set(deps);
}

/// The wired seams, or the production defaults. An unwired seam degrades to "no
/// test doubles", never to a broken route.
pub fn workflow_control() -> WorkflowControlDeps {
    CONTROL.get().cloned().unwrap_or_default()
}

fn workflow_error(status: u16, message: impl Into<String>) -> BoughError {
    BoughError::http(status, ErrorKind::Workflow, message)
}

fn agent_row(db: &SharedDb, run_id: &str, agent_id: &str) -> Option<WorkflowAgent> {
    db.lock()
        .unwrap_or_else(|e| e.into_inner())
        .list_workflow_agents(run_id)
        .ok()?
        .into_iter()
        .find(|a| a.id == agent_id)
}

fn publish_agent(db: &SharedDb, bus: &Bus, run_id: &str, agent_id: &str) {
    let run = db
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_workflow(run_id)
        .ok()
        .flatten();
    if let (Some(run), Some(row)) = (run, agent_row(db, run_id, agent_id)) {
        bus.publish(EventInput {
            r#type: EventType::WorkflowAgent,
            session_id: Some(run.session_id),
            data: serde_json::to_value(&row).unwrap_or(Value::Null),
        });
    }
}

// ---------------------------------------------------------------------------
// The production agent runner
// ---------------------------------------------------------------------------

/// Turn one `agent()` call into a real subagent, and carry the run's stop into
/// it.
///
/// The abort cascade is the whole point. A workflow's `stop` cancels the run's
/// token; without the cascade the script would stop and the children would keep
/// running — a fan-out billing with nobody left to read it. With it, the child's
/// turn is interrupted, persists its partial work, and reports `interrupted`,
/// which this fails with so the engine's combinators see a failure rather than a
/// truncated report (spec §8).
///
/// The `is_running` guard on the cascade matters as much: a child that already
/// resolved has its report and its outcome persisted on its own branch, and
/// interrupting it now would flip a finished session to `interrupted`.
pub struct SubagentRunner {
    turn_ctx: TurnCtx,
    launch: LaunchFn,
    registry: Arc<TurnRegistry>,
    child: Option<Arc<dyn Fn(&TurnCtx) -> LaunchDeps + Send + Sync>>,
}

pub fn create_subagent_runner(turn_ctx: TurnCtx, deps: &WorkflowControlDeps) -> SubagentRunner {
    let registry = deps
        .registry
        .clone()
        .unwrap_or_else(|| turn_ctx.app.turn_registry.clone());
    SubagentRunner {
        launch: deps
            .launch
            .clone()
            .unwrap_or_else(|| Arc::new(launch_subagent)),
        registry,
        child: deps.child.clone(),
        turn_ctx,
    }
}

fn status_word(status: SubagentStatus) -> &'static str {
    match status {
        SubagentStatus::Done => "done",
        SubagentStatus::Error => "error",
        SubagentStatus::Interrupted => "interrupted",
        SubagentStatus::Orphaned => "orphaned",
    }
}

#[async_trait]
impl AgentRunner for SubagentRunner {
    async fn run(
        &self,
        call: &AgentCall,
        cancel: CancellationToken,
        on_spawned: OnSpawned,
    ) -> Result<String, BoughError> {
        if cancel.is_cancelled() {
            return Err(workflow_error(
                409,
                "workflow stopped — this agent was never launched",
            ));
        }

        let opts = SubagentOptions {
            name: Some(Value::String(call.label.clone())),
            model: call.model.clone(),
            effort: None,
        };
        let child_deps = match &self.child {
            Some(f) => f(&self.turn_ctx),
            None => LaunchDeps::default(),
        };

        // Exempt from the width caps, never from the nesting rule (spec §8).
        let launched = capped_launch(
            &self.turn_ctx,
            &ReserveOptions {
                mode: Some(DelegationMode::Blocking),
                verb: Some("workflow agent()".to_string()),
                exempt: true,
                caps: None,
            },
            || (self.launch)(&self.turn_ctx, &call.prompt, &opts, &child_deps),
        )?;

        let session_id = launched.session_id.clone();
        on_spawned(&session_id);

        let fut = launched.result.clone();
        tokio::pin!(fut);
        let mut cascaded = false;
        let result = loop {
            tokio::select! {
                r = &mut fut => break r,
                _ = cancel.cancelled(), if !cascaded => {
                    cascaded = true;
                    if self.registry.is_running(&session_id) {
                        interrupt_turn(&session_id, &self.registry);
                    }
                }
            }
        };

        if !result.ok {
            // Named by status, not by a bare "failed": a stopped run, a child
            // that errored and a child the server restarted under call for
            // different moves from the script and from the person reading the
            // run view.
            let status = if result.status == SubagentStatus::Interrupted {
                409
            } else {
                424
            };
            let report = if result.report.is_empty() {
                "(no report)".to_string()
            } else {
                result.report.clone()
            };
            return Err(workflow_error(
                status,
                format!(
                    "workflow agent \"{}\" {}: {}",
                    call.label,
                    status_word(result.status),
                    clip(&report, 400),
                ),
            ));
        }
        Ok(result.report)
    }
}

// ---------------------------------------------------------------------------
// Single-agent control
// ---------------------------------------------------------------------------

/// The run id, known only after `start_workflow` returns — see
/// [`workflow_ctx_for`].
///
/// A watch channel rather than a oneshot because every concurrent call in the
/// fan-out reads it, and they all read the same answer.
pub struct RunBinding {
    tx: watch::Sender<Option<Option<String>>>,
}

impl RunBinding {
    fn new() -> (Arc<RunBinding>, watch::Receiver<Option<Option<String>>>) {
        let (tx, rx) = watch::channel(None);
        (Arc::new(RunBinding { tx }), rx)
    }

    /// Settle the binding. `None` means the start failed and nothing can ever
    /// claim — settled anyway, rather than leaving a wait nobody will release.
    pub fn bind(&self, run_id: Option<String>) {
        let _ = self.tx.send(Some(run_id));
    }
}

async fn await_binding(rx: &mut watch::Receiver<Option<Option<String>>>) -> Option<String> {
    loop {
        let seen = rx.borrow().clone();
        if let Some(v) = seen {
            return v;
        }
        if rx.changed().await.is_err() {
            return None;
        }
    }
}

/// Wrap the engine-facing runner so each call owns a claimable handle.
///
/// Wrapped OUTSIDE the structured-output decorator on purpose: one journal row
/// is one `agent()` call however many times a schema mismatch made it retry, so
/// the claim — and the restart loop — must span the retries rather than sit
/// inside one attempt.
struct ControlledRunner {
    db: SharedDb,
    bus: Arc<Bus>,
    binding: watch::Receiver<Option<Option<String>>>,
    inner: Arc<dyn AgentRunner>,
    agents: Arc<WorkflowAgentRegistry>,
}

#[async_trait]
impl AgentRunner for ControlledRunner {
    async fn run(
        &self,
        call: &AgentCall,
        cancel: CancellationToken,
        on_spawned: OnSpawned,
    ) -> Result<String, BoughError> {
        let mut rx = self.binding.clone();
        let run_id = await_binding(&mut rx).await;
        let handle = run_id
            .as_deref()
            .and_then(|rid| self.agents.claim(&self.db, rid));

        let out = loop {
            // A child token: the run's cancel cascades into it, and cancelling
            // it fails exactly this one call.
            let own = cancel.child_token();
            if let Some(h) = &handle {
                h.set_ctrl(own.clone());
                h.set_restart(false);
            }
            let hooked: OnSpawned = {
                let handle = handle.clone();
                let outer = on_spawned.clone();
                Arc::new(move |sid: &str| {
                    if let Some(h) = &handle {
                        h.set_session(sid);
                    }
                    outer(sid);
                })
            };
            match self.inner.run(call, own, hooked).await {
                Ok(report) => break Ok(report),
                Err(err) => {
                    // A restart re-issues the SAME call on a fresh subagent
                    // session. The script is still parked on the promise it was
                    // already awaiting, and the journal row is still `running` —
                    // the engine writes it only on settle — so the only repair
                    // needed is to unpoint it from the abandoned session.
                    let restarting = handle.as_ref().is_some_and(|h| h.restart_wanted());
                    if restarting && !cancel.is_cancelled() {
                        if let (Some(h), Some(rid)) = (&handle, run_id.as_deref()) {
                            let _ = self
                                .db
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .update_workflow_agent(
                                    &h.agent_id,
                                    WorkflowAgentPatch {
                                        session_id: Patch::Clear,
                                        error: Patch::Clear,
                                        ..Default::default()
                                    },
                                );
                            publish_agent(&self.db, &self.bus, rid, &h.agent_id);
                            continue;
                        }
                    }
                    break Err(err);
                }
            }
        };

        if let Some(h) = &handle {
            self.agents.release(h);
        }
        out
    }
}

/// The run view's `x` / `r` on one selected agent; the rest of the run
/// continues.
///
/// `stop` fails just that `agent()` call — the script sees the rejection and its
/// `parallel()` slot goes `null` or its `pipeline()` item drops. `restart`
/// re-issues it on a fresh subagent session.
///
/// NOTE (accepted delta, same as TS): a single-agent stop lands its journal row
/// as `error`, not `stopped`. The row's terminal write belongs to the engine,
/// which maps only a RUN-level abort to `stopped`; the error text says plainly
/// that the agent was stopped.
pub fn control_workflow_agent(
    db: &SharedDb,
    run_id: &str,
    agent_id: &str,
    action: &str,
    deps: &WorkflowControlDeps,
) -> Result<WorkflowAgent, BoughError> {
    if db
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_workflow(run_id)?
        .is_none()
    {
        return Err(BoughError::not_found(format!(
            "workflow {run_id} not found"
        )));
    }
    let row = agent_row(db, run_id, agent_id).ok_or_else(|| {
        BoughError::not_found(format!(
            "workflow agent {agent_id} not found in run {run_id}"
        ))
    })?;
    if row.status != WorkflowAgentStatus::Running {
        return Err(BoughError::http(
            409,
            ErrorKind::Conflict,
            format!(
                "workflow agent \"{}\" is {}, not running — only a running agent can be {}. \
                 Rerun the workflow to re-issue a finished call.",
                row.label,
                serde_json::to_value(row.status)
                    .ok()
                    .and_then(|v| v.as_str().map(String::from))
                    .unwrap_or_default(),
                if action == "stop" {
                    "stopped"
                } else {
                    "restarted"
                },
            ),
        ));
    }
    let agents = deps
        .agents
        .clone()
        .unwrap_or_else(|| workflow_agents().clone());
    let handle = agents.get(run_id, agent_id).ok_or_else(|| {
        BoughError::http(
            409,
            ErrorKind::Conflict,
            format!(
                "workflow agent \"{}\" is not live in this process — the server restarted \
                 since it started, so there is nothing here to {action}. Stop the run and \
                 rerun it: the journal replays everything that already succeeded.",
                row.label,
            ),
        )
    })?;
    handle.set_restart(action == "restart");
    handle
        .ctrl
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .cancel();
    Ok(agent_row(db, run_id, agent_id).unwrap_or(row))
}

// ---------------------------------------------------------------------------
// Assembling a run
// ---------------------------------------------------------------------------

/// The lineage anchor: the message a workflow's agents hang off in the tree
/// view.
///
/// The owning session's latest message, because that is the one the user was
/// looking at when the run started. A session with no messages yet gets a
/// synthetic id rather than an empty string — `origin_message_id` is a pointer
/// for the tree, not a foreign key, and an empty one would read as "this branch
/// came from nowhere".
pub fn workflow_anchor(db: &SharedDb, session_id: &str) -> String {
    db.lock()
        .unwrap_or_else(|e| e.into_inner())
        .thread_for(session_id)
        .ok()
        .and_then(|t| t.last().map(|m| m.id.clone()))
        .unwrap_or_else(|| format!("workflow:{session_id}"))
}

/// The `TurnCtx` a workflow's launches run under.
///
/// The cancel token is inert by construction. A workflow outlives every turn, so
/// there is no turn interrupt to inherit; the run's abort arrives per call, as
/// the [`AgentRunner`]'s own token, and `launch_subagent` reads nothing off this
/// one.
pub fn workflow_launch_ctx(
    ctx: &AppCtx,
    session_id: &str,
    anchor_message_id: Option<&str>,
) -> Result<TurnCtx, BoughError> {
    let session = ctx
        .db
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_session(session_id)?
        .ok_or_else(|| BoughError::not_found(format!("session {session_id} not found")))?;
    let runtime = ctx
        .db
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_session_runtime(session_id)?;
    Ok(TurnCtx {
        app: ctx.clone(),
        session_id: session_id.to_string(),
        // No turn owns a workflow. The id is a label for the caps ledger, which
        // this path is exempt from anyway.
        turn_id: format!("workflow:{session_id}"),
        message_id: anchor_message_id
            .map(String::from)
            .unwrap_or_else(|| workflow_anchor(&ctx.db, session_id)),
        workspace: runtime.workspace.unwrap_or_else(|| {
            std::env::current_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|_| ".".to_string())
        }),
        model: workflow_ctx_model(ctx, &session.model),
        cancel: CancellationToken::new(),
        exits: Arc::new(Mutex::new(Vec::new())),
        record: None,
        reads: Arc::new(Mutex::new(Vec::new())),
        touched: Arc::new(Mutex::new(Vec::new())),
        round_refs: Arc::new(Mutex::new(Vec::new())),
        mcp_grant: None,
        depth: 0,
    })
}

fn workflow_ctx_model(ctx: &AppCtx, session_model: &Option<String>) -> String {
    session_model
        .clone()
        .or_else(|| ctx.model.clone())
        .unwrap_or_else(|| DEFAULT_MODEL.to_string())
}

/// The model a call that names none will actually run on.
///
/// Mirrors the resolution in [`workflow_launch_ctx`] — session pin, else the ctx
/// default, else the built-in — and exists so the journal key can hash the
/// RESOLVED model rather than only one the script named. Without it, repinning a
/// session and rerunning an unchanged script replayed every row and returned the
/// previous model's answers as a fresh run.
pub fn workflow_effective_model(ctx: &AppCtx, session_id: &str) -> String {
    let pinned = ctx
        .db
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_session(session_id)
        .ok()
        .flatten()
        .and_then(|s| s.model);
    workflow_ctx_model(ctx, &pinned)
}

/// Build the production [`WorkflowCtx`], plus the binding that tells its runner
/// which run it belongs to.
///
/// The order of the three wrappers is the design: [`create_subagent_runner`]
/// launches, `decorate` enforces `{schema}` around it with retries, and
/// [`ControlledRunner`] sits outermost so one claim and one restart loop cover
/// the whole call. `bind` must be invoked with the run id the instant
/// `start_workflow` returns it — nothing can reach the runner before then,
/// because the worker cannot send a host call in the same tick.
pub fn workflow_ctx_for(
    ctx: &AppCtx,
    session_id: &str,
    deps: &WorkflowControlDeps,
    anchor_message_id: Option<&str>,
) -> Result<(WorkflowCtx, Arc<RunBinding>), BoughError> {
    let turn_ctx = workflow_launch_ctx(ctx, session_id, anchor_message_id)?;
    let notify = {
        let app = ctx.clone();
        let custom = deps.notify.clone();
        let f: crate::workflow::engine::NotifyFn =
            Arc::new(move |sid: &str, text: &str| match &custom {
                Some(n) => n(&app, sid, text),
                None => {
                    post_system_note(&app, sid, text, &NoteDeps::default());
                }
            });
        f
    };

    let base = WorkflowCtx {
        db: ctx.db.clone(),
        bus: ctx.bus.clone(),
        runner: Arc::new(create_subagent_runner(turn_ctx, deps)),
        notify: Some(notify),
        now: Some(deps.now.clone().unwrap_or_else(|| ctx.now.clone())),
    };
    let decorated = match &deps.decorate {
        Some(d) => d(base),
        None => structured_workflow_ctx(base, None),
    };
    let (binding, rx) = RunBinding::new();
    let controlled = ControlledRunner {
        db: ctx.db.clone(),
        bus: ctx.bus.clone(),
        binding: rx,
        inner: decorated.runner.clone(),
        agents: deps
            .agents
            .clone()
            .unwrap_or_else(|| workflow_agents().clone()),
    };
    Ok((
        WorkflowCtx {
            runner: Arc::new(controlled),
            ..decorated
        },
        binding,
    ))
}

// ---------------------------------------------------------------------------
// Start and rerun
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default)]
pub struct StartRunOpts {
    pub session_id: String,
    pub script: String,
    pub args: Option<Value>,
    /// Absent = the owning session's latest message.
    pub anchor_message_id: Option<String>,
    pub concurrency: Option<usize>,
    pub timeout_ms: Option<u64>,
}

/// Start a run with real subagents behind it.
///
/// `meta` is extracted and validated HERE, at the submit boundary, so a script
/// whose meta is missing or computed is refused with a 400 before a worker is
/// spawned or a row is written — rather than failing mid-run, after the user has
/// paid for agents (spec §8).
pub async fn start_workflow_run(
    ctx: &AppCtx,
    opts: StartRunOpts,
    deps: &WorkflowControlDeps,
) -> Result<WorkflowRun, BoughError> {
    let meta = extract_meta(&opts.script)?;
    let (workflow_ctx, binding) = workflow_ctx_for(
        ctx,
        &opts.session_id,
        deps,
        opts.anchor_message_id.as_deref(),
    )?;
    let started = start_workflow(
        &workflow_ctx,
        StartOpts {
            session_id: opts.session_id.clone(),
            script: opts.script,
            meta: Some(meta),
            args: opts.args,
            effective_model: Some(workflow_effective_model(ctx, &opts.session_id)),
            concurrency: opts.concurrency,
            timeout_ms: opts.timeout_ms,
            ..Default::default()
        },
    )
    .await;
    match started {
        Ok(run) => {
            binding.bind(Some(run.id.clone()));
            Ok(run)
        }
        Err(err) => {
            // Nothing started, so nothing can claim: settle the binding rather
            // than leaving a wait nobody will ever release.
            binding.bind(None);
            Err(err)
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct RerunRunOpts {
    /// Absent = the `~/.bough/workflows/<id>.js` mirror, then the stored script.
    pub script: Option<String>,
    pub args: Option<Value>,
}

/// Rerun a finished run: unchanged `agent()` calls replay from its journal,
/// edited and new ones run live.
///
/// The script is resolved HERE rather than left to the engine, because meta
/// travels with the script: a user who edited the mirror may have renamed the
/// run or changed its phases, and a rerun that kept the source run's meta would
/// label the new run after the old script.
pub async fn rerun_workflow_run(
    ctx: &AppCtx,
    id: &str,
    opts: RerunRunOpts,
    deps: &WorkflowControlDeps,
) -> Result<WorkflowRun, BoughError> {
    let src = ctx
        .db
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_workflow(id)?
        .ok_or_else(|| BoughError::not_found(format!("workflow {id} not found")))?;
    if is_workflow_live(id) {
        return Err(BoughError::http(
            409,
            ErrorKind::Conflict,
            format!(
                "workflow {id} is still running — stop it first, then rerun. A rerun replays \
                 the journal of a finished run; replaying one that is still writing to it \
                 would race."
            ),
        ));
    }
    let (script, _from) = resolve_rerun_script(&src, opts.script.as_deref()).await;
    let meta = extract_meta(&script)?;
    let (workflow_ctx, binding) = workflow_ctx_for(ctx, &src.session_id, deps, None)?;
    let started = rerun_workflow(
        &workflow_ctx,
        id,
        RerunOpts {
            script: Some(script),
            meta: Some(meta),
            args: opts.args,
            // Same resolution as the original run. This is the whole point of
            // hashing the resolved model: if the session has been repinned
            // since, the keys no longer match and the calls re-run instead of
            // replaying the old model's answers.
            effective_model: Some(workflow_effective_model(ctx, &src.session_id)),
        },
    )
    .await;
    match started {
        Ok(run) => {
            binding.bind(Some(run.id.clone()));
            Ok(run)
        }
        Err(err) => {
            binding.bind(None);
            Err(err)
        }
    }
}

// ---------------------------------------------------------------------------
// The transcript card
// ---------------------------------------------------------------------------

/// Record a launched run on the message whose program launched it.
///
/// WHY A PART AND NOT A COLUMN. The alternative was an `anchor_message_id` on
/// the `workflows` table, and the schema is explicitly a closed table set — "a
/// later task that needs a column stops and asks". The part union is the
/// extension point that IS open, and it buys the right lifetime for free: a
/// part rides the message, so the card lands exactly where the launch
/// happened, survives compaction into the span summary, and needs no join to
/// find.
///
/// Identity only. Status, counts and elapsed time are read live from the run
/// row by the renderer, because a run is detached and any status frozen in here
/// would be stale before the next frame.
///
/// Idempotent on the run id, and it preserves `pending` rather than setting it
/// — both for the reasons [`append_ask_part`](crate::hostfn::ask::append_ask_part)
/// gives: the same launch must never appear twice, and a card appended as a
/// turn dies must not flip a finished message back to streaming and leave the
/// session busy forever.
pub fn append_workflow_part(
    db: &SharedDb,
    bus: &Bus,
    session_id: &str,
    message_id: &str,
    run: &WorkflowRun,
) -> bool {
    let guard = db.lock().unwrap_or_else(|e| e.into_inner());
    let Ok(Some(message)) = guard.get_message(message_id) else {
        return false;
    };
    let duplicate = message
        .parts
        .iter()
        .any(|p| matches!(p, Part::Workflow { id, .. } if id == &run.id));
    if duplicate {
        return false;
    }
    let part = Part::Workflow {
        id: run.id.clone(),
        name: run.name.clone(),
        description: run.description.clone(),
        rerun_of: run.resume_of.clone(),
    };
    let mut parts = message.parts.clone();
    parts.push(part.clone());
    if guard
        .update_message(message_id, &parts, message.pending)
        .is_err()
    {
        return false;
    }
    drop(guard);
    bus.publish(EventInput {
        r#type: EventType::MessagePart,
        session_id: Some(session_id.to_string()),
        data: serde_json::to_value(MessagePartData {
            message_id: message_id.to_string(),
            part,
        })
        .unwrap_or_default(),
    });
    true
}

// ---------------------------------------------------------------------------
// The program-side `workflow.*` verb
// ---------------------------------------------------------------------------

/// zod's own wording for the three shapes the verb arguments can be wrong in,
/// so the teaching error a program catches reads the same on both runtimes.
fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn require_string(bag: &serde_json::Map<String, Value>, field: &str) -> Result<String, String> {
    match bag.get(field) {
        None | Some(Value::Null) => Err(format!("{field}: Required")),
        Some(Value::String(s)) if s.is_empty() => Err(format!(
            "{field}: String must contain at least 1 character(s)"
        )),
        Some(Value::String(s)) => Ok(s.clone()),
        Some(other) => Err(format!(
            "{field}: Expected string, received {}",
            type_name(other)
        )),
    }
}

fn optional_string(
    bag: &serde_json::Map<String, Value>,
    field: &str,
) -> Result<Option<String>, String> {
    match bag.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.is_empty() => Err(format!(
            "{field}: String must contain at least 1 character(s)"
        )),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(other) => Err(format!(
            "{field}: Expected string, received {}",
            type_name(other)
        )),
    }
}

/// The argument bag, or the 400 that names the shape the verb wanted.
fn args_object(
    verb: &str,
    shape: &str,
    raw: &Value,
) -> Result<serde_json::Map<String, Value>, BoughError> {
    match raw {
        Value::Null => Ok(serde_json::Map::new()),
        Value::Object(o) => Ok(o.clone()),
        other => Err(arg_error(
            verb,
            shape,
            format!("args: Expected object, received {}", type_name(other)),
        )),
    }
}

fn arg_error(verb: &str, shape: &str, detail: impl std::fmt::Display) -> BoughError {
    workflow_error(400, format!("workflow.{verb}({shape}): {detail}"))
}

/// One verb-dispatched entry point for the program-side `workflow.*` methods.
///
/// Every verb answers with the SUMMARY, never the run row: the row carries the
/// whole script, and a `workflow.list()` that shipped N copies of it would
/// flood the model's context for no purpose.
pub async fn workflow_verb(
    ctx: &AppCtx,
    session_id: &str,
    verb: &str,
    args: &Value,
    deps: &WorkflowControlDeps,
    anchor_message_id: Option<&str>,
) -> Result<Value, BoughError> {
    let now: Clock = deps.now.clone().unwrap_or_else(|| ctx.now.clone());
    // No anchor = no card. A run launched over REST has no message to hang one
    // on.
    let card = |run: &WorkflowRun| {
        if let Some(sink) = &deps.card {
            sink(run);
            return;
        }
        if let Some(message_id) = anchor_message_id {
            append_workflow_part(&ctx.db, &ctx.bus, session_id, message_id, run);
        }
    };
    match verb {
        "start" => {
            let bag = args_object(verb, "{script, args?}", args)?;
            let script = require_string(&bag, "script")
                .map_err(|d| arg_error(verb, "{script, args?}", d))?;
            let run = start_workflow_run(
                ctx,
                StartRunOpts {
                    session_id: session_id.to_string(),
                    script,
                    args: bag.get("args").cloned(),
                    anchor_message_id: anchor_message_id.map(str::to_string),
                    ..Default::default()
                },
                deps,
            )
            .await?;
            card(&run);
            Ok(workflow_summary(&ctx.db, &run))
        }
        "rerun" => {
            let shape = "{id, script?, args?}";
            let bag = args_object(verb, shape, args)?;
            let id = require_string(&bag, "id").map_err(|d| arg_error(verb, shape, d))?;
            let script = optional_string(&bag, "script").map_err(|d| arg_error(verb, shape, d))?;
            let run = rerun_workflow_run(
                ctx,
                &id,
                RerunRunOpts {
                    script,
                    args: bag.get("args").cloned(),
                },
                deps,
            )
            .await?;
            // A rerun is its own run with its own id, so it gets its own card:
            // the whole point of the first one is that it records what
            // happened, and overwriting it would erase the failed attempt the
            // rerun exists because of.
            card(&run);
            // `replay` is REQUIRED on an operation that replays (spec §8). The
            // run is detached, so at this instant the live counts are still
            // zero and `available` is the number that matters: it is the
            // ceiling the new run's keys will be measured against, and
            // `available: 40` next to a later `replayed: 0` is the whole
            // signal.
            let mut out = workflow_summary(&ctx.db, &run);
            let replay = super::report::summarize(&ctx.db, &run)?.to_json();
            if let Some(o) = out.as_object_mut() {
                o.insert("replay".to_string(), replay);
            }
            Ok(out)
        }
        "stop" | "pause" | "resume" | "status" => {
            let bag = args_object(verb, "{id}", args)?;
            let id = require_string(&bag, "id").map_err(|d| arg_error(verb, "{id}", d))?;
            if verb == "status" {
                let run = ctx
                    .db
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get_workflow(&id)?
                    .ok_or_else(|| BoughError::not_found(format!("workflow {id} not found")))?;
                let registry = deps
                    .agents
                    .clone()
                    .unwrap_or_else(|| workflow_agents().clone());
                let accounting = run_accounting(&ctx.db, &run, now(), AccountingOpts::default())?;
                let mut out = workflow_summary(&ctx.db, &run);
                if let Some(o) = out.as_object_mut() {
                    o.insert(
                        "agentRows".to_string(),
                        serde_json::to_value(workflow_agent_views(
                            &ctx.db,
                            &run.id,
                            &registry.live_ids(&run.id),
                        )?)
                        .unwrap_or(Value::Null),
                    );
                    o.insert("replay".to_string(), accounting.replay.to_json());
                    o.insert(
                        "cost".to_string(),
                        serde_json::to_value(&accounting.cost).unwrap_or(Value::Null),
                    );
                    o.insert(
                        "warning".to_string(),
                        serde_json::to_value(&accounting.warning).unwrap_or(Value::Null),
                    );
                }
                return Ok(out);
            }
            let run = match verb {
                "stop" => stop_workflow(&ctx.db, &ctx.bus, Some(&now), &id)?,
                "pause" => pause_workflow(&ctx.db, &ctx.bus, &id)?,
                _ => resume_workflow(&ctx.db, &ctx.bus, &id)?,
            };
            Ok(workflow_summary(&ctx.db, &run))
        }
        "list" => {
            let runs = ctx
                .db
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .list_workflows(Some(session_id))?;
            Ok(Value::Array(
                runs.iter().map(|r| workflow_summary(&ctx.db, r)).collect(),
            ))
        }
        other => Err(workflow_error(
            400,
            format!(
                "unknown workflow verb: {other} — it is one of \
                 start|rerun|stop|pause|resume|status|list, called as workflow.<verb>({{…}})."
            ),
        )),
    }
}

// ---------------------------------------------------------------------------
// The confirm gate
// ---------------------------------------------------------------------------

/// Whether a program-launched run has to be approved first. Default ON.
///
/// `BOUGH_WORKFLOW_CONFIRM=0` turns the gate off for a session that wants the
/// old behaviour (and for the headless `bough exec`, where there is nobody to
/// ask).
pub fn workflow_confirm_enabled() -> bool {
    std::env::var("BOUGH_WORKFLOW_CONFIRM").unwrap_or_else(|_| "1".to_string()) != "0"
}

/// The approval card's text: what is about to run, before any of it bills.
///
/// Everything here is read from `meta` WITHOUT running the script, which is the
/// only reason this can be shown at submit time at all. The phase list is the
/// shape of the fan-out; the agent count is deliberately NOT guessed, because
/// the number of `agent()` calls is a property of the script's control flow and
/// a wrong estimate on a spend warning is worse than none.
pub fn confirm_workflow_text(meta: &WorkflowMeta) -> String {
    let mut lines = vec![
        format!("Run the workflow \"{}\"?", meta.name),
        meta.description.clone(),
    ];
    let phases = meta.phases.clone().unwrap_or_default();
    if !phases.is_empty() {
        lines.push(String::new());
        for (i, p) in phases.iter().enumerate() {
            let detail = p
                .detail
                .as_deref()
                .filter(|d| !d.is_empty())
                .map(|d| format!(" — {d}"))
                .unwrap_or_default();
            lines.push(format!("  {}. {}{}", i + 1, p.title, detail));
        }
    }
    lines.push(String::new());
    lines.push(
        "It runs detached and fans out subagents in parallel, so it can spend a lot of \
         tokens quickly. `x` in the workflows tab (^w) stops a run at any point."
            .to_string(),
    );
    lines.join("\n")
}

/// A refused launch, worded for the thing that was refused.
fn refused_workflow(said: &str) -> BoughError {
    BoughError::ask_declined(format!(
        "the user declined to run the workflow (\"{said}\"). NOTHING was started — no run, \
         no agents, no cost. Do not start it again. Say what it would have done and let \
         them decide, or do the work directly if it is small enough not to need a workflow."
    ))
}

// ---------------------------------------------------------------------------
// The bridged `workflow` host function
// ---------------------------------------------------------------------------

struct WorkflowFnState {
    /// Launched runs waiting for the runner's last write.
    buffered: Vec<WorkflowRun>,
    /// True once the supervisor message is closed and safe to append to
    /// directly.
    closed: bool,
    /// The armed bus subscription, when there is one.
    sub: Option<u64>,
}

struct WorkflowInner {
    ctx: TurnCtx,
    deps: WorkflowControlDeps,
    confirm_gate: bool,
    st: Mutex<WorkflowFnState>,
}

impl WorkflowInner {
    fn write(&self, run: &WorkflowRun) {
        append_workflow_part(
            &self.ctx.app.db,
            &self.ctx.app.bus,
            &self.ctx.session_id,
            &self.ctx.message_id,
            run,
        );
    }

    fn flush(&self) {
        let runs: Vec<WorkflowRun> = {
            let mut st = self.st.lock().unwrap();
            st.closed = true;
            std::mem::take(&mut st.buffered)
        };
        for run in runs {
            self.write(&run);
        }
    }

    fn disarm(&self) {
        let sub = self.st.lock().unwrap().sub.take();
        if let Some(id) = sub {
            self.ctx.app.bus.unsubscribe(id);
        }
    }
}

/// The bridged `workflow` host function for one turn.
///
/// Bound to the turn's session and its in-flight supervisor message: a run
/// started from a program belongs to the session that started it, and its
/// agents hang off the message that was streaming when the program called —
/// which is what puts them in the right place in the tree.
pub struct WorkflowHostFn {
    inner: Arc<WorkflowInner>,
}

/// Build `workflow(verb, argsJson)` for one turn.
pub fn create_workflow_host_fn(
    ctx: &TurnCtx,
    deps: WorkflowControlDeps,
    confirm_gate: bool,
) -> WorkflowHostFn {
    WorkflowHostFn {
        inner: Arc::new(WorkflowInner {
            ctx: ctx.clone(),
            deps,
            confirm_gate,
            st: Mutex::new(WorkflowFnState {
                buffered: Vec::new(),
                closed: false,
                sub: None,
            }),
        }),
    }
}

impl WorkflowHostFn {
    /// Watch this turn's own lifecycle, from the first launch onwards.
    ///
    /// Armed lazily, so a turn that starts no workflow never subscribes, and
    /// dropped on `turn.finished` — which the runner emits on success, failure
    /// and interrupt alike, so a run launched by a turn that then died still
    /// leaves a card.
    fn arm(&self) {
        let mut st = self.inner.st.lock().unwrap();
        if st.sub.is_some() {
            return;
        }
        let weak: std::sync::Weak<WorkflowInner> = Arc::downgrade(&self.inner);
        let id = self.inner.ctx.app.bus.subscribe(Arc::new(move |event| {
            let Some(inner) = weak.upgrade() else { return };
            if event.r#type == EventType::MessageFinished {
                let finished: Option<MessageFinishedData> =
                    serde_json::from_value(event.data.clone()).ok();
                if finished.map(|f| f.message_id) == Some(inner.ctx.message_id.clone()) {
                    inner.disarm();
                    inner.flush();
                }
                return;
            }
            if event.r#type == EventType::TurnFinished
                && event.session_id.as_deref() == Some(inner.ctx.session_id.as_str())
            {
                inner.disarm();
                inner.flush();
            }
        }));
        st.sub = Some(id);
    }

    /// Park on the human before a fan-out starts, not after it has billed.
    ///
    /// Only `start` and `rerun` — the two verbs that dispatch agents. `stop`,
    /// `pause`, `status` and `list` are reads and brakes, and a confirm on a
    /// brake is how you teach someone to hit enter without reading.
    ///
    /// A decline reaches the program as `AskDeclinedError`, the same catchable
    /// shape `ask()` raises, so a script-writing turn can say "you declined,
    /// here is what I would have run" instead of dying. Nothing is created: the
    /// gate is BEFORE [`start_workflow_run`], so a refused launch leaves no run
    /// row, no journal, no mirrored script.
    async fn confirm(&self, verb: &str, args: &Value) -> Result<(), BoughError> {
        if !self.inner.confirm_gate || (verb != "start" && verb != "rerun") {
            return Ok(());
        }
        let script = args.get("script").and_then(Value::as_str).unwrap_or("");
        // A rerun with no inline script re-runs the source run's, which this
        // cannot read without a db round-trip it does not own. Gate on the id
        // it was given.
        let meta = if script.trim().is_empty() {
            WorkflowMeta {
                name: "the previous script".to_string(),
                description: "relaunched from its journal".to_string(),
                phases: None,
            }
        } else {
            extract_meta(script)?
        };
        let ctx = &self.inner.ctx;
        let raised = ctx.app.host.asks.raise(
            &ctx.app.bus,
            AskInput {
                session_id: ctx.session_id.clone(),
                message_id: ctx.message_id.clone(),
                question: confirm_workflow_text(&meta),
                options: Some(vec!["run it".to_string(), "no".to_string()]),
            },
            Some(&ctx.cancel),
        );
        let said = match raised.settled().await {
            (AskSettlement::Answered, ans) => ans.unwrap_or_default().trim().to_lowercase(),
            // A dismissal arrives as `ask()`'s own decline, whose advice —
            // "proceed on a default you state out loud" — is exactly wrong
            // here: the default for a refused fan-out is to not run it.
            // Re-word, keep the catchable type.
            (AskSettlement::Declined, _) => return Err(refused_workflow("the card was dismissed")),
            (AskSettlement::Interrupted, _) => {
                return Err(BoughError::program(
                    "workflow confirmation interrupted — the turn was stopped before the \
                     launch was approved. NOTHING was started.",
                ))
            }
        };
        // Anything that is not an affirmative is a refusal. The free-text box
        // is always open beside the options, and treating "not yet" as consent
        // to spend is the one failure mode a spend gate may not have.
        if said != "run it" && said != "yes" && said != "y" {
            return Err(refused_workflow(&said));
        }
        Ok(())
    }

    pub async fn call(&self, verb: &str, args_json: &str) -> Result<String, BoughError> {
        let text = args_json.trim();
        let args: Value = if text.is_empty() {
            Value::Null
        } else {
            serde_json::from_str(text).map_err(|_| {
                workflow_error(
                    400,
                    format!("workflow.{verb}(…): arguments must be a JSON value"),
                )
            })?
        };
        self.confirm(verb, &args).await?;
        let mut deps = self.inner.deps.clone();
        // The buffering sink, per call: storing it on `inner.deps` would be a
        // reference cycle through the Arc that owns it.
        let sink = Arc::downgrade(&self.inner);
        deps.card = Some(Arc::new(move |run: &WorkflowRun| {
            let Some(inner) = sink.upgrade() else { return };
            let closed = {
                let mut st = inner.st.lock().unwrap();
                if st.closed {
                    true
                } else {
                    st.buffered.push(run.clone());
                    false
                }
            };
            if closed {
                inner.write(run);
            }
        }));
        // Arm BEFORE the launch, so the `message.finished` that releases the
        // buffer cannot arrive between the sink's push and the subscription.
        if verb == "start" || verb == "rerun" {
            self.arm();
        }
        let value = workflow_verb(
            &self.inner.ctx.app,
            &self.inner.ctx.session_id,
            verb,
            &args,
            &deps,
            Some(&self.inner.ctx.message_id),
        )
        .await?;
        Ok(serde_json::to_string(&value).unwrap_or_else(|_| "null".to_string()))
    }

    /// The `HostFns.workflow` adapter: JSON-string args in protocol order.
    pub fn into_host_fn(self) -> crate::types::HostFn {
        use futures::FutureExt;
        let this = Arc::new(self);
        Arc::new(move |args: Vec<String>| {
            let this = this.clone();
            async move {
                let verb = args.first().cloned().unwrap_or_default();
                let bag = args.get(1).cloned().unwrap_or_else(|| "null".to_string());
                this.call(&verb, &bag).await
            }
            .boxed()
        })
    }
}

// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::sqlite_db::{DbOptions, SqliteDb};
    use crate::schema::parts::{
        Message, Role, Session, SessionKind, Usage, WorkflowAgentStatus, WorkflowStatus,
    };
    use std::sync::{Arc, Mutex};

    fn mem_db() -> SharedDb {
        Arc::new(Mutex::new(
            SqliteDb::new(":memory:", DbOptions::default()).unwrap(),
        ))
    }

    fn session(db: &SharedDb, id: &str) {
        db.lock()
            .unwrap()
            .create_session(Session {
                id: id.into(),
                title: "s".into(),
                kind: SessionKind::Root,
                created_at: 1,
                parent_id: None,
                origin_id: None,
                origin_message_id: None,
                workspace: Some("/tmp/w".into()),
                origin_dir: Some("/tmp/w".into()),
                base: None,
                model: None,
                effort: None,
                draft: None,
                context_tokens: None,
                cached_tokens: None,
                last_llm_at: None,
                outcome_ok: None,
            })
            .unwrap();
    }

    fn run(db: &SharedDb, id: &str) -> WorkflowRun {
        session(db, &format!("s-{id}"));
        let row = WorkflowRun {
            id: id.into(),
            session_id: format!("s-{id}"),
            name: "w".into(),
            description: String::new(),
            script: "return 1".into(),
            phases: vec![],
            status: WorkflowStatus::Done,
            current_phase: None,
            result: None,
            error: None,
            args: None,
            resume_of: None,
            created_at: 1_000,
            finished_at: Some(3_000),
        };
        db.lock().unwrap().create_workflow(row.clone()).unwrap();
        row
    }

    fn agent(db: &SharedDb, run_id: &str, idx: i64, key: &str, session_id: Option<&str>) {
        if let Some(sid) = session_id {
            session(db, sid);
        }
        db.lock()
            .unwrap()
            .create_workflow_agent(WorkflowAgent {
                id: format!("{run_id}-a{idx}"),
                run_id: run_id.into(),
                idx,
                key: key.into(),
                label: format!("call {idx}"),
                phase: None,
                prompt: format!("prompt {idx}"),
                model: Some("m".into()),
                status: WorkflowAgentStatus::Done,
                result: Some("r".into()),
                error: None,
                session_id: session_id.map(String::from),
                started_at: 1_000,
                finished_at: Some(2_000),
            })
            .unwrap();
    }

    /// The view lists a fan-out in the SCRIPT's shape, not in the order its
    /// calls happened to reach the host — which under `pipeline()` is latency
    /// order and differs run to run.
    #[test]
    fn rows_list_in_structural_order_not_dispatch_order() {
        let db = mem_db();
        run(&db, "wf");
        // Dispatch order 0,1,2 — structural order 0.0, 0.1, 0.10, so the row
        // dispatched LAST sorts in the middle, and "0.10" sorts after "0.9".
        agent(&db, "wf", 0, "0.10|a", None);
        agent(&db, "wf", 1, "0.0|b", None);
        agent(&db, "wf", 2, "0.9|c", None);
        let views = workflow_agent_views(&db, "wf", &[]).unwrap();
        let keys: Vec<&str> = views.iter().map(|v| v.agent.key.as_str()).collect();
        assert_eq!(
            keys,
            ["0.0|b", "0.9|c", "0.10|a"],
            "numeric, not lexicographic"
        );
    }

    /// A row whose key predates coordinates has no position, so it falls back
    /// to `idx` — an old journal still lists sensibly.
    #[test]
    fn pre_coordinate_rows_fall_back_to_the_dispatch_index() {
        let db = mem_db();
        run(&db, "wf");
        agent(&db, "wf", 2, "cc", None);
        agent(&db, "wf", 0, "aa", None);
        agent(&db, "wf", 1, "bb", None);
        let views = workflow_agent_views(&db, "wf", &[]).unwrap();
        assert_eq!(
            views.iter().map(|v| v.agent.idx).collect::<Vec<_>>(),
            [0, 1, 2]
        );
    }

    /// A row with a session carries its live cost and the last four tool-call
    /// gists; one without carries zeros rather than a missing field.
    #[test]
    fn a_backed_row_carries_its_tokens_and_the_last_four_tool_calls() {
        let db = mem_db();
        run(&db, "wf");
        agent(&db, "wf", 0, "0|a", Some("kid"));
        agent(&db, "wf", 1, "1|b", None);
        {
            let guard = db.lock().unwrap();
            guard
                .add_session_usage(
                    "kid",
                    &Usage {
                        input_tokens: 70,
                        output_tokens: 30,
                        ..Usage::default()
                    },
                    2_000,
                )
                .unwrap();
            let parts: Vec<Part> = (0..6)
                .map(|i| Part::ToolCall {
                    id: format!("t{i}"),
                    name: "bash".into(),
                    input: json!({ "code": format!("echo {i}\nsecond line") }),
                })
                .collect();
            guard
                .create_message(Message {
                    id: "m1".into(),
                    session_id: "kid".into(),
                    role: Role::Supervisor,
                    parts,
                    pending: false,
                    created_at: 1,
                })
                .unwrap();
        }
        let views = workflow_agent_views(&db, "wf", &["wf-a0".to_string()]).unwrap();
        assert_eq!(views[0].tokens, 100);
        assert_eq!(views[0].tool_calls, 6);
        assert_eq!(
            views[0].activity,
            [
                "bash(echo 2)",
                "bash(echo 3)",
                "bash(echo 4)",
                "bash(echo 5)"
            ],
            "the LAST four, first line only"
        );
        assert!(views[0].live, "the registry is holding this one");
        // No session: zeros, and not live.
        assert_eq!(views[1].tokens, 0);
        assert_eq!(views[1].tool_calls, 0);
        assert!(views[1].activity.is_empty());
        assert!(!views[1].live);
    }

    /// The detail body's eight keys, and the two that make an orphan legible:
    /// `live` (this process holds it) against the run's own status.
    #[test]
    fn the_detail_body_carries_the_run_its_rows_and_the_accounting() {
        let db = mem_db();
        let r = run(&db, "wf");
        agent(&db, "wf", 0, "0|a", None);
        let body = workflow_detail(&db, &r, &[], 9_000).unwrap();
        for key in [
            "workflow",
            "agents",
            "scriptFile",
            "live",
            "replay",
            "cost",
            "warning",
            "guideline",
        ] {
            assert!(body.get(key).is_some(), "missing {key}: {body}");
        }
        assert_eq!(body["workflow"]["id"], "wf");
        assert_eq!(body["agents"].as_array().unwrap().len(), 1);
        assert_eq!(
            body["live"], false,
            "no live registry entry in this process"
        );
        assert!(
            body["scriptFile"]
                .as_str()
                .unwrap()
                .ends_with("/workflows/wf.js"),
            "{body}"
        );
        // The accounting fields are the real folds, not placeholders.
        assert_eq!(body["replay"]["total"], 1);
        assert_eq!(body["replay"]["final"], true);
        assert_eq!(body["cost"]["agents"], 1);
        assert_eq!(body["warning"], Value::Null, "a one-agent run is not large");
    }

    #[test]
    fn a_gist_is_the_first_line_of_the_code_clipped() {
        assert_eq!(gist(&json!({ "code": "  ls -la\nrm -rf /" }), 48), "ls -la");
        assert_eq!(gist(&json!("plain string\nsecond"), 48), "plain string");
        assert_eq!(gist(&json!({ "other": 1 }), 48), "");
        assert_eq!(gist(&Value::Null, 48), "");
        // `clip` counts the ellipsis: eight units total, seven of them x's.
        assert_eq!(
            gist(&json!({ "code": "x".repeat(60) }), 8),
            format!("{}…", "x".repeat(7))
        );
    }

    // -----------------------------------------------------------------------
    // the program-side verb, its card and its confirm gate
    // (ported from `src/workflow/control.test.ts`)
    // -----------------------------------------------------------------------

    use crate::agents::testkit::turn_ctx_for;
    use crate::schema::events::BoughEvent;
    use crate::schema::parts::AskQuestion;

    /// `BOUGH_HOME` is process-global, so a test that touches the script
    /// mirror takes the crate-wide lock and runs its body on a runtime built
    /// inside the guarded closure — the same discipline the engine tests use.
    fn with_home<F>(f: impl FnOnce() -> F)
    where
        F: std::future::Future<Output = ()>,
    {
        let home = std::env::temp_dir().join(format!("bough-wfcontrol-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&home).expect("temp home");
        crate::paths::test_env::with_env(&[("BOUGH_HOME", home.to_str())], || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime")
                .block_on(f());
        });
        let _ = std::fs::remove_dir_all(&home);
    }

    /// A turn ctx over a real session, with a pending supervisor message to
    /// anchor cards on.
    fn turn_with_message(db: &SharedDb, session_id: &str, message_id: &str) -> TurnCtx {
        session(db, session_id);
        db.lock()
            .unwrap()
            .create_message(Message {
                id: message_id.into(),
                session_id: session_id.into(),
                role: Role::Supervisor,
                parts: vec![],
                pending: true,
                created_at: 2_000,
            })
            .unwrap();
        let mut ctx = turn_ctx_for(db, session_id, "t1", 0);
        ctx.message_id = message_id.to_string();
        ctx
    }

    fn part_types(db: &SharedDb, message_id: &str) -> Vec<String> {
        db.lock()
            .unwrap()
            .get_message(message_id)
            .unwrap()
            .unwrap()
            .parts
            .iter()
            .map(|p| match p {
                Part::Text { .. } => "text".to_string(),
                Part::Workflow { .. } => "workflow".to_string(),
                other => serde_json::to_value(other)
                    .ok()
                    .and_then(|v| v.get("type").and_then(|t| t.as_str()).map(String::from))
                    .unwrap_or_default(),
            })
            .collect()
    }

    /// The card must survive the turn runner, which is the whole reason it is
    /// buffered.
    ///
    /// The runner keeps the supervisor message's `parts` in memory and rewrites
    /// the row WHOLESALE on every append, so a part written from out here is
    /// erased by the runner's very next write — and `workflow.start()` is
    /// always called from inside a program, so that next write is the program
    /// step's own tool_result, milliseconds later. Written directly, the card
    /// would appear and vanish every time, which is indistinguishable from
    /// never having been implemented.
    #[test]
    fn a_launch_card_survives_the_runners_next_wholesale_write() {
        with_home(|| async {
            let db = mem_db();
            let ctx = turn_with_message(&db, "s1", "m1");
            // The approval gate is off: this test is about the card surviving
            // the runner, and a gate nobody answers would park it forever.
            let workflow = create_workflow_host_fn(&ctx, WorkflowControlDeps::default(), false);
            // No `agent()` call: this test is about the transcript, and a live
            // fan-out racing the teardown is a different test's problem.
            let script = "export const meta = {name: \"w\", description: \"d\"}\nreturn 1\n";
            workflow
                .call("start", &json!({ "script": script }).to_string())
                .await
                .expect("the run starts");

            // The runner's own array has never heard of the card, and this is
            // what it does next: one wholesale write of the parts IT is
            // holding.
            let text = vec![Part::Text {
                text: "the program ran".into(),
            }];
            db.lock()
                .unwrap()
                .update_message("m1", &text, true)
                .unwrap();
            assert_eq!(
                part_types(&db, "m1"),
                ["text"],
                "buffered, so nothing is on the row while the turn is still writing to it"
            );

            // The runner's LAST write, then the event that releases the buffer.
            db.lock()
                .unwrap()
                .update_message("m1", &text, false)
                .unwrap();
            ctx.app.bus.publish(EventInput {
                r#type: EventType::MessageFinished,
                session_id: Some("s1".into()),
                data: json!({ "messageId": "m1" }),
            });

            assert_eq!(part_types(&db, "m1"), ["text", "workflow"]);
            let parts = db.lock().unwrap().get_message("m1").unwrap().unwrap().parts;
            let Part::Workflow { name, .. } = &parts[1] else {
                panic!("the card")
            };
            assert_eq!(name, "w");
        });
    }

    /// The gate is BEFORE the run exists, which is the only place it is worth
    /// anything.
    ///
    /// A confirm that fired after `start_workflow_run` would already have
    /// written a run row, mirrored the script and — for a `parallel()` script —
    /// issued every call in the first tick. Declining has to leave nothing
    /// behind, so this asserts on the absence of a run row, not merely on the
    /// thrown error.
    #[test]
    fn a_declined_workflow_starts_nothing_at_all() {
        with_home(|| async {
            let db = mem_db();
            let ctx = turn_with_message(&db, "s1", "m1");
            let workflow = create_workflow_host_fn(&ctx, WorkflowControlDeps::default(), true);

            // Answer the hold the moment it is raised — the TUI's job, done
            // inline.
            let holds = ctx.app.host.asks.clone();
            let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(vec![]));
            let sink = seen.clone();
            let sub = ctx.app.bus.subscribe(Arc::new(move |e: &BoughEvent| {
                if e.r#type != EventType::AskQuestion {
                    return;
                }
                let Ok(q) = serde_json::from_value::<AskQuestion>(e.data.clone()) else {
                    return;
                };
                if q.status != crate::schema::parts::AskQuestionStatus::Pending {
                    return;
                }
                sink.lock().unwrap().push(q.question.clone());
                holds.decline(&q.id);
            }));

            let script = "export const meta = {name: \"w\", description: \"d\", \
                          phases: [{title: \"describe\"}]}\nreturn 1\n";
            let err = workflow
                .call("start", &json!({ "script": script }).to_string())
                .await
                .expect_err("the launch is refused");
            ctx.app.bus.unsubscribe(sub);

            assert!(
                err.to_string().contains("declined to run the workflow"),
                "{err}"
            );
            assert_eq!(
                err.name(),
                "AskDeclinedError",
                "the same catchable shape ask() raises"
            );
            let asked = seen.lock().unwrap().clone();
            assert_eq!(asked.len(), 1);
            assert!(asked[0].contains("Run the workflow \"w\"?"), "{}", asked[0]);
            assert!(
                asked[0].contains("1. describe"),
                "the phases are on the card, before any of it runs: {}",
                asked[0]
            );
            assert_eq!(
                db.lock().unwrap().list_workflows(Some("s1")).unwrap().len(),
                0,
                "no run row, no journal, no cost"
            );
        });
    }

    /// The brakes and the reads do NOT park on a human: a confirm on a brake is
    /// how you teach someone to hit enter without reading.
    #[test]
    fn only_start_and_rerun_park_on_the_confirm_gate() {
        with_home(|| async {
            let db = mem_db();
            let ctx = turn_with_message(&db, "s1", "m1");
            let workflow = create_workflow_host_fn(&ctx, WorkflowControlDeps::default(), true);
            // Nothing answers holds here, so a verb that raised one would hang
            // rather than return.
            let out = workflow.call("list", "{}").await.unwrap();
            assert_eq!(out, "[]");
            assert_eq!(ctx.app.host.asks.size(), 0, "no hold was raised");
        });
    }

    /// An unknown verb teaches the whole set rather than failing anonymously,
    /// and a malformed bag names the shape the verb wanted.
    #[test]
    fn the_verb_errors_name_the_verb_set_and_the_argument_shape() {
        with_home(|| async {
            let db = mem_db();
            let ctx = turn_with_message(&db, "s1", "m1");
            let workflow = create_workflow_host_fn(&ctx, WorkflowControlDeps::default(), false);

            let err = workflow.call("frobnicate", "{}").await.unwrap_err();
            assert_eq!(
                err.to_string(),
                "unknown workflow verb: frobnicate — it is one of \
                 start|rerun|stop|pause|resume|status|list, called as workflow.<verb>({…})."
            );

            let err = workflow.call("start", "{}").await.unwrap_err();
            assert_eq!(
                err.to_string(),
                "workflow.start({script, args?}): script: Required"
            );

            let err = workflow.call("status", "{}").await.unwrap_err();
            assert_eq!(err.to_string(), "workflow.status({id}): id: Required");

            let err = workflow.call("start", "not json").await.unwrap_err();
            assert_eq!(
                err.to_string(),
                "workflow.start(…): arguments must be a JSON value"
            );
        });
    }

    /// The approval text is read from `meta` without running the script: what
    /// is about to run, before any of it bills.
    #[test]
    fn the_confirm_text_lists_the_phases_and_the_spend_warning() {
        let meta = WorkflowMeta {
            name: "audit".into(),
            description: "sweep the repo".into(),
            phases: Some(vec![
                crate::schema::parts::WorkflowPhase {
                    title: "map".into(),
                    detail: Some("one agent per crate".into()),
                },
                crate::schema::parts::WorkflowPhase {
                    title: "reduce".into(),
                    detail: None,
                },
            ]),
        };
        assert_eq!(
            confirm_workflow_text(&meta),
            "Run the workflow \"audit\"?\nsweep the repo\n\n  1. map — one agent per crate\n  \
             2. reduce\n\nIt runs detached and fans out subagents in parallel, so it can spend \
             a lot of tokens quickly. `x` in the workflows tab (^w) stops a run at any point."
        );
    }

    /// A card is idempotent on the run id and preserves `pending` — a card
    /// appended as a turn dies must not flip a finished message back to
    /// streaming and leave the session busy forever.
    #[test]
    fn the_card_is_idempotent_and_never_reopens_a_finished_message() {
        let db = mem_db();
        let bus = Bus::new(crate::types::system_clock());
        let r = run(&db, "wf");
        db.lock()
            .unwrap()
            .create_message(Message {
                id: "m1".into(),
                session_id: r.session_id.clone(),
                role: Role::Supervisor,
                parts: vec![],
                pending: false,
                created_at: 1,
            })
            .unwrap();
        assert!(append_workflow_part(&db, &bus, &r.session_id, "m1", &r));
        assert!(
            !append_workflow_part(&db, &bus, &r.session_id, "m1", &r),
            "the same launch must never appear twice"
        );
        let message = db.lock().unwrap().get_message("m1").unwrap().unwrap();
        assert_eq!(message.parts.len(), 1);
        assert!(!message.pending, "preserved, never set");
        assert!(
            !append_workflow_part(&db, &bus, &r.session_id, "gone", &r),
            "a message that no longer exists is not an error"
        );
    }
}
