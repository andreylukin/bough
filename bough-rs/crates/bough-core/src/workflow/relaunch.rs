//! Relaunching a workflow from a stopped run's journal — the only way to change
//! what a run is doing — and the accounting that says what the change cost.
//! Port of `src/workflow/relaunch.ts` (row 3.10).
//!
//! WHY THIS EXISTS. There is no mid-run user input (spec §8): a workflow script
//! is not editable while it executes and never stops to ask. Pause gates NEW
//! `agent()` calls and lets the dispatched ones finish; that is steering the
//! THROTTLE, not the work. To steer the work you stop the run, edit the script
//! — the mirror at `~/.bough/workflows/<id>.js`, an explicit body over HTTP, or
//! an agent rewriting it — and relaunch seeded from the stopped run's journal.
//! The result is a NEW run with its own id that READS the old run's rows and
//! never writes to them, because history is a tree and nothing in bough is
//! destructively rewritten.
//!
//! THE INVARIANT THIS HOLDS: **replay never crosses the first changed call, and
//! what it did cross is reported.** Both halves, because either one alone is a
//! defect.
//!
//! - *Never crosses.* The engine replays the longest unchanged PREFIX and runs
//!   the first divergent call — and everything after it — live, even calls whose
//!   own key is byte-identical. Workflow agents share ONE checkout: a key covers
//!   a call's prompt, not the filesystem that prompt runs against. A miss costs
//!   money; a stale hit is a wrong answer presented as a fresh one.
//! - *Reported.* A relaunch that replayed 38 of 40 and one that replayed 0 of 40
//!   produce the same 201, the same events and eventually the same result — they
//!   differ only in an invoice nobody sees for a month. [`forced`] is the number
//!   this module exists to expose: calls that ran live although their key still
//!   matched, which is precisely what the prefix rule costs. If it is large and
//!   `divergedAt` is small, the fix is to move the edited call later in the
//!   script, and no other surface would say so.
//!
//! WHAT IS NOT HERE. The prefix mechanism itself is `workflow::replay`'s,
//! because that is where the journal rule lives and one definition of it is the
//! maximum safe number. Script mirrors and the "explicit → mirror → stored"
//! resolution are `workflow::journal_fs`. Building a `WorkflowCtx` with real
//! subagents behind it is `workflow::control`, and it is INJECTED rather than
//! imported: this module is reachable from the route table, and importing the
//! control layer would close a cycle.
//!
//! [`forced`]: RelaunchReport::forced
//!
//! PORT STATUS (row 3.10). The two PURE halves — the preview and the report —
//! are ported and wired: `GET /workflows/:id/replay` answers off real rows. The
//! OPERATION (`relaunch_workflow`) needs `workflow::engine::start_workflow`
//! (row 3.9), which is not landed; it is not stubbed here, because a relaunch
//! that started nothing and answered 201 is the exact failure this module's
//! second invariant exists to prevent. The route keeps answering the unknown-run
//! 404 until the engine lands.

use serde::{Deserialize, Serialize};

use crate::errors::{BoughError, ErrorKind};
use crate::schema::parts::{WorkflowRun, WorkflowStatus};
use crate::types::{AppCtx, SharedDb};

use super::control::{workflow_control, workflow_ctx_for, workflow_effective_model};
use super::engine::{is_workflow_live, start_workflow, StartOpts};
use super::journal_fs::{resolve_rerun_script, ScriptSource};
use super::meta::extract_meta;
use super::pos::CallPos;
use super::replay::{empty_replay_plan, replay_audit, replay_plan, replayable_prefix};
use super::report::{bucket_of, Bucket, DivergenceView};

// ---------------------------------------------------------------------------
// What the source journal offers (pure)
// ---------------------------------------------------------------------------

