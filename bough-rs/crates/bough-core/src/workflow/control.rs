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
use crate::paths::workflow_script_path;
use crate::schema::events::{EventInput, EventType};
use crate::schema::parts::{Part, WorkflowAgent, WorkflowAgentStatus, WorkflowRun};
use crate::turn::queue::TurnRegistry;
use crate::turn::runner::{interrupt_turn, DEFAULT_MODEL};
use crate::types::{AppCtx, Clock, Patch, SharedDb, TurnCtx, WorkflowAgentPatch};

use super::engine::{
    is_workflow_live, rerun_workflow, start_workflow, RerunOpts, StartOpts, WorkflowCtx,
};
use super::journal_fs::resolve_rerun_script;
use super::key::clip;
use super::meta::extract_meta;
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
}
