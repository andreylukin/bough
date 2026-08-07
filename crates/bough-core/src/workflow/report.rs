//! What a run replayed, what it cost, and whether it is bigger than the user
//! asked for. Port of `src/workflow/report.ts` (row 3.10).
//!
//! WHY THIS EXISTS. Two sentences in spec §8, both about a number nobody can
//! infer from the outcome:
//!
//! - *"Any operation that replays returns how many calls were served from the
//!   journal and how many ran live."* A relaunch whose keys all HIT looks like a
//!   fast relaunch. A relaunch whose keys all MISS looks like a relaunch too —
//!   same 201, same run row, same eventual result — it just costs forty agents
//!   instead of nothing. A key defect can therefore sit in the tree for a whole
//!   milestone with nothing looking wrong. Three did exactly that in TS.
//! - *"A run can spawn hundreds of agents and quietly become the most expensive
//!   thing in the product."* Cost is a surface, not something reconstructed from
//!   the bill: tokens and elapsed time per agent and per phase, visible WHILE it
//!   runs.
//!
//! THE INVARIANT THIS HOLDS: **every journaled call is counted exactly once, in
//! exactly one bucket, and the buckets sum to the total.** `replayed + ranLive
//! + pending == total`, always, for a run in any state. That is what makes the
//! number safe to read as money — a replayed call cost nothing, a live call
//! cost an agent, and there is no third thing quietly outside the arithmetic.
//!
//! `available` is the other half of the signal, and it is the half that names
//! the defect rather than the symptom: `available: 40, replayed: 0` says "there
//! were forty answers here and this run's keys matched none of them" — a broken
//! key. `available: 0` says the source had nothing to give, which is an
//! ordinary full run and no defect at all.
//!
//! EVERYTHING HERE IS A FOLD OVER ROWS THE ENGINE WROTE. The counts come off
//! `workflow_agents`; nothing in this module decides what replays and nothing
//! here can turn a miss into a hit. A report that recomputed replay a second
//! way could disagree with the journal, and then the number that exists to
//! expose a defect would be one. It also means these functions answer for a
//! finished run, a run in flight, and an orphaned run, with no engine, no
//! worker and no LLM anywhere near them.
//!
//! THE LARGE-RUN FLAG IS ADVICE, AND SO IS THE SIZE GUIDELINE. Neither pauses,
//! throttles nor refuses anything — the flag is computed at VIEW time from rows
//! that already exist, so there is no code path from it back into the engine,
//! which is the strongest form "advisory" can take.
//!
//! PORT NOTE — the replay numbers are READ, never re-derived. `available` and
//! `diverged` come out of `workflow::replay`'s own fold (`replay_plan` →
//! `replayable_prefix` / `replay_audit`), exactly as TS reads them out of
//! `run.ts`. A ceiling computed a different way could exceed what the engine
//! would ever hand out, and then `available > replayed` would read as drift on
//! a run that replayed everything it could. The only thing this module adds is
//! [`DivergenceView`], the serde projection of `replay::Divergence` — that type
//! is a computation input over there and a wire field here.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::errors::BoughError;
use crate::paths::workflows_dir;
use crate::schema::parts::{WorkflowAgent, WorkflowAgentStatus, WorkflowRun, WorkflowStatus};
use crate::types::SharedDb;
use crate::workflow::pos::CallPos;
use crate::workflow::replay::{
    empty_replay_plan, replay_audit, replay_plan, replayable_prefix, Divergence,
};

// ---------------------------------------------------------------------------
// Divergence, as the wire carries it
// ---------------------------------------------------------------------------

/// The serde projection of [`replay::Divergence`].
///
/// `workflow::replay` owns the divergence as a COMPUTATION — it is built by
/// `classify_divergence` and folded by `replay_audit`, and nothing there needs
/// a wire shape. `ReplaySummary.diverged` and `RelaunchReport.diverged` are the
/// only places it reaches a client, so the JSON contract
/// (`{pos, kind, sourcePos?, reason}`, `kind` lowercase) lives here, next to
/// the struct that carries it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DivergenceView {
    pub pos: CallPos,
    /// `changed | moved | added | unanswered` — four reasons with four fixes.
    pub kind: String,
    /// Where the source ran this same call. `moved` only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_pos: Option<CallPos>,
    /// The one sentence every surface prints.
    pub reason: String,
}

impl From<Divergence> for DivergenceView {
    fn from(d: Divergence) -> DivergenceView {
        DivergenceView {
            pos: d.pos,
            kind: d.kind.as_str().to_string(),
            source_pos: d.source_pos,
            reason: d.reason,
        }
    }
}

// ---------------------------------------------------------------------------
// Replay reporting
// ---------------------------------------------------------------------------

/// What one replaying operation cost: the `{replayed, ranLive, total}` spec §8
/// requires, plus the context that makes a zero legible.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ReplaySummary {
    pub run_id: String,
    /// The run this one replays from — `null` when it is a first run.
    pub source_id: Option<String>,
    /// Calls served from the journal: no subagent, no cost.
    pub replayed: usize,
    /// Calls that ran an agent and settled — the ones this run paid for.
    pub ran_live: usize,
    /// Every call journaled so far. `replayed + ranLive + pending`.
    pub total: usize,
    /// Queued or running. Non-zero only while the run is in flight.
    pub pending: usize,
    /// `ranLive` split three ways: "3 ran" and "3 failed" are different news.
    pub succeeded: usize,
    pub failed: usize,
    pub stopped: usize,
    /// The ceiling on `replayed`. A non-zero `available` with a zero
    /// `replayed` is the key-drift signal.
    pub available: usize,
    /// Has the run ended? Until it has, these are counts so far, not a bill.
    pub final_: bool,
    /// Where replay stopped in the SCRIPT, and why. `null` when the prefix
    /// held, or when there was no journal to replay.
    pub diverged: Option<DivergenceView>,
    /// `diverged?.pos`, lifted so a client can sort or link on it.
    pub diverged_pos: Option<CallPos>,
    /// The prompts this run did NOT replay, in call order. On a relaunch this
    /// is the edit, made visible: if it holds a prompt you did not touch, a key
    /// drifted.
    pub live_prompts: Vec<String>,
    /// The one-line human form. Carried on the wire so every client says the
    /// same thing.
    pub line: String,
}