/// What a relaunch could replay at best, known BEFORE the new script runs.
///
/// A ceiling, never a promise: which of these the relaunch actually claims
/// depends on the keys the edited script produces, and that answer only exists
/// once it has run. Reported anyway, because `available: 40, replayed: 0`
/// afterwards is the signature of a broken key and `available: 0` is an
/// ordinary first run — and a bare `replayed: 0` cannot tell them apart.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RelaunchPreview {
    pub source_id: String,
    /// Calls the source run journaled.
    pub journaled: usize,
    /// Of those, the ones that ANSWERED — `done`/`cached` with a result.
    pub answers: usize,
    /// The leading run of answered calls: the most a relaunch can replay however
    /// unchanged the script is. Smaller than `answers` whenever the source
    /// failed or was stopped part-way, because replay stops at the first call it
    /// cannot serve.
    pub replayable_prefix: usize,
}

pub fn relaunch_preview(db: &SharedDb, source_id: &str) -> Result<RelaunchPreview, BoughError> {
    let plan = replay_plan(db, source_id)?;
    Ok(RelaunchPreview {
        source_id: source_id.to_string(),
        journaled: plan.steps.len(),
        answers: plan.steps.iter().filter(|s| s.result.is_some()).count(),
        replayable_prefix: replayable_prefix(&plan),
    })
}

// ---------------------------------------------------------------------------
// The operation
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default)]
pub struct RelaunchOpts {
    /// The edited script. Absent = the `~/.bough/workflows/<id>.js` mirror the
    /// user may have edited, then the stored row — which makes "edit the file,
    /// relaunch" the whole loop and a rerun the case where the file is
    /// untouched.
    pub script: Option<String>,
    /// Absent = the source run's input, verbatim.
    pub args: Option<serde_json::Value>,
}

pub struct RelaunchResult {
    /// The NEW run. Its `resume_of` points at the source; the source is
    /// untouched.
    pub run: WorkflowRun,
    pub source: WorkflowRun,
    /// Where the script came from — it decides what actually runs, so it is
    /// reported.
    pub script: ScriptSource,
    pub replay: RelaunchPreview,
}

/// Stop-edit-relaunch, the second half: start a new run seeded from `source_id`'s
/// journal.
///
/// A source that is still live is REFUSED rather than raced. Its journal is
/// still being written, so the prefix a relaunch would replay is not yet a fact
/// — and the two runs would then be driving agents against one checkout with no
/// idea about each other. The error says to stop it first, and pausing before
/// stopping is what preserves the most work: a dispatched agent that finishes is
/// journaled and replays, one killed in flight is not and starts over (spec §8).
///
/// `meta` is extracted from the EDITED script, at this boundary, so a script
/// whose meta was broken by the edit is refused before a worker spawns or a row
/// is written — and so a renamed run is named after the script actually running.
///
/// RUST DELTA. TS injects `ctxFor` through `ctx.relaunch` because importing
/// `workflow/control.ts` would close a module cycle through `server/app.ts`.
/// There is no such cycle here — both modules are in one crate and the route
/// table lives in another — so [`workflow_ctx_for`] is called directly, and
/// with it the "unwired boot seam" 500 that only existed to catch a wiring
/// mistake this spelling cannot make.
pub async fn relaunch_workflow(
    ctx: &AppCtx,
    source_id: &str,
    opts: RelaunchOpts,
) -> Result<RelaunchResult, BoughError> {
    let source = ctx
        .db
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_workflow(source_id)?
        .ok_or_else(|| BoughError::not_found(format!("workflow {source_id} not found")))?;
    if is_workflow_live(source_id) {
        return Err(BoughError::http(
            409,
            ErrorKind::Conflict,
            format!(
                "workflow {source_id} is still running — stop it first, then relaunch. A \
                 relaunch replays the journal of a run that has finished writing one; \
                 seeding from a run that is still journaling would replay a prefix that is \
                 still moving. Pause before you stop: agents already dispatched finish and \
                 are journaled, so they replay instead of starting over."
            ),
        ));
    }

    let (script, from) = resolve_rerun_script(&source, opts.script.as_deref()).await;
    let meta = extract_meta(&script)?;
    let preview = relaunch_preview(&ctx.db, source_id)?;
    let deps = workflow_control();
    let (workflow_ctx, binding) = workflow_ctx_for(ctx, &source.session_id, &deps, None)?;
    let started = start_workflow(
        &workflow_ctx,
        StartOpts {
            session_id: source.session_id.clone(),
            script,
            meta: Some(meta),
            // `None` means "keep the source run's input" — the engine reads it
            // off the source row. `Some(null)` would silently blank it.
            args: opts.args,
            resume_of: Some(source_id.to_string()),
            effective_model: Some(workflow_effective_model(ctx, &source.session_id)),
            ..Default::default()
        },
    )
    .await;
    match started {
        Ok(run) => {
            binding.bind(Some(run.id.clone()));
            Ok(RelaunchResult {
                run,
                source,
                script: from,
                replay: preview,
            })
        }
        Err(err) => {
            // Nothing started, so nothing can claim: settle the binding rather
            // than leaving a wait nobody will ever release.
            binding.bind(None);
            Err(err)
        }
    }
}

