//! Prefix-bounded replay, pure (port of the replay half of
//! `src/workflow/run.ts`).
//!
//! **Replay stops at the first changed call and never resumes.** A key covers a
//! call's PROMPT, not the filesystem that prompt runs against, and workflow
//! agents all share one checkout: two calls can say "run the test suite"
//! byte-identically and mean different questions because an upstream agent
//! rewrote the code in between. A miss costs money; a stale hit is a wrong
//! answer presented as a fresh one, so the engine buys the cheap failure. The
//! plan is therefore indexed by call POSITION, not a key→result map: position
//! is part of the identity of a call.
//!
//! **Only successful calls replay.** A failed call re-runs live — the failure
//! may well be the thing the author just fixed — and, under the prefix rule, so
//! does everything after it.

use std::collections::HashMap;

use crate::errors::BoughError;
use crate::schema::parts::{WorkflowAgent, WorkflowAgentStatus};
use crate::types::SharedDb;

use super::pos::{compare_pos, split_journal_key, CallPos};

/// One call of a source run's journal, as the replay decision sees it.
#[derive(Clone, Debug, PartialEq)]
pub struct ReplayStep {
    /// The call's STRUCTURAL coordinate — what a relaunch matches on.
    pub pos: CallPos,
    /// Hash of what the call asks for, position excluded. `call_key`'s output.
    pub content: String,
    /// The stored key exactly as journaled — `<pos>|<content>`.
    pub key: String,
    /// The call's dispatch index in the source run. Display and row ordering.
    pub idx: i64,
    /// The stored report, or `None` when that call has no answer to hand back.
    pub result: Option<String>,
    /// Carried for reporting — which call the prefix broke on is the useful line.
    pub prompt: String,
}

/// A source run's calls, addressable by structural coordinate and ordered by
/// it. `by_content` exists for one job: telling a call that MOVED apart from a
/// call that was EDITED when replay stops.
#[derive(Clone, Debug, Default)]
pub struct ReplayPlan {
    /// Every journaled call, sorted by [`compare_pos`].
    pub steps: Vec<ReplayStep>,
    pub by_pos: HashMap<CallPos, ReplayStep>,
    /// Content hash → the coordinates the source ran that exact call at.
    pub by_content: HashMap<String, Vec<CallPos>>,
}

/// The plan a first run replays from: nothing.
pub fn empty_replay_plan() -> ReplayPlan {
    ReplayPlan::default()
}

/// Read a source run's journal into a replay plan.
///
/// Only `done`/`cached` rows with a non-null result are answers; `error`,
/// `stopped`, `queued` and `running` are not.
///
/// A row journaled before coordinates existed has no `pos` half in its key. It
/// is given its dispatch index as a coordinate, which is what a sequential
/// script produces anyway — so an old sequential journal still replays, and an
/// old concurrent one misses and re-runs, which is the safe direction.
pub fn replay_plan(db: &SharedDb, source_run_id: &str) -> Result<ReplayPlan, BoughError> {
    let rows = {
        let db = db.lock().expect("db mutex");
        db.list_workflow_agents(source_run_id)?
    };
    Ok(plan_from_rows(&rows))
}

/// The same fold over rows already in hand (the engine holds them at start).
pub fn plan_from_rows(rows: &[WorkflowAgent]) -> ReplayPlan {
    let mut plan = empty_replay_plan();
    for a in rows {
        let answered = matches!(
            a.status,
            WorkflowAgentStatus::Done | WorkflowAgentStatus::Cached
        ) && a.result.is_some();
        let split = split_journal_key(&a.key);
        let step = ReplayStep {
            pos: split.pos.unwrap_or_else(|| a.idx.to_string()),
            content: split.content,
            key: a.key.clone(),
            idx: a.idx,
            result: if answered { a.result.clone() } else { None },
            prompt: a.prompt.clone(),
        };
        plan.by_pos.insert(step.pos.clone(), step.clone());
        plan.by_content
            .entry(step.content.clone())
            .or_default()
            .push(step.pos.clone());
        plan.steps.push(step);
    }
    plan.steps.sort_by(|x, y| compare_pos(&x.pos, &y.pos));
    plan
}

