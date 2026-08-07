//! The workflow engine: the host side of the workflow worker, plus the journal
//! that makes rerun cheap (port of `src/workflow/run.ts`).
//!
//! WHY THIS EXISTS. Subagent fan-out is capped at 8 per turn and 4 concurrent
//! tree-wide, which is right for delegation inside a turn and useless for
//! "audit these 300 handlers". A workflow lifts that ceiling by moving the loop
//! into a script that runs DETACHED from the turn that started it: the script
//! owns the control flow, this module owns the agents, and the turn that called
//! `workflow.start` is free to end.
//!
//! THE INVARIANT THIS HOLDS: **every `agent()` call is journaled by key before
//! it runs, and a relaunch replays the longest UNCHANGED PREFIX of those calls
//! instead of paying for it.** `key` is `hash(prompt + label + phase + model +
//! schema)` — everything that decides what the subagent will be asked. Four
//! consequences shape the code below:
//!
//!   - **Replay stops at the first changed call and never resumes**
//!     (`workflow/replay.rs`). A key covers a call's PROMPT, not the filesystem
//!     that prompt runs against, and workflow agents all share one checkout.
//!   - **Position comes from the script's STRUCTURE, never from arrival order.**
//!     `js/wf_worker.js` sends a structural coordinate with every `agent()`
//!     call; a bare call falls back to the enclosing frame's counter. The
//!     journal key is `<pos>|<contentHash>`, so a call that MOVED and a call
//!     that was EDITED are different facts.
//!   - **The journal row is written BEFORE the semaphore is acquired**, so the
//!     run view can show a queued agent, and `startedAt` is reset when the call
//!     actually starts.
//!   - **Pause gates ADMISSION, not issuance, and a stopped run leaves nothing
//!     non-terminal.** Both are checked in [`RunState::admit`], after a
//!     semaphore slot is taken, with the row still `queued`: a `parallel()`
//!     fan-out issues every call at dispatch, so a single gate check before the
//!     semaphore is a no-op for precisely the shape workflows exist for.
//!   - Only successful calls replay. A failed call re-runs live, and so does
//!     everything after it.
//!
//! CONCURRENCY (ARCHITECTURE §4.2). One sidecar process per run, driven by one
//! message-loop task. **The prefix decision and the journal-row insert happen in
//! one non-await section on that task** — the TS synchronous-decision guarantee.
//! Deciding after an await would let a later call's hit land before an earlier
//! call's miss moved the frontier, and a run would replay past its own
//! divergence. Only the part that can block — the pause gate, the semaphore, the
//! subagent itself — moves to a spawned per-call task, which posts its answer
//! back through the worker's writer so a slow agent never blocks a `log` line.
//!
//! The [`AgentRunner`] is injected, so the whole engine — worker, journal,
//! semaphore, pause gate, replay — is drivable offline with no LLM, no key and
//! no subagent.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::bus::Bus;
use crate::errors::{BoughError, ErrorKind};
use crate::harness::protocol::{FromWorkflowWorker, WORKFLOW_HOST_FN_NAMES};
use crate::harness::wf::WorkflowWorker;
use crate::paths::workflow_script_path;
use crate::schema::events::{EventInput, EventType, WorkflowLogData};
use crate::schema::parts::{WorkflowAgent, WorkflowAgentStatus, WorkflowRun, WorkflowStatus};
use crate::types::{system_clock, Clock, Db, Patch, SharedDb, WorkflowAgentPatch, WorkflowPatch};

use super::journal_fs::{mirror_script, resolve_rerun_script};
use super::key::{call_key, clip};
use super::meta::WorkflowMeta;
use super::pos::{compare_pos, journal_key, CallPos};
use super::replay::{classify_divergence, empty_replay_plan, replay_plan, Divergence, ReplayPlan};
use super::runner::{AgentCall, AgentRunner, OnSpawned};

// Re-exported so callers keep the TS module shape (`run.ts` owned these).
pub use crate::harness::wf::{check_workflow_syntax, workflow_body, WORKFLOW_PROGRAM_PARAMS};

// ---------------------------------------------------------------------------
// Limits
// ---------------------------------------------------------------------------

/// How many agents a run may have in flight. The run's OWN semaphore — the
/// subagent caps deliberately do not apply inside a workflow.
pub fn workflow_concurrency() -> usize {
    match std::env::var("BOUGH_WORKFLOW_CONCURRENCY")
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
    {
        Some(n) if n.is_finite() && n > 0.0 => n as usize,
        _ => default_workflow_concurrency(),
    }
}

/// Up to 16 at once, fewer on a small machine. Two cores are left for
/// everything that is NOT a workflow agent: the server's own turn runner, the
/// program worker a supervisor is running, the subagent turns those spawn. A
/// runtime that reports nothing usable falls back to 4 rather than to 1, because
/// a conservative guess here costs wall-clock on every fan-out.
pub fn default_workflow_concurrency() -> usize {
    match std::thread::available_parallelism() {
        Ok(cores) => cores.get().saturating_sub(2).clamp(1, 16),
        Err(_) => 4,
    }
}