// ---------------------------------------------------------------------------
// Reporting what the relaunch cost
// ---------------------------------------------------------------------------

/// What a run actually did with its journal. Derived entirely from rows, so it
/// reads the same for a finished run, one still in flight, and one a restart
/// orphaned.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RelaunchReport {
    pub run_id: String,
    /// The run this one replays from, or `null` when it is a first run.
    pub source_id: Option<String>,
    pub total: usize,
    /// Served from the journal: no subagent, no cost.
    pub replayed: usize,
    /// Ran an agent and settled — what this run paid for.
    pub ran_live: usize,
    /// Queued or running. Non-zero only while the run is in flight.
    pub pending: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub stopped: usize,
    /// Answers the source run offered — the ceiling on `replayed`.
    ///
    /// NOTE the deliberate difference from `report::ReplaySummary::available`:
    /// there it is the replayable PREFIX, here it is ALL answers. Two different
    /// ceilings for two different questions, and the TS source states both.
    pub available: usize,
    /// The dispatch index of the call replay stopped at, or `null` when the
    /// prefix held all the way. "Call N of this run" — a human coordinate.
    pub diverged_at: Option<i64>,
    /// Where replay stopped in the SCRIPT, and why.
    ///
    /// The structural coordinate is the load-bearing half. `divergedAt` is a
    /// dispatch index, and dispatch index is precisely what is not reproducible
    /// across runs of a barrier-free `pipeline()` — quoting it alone is how a
    /// transposed position came to be reported as an edited prompt.
    pub diverged: Option<DivergenceView>,
    /// `diverged?.pos`, lifted so a client can sort or link on it.
    pub diverged_pos: Option<CallPos>,
    /// Calls that ran live even though their key still matched the source at
    /// their own position — the price of the prefix rule, stated rather than
    /// hidden. Every one is a call a key-matching cache would have served and
    /// this engine deliberately did not, because an earlier call changed and
    /// agents share a checkout.
    pub forced: usize,
    /// Has the run ended? Until it has, these are counts so far, not a bill.
    pub final_: bool,
    /// The prompts that cost an agent, in call order. On a relaunch: the edit,
    /// visible.
    pub live_prompts: Vec<String>,
}

impl RelaunchReport {
    /// The wire object: `final_` spelled `final`, plus the human `line` the
    /// endpoint carries so every client says the same thing.
    pub fn to_json(&self) -> serde_json::Value {
        let mut value = serde_json::to_value(self).unwrap_or(serde_json::Value::Null);
        if let Some(obj) = value.as_object_mut() {
            if let Some(v) = obj.remove("final_") {
                obj.insert("final".to_string(), v);
            }
            obj.insert(
                "line".to_string(),
                serde_json::Value::String(relaunch_line(self)),
            );
        }
        value
    }
}