impl ReplaySummary {
    /// `final` is a Rust keyword; the wire name is not.
    fn rename_final(mut value: serde_json::Value) -> serde_json::Value {
        if let Some(obj) = value.as_object_mut() {
            if let Some(v) = obj.remove("final_") {
                obj.insert("final".to_string(), v);
            }
        }
        value
    }

    /// The wire object, with `final_` spelled `final`.
    pub fn to_json(&self) -> serde_json::Value {
        Self::rename_final(serde_json::to_value(self).unwrap_or(serde_json::Value::Null))
    }
}

/// Buckets a row exactly once. The ONE place the status → bucket mapping lives
/// — `relaunch.rs` folds the same rows and must not carry a second copy, or the
/// two surfaces could disagree about what a status means.
pub fn bucket_of(a: &WorkflowAgent) -> Bucket {
    match a.status {
        WorkflowAgentStatus::Cached => Bucket::Replayed,
        WorkflowAgentStatus::Queued | WorkflowAgentStatus::Running => Bucket::Pending,
        WorkflowAgentStatus::Done => Bucket::Succeeded,
        WorkflowAgentStatus::Error => Bucket::Failed,
        WorkflowAgentStatus::Stopped => Bucket::Stopped,
    }
}

/// The five buckets the invariant partitions rows into.
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub enum Bucket {
    Replayed,
    Pending,
    Succeeded,
    Failed,
    Stopped,
}

/// A run whose worker is no longer executing — its counts will not move again.
fn is_final(run: &WorkflowRun) -> bool {
    !matches!(run.status, WorkflowStatus::Running | WorkflowStatus::Paused)
}

/// Count one run's journal.
///
/// 404 on an unknown id rather than zeroes, because "nothing replayed" and "no
/// such run" are the same shape and opposite problems: the first is a defect in
/// the key, the second is a defect in the caller.
pub fn replay_summary(db: &SharedDb, run_id: &str) -> Result<ReplaySummary, BoughError> {
    let run = db
        .lock()
        .unwrap()
        .get_workflow(run_id)?
        .ok_or_else(|| BoughError::not_found(format!("workflow {run_id} not found")))?;
    summarize(db, &run)
}

/// `replay_summary` for a row the caller already has. Saves the re-read, same
/// answer.
pub fn summarize(db: &SharedDb, run: &WorkflowRun) -> Result<ReplaySummary, BoughError> {
    let rows = db.lock().unwrap().list_workflow_agents(&run.id)?;
    let (mut replayed, mut pending, mut succeeded, mut failed, mut stopped) = (0, 0, 0, 0, 0);
    let mut live_prompts = Vec::new();
    for row in &rows {
        let placed = bucket_of(row);
        match placed {
            Bucket::Replayed => replayed += 1,
            Bucket::Pending => pending += 1,
            Bucket::Succeeded => succeeded += 1,
            Bucket::Failed => failed += 1,
            Bucket::Stopped => stopped += 1,
        }
        if placed != Bucket::Replayed {
            live_prompts.push(row.prompt.clone());
        }
    }
    // Read through the ENGINE's own fold, for the same reason `available` is: a
    // report that re-derived where the prefix broke could disagree with the run
    // that broke it. A run with no source reads an EMPTY plan, which is what
    // makes a first run report `diverged: null` — an accusation with no
    // defendant otherwise.
    let plan = match &run.resume_of {
        Some(src) => replay_plan(db, src)?,
        None => empty_replay_plan(),
    };
    let diverged: Option<DivergenceView> = replay_audit(&plan, &rows)
        .diverged
        .map(DivergenceView::from);
    let mut summary = ReplaySummary {
        run_id: run.id.clone(),
        source_id: run.resume_of.clone(),
        replayed,
        ran_live: succeeded + failed + stopped,
        total: rows.len(),
        pending,
        succeeded,
        failed,
        stopped,
        // The SOURCE's journal, not this run's: what was on offer, whether or
        // not any of it was claimed. Read through the engine's own plan rather
        // than a second walk over the rows — a ceiling computed a different way
        // could exceed what the engine would ever hand out, and then
        // `available > replayed` would read as drift on a run that replayed
        // everything it could.
        available: if run.resume_of.is_some() {
            replayable_prefix(&plan)
        } else {
            0
        },
        final_: is_final(run),
        diverged_pos: diverged.as_ref().map(|d| d.pos.clone()),
        diverged,
        live_prompts,
        line: String::new(),
    };
    summary.line = replay_line(&summary);
    Ok(summary)
}