/// Wall-clock ceiling on a whole run. A liveness backstop, not a budget.
pub fn workflow_timeout_ms() -> u64 {
    match std::env::var("BOUGH_WORKFLOW_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
    {
        Some(n) if n.is_finite() && n > 0.0 => n as u64,
        _ => 60 * 60_000,
    }
}

/// Lifetime agent cap per run — a runaway-loop backstop, not a working limit. A
/// script that means to launch 300 agents is doing its job; one that means to
/// launch 300 and has an off-by-one in a `while` is not. It was 200, which is
/// inside the range a real audit legitimately asks for, so it was a working
/// limit wearing a backstop's name.
pub const MAX_AGENTS_PER_RUN: i64 = 1000;

// ---------------------------------------------------------------------------
// Context and options
// ---------------------------------------------------------------------------

/// Deliver the finished-run note to the owning session. Absent = the run still
/// lands in the database and on the bus; nobody is woken.
pub type NotifyFn = Arc<dyn Fn(&str, &str) + Send + Sync>;

#[derive(Clone)]
pub struct WorkflowCtx {
    pub db: SharedDb,
    pub bus: Arc<Bus>,
    pub runner: Arc<dyn AgentRunner>,
    pub notify: Option<NotifyFn>,
    /// Injected clock. Absent = the system clock.
    pub now: Option<Clock>,
}

impl WorkflowCtx {
    fn clock(&self) -> Clock {
        self.now.clone().unwrap_or_else(system_clock)
    }

    /// Stop a run. See [`stop_workflow`].
    pub fn stop(&self, id: &str) -> Result<WorkflowRun, BoughError> {
        stop_workflow(&self.db, &self.bus, self.now.as_ref(), id)
    }

    /// Pause a run. See [`pause_workflow`].
    pub fn pause(&self, id: &str) -> Result<WorkflowRun, BoughError> {
        pause_workflow(&self.db, &self.bus, id)
    }

    /// Resume a run. See [`resume_workflow`].
    pub fn resume(&self, id: &str) -> Result<WorkflowRun, BoughError> {
        resume_workflow(&self.db, &self.bus, id)
    }
}

/// The validated `meta` literal, extracted at the submit boundary
/// (`workflow/meta.rs`). Structurally what `startWorkflow` takes in TS.
pub type WorkflowMetaInput = WorkflowMeta;

#[derive(Clone, Debug, Default)]
pub struct StartOpts {
    pub session_id: String,
    pub script: String,
    /// Absent = inherited from `resume_of`, else a plain default.
    pub meta: Option<WorkflowMetaInput>,
    /// `None` is TS `undefined` (a relaunch inherits the source's input);
    /// `Some(Value::Null)` is an explicit null.
    pub args: Option<Value>,
    /// Journal-replay source: matching calls return that run's results instantly.
    pub resume_of: Option<String>,
    /// Overrides for the run's semaphore and wall clock. Absent = env defaults.
    pub concurrency: Option<usize>,
    pub timeout_ms: Option<u64>,
    /// The model a call that names none will actually run on (session pin, else
    /// the ctx default, else the built-in). Folded into the journal key so a
    /// rerun after a model change re-runs instead of replaying the old model's
    /// answers.
    pub effective_model: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct RerunOpts {
    /// Script override. Absent = the `~/.bough/workflows/<id>.js` mirror, which
    /// the user may have edited, falling back to the stored script.
    pub script: Option<String>,
    pub args: Option<Value>,
    /// Absent = the source run's meta. Pass it when an edited script changed it.
    pub meta: Option<WorkflowMetaInput>,
    /// See [`StartOpts::effective_model`]. Must resolve the same way the source
    /// run did.
    pub effective_model: Option<String>,
}

// ---------------------------------------------------------------------------
// Labels (pure)
// ---------------------------------------------------------------------------

/// The display label for a call that passed none. The naive fallback — the
/// prompt's first line — collapses a fan-out into N identical rows whenever the
/// script shares a preamble across its agents, which is the normal way to write
/// one. So walk the prompt for the first line no sibling has claimed.
///
/// Display only: [`call_key`] hashes the deterministic first-line label, so
/// replay never depends on which siblings happened to exist.
pub fn distinct_label(prompt: &str, taken: &[String]) -> String {
    let lines: Vec<String> = prompt
        .trim()
        .split('\n')
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    for line in &lines {
        let candidate = clip(line, 40);
        if !taken.contains(&candidate) {
            return candidate;
        }
    }
    // Every line collides (identical prompts): number them so they stay
    // separable.
    let base = clip(lines.first().map(String::as_str).unwrap_or("agent"), 36);
    let n = taken.iter().filter(|t| t.starts_with(&base)).count() + 1;
    format!("{base} #{n}")
}

// ---------------------------------------------------------------------------
// The live registry
// ---------------------------------------------------------------------------

/// In-flight runs, by id. Process-wide on purpose, like the job registry and
/// the spawn caps: a run outlives the turn, the request and the client that
/// started it, so a per-caller instance would hold nothing. A server restart
/// empties it, which is precisely what [`recover_orphaned_workflows`]
/// reconciles at boot.
fn live() -> &'static Mutex<HashMap<String, Arc<RunState>>> {
    static LIVE: OnceLock<Mutex<HashMap<String, Arc<RunState>>>> = OnceLock::new();
    LIVE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn live_get(id: &str) -> Option<Arc<RunState>> {
    live()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(id)
        .cloned()
}

fn live_take(id: &str) -> Option<Arc<RunState>> {
    live().lock().unwrap_or_else(|e| e.into_inner()).remove(id)
}

/// Is this run still executing in this process?
pub fn is_workflow_live(id: &str) -> bool {
    live()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .contains_key(id)
}

// ---------------------------------------------------------------------------
// Run state: semaphore, pause gate, replay frontier
// ---------------------------------------------------------------------------

struct Inner {
    /// Dispatch index. Assigned in the synchronous section, never re-derived.
    idx: i64,
    /// The STRUCTURALLY SMALLEST coordinate at which this run's calls stopped
    /// matching the source, or `None` while the prefix still holds. A
    /// coordinate rather than a boolean, because dispatch order is not
    /// structural order: `pipeline()` issues stage 2 in stage-1 completion
    /// order, so a call can arrive after a divergence and still sit BEFORE it in
    /// the script. Under a boolean flag that call would be forced live for no
    /// reason — the flag would be recording latency.
    diverged_pos: Option<CallPos>,
    diverged_at: Option<i64>,
    divergence: Option<Divergence>,
    paused: bool,
    in_flight: usize,
    /// FIFO, like the TS array-shift queue.
    sem_queue: VecDeque<oneshot::Sender<()>>,
    gate: VecDeque<oneshot::Sender<()>>,
}

struct RunState {
    id: String,
    session_id: String,
    db: SharedDb,
    bus: Arc<Bus>,
    runner: Arc<dyn AgentRunner>,
    notify: Option<NotifyFn>,
    now: Clock,
    ctrl: CancellationToken,
    limit: usize,
    plan: ReplayPlan,
    effective_model: Option<String>,
    inner: Mutex<Inner>,
}

impl RunState {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn is_paused(&self) -> bool {
        self.lock().paused
    }

    fn now(&self) -> i64 {
        (self.now)()
    }

    // ---- the semaphore ----------------------------------------------------

    fn try_acquire(&self) -> Option<oneshot::Receiver<()>> {
        let mut inner = self.lock();
        if inner.in_flight < self.limit {
            inner.in_flight += 1;
            return None;
        }
        let (tx, rx) = oneshot::channel();
        inner.sem_queue.push_back(tx);
        Some(rx)
    }

    async fn acquire(&self) {
        if let Some(rx) = self.try_acquire() {
            let _ = rx.await;
        }
    }

    fn release(&self) {
        let mut inner = self.lock();
        inner.in_flight = inner.in_flight.saturating_sub(1);
        while let Some(tx) = inner.sem_queue.pop_front() {
            inner.in_flight += 1;
            if tx.send(()).is_ok() {
                return;
            }
            // The waiter is gone (its task was dropped): give the slot back and
            // try the next one, or the run would leak capacity.
            inner.in_flight = inner.in_flight.saturating_sub(1);
        }
    }

    // ---- the pause gate ---------------------------------------------------

    fn gate_ticket(&self) -> Option<oneshot::Receiver<()>> {
        let mut inner = self.lock();
        if !inner.paused {
            return None;
        }
        let (tx, rx) = oneshot::channel();
        inner.gate.push_back(tx);
        Some(rx)
    }

    async fn await_gate(&self) {
        if let Some(rx) = self.gate_ticket() {
            let _ = rx.await;
        }
    }

    /// Open the gate and release the parked calls, FIFO.
    fn open_gate(&self) {
        let waiters: Vec<oneshot::Sender<()>> = {
            let mut inner = self.lock();
            inner.paused = false;
            inner.gate.drain(..).collect()
        };
        for tx in waiters {
            let _ = tx.send(());
        }
    }

    /// Take a semaphore slot, but only once the gate is ALSO open — and re-check
    /// both as a loop, because either can change while this call awaits the
    /// other.
    ///
    /// WHY THIS IS NOT JUST `acquire()`. Pause used to be consulted once, before
    /// the journal row and before the semaphore, which made it a no-op for the
    /// one shape workflows exist for: `parallel()` issues every thunk at
    /// dispatch, so a fan-out of six at concurrency two has all six calls past
    /// the gate within the first tick and four of them merely parked on the
    /// semaphore. Pausing then released nothing and gated nothing.
    ///
    /// A call parked here keeps its journal row at `queued`, never `running`.
    /// The slot is RELEASED while parked rather than held: a paused run admits
    /// nothing, so holding it would only mean that on resume the semaphore's own
    /// FIFO no longer matches the order calls arrived in.
    ///
    /// Returns false when the run was stopped while this call was parked.
    async fn admit(&self) -> bool {
        loop {
            if self.ctrl.is_cancelled() {
                return false;
            }
            if self.is_paused() {
                self.await_gate().await;
                // Re-check abort before touching the semaphore: stop opens the
                // gate too.
                continue;
            }
            self.acquire().await;
            if self.ctrl.is_cancelled() || self.is_paused() {
                self.release();
                if self.ctrl.is_cancelled() {
                    return false;
                }
                continue;
            }
            return true;
        }
    }
}

// ---------------------------------------------------------------------------
// Db + bus helpers
// ---------------------------------------------------------------------------

fn with_db<T>(db: &SharedDb, f: impl FnOnce(&dyn Db) -> T) -> T {
    let guard = db.lock().unwrap_or_else(|p| p.into_inner());
    f(&*guard)
}

fn publish_run(db: &SharedDb, bus: &Bus, id: &str) -> Option<WorkflowRun> {
    let run = with_db(db, |d| d.get_workflow(id)).ok().flatten();
    if let Some(run) = &run {
        bus.publish(EventInput {
            r#type: EventType::WorkflowUpdated,
            session_id: Some(run.session_id.clone()),
            data: serde_json::to_value(run).unwrap_or(Value::Null),
        });
    }
    run
}

fn publish_agent(db: &SharedDb, bus: &Bus, session_id: &str, run_id: &str, agent_id: &str) {
    let row = with_db(db, |d| d.list_workflow_agents(run_id))
        .ok()
        .and_then(|rows| rows.into_iter().find(|a| a.id == agent_id));
    if let Some(row) = row {
        bus.publish(EventInput {
            r#type: EventType::WorkflowAgent,
            session_id: Some(session_id.to_string()),
            data: serde_json::to_value(&row).unwrap_or(Value::Null),
        });
    }
}

fn publish_log(bus: &Bus, session_id: &str, run_id: &str, line: String) {
    bus.publish(EventInput {
        r#type: EventType::WorkflowLog,
        session_id: Some(session_id.to_string()),
        data: serde_json::to_value(WorkflowLogData {
            run_id: run_id.to_string(),
            line,
        })
        .unwrap_or(Value::Null),
    });
}

fn workflow_error(status: u16, message: impl Into<String>) -> BoughError {
    BoughError::http(status, ErrorKind::Workflow, message)
}

// ---------------------------------------------------------------------------
// Starting a run
// ---------------------------------------------------------------------------

/// Start a workflow: persist the run and its script mirror, build the
/// journal-replay map when resuming, and launch the worker.
///
/// Returns the run row IMMEDIATELY — the script is detached from here on.
/// Progress flows over `workflow.*` bus events and completion posts a system
/// note, which is what lets the turn that called `workflow.start` end while the
/// fan-out continues.
pub async fn start_workflow(ctx: &WorkflowCtx, opts: StartOpts) -> Result<WorkflowRun, BoughError> {
    let now = ctx.clock();

    if with_db(&ctx.db, |d| d.get_session(&opts.session_id))?.is_none() {
        return Err(BoughError::not_found(format!(
            "session {} not found",
            opts.session_id
        )));
    }
    if opts.script.trim().is_empty() {
        return Err(BoughError::workflow_script(
            "workflow: script must be a non-empty string",
        ));
    }
    let body = workflow_body(&opts.script);
    if let Some(bad) = check_workflow_syntax(&body).await {
        return Err(BoughError::workflow_script(bad));
    }

    // Journal replay, PREFIX-BOUNDED. The source run's calls in structural
    // order; the engine below replays the longest leading run of them whose
    // keys still match and stops for good at the first that does not. Only
    // calls that ANSWERED are replayable — a failed one re-runs live, because
    // the failure may be the very thing this edit fixes, and everything after
    // it re-runs too, because a live call may have changed the checkout the
    // later answers were computed against.
    let mut plan = empty_replay_plan();
    let mut args: Value = opts.args.clone().unwrap_or(Value::Null);
    let mut meta = opts.meta.clone();
    if let Some(source_id) = &opts.resume_of {
        let src = with_db(&ctx.db, |d| d.get_workflow(source_id))?
            .ok_or_else(|| BoughError::not_found(format!("workflow {source_id} not found")))?;
        if opts.args.is_none() {
            // A relaunch keeps its input by default.
            args = src.args.clone().unwrap_or(Value::Null);
        }
        if meta.is_none() {
            meta = Some(WorkflowMeta {
                name: src.name.clone(),
                description: src.description.clone(),
                phases: Some(src.phases.clone()),
            });
        }
        plan = replay_plan(&ctx.db, source_id)?;
    }

    let id = Uuid::new_v4().to_string();
    let run = with_db(&ctx.db, |d| {
        d.create_workflow(WorkflowRun {
            id: id.clone(),
            session_id: opts.session_id.clone(),
            name: meta
                .as_ref()
                .map(|m| m.name.clone())
                .unwrap_or_else(|| "workflow".to_string()),
            description: meta
                .as_ref()
                .map(|m| m.description.clone())
                .unwrap_or_default(),
            script: opts.script.clone(),
            phases: meta
                .as_ref()
                .and_then(|m| m.phases.clone())
                .unwrap_or_default(),
            status: WorkflowStatus::Running,
            current_phase: None,
            result: None,
            error: None,
            args: Some(args.clone()),
            resume_of: opts.resume_of.clone(),
            created_at: now(),
            finished_at: None,
        })
    })?;

    // Mirror the script to a real file so "edit it and relaunch" is a file edit
    // away. A convenience — the canonical script is the row — and best-effort: a
    // read-only `~/.bough` must not stop a run from starting.
    mirror_script(&id, &opts.script).await;

    ctx.bus.publish(EventInput {
        r#type: EventType::WorkflowUpdated,
        session_id: Some(run.session_id.clone()),
        data: serde_json::to_value(&run).unwrap_or(Value::Null),
    });

    let state = Arc::new(RunState {
        id: id.clone(),
        session_id: run.session_id.clone(),
        db: ctx.db.clone(),
        bus: ctx.bus.clone(),
        runner: ctx.runner.clone(),
        notify: ctx.notify.clone(),
        now: now.clone(),
        ctrl: CancellationToken::new(),
        limit: opts.concurrency.unwrap_or_else(workflow_concurrency).max(1),
        plan,
        effective_model: opts.effective_model.clone(),
        inner: Mutex::new(Inner {
            idx: 0,
            diverged_pos: None,
            diverged_at: None,
            divergence: None,
            paused: false,
            in_flight: 0,
            sem_queue: VecDeque::new(),
            gate: VecDeque::new(),
        }),
    });

    let mut worker = match WorkflowWorker::spawn().await {
        Ok(w) => w,
        Err(e) => {
            // No worker, no run: settle it here rather than leaving a `running`
            // row nothing in this process could ever finish.
            with_db(&ctx.db, |d| {
                d.update_workflow(
                    &id,
                    WorkflowPatch {
                        status: Some(WorkflowStatus::Error),
                        error: Patch::Set(format!("workflow worker error: {e}")),
                        finished_at: Patch::Set(now()),
                        ..Default::default()
                    },
                )
            })?;
            let updated = publish_run(&ctx.db, &ctx.bus, &id);
            return updated
                .ok_or_else(|| workflow_error(500, format!("workflow worker error: {e}")));
        }
    };

    live()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(id.clone(), state.clone());

    let timeout_ms = opts.timeout_ms.unwrap_or_else(workflow_timeout_ms);
    let args_json = serde_json::to_string(&args).unwrap_or_else(|_| "null".to_string());
    worker.post_run(&body, &args_json);

    // The message loop. One task per run; host calls that can block are spawned
    // off it (ARCHITECTURE §4.2).
    let loop_state = state.clone();
    tokio::spawn(async move {
        let sender = worker.sender();
        let wall = tokio::time::sleep(Duration::from_millis(timeout_ms));
        tokio::pin!(wall);
        let mut group_kill = false;
        loop {
            tokio::select! {
                msg = worker.next() => match msg {
                    Some(FromWorkflowWorker::Done { result_json }) => {
                        // A script that returned something unserializable
                        // finishes as null rather than not at all.
                        let result = serde_json::from_str::<Value>(&result_json).unwrap_or(Value::Null);
                        finish(&loop_state, WorkflowStatus::Done, Some(result), None);
                        break;
                    }
                    Some(FromWorkflowWorker::Error { message, .. }) => {
                        finish(&loop_state, WorkflowStatus::Error, None, Some(message));
                        break;
                    }
                    // Wind-down ack / pre-flight answer: nothing to do.
                    Some(FromWorkflowWorker::Aborted) => continue,
                    Some(FromWorkflowWorker::CheckResult { .. }) => continue,
                    Some(FromWorkflowWorker::Host { id: call_id, fn_name, args, pos }) => {
                        serve_host_call(&loop_state, &sender, call_id, &fn_name, &args, pos.as_deref());
                    }
                    // The worker's stdout closed: the sidecar equivalent of
                    // `worker.onerror`.
                    None => {
                        let detail = match worker.stderr_text() {
                            s if s.is_empty() => "the worker exited before posting a result".to_string(),
                            s => s,
                        };
                        finish(
                            &loop_state,
                            WorkflowStatus::Error,
                            None,
                            Some(format!("workflow worker error: {detail}")),
                        );
                        break;
                    }
                },
                // `stop_workflow` from another task: it has already swept the
                // rows and settled the run; all that is left is the worker.
                _ = loop_state.ctrl.cancelled() => {
                    group_kill = true;
                    break;
                }
                _ = &mut wall => {
                    finish(
                        &loop_state,
                        WorkflowStatus::Error,
                        None,
                        Some(format!("workflow timed out after {timeout_ms}ms")),
                    );
                    break;
                }
            }
        }
        if group_kill {
            worker.kill_group();
        } else {
            worker.kill();
        }
    });

    Ok(run)
}

/// One bridged call from the script. Runs ON the message-loop task: `phase` and
/// `log` are pure writes, and `agent` does its decision and its journal insert
/// here before handing the blocking part to a spawned task.
fn serve_host_call(
    state: &Arc<RunState>,
    sender: &tokio::sync::mpsc::UnboundedSender<String>,
    call_id: u64,
    fn_name: &str,
    args: &[Value],
    pos: Option<&str>,
) {
    let reply = |ok: bool, value: String| {
        let _ = sender.send(
            json!({"type": "host_result", "id": call_id, "ok": ok, "value": value}).to_string(),
        );
    };
    // Validate against the canonical list before dispatching: the worker global
    // is reachable from the script, so `fn` is not guaranteed to be one of ours.
    if !WORKFLOW_HOST_FN_NAMES.contains(&fn_name) {
        reply(false, format!("unknown workflow host function: {fn_name}"));
        return;
    }
    let first = |i: usize| {
        args.get(i)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };

    match fn_name {
        "phase" => {
            let title = first(0);
            let _ = with_db(&state.db, |d| {
                d.update_workflow(
                    &state.id,
                    WorkflowPatch {
                        current_phase: Patch::Set(title),
                        ..Default::default()
                    },
                )
            });
            publish_run(&state.db, &state.bus, &state.id);
            reply(true, String::new());
        }
        "log" => {
            publish_log(&state.bus, &state.session_id, &state.id, first(0));
            reply(true, String::new());
        }
        "agent" => {
            let prompt = first(0);
            let opts_json = first(1);
            // THE SYNCHRONOUS SECTION: parse, index, coordinate, key, prefix
            // decision — and, when the run is not paused, the journal row too.
            // No await between them, so the answer is a pure function of
            // (coordinate, key) and never of which concurrent call resumed
            // first.
            let decision = match decide(state, &prompt, &opts_json, pos) {
                Err(e) => {
                    reply(false, e.to_string());
                    return;
                }
                Ok(d) => d,
            };
            let journaled = if state.is_paused() {
                // Pause parks the call BEFORE it journals: a call that has not
                // been admitted yet has no row, so the UI never shows a
                // "running" agent that has not actually started. The spawned
                // task journals it once the gate opens.
                None
            } else {
                match journal(state, &decision) {
                    Err(e) => {
                        reply(false, e.to_string());
                        return;
                    }
                    Ok(row) => Some(row),
                }
            };
            // A journal hit: no live call, no semaphore slot, no cost.
            if let Some(row) = &journaled {
                if let Some(cached) = &decision.cached {
                    let _ = row;
                    reply(true, cached.clone());
                    return;
                }
            }
            let state = state.clone();
            let sender = sender.clone();
            tokio::spawn(async move {
                let answer = run_call(&state, decision, journaled).await;
                let msg = match answer {
                    Ok(v) => json!({"type":"host_result","id":call_id,"ok":true,"value":v}),
                    Err(e) => {
                        json!({"type":"host_result","id":call_id,"ok":false,"value":e.to_string()})
                    }
                };
                let _ = sender.send(msg.to_string());
            });
        }
        _ => reply(false, format!("unknown workflow host function: {fn_name}")),
    }
}

/// Everything the synchronous section decided about one `agent()` call.
#[derive(Debug)]
struct Decision {
    at: i64,
    call: AgentCall,
    /// True when the caller passed an explicit label — the display label is then
    /// the same as the hashed one.
    explicit_label: bool,
    /// `<pos>|<contentHash>`; the halves stay recoverable (`pos::split_journal_key`).
    key: String,
    /// `Some` = this call replays from the source journal.
    cached: Option<String>,
}

/// The `agent()` options blob, defensively parsed — it crossed a string-only
/// wire.
fn parse_agent_opts(opts_json: &str) -> serde_json::Map<String, Value> {
    serde_json::from_str::<Value>(if opts_json.is_empty() {
        "{}"
    } else {
        opts_json
    })
    .ok()
    .and_then(|v| v.as_object().cloned())
    .unwrap_or_default()
}

/// The prefix decision, made SYNCHRONOUSLY — before the gate, before the
/// semaphore, in the same uninterrupted block that assigned `at`.
fn decide(
    state: &Arc<RunState>,
    prompt: &str,
    opts_json: &str,
    pos: Option<&str>,
) -> Result<Decision, BoughError> {
    let raw = parse_agent_opts(opts_json);
    if prompt.trim().is_empty() {
        return Err(workflow_error(
            400,
            "agent(prompt, opts): prompt must be a non-empty string",
        ));
    }
    let explicit = raw
        .get("label")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string);
    let call = AgentCall {
        prompt: prompt.to_string(),
        label: explicit
            .clone()
            .unwrap_or_else(|| clip(prompt.trim().split('\n').next().unwrap_or_default(), 40)),
        phase: raw.get("phase").and_then(Value::as_str).map(str::to_string),
        model: raw.get("model").and_then(Value::as_str).map(str::to_string),
        schema: raw.get("schema").cloned(),
    };

    let mut inner = state.lock();
    let at = inner.idx;
    inner.idx += 1;
    if at >= MAX_AGENTS_PER_RUN {
        return Err(workflow_error(
            429,
            format!(
                "workflow agent cap reached ({MAX_AGENTS_PER_RUN} per run) — this is a \
                 runaway-loop backstop; split the work across separate runs"
            ),
        ));
    }

    // The call's coordinate. The worker computes it from the script's SHAPE;
    // absent only when something other than that worker is driving this host, in
    // which case the old monotonic counter is the right answer and is exactly
    // what a sequential script's coordinates already are.
    let call_pos: CallPos = match pos {
        Some(p) if !p.is_empty() => p.to_string(),
        _ => at.to_string(),
    };
    let content = call_key(&call, state.effective_model.as_deref());
    let key = journal_key(&call_pos, &content);

    let mut cached = None;
    if !state.plan.steps.is_empty() {
        let blocked = inner
            .diverged_pos
            .as_ref()
            .is_some_and(|d| !compare_pos(&call_pos, d).is_lt());
        let step = state.plan.by_pos.get(&call_pos);
        if blocked {
            // Behind a divergence that has already been announced. It runs live
            // and says nothing more — one line per divergence, not one per
            // consequence.
        } else if let Some(step) = step.filter(|s| s.content == content && s.result.is_some()) {
            cached = step.result.clone();
        } else {
            let why = classify_divergence(&state.plan, &call_pos, &content);
            inner.diverged_pos = Some(call_pos.clone());
            inner.diverged_at = Some(at);
            inner.divergence = Some(why.clone());
            // Said out loud, because this is the moment a relaunch stops being
            // free and the whole rest of the run becomes live work. A run that
            // quietly replayed nothing looks exactly like one that replayed
            // everything.
            let line = format!(
                "replay ends at {call_pos} (call {at}, {}): {} — it and everything after it in \
                 the script run live, including calls whose own key is unchanged (agents share \
                 one checkout)",
                clip(&call.label, 60),
                why.reason
            );
            drop(inner);
            publish_log(&state.bus, &state.session_id, &state.id, line);
            return Ok(Decision {
                at,
                call,
                explicit_label: explicit.is_some(),
                key,
                cached: None,
            });
        }
    }

    Ok(Decision {
        at,
        call,
        explicit_label: explicit.is_some(),
        key,
        cached,
    })
}

/// Write the journal row. Synchronous, and — when the run is not paused —
/// on the message-loop task in the same non-await section as [`decide`].
fn journal(state: &Arc<RunState>, d: &Decision) -> Result<WorkflowAgent, BoughError> {
    // Stop opens the gate on the way DOWN, not only resume — so nothing is left
    // parked on a run that no longer exists. A call woken that way must not
    // journal: the wind-down has already swept every non-terminal row, so a row
    // written after it would sit at `queued` with nothing left in this process
    // that could settle it.
    if state.ctrl.is_cancelled() {
        return Err(workflow_error(
            409,
            "workflow stopped — this call was never journaled",
        ));
    }
    let now = state.now();
    // Display label: an explicit one wins; otherwise a line this agent does not
    // share with the siblings already in the run.
    let shown = if d.explicit_label {
        d.call.label.clone()
    } else {
        let taken: Vec<String> = with_db(&state.db, |db| db.list_workflow_agents(&state.id))?
            .into_iter()
            .map(|a| a.label)
            .collect();
        distinct_label(&d.call.prompt, &taken)
    };
    let current_phase =
        with_db(&state.db, |db| db.get_workflow(&state.id))?.and_then(|r| r.current_phase);
    let row = with_db(&state.db, |db| {
        db.create_workflow_agent(WorkflowAgent {
            id: Uuid::new_v4().to_string(),
            run_id: state.id.clone(),
            idx: d.at,
            key: d.key.clone(),
            label: shown,
            phase: d.call.phase.clone().or(current_phase),
            prompt: d.call.prompt.clone(),
            // The RESOLVED model, for the same reason `call_key` hashes the
            // resolved one: storing only `call.model` left the run view with a
            // blank model on every ordinary call — the column you would check to
            // notice that a session pin never reached the agents.
            model: d
                .call
                .model
                .clone()
                .or_else(|| state.effective_model.clone()),
            status: if d.cached.is_some() {
                WorkflowAgentStatus::Cached
            } else {
                WorkflowAgentStatus::Queued
            },
            result: d.cached.clone(),
            error: None,
            session_id: None,
            started_at: now,
            finished_at: d.cached.as_ref().map(|_| now),
        })
    })?;
    publish_agent(&state.db, &state.bus, &state.session_id, &state.id, &row.id);
    Ok(row)
}

/// The blocking half of one `agent()` call: the pause gate, the semaphore, the
/// subagent, and settling the row. Runs on its own task.
async fn run_call(
    state: &Arc<RunState>,
    d: Decision,
    journaled: Option<WorkflowAgent>,
) -> Result<String, BoughError> {
    let row = match journaled {
        Some(row) => row,
        None => {
            // Parked: the run was paused when the call arrived, so the gate is
            // waited on BEFORE the row exists.
            state.await_gate().await;
            let row = journal(state, &d)?;
            if let Some(cached) = &d.cached {
                return Ok(cached.clone());
            }
            row
        }
    };
    if let Some(cached) = &d.cached {
        return Ok(cached.clone());
    }

    // The gate and the semaphore, together. A paused run holds the call HERE,
    // with its row still `queued`, however many calls the script dispatched.
    let admitted = state.admit().await;

    // ONE try/catch, not two nested ones. The abort check used to sit inside an
    // outer block whose only cleanup released the semaphore, so failing on it
    // stepped straight over the handler that settles the row — a call stopped
    // between journaling and starting left `queued` behind forever.
    let outcome = async {
        if !admitted || state.ctrl.is_cancelled() {
            return Err(workflow_error(
                409,
                format!(
                    "workflow stopped — \"{}\" was queued and never started",
                    clip(&d.call.label, 60)
                ),
            ));
        }
        // Off the semaphore and past the gate: the clock starts HERE, not when
        // the call journaled, so elapsed time excludes time parked or paused.
        with_db(&state.db, |db| {
            db.update_workflow_agent(
                &row.id,
                WorkflowAgentPatch {
                    status: Some(WorkflowAgentStatus::Running),
                    started_at: Some(state.now()),
                    ..Default::default()
                },
            )
        })?;
        publish_agent(&state.db, &state.bus, &state.session_id, &state.id, &row.id);

        let on_spawned: OnSpawned = {
            let state = state.clone();
            let row_id = row.id.clone();
            Arc::new(move |sid: &str| {
                let _ = with_db(&state.db, |db| {
                    db.update_workflow_agent(
                        &row_id,
                        WorkflowAgentPatch {
                            session_id: Patch::Set(sid.to_string()),
                            ..Default::default()
                        },
                    )
                });
                publish_agent(&state.db, &state.bus, &state.session_id, &state.id, &row_id);
            })
        };
        let report = state
            .runner
            .run(&d.call, state.ctrl.clone(), on_spawned)
            .await?;
        with_db(&state.db, |db| {
            db.update_workflow_agent(
                &row.id,
                WorkflowAgentPatch {
                    status: Some(WorkflowAgentStatus::Done),
                    result: Patch::Set(report.clone()),
                    finished_at: Patch::Set(state.now()),
                    ..Default::default()
                },
            )
        })?;
        publish_agent(&state.db, &state.bus, &state.session_id, &state.id, &row.id);
        Ok(report)
    }
    .await;

    let outcome = match outcome {
        Ok(report) => Ok(report),
        Err(err) => {
            let _ = with_db(&state.db, |db| {
                db.update_workflow_agent(
                    &row.id,
                    WorkflowAgentPatch {
                        status: Some(if state.ctrl.is_cancelled() {
                            WorkflowAgentStatus::Stopped
                        } else {
                            WorkflowAgentStatus::Error
                        }),
                        error: Patch::Set(err.to_string()),
                        finished_at: Patch::Set(state.now()),
                        ..Default::default()
                    },
                )
            });
            publish_agent(&state.db, &state.bus, &state.session_id, &state.id, &row.id);
            // Returned as an error, not swallowed: the script's own combinators
            // decide what a failed agent means — `null` in a parallel() slot, a
            // dropped item in a pipeline() — and neither works if this resolves.
            Err(err)
        }
    };
    // Only if a slot was actually taken. `admit()` returning false took none.
    if admitted {
        state.release();
    }
    outcome
}

// ---------------------------------------------------------------------------
// Finishing
// ---------------------------------------------------------------------------

/// Wind-down: leave the live registry (which is what makes this idempotent),
/// abort the run controller — aborting is what interrupts in-flight subagent
/// TURNS — then open the gate, sweep every non-terminal row, settle the run,
/// publish, notify.
///
/// ABORT BEFORE THE GATE, deliberately: everything unparked wakes to an
/// already-aborted signal by construction rather than by scheduler timing.
fn finish(
    state: &Arc<RunState>,
    status: WorkflowStatus,
    result: Option<Value>,
    error: Option<String>,
) {
    let Some(state) = live_take(&state.id) else {
        return;
    };
    state.ctrl.cancel();
    state.open_gate();
    sweep_rows(&state.db, &state.id, state.now());
    let _ = with_db(&state.db, |d| {
        d.update_workflow(
            &state.id,
            WorkflowPatch {
                status: Some(status),
                result: match &result {
                    Some(v) => Patch::Set(v.clone()),
                    None => Patch::Clear,
                },
                error: match &error {
                    Some(e) => Patch::Set(e.clone()),
                    None => Patch::Clear,
                },
                finished_at: Patch::Set(state.now()),
                ..Default::default()
            },
        )
    });
    let Some(updated) = publish_run(&state.db, &state.bus, &state.id) else {
        return;
    };
    let Some(notify) = state.notify.clone() else {
        return;
    };
    let agents = with_db(&state.db, |d| d.list_workflow_agents(&state.id)).unwrap_or_default();
    notify(
        &updated.session_id,
        &completion_note(&state, &updated, &agents, status, result, error),
    );
}

/// Every row that exists at this instant. Rows can still settle after it — a
/// call unparked by the abort writes its own terminal status — but none can be
/// CREATED after it, which is what closes the hole.
fn sweep_rows(db: &SharedDb, run_id: &str, now: i64) {
    let rows = with_db(db, |d| d.list_workflow_agents(run_id)).unwrap_or_default();
    for a in rows {
        if matches!(
            a.status,
            WorkflowAgentStatus::Running | WorkflowAgentStatus::Queued
        ) {
            let _ = with_db(db, |d| {
                d.update_workflow_agent(
                    &a.id,
                    WorkflowAgentPatch {
                        status: Some(WorkflowAgentStatus::Stopped),
                        finished_at: Patch::Set(now),
                        ..Default::default()
                    },
                )
            });
        }
    }
}

/// The completion note the owning session wakes to.
///
/// Replay is REPORTED, always. A relaunch that replayed nothing and one that
/// replayed everything produce the same row, the same events and the same
/// result — the counts are the only thing that makes a broken key visible, so
/// they ride the note the model actually reads rather than a view someone may
/// open.
fn completion_note(
    state: &Arc<RunState>,
    updated: &WorkflowRun,
    agents: &[WorkflowAgent],
    status: WorkflowStatus,
    result: Option<Value>,
    error: Option<String>,
) -> String {
    let ok_count = agents
        .iter()
        .filter(|a| {
            matches!(
                a.status,
                WorkflowAgentStatus::Done | WorkflowAgentStatus::Cached
            )
        })
        .count();
    let replayed = agents
        .iter()
        .filter(|a| a.status == WorkflowAgentStatus::Cached)
        .count();
    let (diverged_pos, diverged_at, divergence) = {
        let inner = state.lock();
        (
            inner.diverged_pos.clone(),
            inner.diverged_at,
            inner.divergence.clone(),
        )
    };
    let status_word = match status {
        WorkflowStatus::Done => "done",
        WorkflowStatus::Error => "error",
        WorkflowStatus::Stopped => "stopped",
        WorkflowStatus::Paused => "paused",
        WorkflowStatus::Running => "running",
        WorkflowStatus::Orphaned => "orphaned",
    };
    let replay_line = if !state.plan.steps.is_empty() {
        let tail = match &divergence {
            None => " (the whole prefix matched).".to_string(),
            Some(d) => format!(
                ", from {} (call {}) on — {}.",
                diverged_pos.clone().unwrap_or_default(),
                diverged_at.unwrap_or_default(),
                d.reason
            ),
        };
        format!(
            "Replay: {replayed} replayed from run {}, {} ran live{tail}",
            updated
                .resume_of
                .clone()
                .unwrap_or_else(|| "null".to_string()),
            agents.len() - replayed
        )
    } else {
        // A FIRST run has no PRIOR journal to replay from — it writes one.
        "Replay: not a relaunch — this run started fresh and journalled as it went, so a rerun \
         can replay its unchanged prefix."
            .to_string()
    };
    let head = format!(
        "[workflow {status_word}] \"{}\" ({}) — {ok_count}/{} agents succeeded.\n{replay_line}",
        updated.name,
        state.id,
        agents.len()
    );

    // WHEN THE SCRIPT RETURNED NOTHING, THE NOTE CARRIES THE AGENTS' REPORTS
    // ANYWAY. Walked live on haiku: a two-agent script returned `{}`, the model
    // read `Result: {}`, and then spent a whole extra round fetching the reports
    // it had just produced. Only for an EMPTY result: a script that returned a
    // summary has already decided what matters.
    let empty = match &result {
        None | Some(Value::Null) => true,
        Some(Value::Object(m)) => m.is_empty(),
        Some(Value::Array(a)) => a.is_empty(),
        _ => false,
    };
    let reports = agents
        .iter()
        .filter(|a| a.result.as_deref().is_some_and(|r| !r.is_empty()))
        .map(|a| {
            format!(
                "- {}: {}",
                a.label,
                clip(a.result.as_deref().unwrap_or_default(), 600)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let tail = match status {
        WorkflowStatus::Done => {
            let pretty = serde_json::to_string_pretty(&result.unwrap_or(Value::Null))
                .unwrap_or_else(|_| "null".to_string());
            let mut out = format!("Result:\n{}", clip(&pretty, 4000));
            if empty && !reports.is_empty() {
                out.push_str(&format!(
                    "\nThe script returned nothing, so here is what each agent reported — do \
                     NOT call workflow.status to fetch these again:\n{}",
                    clip(&reports, 4000)
                ));
            }
            out
        }
        WorkflowStatus::Error => {
            format!(
                "Error: {}",
                clip(error.as_deref().unwrap_or("unknown"), 2000)
            )
        }
        _ => "Stopped by the user.".to_string(),
    };
    format!("{head}\n{tail}")
}

// ---------------------------------------------------------------------------
// Control
// ---------------------------------------------------------------------------

/// Stop a run: kill the worker AND interrupt the subagent turns it started.
/// Both, because the worker holds the script and the run's abort signal holds
/// the agents — terminating only the worker would leave a fan-out billing with
/// nobody reading it.
///
/// On a non-live run: `running`/`paused` → `orphaned` (the process that owned it
/// died); otherwise the row as-is, so a second stop is a no-op.
pub fn stop_workflow(
    db: &SharedDb,
    bus: &Bus,
    now: Option<&Clock>,
    id: &str,
) -> Result<WorkflowRun, BoughError> {
    let now: Clock = now.cloned().unwrap_or_else(system_clock);
    let run = with_db(db, |d| d.get_workflow(id))?
        .ok_or_else(|| BoughError::not_found(format!("workflow {id} not found")))?;
    let Some(state) = live_take(id) else {
        // Not live here: either it already finished, or the process that owned
        // it died.
        if matches!(run.status, WorkflowStatus::Running | WorkflowStatus::Paused) {
            with_db(db, |d| {
                d.update_workflow(
                    id,
                    WorkflowPatch {
                        status: Some(WorkflowStatus::Orphaned),
                        finished_at: Patch::Set(now()),
                        ..Default::default()
                    },
                )
            })?;
            return publish_run(db, bus, id)
                .ok_or_else(|| BoughError::not_found(format!("workflow {id} not found")));
        }
        return Ok(run);
    };
    // Aborting is what interrupts in-flight subagent TURNS; the message-loop
    // task sees the same token and kills the worker.
    state.ctrl.cancel();
    // Then release anything parked on the pause gate, so no promise leaks with
    // the worker gone. ABORT FIRST, deliberately: everything unparked here wakes
    // to an already-aborted signal and takes the wind-down path — journaling
    // nothing if it had not journaled yet, settling its own row if it had.
    state.open_gate();
    sweep_rows(db, id, now());
    with_db(db, |d| {
        d.update_workflow(
            id,
            WorkflowPatch {
                status: Some(WorkflowStatus::Stopped),
                finished_at: Patch::Set(now()),
                ..Default::default()
            },
        )
    })?;
    publish_run(db, bus, id)
        .ok_or_else(|| BoughError::not_found(format!("workflow {id} not found")))
}

/// Pause: no further agent STARTS; the ones already running finish normally.
///
/// "Starts", not "is issued". The distinction is the whole of pause's promise —
/// it is what preserves the most work before a stop, and it only does that if it
/// bites on a fan-out, whose calls are all issued at dispatch and then sit on
/// the semaphore.
pub fn pause_workflow(db: &SharedDb, bus: &Bus, id: &str) -> Result<WorkflowRun, BoughError> {
    let state = live_get(id).ok_or_else(|| {
        workflow_error(409, format!("workflow {id} is not running in this process"))
    })?;
    state.lock().paused = true;
    with_db(db, |d| {
        d.update_workflow(
            id,
            WorkflowPatch {
                status: Some(WorkflowStatus::Paused),
                ..Default::default()
            },
        )
    })?;
    publish_run(db, bus, id)
        .ok_or_else(|| BoughError::not_found(format!("workflow {id} not found")))
}

/// Resume: open the gate and release the parked calls, FIFO.
pub fn resume_workflow(db: &SharedDb, bus: &Bus, id: &str) -> Result<WorkflowRun, BoughError> {
    let state = live_get(id).ok_or_else(|| {
        workflow_error(409, format!("workflow {id} is not running in this process"))
    })?;
    state.open_gate();
    with_db(db, |d| {
        d.update_workflow(
            id,
            WorkflowPatch {
                status: Some(WorkflowStatus::Running),
                ..Default::default()
            },
        )
    })?;
    publish_run(db, bus, id)
        .ok_or_else(|| BoughError::not_found(format!("workflow {id} not found")))
}

/// Rerun a finished run with journal replay: the unchanged PREFIX of its
/// `agent()` calls returns the old run's results instantly, and the first
/// changed call plus everything after it runs live. The script defaults to the
/// run's file mirror, so "edit the file, press r" is the whole iteration loop.
///
/// A rerun is a NEW run pointing back via `resume_of`, never an edit of the old
/// one — nothing in bough is destructively rewritten.
pub async fn rerun_workflow(
    ctx: &WorkflowCtx,
    id: &str,
    opts: RerunOpts,
) -> Result<WorkflowRun, BoughError> {
    let src = with_db(&ctx.db, |d| d.get_workflow(id))?
        .ok_or_else(|| BoughError::not_found(format!("workflow {id} not found")))?;
    if is_workflow_live(id) {
        return Err(workflow_error(
            409,
            format!("workflow {id} is still running — stop it first"),
        ));
    }
    // Explicit script, else the mirror the user may have edited, else the stored
    // row — one resolution, in `workflow/journal_fs.rs`.
    let (script, _from) = resolve_rerun_script(&src, opts.script.as_deref()).await;
    start_workflow(
        ctx,
        StartOpts {
            session_id: src.session_id.clone(),
            script,
            meta: opts.meta,
            args: opts.args,
            resume_of: Some(id.to_string()),
            effective_model: opts.effective_model,
            ..Default::default()
        },
    )
    .await
}

/// Boot recovery: runs left `running`/`paused` by a process that died. Same rule
/// as orphaned turns — a restart is SURFACED, not resumed. The worker and every
/// subagent turn it was driving went with the old process; re-running them would
/// spend the user's money on work they did not ask for twice.
pub fn recover_orphaned_workflows(
    db: &SharedDb,
    bus: Option<&Bus>,
    now: Option<&Clock>,
) -> Result<Vec<String>, BoughError> {
    let now: Clock = now.cloned().unwrap_or_else(system_clock);
    let mut recovered = Vec::new();
    for run in with_db(db, |d| d.unfinished_workflows())? {
        if is_workflow_live(&run.id) {
            continue;
        }
        sweep_rows(db, &run.id, now());
        with_db(db, |d| {
            d.update_workflow(
                &run.id,
                WorkflowPatch {
                    status: Some(WorkflowStatus::Orphaned),
                    error: Patch::Set(
                        "the server restarted before this workflow finished".to_string(),
                    ),
                    finished_at: Patch::Set(now()),
                    ..Default::default()
                },
            )
        })?;
        recovered.push(run.id.clone());
        if let Some(bus) = bus {
            publish_run(db, bus, &run.id);
        }
    }
    Ok(recovered)
}

// ---------------------------------------------------------------------------
// Views
// ---------------------------------------------------------------------------

/// A run trimmed for program and route consumption. The script text is omitted
/// — it is the largest field by far and a `workflow.list()` that carried N
/// copies of it would flood the model's context for no purpose.
pub fn workflow_summary(db: &SharedDb, run: &WorkflowRun) -> Value {
    let agents = with_db(db, |d| d.list_workflow_agents(&run.id)).unwrap_or_default();
    let count = |f: fn(&WorkflowAgent) -> bool| agents.iter().filter(|a| f(a)).count();
    json!({
        "id": run.id,
        "name": run.name,
        "description": run.description,
        "status": run.status,
        "currentPhase": run.current_phase,
        "phases": run.phases,
        "agents": {
            "total": agents.len(),
            "done": count(|a| matches!(a.status, WorkflowAgentStatus::Done | WorkflowAgentStatus::Cached)),
            "cached": count(|a| a.status == WorkflowAgentStatus::Cached),
            "running": count(|a| a.status == WorkflowAgentStatus::Running),
            "queued": count(|a| a.status == WorkflowAgentStatus::Queued),
            "failed": count(|a| a.status == WorkflowAgentStatus::Error),
        },
        "result": run.result,
        "error": run.error,
        "resumeOf": run.resume_of,
        "createdAt": run.created_at,
        "finishedAt": run.finished_at,
        "scriptFile": workflow_script_path(&run.id).to_string_lossy(),
    })
}

// ---------------------------------------------------------------------------
// Tests — the engine driven through a REAL workflow worker with a fake
// `AgentRunner` in place of the subagents. Nothing here mocks the bridge: the
// things that can go wrong are concurrency and lifecycle, and a fake bridge
// would prove neither (run.test.ts's own header).
//
// Hermetic and offline: an in-memory database, a real bus, no network, no key,
// and `BOUGH_HOME` pointed at a temp dir for the duration of each test so the
// script mirror never touches the real `~/.bough`.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::testkit::{seed_session, shared_db, SeedOpts};
    use crate::schema::events::BoughEvent;
    use crate::workflow::replay::replay_audit;
    use crate::workflow::runner::FnRunner;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Fx {
        db: SharedDb,
        bus: Arc<Bus>,
        session_id: String,
        events: Arc<Mutex<Vec<BoughEvent>>>,
        notes: Arc<Mutex<Vec<String>>>,
    }

    fn fixture() -> Fx {
        let db = shared_db();
        let bus = Arc::new(Bus::new(system_clock()));
        let events: Arc<Mutex<Vec<BoughEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = events.clone();
        bus.subscribe(Arc::new(move |e: &BoughEvent| {
            sink.lock().unwrap().push(e.clone())
        }));
        let session = seed_session(&db, SeedOpts::default());
        Fx {
            db,
            bus,
            session_id: session.id,
            events,
            notes: Arc::new(Mutex::new(Vec::new())),
        }
    }

    impl Fx {
        fn ctx(&self, runner: Arc<dyn AgentRunner>) -> WorkflowCtx {
            let notes = self.notes.clone();
            WorkflowCtx {
                db: self.db.clone(),
                bus: self.bus.clone(),
                runner,
                notify: Some(Arc::new(move |_sid: &str, text: &str| {
                    notes.lock().unwrap().push(text.to_string())
                })),
                now: None,
            }
        }

        fn rows(&self, run_id: &str) -> Vec<WorkflowAgent> {
            with_db(&self.db, |d| d.list_workflow_agents(run_id)).unwrap()
        }

        fn run(&self, id: &str) -> WorkflowRun {
            with_db(&self.db, |d| d.get_workflow(id))
                .unwrap()
                .expect("the run row")
        }

        fn log_lines(&self) -> Vec<String> {
            self.events
                .lock()
                .unwrap()
                .iter()
                .filter(|e| e.r#type == EventType::WorkflowLog)
                .map(|e| e.data["line"].as_str().unwrap_or_default().to_string())
                .collect()
        }
    }

    /// `BOUGH_HOME` is process-global, so every engine test takes the crate-wide
    /// lock in `paths::test_env` and runs its body on a current-thread runtime
    /// built inside the guarded closure.
    pub(super) fn with_home<F>(f: impl FnOnce() -> F)
    where
        F: std::future::Future<Output = ()>,
    {
        let home = std::env::temp_dir().join(format!("bough-wfengine-{}", Uuid::new_v4()));
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

    async fn until(what: &str, cond: impl Fn() -> bool) {
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        while !cond() {
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for {what}"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    /// Wait for a run to reach a terminal status.
    async fn finished(fx: &Fx, id: &str) -> WorkflowRun {
        let db = fx.db.clone();
        let run_id = id.to_string();
        until("the run to finish", || {
            !matches!(
                with_db(&db, |d| d.get_workflow(&run_id))
                    .unwrap()
                    .map(|r| r.status),
                Some(WorkflowStatus::Running) | Some(WorkflowStatus::Paused)
            )
        })
        .await;
        fx.run(id)
    }

    /// A runner that reports its own prompt back, so stage output is inspectable.
    fn echo_runner() -> (Arc<dyn AgentRunner>, Arc<Mutex<Vec<String>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        let runner: Arc<dyn AgentRunner> = Arc::new(FnRunner(move |call: AgentCall, _c, _s| {
            let sink = sink.clone();
            async move {
                sink.lock().unwrap().push(call.prompt.clone());
                Ok(call.prompt.clone())
            }
        }));
        (runner, seen)
    }

    /// A runner whose every call parks until the test releases it by prompt. The
    /// pause and stop tests need to hold agents in flight at an exact moment,
    /// and a gate makes that a fact about the schedule rather than a race
    /// against a timer.
    #[derive(Clone, Default)]
    struct Gates {
        started: Arc<Mutex<Vec<String>>>,
        gates: Arc<Mutex<HashMap<String, oneshot::Sender<()>>>>,
    }

    impl Gates {
        fn runner(&self) -> Arc<dyn AgentRunner> {
            let started = self.started.clone();
            let gates = self.gates.clone();
            Arc::new(FnRunner(move |call: AgentCall, _c, _s| {
                let started = started.clone();
                let gates = gates.clone();
                async move {
                    let (tx, rx) = oneshot::channel();
                    gates.lock().unwrap().insert(call.prompt.clone(), tx);
                    started.lock().unwrap().push(call.prompt.clone());
                    let _ = rx.await;
                    Ok(format!("report {}", call.prompt))
                }
            }))
        }
        fn started(&self) -> Vec<String> {
            self.started.lock().unwrap().clone()
        }
        fn release(&self, prompt: &str) -> bool {
            let tx = self.gates.lock().unwrap().remove(prompt);
            match tx {
                Some(tx) => tx.send(()).is_ok(),
                None => false,
            }
        }
        fn release_all(&self) {
            let all: Vec<oneshot::Sender<()>> = self
                .gates
                .lock()
                .unwrap()
                .drain()
                .map(|(_, tx)| tx)
                .collect();
            for tx in all {
                let _ = tx.send(());
            }
        }
    }

    fn start_opts(fx: &Fx, script: &str) -> StartOpts {
        StartOpts {
            session_id: fx.session_id.clone(),
            script: script.to_string(),
            meta: Some(WorkflowMeta {
                name: "test".into(),
                description: "a test workflow".into(),
                phases: None,
            }),
            concurrency: Some(4),
            ..Default::default()
        }
    }

    /// Six agents through one `parallel()` — the shape a workflow exists for.
    const FANOUT: &str =
        "return await parallel([0,1,2,3,4,5].map((i) => () => agent('work ' + i, { label: 'w' + i })))";

    // ---- the first run -----------------------------------------------------

    #[test]
    fn a_first_run_journals_every_call_and_reports_that_it_is_not_a_relaunch() {
        with_home(|| async {
            let fx = fixture();
            let (runner, seen) = echo_runner();
            let ctx = fx.ctx(runner);
            let run = start_workflow(
                &ctx,
                start_opts(&fx, "phase('Review')\nconst a = await agent('one')\nconst b = await agent('two')\nreturn [a, b]"),
            )
            .await
            .expect("the run starts");

            // The row comes back IMMEDIATELY — the script is detached.
            assert_eq!(run.status, WorkflowStatus::Running);
            let done = finished(&fx, &run.id).await;
            assert_eq!(done.status, WorkflowStatus::Done);
            assert_eq!(done.result, Some(json!(["one", "two"])));
            assert_eq!(done.current_phase.as_deref(), Some("Review"));
            assert_eq!(*seen.lock().unwrap(), vec!["one", "two"]);

            let rows = fx.rows(&run.id);
            assert_eq!(rows.len(), 2);
            for (i, row) in rows.iter().enumerate() {
                assert_eq!(row.idx, i as i64);
                assert_eq!(row.status, WorkflowAgentStatus::Done);
                assert!(row.result.is_some());
                // Sequential coordinates are the old monotonic numbering.
                assert!(row.key.starts_with(&format!("{i}|")), "{}", row.key);
                assert_eq!(row.phase.as_deref(), Some("Review"));
            }

            // The note names the run, the tally and the replay accounting.
            let note = fx
                .notes
                .lock()
                .unwrap()
                .first()
                .cloned()
                .expect("a completion note");
            assert!(
                note.starts_with(&format!("[workflow done] \"test\" ({})", run.id)),
                "{note}"
            );
            assert!(note.contains("2/2 agents succeeded."), "{note}");
            assert!(note.contains("Replay: not a relaunch"), "{note}");
            assert!(
                note.contains("Result:\n[\n  \"one\",\n  \"two\"\n]"),
                "{note}"
            );

            // And the wire: one run row per transition, one agent event per row
            // transition.
            let events = fx.events.lock().unwrap();
            assert!(events
                .iter()
                .any(|e| e.r#type == EventType::WorkflowUpdated));
            assert!(
                events
                    .iter()
                    .filter(|e| e.r#type == EventType::WorkflowAgent)
                    .count()
                    >= 4
            );
            // The summary omits the script and counts the buckets.
            let summary = workflow_summary(&fx.db, &done);
            assert!(summary.get("script").is_none());
            assert_eq!(summary["agents"]["total"], 2);
            assert_eq!(summary["agents"]["done"], 2);
            assert_eq!(summary["agents"]["cached"], 0);
        });
    }

    /// A script that returned nothing gets its agents' reports in the note
    /// anyway — the round-trip the prompt warns about is incurred regardless.
    #[test]
    fn an_empty_result_carries_the_agent_reports_into_the_note() {
        with_home(|| async {
            let fx = fixture();
            let (runner, _) = echo_runner();
            let ctx = fx.ctx(runner);
            let run = start_workflow(
                &ctx,
                start_opts(&fx, "await agent('audit the parser')\nreturn {}"),
            )
            .await
            .unwrap();
            finished(&fx, &run.id).await;
            let note = fx.notes.lock().unwrap().first().cloned().expect("a note");
            assert!(note.contains("The script returned nothing"), "{note}");
            assert!(note.contains("do NOT call workflow.status"), "{note}");
            assert!(
                note.contains("- audit the parser: audit the parser"),
                "{note}"
            );
        });
    }

    // ---- the journal row exists before the semaphore admits ----------------

    /// The row is written BEFORE the semaphore is acquired, so a saturated run
    /// shows QUEUED agents rather than pretending all of them work — and
    /// `startedAt` is reset when the call actually starts.
    #[test]
    fn the_journal_row_is_written_before_the_semaphore_admits() {
        with_home(|| async {
            let fx = fixture();
            let gates = Gates::default();
            let ctx = fx.ctx(gates.runner());
            let mut opts = start_opts(&fx, FANOUT);
            opts.concurrency = Some(2);
            let run = start_workflow(&ctx, opts).await.unwrap();

            // Six calls issued at dispatch; two admitted, four parked on the
            // semaphore with rows already journaled.
            until("all six rows to be journaled", || {
                fx.rows(&run.id).len() == 6
            })
            .await;
            until("two agents to start", || gates.started().len() == 2).await;
            let rows = fx.rows(&run.id);
            let running: Vec<&WorkflowAgent> = rows
                .iter()
                .filter(|r| r.status == WorkflowAgentStatus::Running)
                .collect();
            let queued: Vec<&WorkflowAgent> = rows
                .iter()
                .filter(|r| r.status == WorkflowAgentStatus::Queued)
                .collect();
            assert_eq!(
                running.len(),
                2,
                "concurrency is the semaphore, not the journal"
            );
            assert_eq!(queued.len(), 4, "the parked calls are visible as queued");
            // Every row carries its own coordinate and label already.
            for row in &rows {
                assert!(row.key.contains('|'), "{}", row.key);
                assert!(row.label.starts_with('w'), "{}", row.label);
            }

            // Drain: each release frees a slot, which admits the next call.
            while gates.started().len() < 6 {
                gates.release_all();
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            gates.release_all();
            let done = finished(&fx, &run.id).await;
            assert_eq!(done.status, WorkflowStatus::Done);
            assert_eq!(fx.rows(&run.id).len(), 6);
        });
    }

    // ---- pause gates ADMISSION --------------------------------------------

    /// The gate is consulted again AFTER a semaphore slot is taken: a
    /// `parallel()` fan-out is past any pre-dispatch check within the first
    /// tick, so a single check is a no-op for exactly the shape workflows exist
    /// for.
    #[test]
    fn pause_gates_admission_for_a_fan_out_not_just_issuance() {
        with_home(|| async {
            let fx = fixture();
            let gates = Gates::default();
            let ctx = fx.ctx(gates.runner());
            let mut opts = start_opts(&fx, FANOUT);
            opts.concurrency = Some(2);
            let run = start_workflow(&ctx, opts).await.unwrap();

            // Every call is issued at dispatch and journaled before anybody can
            // press pause — that is exactly why a pre-dispatch gate check is a
            // no-op for this shape, and why the assertion below is about the
            // SEMAPHORE.
            until("all six rows to be journaled", || {
                fx.rows(&run.id).len() == 6
            })
            .await;
            until("two agents to start", || gates.started().len() == 2).await;
            let paused = pause_workflow(&fx.db, &fx.bus, &run.id).expect("pause");
            assert_eq!(paused.status, WorkflowStatus::Paused);

            // The two in flight finish normally and are journaled — "pause
            // before you stop preserves the most work".
            gates.release_all();
            until("the two in-flight calls to settle", || {
                fx.rows(&run.id)
                    .iter()
                    .filter(|r| r.status == WorkflowAgentStatus::Done)
                    .count()
                    == 2
            })
            .await;

            // And nothing else starts while the run is paused.
            tokio::time::sleep(Duration::from_millis(150)).await;
            assert_eq!(gates.started().len(), 2, "a paused run admits nothing");
            let rows = fx.rows(&run.id);
            assert_eq!(
                rows.iter()
                    .filter(|r| r.status == WorkflowAgentStatus::Queued)
                    .count(),
                4,
                "the parked calls keep their rows at queued, never running"
            );
            assert!(rows.iter().all(|r| r.session_id.is_none()));

            // Resume releases them, FIFO — two at a time, since the semaphore is
            // still the meter.
            resume_workflow(&fx.db, &fx.bus, &run.id).expect("resume");
            while gates.started().len() < 6 {
                gates.release_all();
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            gates.release_all();
            let done = finished(&fx, &run.id).await;
            assert_eq!(done.status, WorkflowStatus::Done);
        });
    }

    /// The regression the fix had to keep: pause still gates a strictly
    /// sequential script, which is the shape that always worked.
    #[test]
    fn pause_still_gates_a_sequential_script() {
        with_home(|| async {
            let fx = fixture();
            let gates = Gates::default();
            let ctx = fx.ctx(gates.runner());
            let run = start_workflow(
                &fx.ctx(gates.runner()),
                start_opts(&fx, "await agent('one')\nawait agent('two')\nreturn 'done'"),
            )
            .await
            .unwrap();
            let _ = ctx;

            until("the first call to start", || gates.started().len() == 1).await;
            pause_workflow(&fx.db, &fx.bus, &run.id).expect("pause");
            gates.release("one");
            tokio::time::sleep(Duration::from_millis(150)).await;
            assert_eq!(gates.started(), vec!["one"], "the next call must not start");

            resume_workflow(&fx.db, &fx.bus, &run.id).expect("resume");
            until("the second call to start", || gates.started().len() == 2).await;
            gates.release_all();
            assert_eq!(finished(&fx, &run.id).await.status, WorkflowStatus::Done);
        });
    }

    /// A stopped run leaves NO row in a non-terminal state: neither the ones
    /// already journaled nor the ones parked on the gate, which must not journal
    /// after the wind-down swept them.
    #[test]
    fn stopping_a_paused_fan_out_leaves_no_queued_or_running_row() {
        with_home(|| async {
            let fx = fixture();
            let gates = Gates::default();
            let ctx = fx.ctx(gates.runner());
            let mut opts = start_opts(&fx, FANOUT);
            opts.concurrency = Some(2);
            let run = start_workflow(&ctx, opts).await.unwrap();

            until("all six rows", || fx.rows(&run.id).len() == 6).await;
            until("two agents to start", || gates.started().len() == 2).await;
            pause_workflow(&fx.db, &fx.bus, &run.id).expect("pause");
            let stopped = stop_workflow(&fx.db, &fx.bus, None, &run.id).expect("stop");
            assert_eq!(stopped.status, WorkflowStatus::Stopped);

            gates.release_all();
            // Give the unparked calls a moment to settle their own rows.
            tokio::time::sleep(Duration::from_millis(200)).await;
            let rows = fx.rows(&run.id);
            assert!(
                rows.iter().all(|r| !matches!(
                    r.status,
                    WorkflowAgentStatus::Queued | WorkflowAgentStatus::Running
                )),
                "a stopped run left a non-terminal row: {:?}",
                rows.iter()
                    .map(|r| (r.label.clone(), r.status))
                    .collect::<Vec<_>>()
            );
            assert!(!is_workflow_live(&run.id));
            // Stop is idempotent on a finished run.
            assert_eq!(
                stop_workflow(&fx.db, &fx.bus, None, &run.id)
                    .unwrap()
                    .status,
                WorkflowStatus::Stopped
            );
        });
    }

    #[test]
    fn pause_and_resume_refuse_a_run_this_process_does_not_own() {
        with_home(|| async {
            let fx = fixture();
            let err = pause_workflow(&fx.db, &fx.bus, "no-such-run").expect_err("409");
            assert_eq!(err.status(), 409);
            assert!(
                err.to_string().contains("is not running in this process"),
                "{err}"
            );
            let err = resume_workflow(&fx.db, &fx.bus, "no-such-run").expect_err("409");
            assert_eq!(err.status(), 409);
        });
    }

    // ---- replay ------------------------------------------------------------

    /// An unchanged rerun replays EVERY call and runs nothing live.
    #[test]
    fn an_unchanged_rerun_replays_every_call() {
        with_home(|| async {
            let fx = fixture();
            let (runner, seen) = echo_runner();
            let ctx = fx.ctx(runner);
            let script =
                "const a = await agent('one')\nconst b = await agent('two')\nreturn [a, b]";
            let first = start_workflow(&ctx, start_opts(&fx, script)).await.unwrap();
            finished(&fx, &first.id).await;
            assert_eq!(seen.lock().unwrap().len(), 2);

            let second = rerun_workflow(&ctx, &first.id, RerunOpts::default())
                .await
                .unwrap();
            let done = finished(&fx, &second.id).await;
            assert_eq!(done.status, WorkflowStatus::Done);
            assert_eq!(
                done.result,
                Some(json!(["one", "two"])),
                "a replay is invisible to the script"
            );
            assert_eq!(seen.lock().unwrap().len(), 2, "not one live call");
            let rows = fx.rows(&second.id);
            assert!(rows.iter().all(|r| r.status == WorkflowAgentStatus::Cached));
            assert_eq!(done.resume_of.as_deref(), Some(first.id.as_str()));
            // The source run is never touched.
            assert_eq!(fx.rows(&first.id).len(), 2);
            let note = fx.notes.lock().unwrap().last().cloned().unwrap();
            assert!(note.contains("2 replayed from run"), "{note}");
            assert!(note.contains("(the whole prefix matched)."), "{note}");
        });
    }

    /// Editing call 2 of 4 replays 1, and runs 2–4 live INCLUDING the calls
    /// whose own key never changed. The audit reports those as `forced`.
    #[test]
    fn replay_stops_at_the_first_changed_call_and_never_resumes() {
        with_home(|| async {
            let fx = fixture();
            let (runner, seen) = echo_runner();
            let ctx = fx.ctx(runner);
            let script = |second: &str| {
                format!(
                    "await agent('one')\nawait agent('{second}')\nawait agent('three')\n\
                     await agent('four')\nreturn 'ok'"
                )
            };
            let first = start_workflow(&ctx, start_opts(&fx, &script("two")))
                .await
                .unwrap();
            finished(&fx, &first.id).await;
            assert_eq!(seen.lock().unwrap().len(), 4);
            seen.lock().unwrap().clear();

            let second = rerun_workflow(
                &ctx,
                &first.id,
                RerunOpts {
                    script: Some(script("TWO EDITED")),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
            finished(&fx, &second.id).await;

            // Call 0 replayed; 1, 2 and 3 ran live even though 2 and 3 are
            // byte-identical (agents share one checkout).
            assert_eq!(
                *seen.lock().unwrap(),
                vec!["TWO EDITED", "three", "four"],
                "replay must not resume after the first change"
            );
            let rows = fx.rows(&second.id);
            assert_eq!(rows[0].status, WorkflowAgentStatus::Cached);
            assert!(rows[1..]
                .iter()
                .all(|r| r.status == WorkflowAgentStatus::Done));

            // The divergence is announced ONCE, naming which of the four things
            // happened.
            let lines: Vec<String> = fx
                .log_lines()
                .into_iter()
                .filter(|l| l.starts_with("replay ends at"))
                .collect();
            assert_eq!(
                lines.len(),
                1,
                "one line per divergence, not one per consequence"
            );
            assert!(
                lines[0].contains("replay ends at 1 (call 1"),
                "{}",
                lines[0]
            );
            assert!(lines[0].contains("was edited"), "{}", lines[0]);
            assert!(
                lines[0].contains("agents share one checkout"),
                "{}",
                lines[0]
            );

            // And the audit fold says the same thing, with the price named.
            let plan = replay_plan(&fx.db, &first.id).unwrap();
            let audit = replay_audit(&plan, &rows);
            assert_eq!(audit.diverged.as_ref().unwrap().pos, "1");
            assert_eq!(audit.diverged_at, Some(1));
            assert_eq!(
                audit.forced, 2,
                "`three` and `four` are unchanged and ran anyway"
            );

            let note = fx.notes.lock().unwrap().last().cloned().unwrap();
            assert!(note.contains("1 replayed from run"), "{note}");
            assert!(note.contains("3 ran live, from 1 (call 1) on —"), "{note}");
        });
    }

    /// A failed source call ends the prefix; its successors re-run even though
    /// they answered — the answers behind a failure were never available.
    #[test]
    fn only_successful_calls_replay() {
        with_home(|| async {
            let fx = fixture();
            let seen = Arc::new(Mutex::new(Vec::new()));
            let fail_first_run = Arc::new(AtomicUsize::new(0));
            let sink = seen.clone();
            let attempts = fail_first_run.clone();
            let runner: Arc<dyn AgentRunner> =
                Arc::new(FnRunner(move |call: AgentCall, _c, _s| {
                    let sink = sink.clone();
                    let attempts = attempts.clone();
                    async move {
                        sink.lock().unwrap().push(call.prompt.clone());
                        // `two` fails the first time it is ever asked and succeeds
                        // afterwards — the ordinary "the author fixed it" shape.
                        if call.prompt == "two" && attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                            return Err(workflow_error(424, "workflow agent \"two\" error: boom"));
                        }
                        Ok(format!("report {}", call.prompt))
                    }
                }));
            let ctx = fx.ctx(runner);
            let script = "await agent('one')\ntry { await agent('two') } catch {}\n\
                          await agent('three')\nreturn 'ok'";
            let first = start_workflow(&ctx, start_opts(&fx, script)).await.unwrap();
            finished(&fx, &first.id).await;
            let rows = fx.rows(&first.id);
            assert_eq!(rows[1].status, WorkflowAgentStatus::Error);
            assert!(rows[1].error.as_deref().unwrap().contains("boom"));
            assert_eq!(rows[2].status, WorkflowAgentStatus::Done);
            seen.lock().unwrap().clear();

            // The rerun replays only call 0: the failure ends the prefix, and
            // `three` re-runs even though the source answered it.
            let second = rerun_workflow(&ctx, &first.id, RerunOpts::default())
                .await
                .unwrap();
            finished(&fx, &second.id).await;
            assert_eq!(*seen.lock().unwrap(), vec!["two", "three"]);
            let rows = fx.rows(&second.id);
            assert_eq!(rows[0].status, WorkflowAgentStatus::Cached);
            assert_eq!(rows[1].status, WorkflowAgentStatus::Done);
            assert_eq!(rows[2].status, WorkflowAgentStatus::Done);
            let lines: Vec<String> = fx
                .log_lines()
                .into_iter()
                .filter(|l| l.starts_with("replay ends at"))
                .collect();
            assert!(
                lines.last().unwrap().contains("has no answer for it"),
                "{lines:?}"
            );
        });
    }

    /// The flagship case: a barrier-free pipeline with skewed stage-1 latency
    /// journals its calls in COMPLETION order, and an unchanged relaunch must
    /// still replay all four. Arrival-order numbering transposed the stage-2
    /// cells and re-billed every call past stage 1.
    #[test]
    fn an_unchanged_pipeline_replays_four_of_four_under_skewed_latency() {
        with_home(|| async {
            let fx = fixture();
            let seen = Arc::new(Mutex::new(Vec::new()));
            let sink = seen.clone();
            let runner: Arc<dyn AgentRunner> =
                Arc::new(FnRunner(move |call: AgentCall, _c, _s| {
                    let sink = sink.clone();
                    async move {
                        // A's first stage is slow, so B laps it: the journal's
                        // arrival order is [s1 A, s1 B, s2 B, s2 A].
                        if call.prompt == "s1 A" {
                            tokio::time::sleep(Duration::from_millis(120)).await;
                        }
                        sink.lock().unwrap().push(call.prompt.clone());
                        Ok(call.prompt.clone())
                    }
                }));
            let ctx = fx.ctx(runner);
            let script = "return await pipeline(args.items, (item) => agent(`s1 ${item}`), \
                          (prev) => agent(`s2 ${prev}`))";
            let mut opts = start_opts(&fx, script);
            opts.args = Some(json!({"items": ["A", "B"]}));
            let first = start_workflow(&ctx, opts).await.unwrap();
            finished(&fx, &first.id).await;
            assert_eq!(seen.lock().unwrap().len(), 4);
            // The defect's fingerprint: completion order is NOT structural order.
            assert_eq!(
                *seen.lock().unwrap(),
                vec!["s1 B", "s2 s1 B", "s1 A", "s2 s1 A"],
                "the latency skew did not take effect — the test proves nothing"
            );
            seen.lock().unwrap().clear();

            let second = rerun_workflow(&ctx, &first.id, RerunOpts::default())
                .await
                .unwrap();
            let done = finished(&fx, &second.id).await;
            assert_eq!(done.status, WorkflowStatus::Done);
            assert!(
                seen.lock().unwrap().is_empty(),
                "an unchanged pipeline re-billed calls"
            );
            assert_eq!(fx.rows(&second.id).len(), 4);
            assert!(fx
                .rows(&second.id)
                .iter()
                .all(|r| r.status == WorkflowAgentStatus::Cached));
        });
    }

    /// "Edit the file, rerun" is the iteration loop: the mirror outranks the
    /// stored row.
    #[test]
    fn a_rerun_runs_the_mirror_the_user_edited() {
        with_home(|| async {
            let fx = fixture();
            let (runner, seen) = echo_runner();
            let ctx = fx.ctx(runner);
            let first = start_workflow(
                &fx.ctx(ctx.runner.clone()),
                start_opts(&fx, "await agent('stored')\nreturn 'ok'"),
            )
            .await
            .unwrap();
            finished(&fx, &first.id).await;
            assert_eq!(*seen.lock().unwrap(), vec!["stored"]);
            // The mirror is on disk and is what an edit lands in.
            assert_eq!(
                super::super::journal_fs::read_mirror(&first.id)
                    .await
                    .as_deref(),
                Some("await agent('stored')\nreturn 'ok'")
            );
            assert!(
                super::super::journal_fs::mirror_script(
                    &first.id,
                    "await agent('edited on disk')\nreturn 'ok'"
                )
                .await
            );
            seen.lock().unwrap().clear();

            let second = rerun_workflow(&ctx, &first.id, RerunOpts::default())
                .await
                .unwrap();
            finished(&fx, &second.id).await;
            assert_eq!(
                *seen.lock().unwrap(),
                vec!["edited on disk"],
                "the mirror is preferred"
            );
            assert_eq!(second.script, "await agent('edited on disk')\nreturn 'ok'");
        });
    }

    #[test]
    fn a_rerun_refuses_a_live_source_and_404s_an_unknown_one() {
        with_home(|| async {
            let fx = fixture();
            let gates = Gates::default();
            let ctx = fx.ctx(gates.runner());
            let run = start_workflow(&ctx, start_opts(&fx, "await agent('one')\nreturn 'ok'"))
                .await
                .unwrap();
            until("the call to start", || gates.started().len() == 1).await;
            let err = rerun_workflow(&ctx, &run.id, RerunOpts::default())
                .await
                .expect_err("a live source is refused");
            assert_eq!(err.status(), 409);
            assert!(
                err.to_string().contains("is still running — stop it first"),
                "{err}"
            );

            let err = rerun_workflow(&ctx, "no-such-run", RerunOpts::default())
                .await
                .expect_err("404");
            assert_eq!(err.status(), 404);

            gates.release_all();
            finished(&fx, &run.id).await;
        });
    }

    // ---- submit-time refusals ---------------------------------------------

    #[test]
    fn a_bad_submit_is_refused_before_a_worker_spawns() {
        with_home(|| async {
            let fx = fixture();
            let (runner, seen) = echo_runner();
            let ctx = fx.ctx(runner);

            let err = start_workflow(&ctx, start_opts(&fx, "   "))
                .await
                .expect_err("blank script");
            assert_eq!(err.status(), 400);
            assert!(
                err.to_string()
                    .contains("script must be a non-empty string"),
                "{err}"
            );

            let err = start_workflow(&ctx, start_opts(&fx, "let agent = 1\nreturn 0"))
                .await
                .expect_err("a shadowed binding");
            assert_eq!(err.status(), 400);
            assert!(err.to_string().contains("does not parse"), "{err}");

            let mut unknown = start_opts(&fx, "return 1");
            unknown.session_id = "no-such-session".into();
            let err = start_workflow(&ctx, unknown)
                .await
                .expect_err("unknown session");
            assert_eq!(err.status(), 404);

            assert!(seen.lock().unwrap().is_empty());
            assert!(with_db(&fx.db, |d| d.list_workflows(None))
                .unwrap()
                .is_empty());
        });
    }

    /// The runaway backstop, and its message.
    #[test]
    fn the_agent_cap_is_a_runaway_backstop_that_says_so() {
        with_home(|| async {
            let fx = fixture();
            let (runner, _) = echo_runner();
            let ctx = fx.ctx(runner);
            // Drive `decide` directly at the cap: 1,000 real agent calls would
            // be a minute of test time for a constant.
            let run = start_workflow(&ctx, start_opts(&fx, "await agent('one')\nreturn 'ok'"))
                .await
                .unwrap();
            let state = live_get(&run.id).expect("the run is live");
            state.lock().idx = MAX_AGENTS_PER_RUN;
            let err = decide(&state, "one more", "{}", Some("0")).expect_err("the cap fires");
            assert_eq!(err.status(), 429);
            assert!(err.to_string().contains("runaway-loop backstop"), "{err}");
            assert!(err.to_string().contains("1000 per run"), "{err}");
            let _ = stop_workflow(&fx.db, &fx.bus, None, &run.id);
        });
    }

    // ---- boot recovery -----------------------------------------------------

    /// A restart is SURFACED, not resumed: re-running would spend the user's
    /// money on work they did not ask for twice.
    #[test]
    fn boot_recovery_orphans_a_run_the_previous_process_left_running() {
        with_home(|| async {
            let fx = fixture();
            let id = Uuid::new_v4().to_string();
            with_db(&fx.db, |d| {
                d.create_workflow(WorkflowRun {
                    id: id.clone(),
                    session_id: fx.session_id.clone(),
                    name: "left running".into(),
                    description: String::new(),
                    script: "return 1".into(),
                    phases: vec![],
                    status: WorkflowStatus::Running,
                    current_phase: None,
                    result: None,
                    error: None,
                    args: None,
                    resume_of: None,
                    created_at: 1,
                    finished_at: None,
                })?;
                d.create_workflow_agent(WorkflowAgent {
                    id: "a1".into(),
                    run_id: id.clone(),
                    idx: 0,
                    key: "0|abc".into(),
                    label: "one".into(),
                    phase: None,
                    prompt: "one".into(),
                    model: None,
                    status: WorkflowAgentStatus::Running,
                    result: None,
                    error: None,
                    session_id: None,
                    started_at: 1,
                    finished_at: None,
                })
            })
            .unwrap();

            let recovered = recover_orphaned_workflows(&fx.db, Some(&fx.bus), None).unwrap();
            assert_eq!(recovered, vec![id.clone()]);
            let run = fx.run(&id);
            assert_eq!(run.status, WorkflowStatus::Orphaned);
            assert_eq!(
                run.error.as_deref(),
                Some("the server restarted before this workflow finished")
            );
            assert_eq!(fx.rows(&id)[0].status, WorkflowAgentStatus::Stopped);
            // Idempotent: a second pass finds nothing unfinished.
            assert!(recover_orphaned_workflows(&fx.db, Some(&fx.bus), None)
                .unwrap()
                .is_empty());
        });
    }
}

// ---------------------------------------------------------------------------
// The cross-engine compatibility gate (G3): a journal written by the TS engine
// must replay under this one.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod ts_journal_compat {
    use super::tests::with_home;
    use super::*;
    use crate::db::sqlite_db::{DbOptions, SqliteDb};
    use crate::workflow::runner::FnRunner;

    /// The fixture was produced by RUNNING the TypeScript engine once, before
    /// that tree was deleted — so it CANNOT be regenerated, only trusted. It is
    /// the only surviving artifact of the engine every already-journalled
    /// workflow run was written by. It carries every
    /// shape the journal key has to survive: sequential calls, a `parallel()`
    /// fan-out, a `pipeline()` (stage-major coordinates), a non-ASCII prompt
    /// (the UTF-16 hash), a `{schema}` call (canonicalized JSON in the key), and
    /// one failing call at the end.
    const FIXTURE: &[u8] = include_bytes!("testdata/ts_journal.db");

    /// THE CUTOVER GATE. If this fails, every workflow anyone has ever run
    /// becomes un-replayable the day the Rust server takes over: the keys are
    /// the compatibility surface, and they are a UTF-16 double-FNV over a
    /// `JSON.stringify` of the call shape.
    #[test]
    fn a_journal_written_by_the_ts_engine_replays_here() {
        with_home(|| async {
            // Work on a COPY: the fixture is read-only test data, and a rerun
            // writes a whole new run into the same database.
            let dir = std::env::temp_dir().join(format!("bough-tsjournal-{}", Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("ts_journal.db");
            std::fs::write(&path, FIXTURE).unwrap();

            let db: SharedDb = Arc::new(Mutex::new(
                SqliteDb::new(path.to_str().unwrap(), DbOptions::default())
                    .expect("the TS-written database opens under the Rust migrator"),
            ));
            let bus = Arc::new(Bus::new(system_clock()));
            let events: Arc<Mutex<Vec<crate::schema::events::BoughEvent>>> =
                Arc::new(Mutex::new(Vec::new()));
            let sink = events.clone();
            bus.subscribe(Arc::new(move |e: &crate::schema::events::BoughEvent| {
                sink.lock().unwrap().push(e.clone())
            }));

            let source = with_db(&db, |d| d.list_workflows(None))
                .unwrap()
                .into_iter()
                .next()
                .expect("the fixture carries one run");
            let source_rows = with_db(&db, |d| d.list_workflow_agents(&source.id)).unwrap();
            assert_eq!(source_rows.len(), 10, "the fixture is the 10-call script");
            assert_eq!(source_rows[9].status, WorkflowAgentStatus::Error);
            // The coordinates the TS worker computed, including the STAGE-MAJOR
            // pipeline cells. If the Rust worker numbered differently, every one
            // of these would miss.
            assert_eq!(
                source_rows
                    .iter()
                    .map(|r| r.key.split('|').next().unwrap())
                    .collect::<Vec<_>>(),
                [
                    "0", "1", "2.0.0", "2.1.0", "3.0.0.0", "3.0.1.0", "3.1.0.0", "3.1.1.0", "4",
                    "5"
                ]
            );

            // Rerun the byte-identical script under THIS engine.
            let live_calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
            let sink = live_calls.clone();
            let runner: Arc<dyn AgentRunner> =
                Arc::new(FnRunner(move |call: AgentCall, _c, _s| {
                    let sink = sink.clone();
                    async move {
                        sink.lock().unwrap().push(call.prompt.clone());
                        Ok(format!("report: {}", call.prompt))
                    }
                }));
            let ctx = WorkflowCtx {
                db: db.clone(),
                bus: bus.clone(),
                runner,
                notify: None,
                now: None,
            };
            let rerun = rerun_workflow(&ctx, &source.id, RerunOpts::default())
                .await
                .expect("the rerun starts");

            let deadline = std::time::Instant::now() + Duration::from_secs(30);
            loop {
                let status = with_db(&db, |d| d.get_workflow(&rerun.id))
                    .unwrap()
                    .unwrap()
                    .status;
                if !matches!(status, WorkflowStatus::Running | WorkflowStatus::Paused) {
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "the rerun never finished"
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            let done = with_db(&db, |d| d.get_workflow(&rerun.id))
                .unwrap()
                .unwrap();
            assert_eq!(done.status, WorkflowStatus::Done);

            let rows = with_db(&db, |d| d.list_workflow_agents(&rerun.id)).unwrap();
            assert_eq!(rows.len(), 10);
            // THE ASSERTION: every answered call replayed, byte for byte, from a
            // journal this engine did not write. Keys match exactly — which is
            // what proves the UTF-16 double-FNV, the JSON.stringify shape, the
            // canonicalized schema and the coordinate format all agree.
            for (source_row, row) in source_rows.iter().zip(rows.iter()) {
                assert_eq!(row.key, source_row.key, "key drift at idx {}", row.idx);
            }
            for row in &rows[..9] {
                assert_eq!(
                    row.status,
                    WorkflowAgentStatus::Cached,
                    "call {} paid again: {}",
                    row.idx,
                    row.label
                );
            }
            for (source_row, row) in source_rows[..9].iter().zip(rows[..9].iter()) {
                assert_eq!(row.result, source_row.result, "replayed report drift");
            }
            // Only the call the source FAILED ran live — and everything after it
            // would have too, if there had been anything after it.
            assert_eq!(*live_calls.lock().unwrap(), vec!["this one fails"]);
            assert_eq!(rows[9].status, WorkflowAgentStatus::Done);

            // The script saw the replayed values, including the parsed
            // `{schema}` one.
            let result = done.result.expect("a result");
            assert_eq!(result["one"], json!("report: audit the handlers"));
            assert_eq!(result["two"], json!("report: fix the 🐛 in parse()"));
            assert_eq!(result["typed"], json!({"ok": true, "n": 1}));
            assert_eq!(
                result["staged"],
                json!(["report: s2 report: s1 A", "report: s2 report: s1 B"])
            );

            // And the divergence is the failed call, named for what it is.
            let line = events
                .lock()
                .unwrap()
                .iter()
                .filter(|e| e.r#type == EventType::WorkflowLog)
                .map(|e| e.data["line"].as_str().unwrap_or_default().to_string())
                .find(|l| l.starts_with("replay ends at"))
                .expect("the divergence is announced once");
            assert!(line.contains("replay ends at 5 (call 9"), "{line}");
            assert!(line.contains("has no answer for it"), "{line}");

            let _ = std::fs::remove_dir_all(&dir);
        });
    }
}