/// 404 on an unknown id: "nothing replayed" and "no such run" are the same
/// shape and opposite problems.
pub fn relaunch_report(db: &SharedDb, run_id: &str) -> Result<RelaunchReport, BoughError> {
    let (run, rows) = {
        let guard = db.lock().unwrap();
        let run = guard
            .get_workflow(run_id)?
            .ok_or_else(|| BoughError::not_found(format!("workflow {run_id} not found")))?;
        let rows = guard.list_workflow_agents(run_id)?;
        (run, rows)
    };
    let plan = match &run.resume_of {
        Some(src) => replay_plan(db, src)?,
        None => empty_replay_plan(),
    };

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
    // The divergence and the forced count come from the ENGINE's own fold, not a
    // second walk here. A report that re-derived the prefix rule its own way
    // could disagree with the journal, and then the number that exists to expose
    // a defect would be one.
    let audit = replay_audit(&plan, &rows);
    let diverged: Option<DivergenceView> = audit.diverged.map(DivergenceView::from);
    Ok(RelaunchReport {
        run_id: run_id.to_string(),
        source_id: run.resume_of.clone(),
        total: rows.len(),
        replayed,
        ran_live: succeeded + failed + stopped,
        pending,
        succeeded,
        failed,
        stopped,
        available: plan.steps.iter().filter(|s| s.result.is_some()).count(),
        diverged_at: audit.diverged_at,
        diverged_pos: diverged.as_ref().map(|d| d.pos.clone()),
        diverged,
        forced: audit.forced,
        final_: !matches!(run.status, WorkflowStatus::Running | WorkflowStatus::Paused),
        live_prompts,
    })
}

/// The one-line human form — a run-view header, a CLI line, a note.
///
/// Written so a failure reads as a failure: "0 replayed of 12 available" is a
/// sentence someone stops on, and "12 agents ran" is one they scroll past. They
/// are the same run.
pub fn relaunch_line(r: &RelaunchReport) -> String {
    if r.total == 0 {
        return if r.pending > 0 {
            "no calls journaled yet"
        } else {
            "no agent calls"
        }
        .to_string();
    }
    let mut parts = vec![
        format!("{} replayed", r.replayed),
        format!("{} ran live", r.ran_live),
    ];
    if r.pending > 0 {
        parts.push(format!("{} still going", r.pending));
    }
    let mut line = format!("{} of {}", parts.join(", "), r.total);
    let Some(source_id) = &r.source_id else {
        return line;
    };
    line.push_str(&format!(" ({} available from {source_id})", r.available));
    if r.available > 0 && r.replayed == 0 {
        // WHY it replayed nothing, not just that it did. "every key changed" was
        // wrong for a transposed position and right for an edit, and the two
        // need opposite fixes.
        let reason = r
            .diverged
            .as_ref()
            .map_or("the first call already differed", |d| d.reason.as_str());
        return format!("{line} — replayed NOTHING: {reason}");
    }
    if let Some(d) = &r.diverged {
        line.push_str(&format!(
            "; replay stopped at {} (call {}) — {}",
            d.pos,
            r.diverged_at.map_or("null".to_string(), |n| n.to_string()),
            d.reason
        ));
        if r.forced > 0 {
            line.push_str(&format!(
                ", so {} unchanged call{} ran live behind it",
                r.forced,
                if r.forced == 1 { "" } else { "s" }
            ));
        }
    }
    line
}

// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::sqlite_db::{DbOptions, SqliteDb};
    use crate::schema::parts::{
        Session, SessionKind, WorkflowAgent, WorkflowAgentStatus, WorkflowRun,
    };
    use std::sync::{Arc, Mutex};

    fn mem_db() -> SharedDb {
        Arc::new(Mutex::new(
            SqliteDb::new(":memory:", DbOptions::default()).unwrap(),
        ))
    }

    fn run(db: &SharedDb, id: &str, status: WorkflowStatus, resume_of: Option<&str>) -> String {
        let guard = db.lock().unwrap();
        guard
            .create_session(Session {
                id: format!("s-{id}"),
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
        guard
            .create_workflow(WorkflowRun {
                id: id.into(),
                session_id: format!("s-{id}"),
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
                finished_at: Some(3_000),
            })
            .unwrap();
        id.to_string()
    }

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

    /// The preview is a CEILING read off the source journal: everything it
    /// journaled, everything that answered, and the leading run of answers —
    /// which is smaller as soon as the source failed part-way.
    #[test]
    fn the_preview_reports_the_ceiling_and_the_prefix_separately() {
        let db = mem_db();
        run(&db, "src", WorkflowStatus::Done, None);
        use WorkflowAgentStatus::*;
        keyed(&db, "src", 0, "0|c0", Done, Some("a"));
        keyed(&db, "src", 1, "1|c1", Done, Some("b"));
        keyed(&db, "src", 2, "2|c2", Error, None); // the prefix ends here
        keyed(&db, "src", 3, "3|c3", Done, Some("d")); // an answer behind a failure
        let p = relaunch_preview(&db, "src").unwrap();
        assert_eq!(p.source_id, "src");
        assert_eq!(p.journaled, 4);
        assert_eq!(p.answers, 3, "three rows carry a result");
        assert_eq!(
            p.replayable_prefix, 2,
            "an answer sitting behind a failed call was never available"
        );
    }

    /// THE gate: `replayed + ranLive + pending == total`, on a run holding one
    /// row of every status, read back out of a real database.
    #[test]
    fn every_journaled_call_is_counted_once_and_the_buckets_sum_to_the_total() {
        let db = mem_db();
        run(&db, "wf", WorkflowStatus::Running, None);
        use WorkflowAgentStatus::*;
        for (i, st) in [Cached, Queued, Running, Done, Error, Stopped]
            .into_iter()
            .enumerate()
        {
            keyed(&db, "wf", i as i64, &format!("{i}|c{i}"), st, None);
        }
        let r = relaunch_report(&db, "wf").unwrap();
        assert_eq!(r.total, 6);
        assert_eq!(r.replayed + r.ran_live + r.pending, r.total);
        assert_eq!(r.replayed, 1);
        assert_eq!(r.pending, 2);
        assert_eq!(r.ran_live, 3);
        assert_eq!((r.succeeded, r.failed, r.stopped), (1, 1, 1));
        assert_eq!(
            r.live_prompts.len(),
            5,
            "every non-replayed prompt, and only those"
        );
        assert!(!r.final_, "a running run's counts can still move");
        // A first run has nothing to diverge from and nothing on offer.
        assert_eq!(r.source_id, None);
        assert_eq!(r.available, 0);
        assert_eq!(r.diverged, None);
        assert_eq!(r.diverged_at, None);
        assert_eq!(r.forced, 0);
        assert_eq!(
            relaunch_line(&r),
            "1 replayed, 3 ran live, 2 still going of 6"
        );
    }

    /// `available` here is ALL the source's answers, not the prefix — the
    /// deliberate difference from `ReplaySummary.available`, pinned so a later
    /// "cleanup" cannot quietly unify them.
    #[test]
    fn available_counts_every_answer_here_and_only_the_prefix_in_the_summary() {
        let db = mem_db();
        run(&db, "src", WorkflowStatus::Done, None);
        use WorkflowAgentStatus::*;
        keyed(&db, "src", 0, "0|c0", Done, Some("a"));
        keyed(&db, "src", 1, "1|c1", Error, None);
        keyed(&db, "src", 2, "2|c2", Done, Some("c"));
        run(&db, "wf", WorkflowStatus::Done, Some("src"));
        keyed(&db, "wf", 0, "0|c0", Cached, Some("a"));

        let report = relaunch_report(&db, "wf").unwrap();
        assert_eq!(report.available, 2, "every answer the source holds");
        let summary = super::super::report::replay_summary(&db, "wf").unwrap();
        assert_eq!(
            summary.available, 1,
            "the replayable PREFIX, which stops at the failure"
        );
    }

    /// A relaunch that replayed nothing says WHY, and one that replayed a
    /// prefix names the slot, the dispatch index and what the prefix rule cost.
    #[test]
    fn the_line_names_the_defect_and_states_what_the_prefix_rule_cost() {
        let db = mem_db();
        run(&db, "src", WorkflowStatus::Done, None);
        use WorkflowAgentStatus::*;
        for i in 0..4 {
            keyed(&db, "src", i, &format!("{i}|c{i}"), Done, Some("answer"));
        }

        // Nothing replayed, with four answers on offer: the key-drift sentence.
        run(&db, "miss", WorkflowStatus::Done, Some("src"));
        keyed(&db, "miss", 0, "0|zzz", Done, Some("x"));
        let r = relaunch_report(&db, "miss").unwrap();
        assert_eq!(r.replayed, 0);
        assert_eq!(r.available, 4);
        assert_eq!(r.diverged.as_ref().unwrap().kind, "changed");
        assert_eq!(
            r.diverged_at,
            Some(0),
            "the dispatch index of the call replay stopped at"
        );
        assert!(
            r.line_is("0 replayed, 1 ran live of 1 (4 available from src) — replayed NOTHING: "),
            "{}",
            relaunch_line(&r)
        );

        // A prefix replayed, then an edit, then two unchanged calls FORCED live.
        run(&db, "part", WorkflowStatus::Done, Some("src"));
        keyed(&db, "part", 0, "0|c0", Cached, Some("answer"));
        keyed(&db, "part", 1, "1|edited", Done, Some("x"));
        keyed(&db, "part", 2, "2|c2", Done, Some("x"));
        keyed(&db, "part", 3, "3|c3", Done, Some("x"));
        let p = relaunch_report(&db, "part").unwrap();
        assert_eq!(p.replayed, 1);
        assert_eq!(p.ran_live, 3);
        assert_eq!(p.replayed + p.ran_live + p.pending, p.total);
        assert_eq!(
            p.forced, 2,
            "two calls whose own key still matched ran live anyway"
        );
        let line = relaunch_line(&p);
        assert!(line.contains("replay stopped at 1 (call 1) — "), "{line}");
        assert!(
            line.ends_with(", so 2 unchanged calls ran live behind it"),
            "{line}"
        );
    }

    /// The endpoint's body: `final` under its real name, and `line` alongside.
    #[test]
    fn the_wire_object_spells_final_and_carries_the_line() {
        let db = mem_db();
        run(&db, "wf", WorkflowStatus::Done, None);
        let json = relaunch_report(&db, "wf").unwrap().to_json();
        assert_eq!(json["final"], true);
        assert!(json.get("final_").is_none(), "{json}");
        assert_eq!(json["line"], "no agent calls");
        for key in [
            "runId",
            "sourceId",
            "total",
            "replayed",
            "ranLive",
            "pending",
            "succeeded",
            "failed",
            "stopped",
            "available",
            "divergedAt",
            "diverged",
            "divergedPos",
            "forced",
            "livePrompts",
        ] {
            assert!(json.get(key).is_some(), "missing {key}: {json}");
        }
    }

    #[test]
    fn an_unknown_run_is_a_404_naming_it() {
        let db = mem_db();
        let err = relaunch_report(&db, "nope").unwrap_err();
        assert_eq!(err.status(), 404);
        assert_eq!(err.to_string(), "workflow nope not found");
    }

    impl RelaunchReport {
        fn line_is(&self, prefix: &str) -> bool {
            relaunch_line(self).starts_with(prefix)
        }
    }
}