/// The one-line human form — the completion note, a CLI line, a run-view header.
///
/// Written so the failure reads as a failure. "0 replayed of 12 available" is a
/// sentence someone stops on; "12 agents ran" is one they scroll past, and they
/// are the same run.
pub fn replay_line(s: &ReplaySummary) -> String {
    if s.total == 0 {
        if s.pending > 0 {
            return "no calls journaled yet".to_string();
        }
        return if s.source_id.is_some() && s.available > 0 {
            format!("no agent calls — {} were available to replay", s.available)
        } else {
            "no agent calls".to_string()
        };
    }
    let mut parts = vec![
        format!("{} replayed", s.replayed),
        format!("{} ran live", s.ran_live),
    ];
    if s.pending > 0 {
        parts.push(format!("{} still going", s.pending));
    }
    let head = format!("{} of {}", parts.join(", "), s.total);
    if s.source_id.is_some() && s.available > 0 && s.replayed == 0 {
        // NOT "every key changed". That sentence was true for an edited script
        // and false — in the most misleading possible way — for a run whose
        // calls kept their keys and changed POSITION, which is the shape a
        // barrier-free pipeline used to produce on every relaunch.
        let reason = s
            .diverged
            .as_ref()
            .map_or("the first call already differed", |d| d.reason.as_str());
        return format!(
            "{head} — replayed NOTHING of {} available: {reason}",
            s.available
        );
    }
    if s.source_id.is_some() {
        if let Some(d) = &s.diverged {
            // "stopped at slot 0.0.0.0", never a bare "stopped at 0.0.0.0". A
            // four-deep `CallPos` — pipeline, item, stage, call — is EXACTLY an
            // IPv4 address, and the word "slot" is the only thing that stops a
            // reader parsing it as a host.
            return format!(
                "{head} ({} available to replay); replay stopped at slot {} — {}",
                s.available, d.pos, d.reason
            );
        }
        return format!("{head} ({} available to replay)", s.available);
    }
    head
}

// ---------------------------------------------------------------------------
// Cost: tokens and elapsed time, per agent and per phase
// ---------------------------------------------------------------------------

/// One `agent()` call's bill. A replayed call has no session, and therefore no
/// cost.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AgentCost {
    pub agent_id: String,
    pub label: String,
    pub phase: Option<String>,
    pub status: WorkflowAgentStatus,
    pub session_id: Option<String>,
    /// Input + output tokens on the backing subagent session. `0` for a replay.
    pub tokens: i64,
    /// `finishedAt - startedAt`, or time so far for a call still running.
    pub elapsed_ms: i64,
    /// Did this call cost an agent, or was it served from the journal?
    pub replayed: bool,
}

/// One phase's bill.
///
/// `elapsedMs` is AGENT time, not wall time: calls inside a phase run
/// concurrently up to the run's semaphore, so summing them overstates the clock
/// and understates nothing. That is the number that answers "which stage is
/// expensive" — a phase's wall clock is mostly a statement about the semaphore.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PhaseCost {
    pub phase: Option<String>,
    pub agents: usize,
    pub replayed: usize,
    pub tokens: i64,
    pub elapsed_ms: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RunCost {
    pub run_id: String,
    pub agents: usize,
    pub replayed: usize,
    pub tokens: i64,
    /// Summed agent time. See [`PhaseCost::elapsed_ms`].
    pub agent_ms: i64,
    /// The run's own clock: `finishedAt - createdAt`, or time so far.
    pub wall_ms: i64,
    pub by_phase: Vec<PhaseCost>,
    pub by_agent: Vec<AgentCost>,
}

/// Tokens and elapsed time for one run, per agent and per phase.
///
/// Tokens come from the backing subagent session's usage totals, which the turn
/// runner writes as each round settles — so a running agent's number grows
/// while you watch it, which is the entire point of putting it in the run view.
pub fn run_cost(db: &SharedDb, run: &WorkflowRun, at: i64) -> Result<RunCost, BoughError> {
    let guard = db.lock().unwrap();
    let rows = guard.list_workflow_agents(&run.id)?;
    let mut by_agent = Vec::with_capacity(rows.len());
    for a in &rows {
        // A replay has no session and no usage: it did not call a model.
        // Counting it as zero is the accounting claim the journal makes.
        let tokens = match &a.session_id {
            Some(sid) => {
                let u = guard.session_usage(sid)?;
                u.input_tokens + u.output_tokens
            }
            None => 0,
        };
        by_agent.push(AgentCost {
            agent_id: a.id.clone(),
            label: a.label.clone(),
            phase: a.phase.clone(),
            status: a.status,
            session_id: a.session_id.clone(),
            tokens,
            elapsed_ms: (a.finished_at.unwrap_or(at) - a.started_at).max(0),
            replayed: a.status == WorkflowAgentStatus::Cached,
        });
    }
    drop(guard);

    // Insertion order is what TS's `Map` preserves; `BTreeMap` would reorder,
    // so the index is kept alongside a Vec.
    let mut order: Vec<String> = Vec::new();
    let mut phases: BTreeMap<String, PhaseCost> = BTreeMap::new();
    for a in &by_agent {
        let key = a.phase.clone().unwrap_or_default();
        let row = phases.entry(key.clone()).or_insert_with(|| {
            order.push(key.clone());
            PhaseCost {
                phase: a.phase.clone(),
                agents: 0,
                replayed: 0,
                tokens: 0,
                elapsed_ms: 0,
            }
        });
        row.agents += 1;
        if a.replayed {
            row.replayed += 1;
        }
        row.tokens += a.tokens;
        row.elapsed_ms += a.elapsed_ms;
    }

    Ok(RunCost {
        run_id: run.id.clone(),
        agents: by_agent.len(),
        replayed: by_agent.iter().filter(|a| a.replayed).count(),
        tokens: by_agent.iter().map(|a| a.tokens).sum(),
        agent_ms: by_agent.iter().map(|a| a.elapsed_ms).sum(),
        wall_ms: (run.finished_at.unwrap_or(at) - run.created_at).max(0),
        by_phase: order
            .into_iter()
            .filter_map(|k| phases.remove(&k))
            .collect(),
        by_agent,
    })
}