/// How many leading calls of a plan could replay AT BEST — the ceiling a
/// relaunch can claim before its own keys are known. Zero is not a defect on
/// its own: a source that failed its first call has nothing to offer.
///
/// "Leading" is in STRUCTURAL order, which is the order the prefix rule is
/// defined over.
pub fn replayable_prefix(plan: &ReplayPlan) -> usize {
    let mut n = 0;
    while n < plan.steps.len() && plan.steps[n].result.is_some() {
        n += 1;
    }
    n
}

/// The four ways a call can fail to replay. They are separated because they
/// call for four different next moves, and one of them used to be reported as
/// another.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DivergenceKind {
    /// Same coordinate, different content hash — the call was EDITED.
    Changed,
    /// The content hash is unchanged and the source ran this exact call at a
    /// DIFFERENT coordinate. The script's shape changed, not its prompts.
    Moved,
    /// No call at that coordinate and nothing anywhere asks the same thing.
    Added,
    /// The source made this exact call and has nothing to hand back.
    Unanswered,
}

impl DivergenceKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            DivergenceKind::Changed => "changed",
            DivergenceKind::Moved => "moved",
            DivergenceKind::Added => "added",
            DivergenceKind::Unanswered => "unanswered",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Divergence {
    /// This run's coordinate for the call replay stopped at.
    pub pos: CallPos,
    pub kind: DivergenceKind,
    /// Where the source ran this same call, when `kind` is `Moved`.
    pub source_pos: Option<CallPos>,
    /// One sentence, in the words every surface prints.
    pub reason: String,
}

/// Why the call at `pos` asking for `content` cannot replay from `plan`.
///
/// `moved` is tested BEFORE `changed`, and the order is the whole point.
/// Asking "is this coordinate occupied?" first made `moved` unreachable for the
/// commonest kind of move: any reorder that preserves the call count leaves
/// every slot full, so a pure swap was reported as "the call at 0 was edited".
/// That is the misdiagnosis that hid the pipeline transposition defect.
pub fn classify_divergence(plan: &ReplayPlan, pos: &str, content: &str) -> Divergence {
    let step = plan.by_pos.get(pos);
    if let Some(step) = step {
        if step.content == content {
            return Divergence {
                pos: pos.to_string(),
                kind: DivergenceKind::Unanswered,
                source_pos: None,
                reason: format!(
                    "the source run made this call at {pos} and has no answer for it — it \
                     failed, was stopped, or never finished, so it runs live"
                ),
            };
        }
    }
    if let Some(elsewhere) = plan.by_content.get(content) {
        if let Some(first) = elsewhere.first() {
            return Divergence {
                pos: pos.to_string(),
                kind: DivergenceKind::Moved,
                source_pos: Some(first.clone()),
                reason: format!(
                    "the call MOVED: its key did not change — the source run made this exact \
                     call at slot {first}, and this run makes it at slot {pos}. The script's \
                     shape changed, not its prompts"
                ),
            };
        }
    }
    if step.is_some() {
        return Divergence {
            pos: pos.to_string(),
            kind: DivergenceKind::Changed,
            source_pos: None,
            reason: format!(
                "the call at slot {pos} was edited: same position in the script, different key"
            ),
        };
    }
    Divergence {
        pos: pos.to_string(),
        kind: DivergenceKind::Added,
        source_pos: None,
        reason: format!(
            "the source run never made a call at slot {pos}, and none of its calls ask for \
             the same thing — this call is new"
        ),
    }
}

/// What a run did with its journal, folded from the rows it wrote. One
/// implementation, so the completion note, `GET /workflows/:id/replay` and the
/// run view cannot disagree about where replay stopped.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ReplayAudit {
    /// The structurally first call replay could not serve, or `None`.
    pub diverged: Option<Divergence>,
    /// Its dispatch index in this run — "call N of this run", for a human line.
    pub diverged_at: Option<i64>,
    /// Calls that ran live although their coordinate AND key still matched a
    /// stored answer — the price of the prefix rule, stated rather than hidden.
    pub forced: usize,
}

