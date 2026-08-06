//! Workflow lifecycle control and the run VIEW. Port of the view half of
//! `src/workflow/control.ts` (row 3.10).
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
//! WHAT IS NOT HERE YET (row 3.10, blocked on nothing but time — the engine it
//! needs has landed): `create_subagent_runner`, `WorkflowAgentRegistry`,
//! `workflow_ctx_for`, `start_workflow_run`, `rerun_workflow_run`,
//! `control_workflow_agent`, `append_workflow_part`, `workflow_verb` and the
//! `workflow()` host fn. Their routes still answer the unknown-run 404 rather
//! than a fabricated 201 — a start that started nothing is precisely the
//! failure `workflow::relaunch`'s accounting exists to expose.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::errors::BoughError;
use crate::paths::workflow_script_path;
use crate::schema::parts::{Part, WorkflowAgent, WorkflowRun};
use crate::types::SharedDb;

use super::engine::is_workflow_live;
use super::key::clip;
use super::pos::{compare_pos, split_journal_key};
use super::report::{run_accounting, AccountingOpts};

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