// ---------------------------------------------------------------------------
// The size guideline
// ---------------------------------------------------------------------------

/// How many agents a generated script should AIM for. Advice to whoever writes
/// the script — the model, or a person — and never a cap.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SizeGuideline {
    Small,
    Medium,
    Large,
    Unrestricted,
}

pub const DEFAULT_GUIDELINE: SizeGuideline = SizeGuideline::Medium;

const GUIDELINES: [SizeGuideline; 4] = [
    SizeGuideline::Small,
    SizeGuideline::Medium,
    SizeGuideline::Large,
    SizeGuideline::Unrestricted,
];

impl SizeGuideline {
    pub fn as_str(self) -> &'static str {
        match self {
            SizeGuideline::Small => "small",
            SizeGuideline::Medium => "medium",
            SizeGuideline::Large => "large",
            SizeGuideline::Unrestricted => "unrestricted",
        }
    }

    /// The count each guideline targets. `unrestricted` has none — TS spells it
    /// `Infinity`, which is not representable in JSON and reaches the wire as
    /// `null`, so it is `None` here.
    pub fn target(self) -> Option<i64> {
        match self {
            SizeGuideline::Small => Some(5),
            SizeGuideline::Medium => Some(15),
            SizeGuideline::Large => Some(50),
            SizeGuideline::Unrestricted => None,
        }
    }
}

/// Parse a stored or posted value. `None` for anything that is not one.
pub fn parse_guideline(value: &str) -> Option<SizeGuideline> {
    let name = value.trim().to_ascii_lowercase();
    GUIDELINES.into_iter().find(|g| g.as_str() == name)
}

/// Parse or produce the 400 the route renders — one message, both entry points.
///
/// `raw` is the JSON value as posted, because the message quotes it with
/// `JSON.stringify` (a number posts back as `3`, a string as `"3"`).
pub fn require_guideline(raw: &serde_json::Value) -> Result<SizeGuideline, BoughError> {
    let text = match raw {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    };
    if let Some(g) = parse_guideline(&text) {
        return Ok(g);
    }
    let listed: Vec<&str> = GUIDELINES.iter().map(|g| g.as_str()).collect();
    Err(BoughError::bad_request(format!(
        "unknown workflow size guideline {} — it is one of {}. It is advice to whoever \
         writes the script (aim for fewer than this many agents), never a cap on what a \
         run may do.",
        json_stringify(raw),
        listed.join(", "),
    )))
}

/// `JSON.stringify(value)`; `undefined` stringifies to the literal `undefined`
/// in TS, which is what an absent `sizeGuideline` produces.
fn json_stringify(value: &serde_json::Value) -> String {
    match value {
        // TS `JSON.stringify(undefined)` is `undefined` (the value, which
        // string-concatenates as "undefined"). An absent key is the only way to
        // hit this, and the route models absent as Null.
        serde_json::Value::Null => "undefined".to_string(),
        other => serde_json::to_string(other).unwrap_or_else(|_| "null".to_string()),
    }
}

/// Where the setting lives: `~/.bough/workflows/size-guideline`, one word.
pub fn guideline_path() -> std::path::PathBuf {
    workflows_dir().join("size-guideline")
}

/// The active guideline: the stored setting, else `BOUGH_WORKFLOW_SIZE`, else
/// `medium`.
///
/// Read SYNCHRONOUSLY and on every call, because its readers are view functions
/// a route renders per request. The file is one word; a cache here would be a
/// staleness bug traded for nothing measurable.
pub fn active_guideline() -> SizeGuideline {
    if let Some(g) = std::fs::read_to_string(guideline_path())
        .ok()
        .and_then(|s| parse_guideline(&s))
    {
        return g;
    }
    std::env::var("BOUGH_WORKFLOW_SIZE")
        .ok()
        .and_then(|v| parse_guideline(&v))
        .unwrap_or(DEFAULT_GUIDELINE)
}

/// Persist the guideline. Returns what was stored, so a caller can echo it back.
pub fn set_guideline(raw: &serde_json::Value) -> Result<SizeGuideline, BoughError> {
    let guideline = require_guideline(raw)?;
    let dir = workflows_dir();
    std::fs::create_dir_all(&dir)
        .and_then(|()| std::fs::write(guideline_path(), format!("{}\n", guideline.as_str())))
        .map_err(|e| BoughError::bad_request(format!("could not store the guideline: {e}")))?;
    Ok(guideline)
}

/// The sentence handed to whoever writes the script. Phrased as a target with
/// an explicit override clause, because a guideline the model reads as a hard
/// cap produces a script that under-fans a job that genuinely needs 200 agents.
pub fn guideline_advice(guideline: SizeGuideline) -> String {
    match guideline.target() {
        None => {
            "Workflow size guideline: unrestricted — fan out as wide as the job needs.".to_string()
        }
        Some(target) => format!(
            "Workflow size guideline: {} — aim for fewer than {target} agents in a generated \
             script. This is advice, not a cap: if the request plainly needs a wider \
             fan-out, write it and say why.",
            guideline.as_str(),
        ),
    }
}

// ---------------------------------------------------------------------------
// The large-run flag
// ---------------------------------------------------------------------------

/// Projected tokens above which a run is flagged. `BOUGH_WORKFLOW_TOKEN_WARN`
/// moves it.
pub fn token_warn_threshold() -> i64 {
    std::env::var("BOUGH_WORKFLOW_TOKEN_WARN")
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|n| n.is_finite() && *n > 0.0)
        .map_or(1_000_000, |n| n as i64)
}