pub fn replay_audit(plan: &ReplayPlan, rows: &[WorkflowAgent]) -> ReplayAudit {
    // Nothing to diverge FROM. A first run has no source, and a relaunch of a
    // run that journaled nothing has an empty one; in both cases every call is
    // live because there was never an alternative. Reporting a divergence here
    // would put "the source run never made a call at 0.0.0.0" on a run with no
    // source — an accusation with no defendant, on the most ordinary path.
    if plan.steps.is_empty() {
        return ReplayAudit::default();
    }
    let mut audit = ReplayAudit::default();
    let mut diverged_pos: Option<CallPos> = None;
    for row in rows {
        if row.status == WorkflowAgentStatus::Cached {
            continue;
        }
        let split = split_journal_key(&row.key);
        let at = split.pos.unwrap_or_else(|| row.idx.to_string());
        let content = split.content;
        if let Some(step) = plan.by_pos.get(&at) {
            if step.content == content && step.result.is_some() {
                audit.forced += 1;
                continue;
            }
        }
        let earlier = match &diverged_pos {
            None => true,
            Some(current) => compare_pos(&at, current).is_lt(),
        };
        if earlier {
            audit.diverged = Some(classify_divergence(plan, &at, &content));
            audit.diverged_at = Some(row.idx);
            diverged_pos = Some(at);
        }
    }
    audit
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(
        idx: i64,
        key: &str,
        status: WorkflowAgentStatus,
        result: Option<&str>,
    ) -> WorkflowAgent {
        WorkflowAgent {
            id: format!("a{idx}"),
            run_id: "run".into(),
            idx,
            key: key.into(),
            label: format!("call {idx}"),
            phase: None,
            prompt: format!("prompt {idx}"),
            model: None,
            status,
            result: result.map(str::to_string),
            error: None,
            session_id: None,
            started_at: 0,
            finished_at: None,
        }
    }

    fn answered(idx: i64, key: &str) -> WorkflowAgent {
        row(
            idx,
            key,
            WorkflowAgentStatus::Done,
            Some(&format!("report {idx}")),
        )
    }

    /// Only `done`/`cached` rows WITH a result are answers, and the plan is
    /// ordered structurally — not by dispatch index.
    #[test]
    fn only_successful_calls_are_answers_and_steps_sort_structurally() {
        let rows = vec![
            answered(0, "0.1|aaa"),
            answered(1, "0.0|bbb"),
            row(2, "0.2|ccc", WorkflowAgentStatus::Error, None),
            row(
                3,
                "0.3|ddd",
                WorkflowAgentStatus::Stopped,
                Some("half a report"),
            ),
            row(4, "0.4|eee", WorkflowAgentStatus::Cached, Some("replayed")),
            row(5, "0.5|fff", WorkflowAgentStatus::Queued, None),
        ];
        let plan = plan_from_rows(&rows);
        assert_eq!(
            plan.steps
                .iter()
                .map(|s| s.pos.as_str())
                .collect::<Vec<_>>(),
            ["0.0", "0.1", "0.2", "0.3", "0.4", "0.5"]
        );
        assert!(plan.by_pos["0.0"].result.is_some());
        assert!(plan.by_pos["0.1"].result.is_some());
        assert!(
            plan.by_pos["0.2"].result.is_none(),
            "an error is not an answer"
        );
        assert!(
            plan.by_pos["0.3"].result.is_none(),
            "a stop is not an answer"
        );
        assert!(
            plan.by_pos["0.4"].result.is_some(),
            "a cached row answers on the next run too"
        );
        assert!(plan.by_pos["0.5"].result.is_none());
        // The prefix stops at the first unanswered step, structurally.
        assert_eq!(replayable_prefix(&plan), 2);
    }

    /// The prefix is a leading run: a failure in the middle ends it even though
    /// later calls answered.
    #[test]
    fn a_failed_call_ends_the_prefix_even_when_later_calls_answered() {
        let rows = vec![
            answered(0, "0|a"),
            row(1, "1|b", WorkflowAgentStatus::Error, None),
            answered(2, "2|c"),
        ];
        assert_eq!(replayable_prefix(&plan_from_rows(&rows)), 1);
    }

    /// A pre-coordinate journal (no `|`) reads its dispatch index as the
    /// coordinate — an old sequential journal still replays.
    #[test]
    fn pre_coordinate_rows_take_their_index_as_the_position() {
        let rows = vec![answered(0, "aaa"), answered(1, "bbb")];
        let plan = plan_from_rows(&rows);
        assert_eq!(plan.steps[0].pos, "0");
        assert_eq!(plan.steps[1].pos, "1");
        assert_eq!(plan.by_pos["1"].content, "bbb");
        assert_eq!(replayable_prefix(&plan), 2);
    }

    /// `moved` before `changed`: a pure swap leaves every slot full, and
    /// slot-first reported it as "the call at 0 was edited".
    #[test]
    fn a_transposition_is_classified_moved_not_changed() {
        let plan = plan_from_rows(&[answered(0, "0|aaa"), answered(1, "1|bbb")]);
        let d = classify_divergence(&plan, "0", "bbb");
        assert_eq!(d.kind, DivergenceKind::Moved);
        assert_eq!(d.source_pos.as_deref(), Some("1"));
        assert!(d.reason.contains("the call MOVED"), "{}", d.reason);
        assert!(d.reason.contains("its key did not change"), "{}", d.reason);
    }

    #[test]
    fn the_other_three_kinds_name_what_happened() {
        let plan = plan_from_rows(&[
            answered(0, "0|aaa"),
            row(1, "1|bbb", WorkflowAgentStatus::Error, None),
        ]);
        // Same slot, different content, and nothing else asks for it.
        let d = classify_divergence(&plan, "0", "zzz");
        assert_eq!(d.kind, DivergenceKind::Changed);
        assert!(d.reason.contains("was edited"), "{}", d.reason);
        // Same slot, same content, no answer.
        let d = classify_divergence(&plan, "1", "bbb");
        assert_eq!(d.kind, DivergenceKind::Unanswered);
        assert!(d.reason.contains("has no answer for it"), "{}", d.reason);
        // Nowhere at all.
        let d = classify_divergence(&plan, "2", "qqq");
        assert_eq!(d.kind, DivergenceKind::Added);
        assert!(d.reason.contains("this call is new"), "{}", d.reason);
    }

    /// The audit reports the STRUCTURALLY first live call the plan could not
    /// serve, not the first by dispatch index — dispatch index is the thing
    /// that was never reproducible.
    #[test]
    fn the_audit_reports_the_structurally_first_divergence_and_counts_forced() {
        let plan = plan_from_rows(&[answered(0, "0.0|aaa"), answered(1, "0.1|bbb")]);
        // This run dispatched 0.1 first (a barrier-free pipeline can), and its
        // key changed; 0.0 diverged too and is structurally earlier.
        let rows = vec![
            row(0, "0.1|zzz", WorkflowAgentStatus::Done, Some("live")),
            row(1, "0.0|yyy", WorkflowAgentStatus::Done, Some("live")),
        ];
        let audit = replay_audit(&plan, &rows);
        assert_eq!(audit.diverged.as_ref().unwrap().pos, "0.0");
        assert_eq!(
            audit.diverged_at,
            Some(1),
            "the human line names its dispatch index"
        );
        assert_eq!(audit.forced, 0);

        // A row whose pos AND key still matched an answer but ran live anyway
        // is `forced` — the price of the prefix rule, stated.
        let rows = vec![
            row(0, "0.0|zzz", WorkflowAgentStatus::Done, Some("live")),
            row(1, "0.1|bbb", WorkflowAgentStatus::Done, Some("live")),
        ];
        let audit = replay_audit(&plan, &rows);
        assert_eq!(
            audit.diverged.as_ref().unwrap().kind,
            DivergenceKind::Changed
        );
        assert_eq!(audit.forced, 1);
    }

    /// A first run has nothing to diverge FROM.
    #[test]
    fn an_empty_plan_never_accuses_anyone() {
        let rows = vec![row(0, "0|aaa", WorkflowAgentStatus::Done, Some("live"))];
        assert_eq!(
            replay_audit(&empty_replay_plan(), &rows),
            ReplayAudit::default()
        );
    }
}