/// A run that is bigger than the guideline, or on course to cost more than the
/// token threshold.
///
/// ADVISORY, and structurally so: computed from rows that already exist, at the
/// moment a view is rendered, and nothing in the engine reads it. `stop` names
/// the control that DOES stop it, because a warning with no adjacent action is
/// a warning people learn to ignore.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LargeRunFlag {
    pub flagged: bool,
    pub advisory: bool,
    pub guideline: SizeGuideline,
    /// The guideline's count, or `null` for `unrestricted`.
    pub target: Option<i64>,
    /// Calls journaled so far. A run still scheduling may exceed this.
    pub scheduled: usize,
    pub tokens: i64,
    pub projected_tokens: i64,
    pub token_threshold: i64,
    /// One sentence per reason it is flagged. Never empty.
    pub reasons: Vec<String>,
    /// The control that stops it. A warning names its own remedy.
    pub stop: String,
}

/// Project this run's final token total from what has settled.
///
/// Live calls only: a replayed call spends nothing, so averaging it in would
/// drag the projection toward zero exactly when a relaunch is running the
/// expensive tail live. A run with nothing settled projects what it has spent —
/// a floor, never a guess.
pub fn project_tokens(cost: &RunCost) -> i64 {
    let settled: Vec<&AgentCost> = cost
        .by_agent
        .iter()
        .filter(|a| !a.replayed && a.status != WorkflowAgentStatus::Queued)
        .collect();
    let finished: Vec<&&AgentCost> = settled
        .iter()
        .filter(|a| a.status != WorkflowAgentStatus::Running)
        .collect();
    if finished.is_empty() {
        return cost.tokens;
    }
    let average = finished.iter().map(|a| a.tokens).sum::<i64>() as f64 / finished.len() as f64;
    let unfinished = cost
        .by_agent
        .iter()
        .filter(|a| {
            !a.replayed
                && matches!(
                    a.status,
                    WorkflowAgentStatus::Queued | WorkflowAgentStatus::Running
                )
        })
        .count();
    // TS `Math.round`: halves go UP, including negatives (−0.5 → −0). Token
    // counts are non-negative, so `.round()` differs nowhere reachable.
    (cost.tokens as f64 + average * unfinished as f64).round() as i64
}

/// Flag a run that schedules more than the guideline's count, or whose
/// projected tokens cross the threshold. `None` when neither is true.
pub fn large_run_flag(
    cost: &RunCost,
    guideline: SizeGuideline,
    threshold: i64,
) -> Option<LargeRunFlag> {
    let target = guideline.target();
    let projected_tokens = project_tokens(cost);
    let mut reasons = Vec::new();
    if let Some(t) = target {
        if cost.agents as i64 > t {
            reasons.push(format!(
                "{} agents scheduled, past the {} guideline of {t}",
                cost.agents,
                guideline.as_str()
            ));
        }
    }
    if projected_tokens > threshold {
        reasons.push(format!(
            "projected {} tokens, past the {} warning threshold",
            group_digits(projected_tokens),
            group_digits(threshold),
        ));
    }
    if reasons.is_empty() {
        return None;
    }
    Some(LargeRunFlag {
        flagged: true,
        advisory: true,
        guideline,
        target,
        scheduled: cost.agents,
        tokens: cost.tokens,
        projected_tokens,
        token_threshold: threshold,
        reasons,
        stop: format!("POST /workflows/{}/stop", cost.run_id),
    })
}

/// `Number.prototype.toLocaleString("en-US")` for an integer: comma every three
/// digits. The reasons are read by a person, and `1000000` is not a number
/// anyone parses at a glance.
fn group_digits(n: i64) -> String {
    let neg = n < 0;
    let digits = n.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    if neg {
        format!("-{out}")
    } else {
        out
    }
}

/// The whole cost surface for one run, as `GET /workflows/:id` carries it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RunAccounting {
    pub replay: ReplaySummary,
    pub cost: RunCost,
    /// `null` when the run is within its guideline and under the threshold.
    pub warning: Option<LargeRunFlag>,
    pub guideline: SizeGuideline,
}

/// Options a caller may pin; every `None` reads the live setting.
#[derive(Default)]
pub struct AccountingOpts {
    pub guideline: Option<SizeGuideline>,
    pub threshold: Option<i64>,
}

pub fn run_accounting(
    db: &SharedDb,
    run: &WorkflowRun,
    at: i64,
    opts: AccountingOpts,
) -> Result<RunAccounting, BoughError> {
    let guideline = opts.guideline.unwrap_or_else(active_guideline);
    let cost = run_cost(db, run, at)?;
    let warning = large_run_flag(
        &cost,
        guideline,
        opts.threshold.unwrap_or_else(token_warn_threshold),
    );
    Ok(RunAccounting {
        replay: summarize(db, run)?,
        cost,
        warning,
        guideline,
    })
}

// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::sqlite_db::{DbOptions, SqliteDb};
    use crate::schema::parts::{Session, SessionKind, Usage};
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

    fn run(
        db: &SharedDb,
        id: &str,
        status: WorkflowStatus,
        resume_of: Option<&str>,
    ) -> WorkflowRun {
        session(db, &format!("sess-{id}"));
        let row = WorkflowRun {
            id: id.into(),
            session_id: format!("sess-{id}"),
            name: "w".into(),
            description: String::new(),
            script: "return 1".into(),
            phases: vec![],
            status,
            current_phase: None,
            result: None,
            error: None,
            args: None,
            resume_of: resume_of.map(String::from),
            created_at: 1_000,
            finished_at: if matches!(status, WorkflowStatus::Running | WorkflowStatus::Paused) {
                None
            } else {
                Some(3_000)
            },
        };
        db.lock().unwrap().create_workflow(row.clone()).unwrap();
        row
    }

    #[allow(clippy::too_many_arguments)]
    fn agent(
        db: &SharedDb,
        run_id: &str,
        idx: i64,
        status: WorkflowAgentStatus,
        phase: Option<&str>,
        session_id: Option<&str>,
        started_at: i64,
        finished_at: Option<i64>,
    ) {
        if let Some(sid) = session_id {
            session(db, sid);
        }
        db.lock()
            .unwrap()
            .create_workflow_agent(WorkflowAgent {
                id: format!("{run_id}-a{idx}"),
                run_id: run_id.into(),
                idx,
                key: format!("k{idx}|h{idx}"),
                label: format!("call {idx}"),
                phase: phase.map(String::from),
                prompt: format!("prompt {idx}"),
                model: Some("m".into()),
                status,
                result: None,
                error: None,
                session_id: session_id.map(String::from),
                started_at,
                finished_at,
            })
            .unwrap();
    }

    /// A source run whose journal offers `answers` leading answered calls at
    /// coordinates `0..answers`, each with content hash `c<i>`.
    fn source_run(db: &SharedDb, id: &str, answers: i64) -> WorkflowRun {
        let row = run(db, id, WorkflowStatus::Done, None);
        for i in 0..answers {
            keyed(
                db,
                id,
                i,
                &format!("{i}|c{i}"),
                WorkflowAgentStatus::Done,
                Some("report"),
            );
        }
        row
    }

    /// A journal row with an explicit key and result — what the replay plan
    /// reads. (`agent` writes `k<idx>|h<idx>`, which no relaunch matches.)
    fn keyed(
        db: &SharedDb,
        run_id: &str,
        idx: i64,
        key: &str,
        status: WorkflowAgentStatus,
        result: Option<&str>,
    ) {
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
                status,
                result: result.map(String::from),
                error: None,
                session_id: None,
                started_at: 1_000,
                finished_at: Some(2_000),
            })
            .unwrap();
    }

    /// THE invariant, on a run holding one row of every status at once:
    /// `replayed + ranLive + pending == total`, and every row is in exactly one
    /// bucket. Asserted against rows a real SQLite database returned.
    #[test]
    fn every_journaled_call_is_counted_once_and_the_buckets_sum_to_the_total() {
        let db = mem_db();
        let r = run(&db, "wf1", WorkflowStatus::Running, None);
        use WorkflowAgentStatus::*;
        for (i, st) in [Cached, Queued, Running, Done, Error, Stopped]
            .into_iter()
            .enumerate()
        {
            agent(&db, "wf1", i as i64, st, None, None, 1_000, None);
        }
        let s = summarize(&db, &r).unwrap();
        assert_eq!(s.total, 6);
        assert_eq!(s.replayed, 1);
        assert_eq!(s.pending, 2, "queued + running");
        assert_eq!(s.ran_live, 3, "done + error + stopped");
        assert_eq!(s.succeeded, 1);
        assert_eq!(s.failed, 1);
        assert_eq!(s.stopped, 1);
        assert_eq!(s.replayed + s.ran_live + s.pending, s.total);
        assert!(!s.final_, "a running run's counts can still move");
        // Every non-replayed row's prompt is carried; the replayed one is not.
        assert_eq!(s.live_prompts.len(), 5);
        assert!(!s.live_prompts.contains(&"prompt 0".to_string()));
    }

    /// A first run reports `diverged: null` and `available: 0` — an accusation
    /// with no defendant otherwise. The plan it audits against is the EMPTY
    /// one, by construction, not by a caller's promise.
    #[test]
    fn a_first_run_never_diverges() {
        let db = mem_db();
        let r = run(&db, "wf2", WorkflowStatus::Done, None);
        agent(
            &db,
            "wf2",
            0,
            WorkflowAgentStatus::Done,
            None,
            None,
            1_000,
            Some(2_000),
        );
        let s = summarize(&db, &r).unwrap();
        assert_eq!(s.source_id, None);
        assert_eq!(s.available, 0);
        assert_eq!(s.diverged, None);
        assert_eq!(s.diverged_pos, None);
        assert_eq!(s.line, "0 replayed, 1 ran live of 1");
        assert!(s.final_);
    }

    /// The line says which defect it is looking at, and says "slot" so a
    /// four-deep coordinate does not read as an IPv4 address. Both numbers come
    /// off a REAL source journal read through `workflow::replay`.
    #[test]
    fn the_replay_line_names_the_defect_and_calls_a_coordinate_a_slot() {
        let db = mem_db();
        source_run(&db, "src", 12);

        // A relaunch whose only call kept its key but MOVED: 12 answers on
        // offer, none claimed. The key-drift sentence, with the right diagnosis.
        let r = run(&db, "wf3", WorkflowStatus::Done, Some("src"));
        for i in 0..3 {
            keyed(
                &db,
                "wf3",
                i,
                &format!("0.0.0.{i}|c5"),
                WorkflowAgentStatus::Done,
                Some("x"),
            );
        }
        let s = summarize(&db, &r).unwrap();
        assert_eq!(
            s.available, 12,
            "the source's answered prefix, read through the plan"
        );
        assert_eq!(s.replayed, 0);
        assert_eq!(s.diverged.as_ref().unwrap().kind, "moved");
        assert_eq!(
            s.diverged.as_ref().unwrap().source_pos.as_deref(),
            Some("5")
        );
        assert_eq!(s.diverged_pos.as_deref(), Some("0.0.0.0"));
        assert!(
            s.line
                .starts_with("0 replayed, 3 ran live of 3 — replayed NOTHING of 12 available: "),
            "{}",
            s.line
        );
        assert!(
            s.line.contains("the call MOVED: its key did not change"),
            "{}",
            s.line
        );

        // Some replayed, and a divergence: the "stopped at slot" sentence.
        let r4 = run(&db, "wf4", WorkflowStatus::Done, Some("src"));
        keyed(
            &db,
            "wf4",
            0,
            "0|c0",
            WorkflowAgentStatus::Cached,
            Some("report"),
        );
        keyed(
            &db,
            "wf4",
            1,
            "0.0.0.0|c5",
            WorkflowAgentStatus::Done,
            Some("x"),
        );
        let s4 = summarize(&db, &r4).unwrap();
        assert!(
            s4.line.contains("replay stopped at slot 0.0.0.0 — "),
            "the word `slot` is load-bearing: {}",
            s4.line
        );
        assert!(
            s4.line
                .starts_with("1 replayed, 1 ran live of 2 (12 available to replay);"),
            "{}",
            s4.line
        );
    }

    /// The empty-journal arms, all three.
    #[test]
    fn a_journal_with_no_rows_says_so_three_different_ways() {
        let db = mem_db();
        let fresh = run(&db, "wf5", WorkflowStatus::Done, None);
        assert_eq!(summarize(&db, &fresh).unwrap().line, "no agent calls");
        source_run(&db, "src7", 7);
        let seeded = run(&db, "wf6", WorkflowStatus::Done, Some("src7"));
        assert_eq!(
            summarize(&db, &seeded).unwrap().line,
            "no agent calls — 7 were available to replay"
        );
        // `pending > 0` with `total == 0` cannot happen off real rows (pending
        // counts rows), so the arm is exercised on the line function directly.
        let mut s = summarize(&db, &fresh).unwrap();
        s.pending = 1;
        assert_eq!(replay_line(&s), "no calls journaled yet");
    }

    /// Cost is a fold over rows and session usage: a replayed call has no
    /// session and therefore no tokens, and phase time is AGENT time.
    #[test]
    fn cost_bills_live_sessions_and_charges_a_replay_nothing() {
        let db = mem_db();
        let r = run(&db, "wf7", WorkflowStatus::Done, None);
        agent(
            &db,
            "wf7",
            0,
            WorkflowAgentStatus::Cached,
            Some("scan"),
            None,
            1_000,
            Some(1_000),
        );
        agent(
            &db,
            "wf7",
            1,
            WorkflowAgentStatus::Done,
            Some("scan"),
            Some("kid-1"),
            1_000,
            Some(2_500),
        );
        db.lock()
            .unwrap()
            .add_session_usage(
                "kid-1",
                &Usage {
                    input_tokens: 300,
                    output_tokens: 200,
                    ..Usage::default()
                },
                2_000,
            )
            .unwrap();

        let cost = run_cost(&db, &r, 9_999).unwrap();
        assert_eq!(cost.agents, 2);
        assert_eq!(cost.replayed, 1);
        assert_eq!(cost.tokens, 500, "only the live call's session is billed");
        assert_eq!(cost.agent_ms, 1_500, "the replay took no time");
        assert_eq!(cost.wall_ms, 2_000, "finishedAt - createdAt, not `now`");
        assert_eq!(cost.by_phase.len(), 1);
        assert_eq!(cost.by_phase[0].phase.as_deref(), Some("scan"));
        assert_eq!(cost.by_phase[0].agents, 2);
        assert_eq!(cost.by_phase[0].replayed, 1);
        assert_eq!(cost.by_phase[0].tokens, 500);
        assert_eq!(cost.by_phase[0].elapsed_ms, 1_500);
        // A replay is charged zero even though its row is `cached` with no session.
        assert_eq!(cost.by_agent[0].tokens, 0);
        assert!(cost.by_agent[0].replayed);
    }

    /// A running call's clock advances against `now`; it never goes negative.
    #[test]
    fn a_running_call_bills_time_so_far_and_never_below_zero() {
        let db = mem_db();
        let r = run(&db, "wf8", WorkflowStatus::Running, None);
        agent(
            &db,
            "wf8",
            0,
            WorkflowAgentStatus::Running,
            None,
            None,
            5_000,
            None,
        );
        assert_eq!(
            run_cost(&db, &r, 8_000).unwrap().by_agent[0].elapsed_ms,
            3_000
        );
        // A clock that went backwards clamps rather than reporting negative time.
        assert_eq!(run_cost(&db, &r, 1_000).unwrap().by_agent[0].elapsed_ms, 0);
    }

    /// The projection averages FINISHED live calls only, and a run with nothing
    /// finished projects what it spent — a floor, never a guess.
    #[test]
    fn the_projection_averages_finished_live_calls_and_floors_at_what_was_spent() {
        let base = RunCost {
            run_id: "wf".into(),
            agents: 0,
            replayed: 0,
            tokens: 0,
            agent_ms: 0,
            wall_ms: 0,
            by_phase: vec![],
            by_agent: vec![],
        };
        let a = |status, tokens, replayed| AgentCost {
            agent_id: "a".into(),
            label: "l".into(),
            phase: None,
            status,
            session_id: None,
            tokens,
            elapsed_ms: 0,
            replayed,
        };
        use WorkflowAgentStatus::*;
        // Two finished at 100 each, two queued: 200 + 100*2.
        let cost = RunCost {
            tokens: 200,
            by_agent: vec![
                a(Done, 100, false),
                a(Done, 100, false),
                a(Queued, 0, false),
                a(Queued, 0, false),
            ],
            ..base.clone()
        };
        assert_eq!(project_tokens(&cost), 400);
        // A replayed call is never averaged in — it would drag the projection
        // to zero exactly when the expensive tail is running live.
        let with_replay = RunCost {
            tokens: 200,
            by_agent: vec![a(Cached, 0, true), a(Done, 200, false), a(Queued, 0, false)],
            ..base.clone()
        };
        assert_eq!(project_tokens(&with_replay), 400);
        // Nothing finished: the floor.
        let unsettled = RunCost {
            tokens: 50,
            by_agent: vec![a(Running, 50, false)],
            ..base.clone()
        };
        assert_eq!(project_tokens(&unsettled), 50);
    }

    /// The flag is advice with a remedy attached, and it fires on either reason.
    #[test]
    fn the_large_run_flag_is_advisory_names_its_reasons_and_names_the_stop() {
        let cost = RunCost {
            run_id: "wf9".into(),
            agents: 20,
            replayed: 0,
            tokens: 2_000_000,
            agent_ms: 0,
            wall_ms: 0,
            by_phase: vec![],
            by_agent: vec![],
        };
        let flag = large_run_flag(&cost, SizeGuideline::Medium, 1_000_000).unwrap();
        assert!(flag.flagged && flag.advisory);
        assert_eq!(flag.target, Some(15));
        assert_eq!(flag.scheduled, 20);
        assert_eq!(flag.stop, "POST /workflows/wf9/stop");
        assert_eq!(flag.reasons.len(), 2);
        assert_eq!(
            flag.reasons[0],
            "20 agents scheduled, past the medium guideline of 15"
        );
        assert_eq!(
            flag.reasons[1],
            "projected 2,000,000 tokens, past the 1,000,000 warning threshold"
        );
        // `unrestricted` has no count to exceed, so only the token reason can fire.
        let unres = large_run_flag(&cost, SizeGuideline::Unrestricted, 1_000_000).unwrap();
        assert_eq!(unres.target, None);
        assert_eq!(unres.reasons.len(), 1);
        // An ordinary run carries no flag at all.
        let small = RunCost {
            agents: 3,
            tokens: 10,
            ..cost.clone()
        };
        assert!(large_run_flag(&small, SizeGuideline::Medium, 1_000_000).is_none());
    }

    #[test]
    fn the_guideline_targets_match_the_ts_table_and_unrestricted_is_null() {
        assert_eq!(SizeGuideline::Small.target(), Some(5));
        assert_eq!(SizeGuideline::Medium.target(), Some(15));
        assert_eq!(SizeGuideline::Large.target(), Some(50));
        assert_eq!(SizeGuideline::Unrestricted.target(), None);
        assert_eq!(DEFAULT_GUIDELINE, SizeGuideline::Medium);
        assert_eq!(parse_guideline("  LARGE \n"), Some(SizeGuideline::Large));
        assert_eq!(parse_guideline("huge"), None);
    }

    /// `unrestricted` gets its OWN sentence. "aim for fewer than Infinity
    /// agents" is not advice, it is noise.
    #[test]
    fn the_advice_reads_as_a_target_with_an_override_clause() {
        assert_eq!(
            guideline_advice(SizeGuideline::Unrestricted),
            "Workflow size guideline: unrestricted — fan out as wide as the job needs."
        );
        let medium = guideline_advice(SizeGuideline::Medium);
        assert!(medium.starts_with("Workflow size guideline: medium — aim for fewer than 15 "));
        assert!(medium.contains("advice, not a cap"), "{medium}");
    }

    /// The 400 quotes what was posted, the way `JSON.stringify` does, and lists
    /// all four values.
    #[test]
    fn an_unknown_guideline_is_a_400_that_quotes_what_was_posted() {
        let err = require_guideline(&serde_json::json!("huge")).unwrap_err();
        assert_eq!(err.status(), 400);
        assert!(
            err.to_string().starts_with(
                "unknown workflow size guideline \"huge\" — it is one of \
                                         small, medium, large, unrestricted."
            ),
            "{err}"
        );
        assert!(err.to_string().contains("never a cap"), "{err}");
        // A number posts back unquoted, as JSON.stringify renders it.
        assert!(require_guideline(&serde_json::json!(3))
            .unwrap_err()
            .to_string()
            .contains("guideline 3 —"));
        // An absent key is `undefined` in TS.
        assert!(require_guideline(&serde_json::Value::Null)
            .unwrap_err()
            .to_string()
            .contains("guideline undefined —"));
    }

    #[test]
    fn thousands_are_grouped_the_way_en_us_groups_them() {
        assert_eq!(group_digits(0), "0");
        assert_eq!(group_digits(999), "999");
        assert_eq!(group_digits(1_000), "1,000");
        assert_eq!(group_digits(1_000_000), "1,000,000");
        assert_eq!(group_digits(-12_345), "-12,345");
    }

    /// The wire spells it `final`, not `final_`.
    #[test]
    fn the_summary_serializes_final_under_its_real_name() {
        let db = mem_db();
        let r = run(&db, "wfA", WorkflowStatus::Done, None);
        let json = summarize(&db, &r).unwrap().to_json();
        assert_eq!(json["final"], true);
        assert!(json.get("final_").is_none(), "{json}");
        for key in [
            "runId",
            "sourceId",
            "replayed",
            "ranLive",
            "total",
            "pending",
            "succeeded",
            "failed",
            "stopped",
            "available",
            "diverged",
            "divergedPos",
            "livePrompts",
            "line",
        ] {
            assert!(json.get(key).is_some(), "missing {key}: {json}");
        }
    }
}
