//! Invariant: the projection of a trajectory into rows is PURE and TOTAL. `tool/call` and
//! `tool/result` fold into ONE row by call id; envelope steps fold into their neighbours or are
//! dropped; and an UNKNOWN step type renders as `Other` and never panics — the step-type map is
//! merge-extensible (§3), so a renderer will meet types it does not own.

use std::collections::{BTreeMap, BTreeSet};

use bough_plugin_agents::Phase;
use bough_plugin_ledger::vocabulary::MailClass;
use bough_plugin_ledger::{Step, StepId, StepType, WakeId};
use bough_plugin_llm::ToolCallId;
use bough_plugin_tools::{RenderIntent, ToolResultBody};
use bough_plugin_tui_render::{about_from_step, AboutView};

use crate::program::{self, ProgramError, ProgramSub, RUN_TOOL};

/// The `from` ref Andrey's own mail carries (`Sender::Andrey::as_ref_str`). Spelled here rather
/// than imported as a constant because `agents` exposes it only through the enum, and the focus
/// pane reads the LEDGER BODY, not a live `Message`.
pub const ANDREY_REF: &str = "andrey";

/// Step types that carry the machinery of a wake rather than anything Andrey reads. They are
/// dropped: `step/start` and `step/end` bracket rows that are already shown in order,
/// `request/header` is the reconstruction anchor, and `inbox/spliced` is the durable twin of the
/// `mail/delivered` row right next to it (rendering both would show one message twice).
/// Bookkeeping the ledger writes about itself — routing, usage, seals, pins, expiries. Not the
/// conversation, so not a row (visual audit F3): a first launch used to be three `· agent/routing`
/// lines and every turn ended in `· usage/round`. The list is CLOSED: a type this binary does not
/// know still renders as `· kind`, because a silent drop is how a new step type would vanish.
pub const MACHINERY: &[&str] = &[
    "agent/routing",
    "agent/dormancy",
    "usage/round",
    "recon/request",
    "rollup/request",
    "rollup/sealed",
    "pin/set",
    "pin/retire",
    "memory/expired",
    "power/changed",
];

pub const ENVELOPE: &[&str] = &[
    "step/start",
    "step/end",
    "request/header",
    "wake/jot",
    "wake/resumed",
    "wake/grace-prompt",
];

/// One rendered row of a trajectory.
#[derive(Clone, Debug, PartialEq)]
pub enum Row {
    Mail {
        step: StepId,
        from: String,
        subject: String,
        class: MailClass,
    },
    Andrey {
        step: StepId,
        text: String,
    },
    Text {
        /// The FIRST step of the group: the click/flash anchor, and what [`Row::step`] returns.
        step: StepId,
        /// Every step folded into this row, oldest first. `parts.len() > 1` is the joined case.
        parts: Vec<StepId>,
        wake: WakeId,
        index: u32,
        text: String,
    },
    Reasoning {
        step: StepId,
        /// Every step folded into this row, oldest first (the same rule as [`Row::Text`]).
        parts: Vec<StepId>,
        wake: WakeId,
        index: u32,
        text: String,
    },
    /// A `claim/proposed` step, with its decision (if any) folded in from later `claim/accepted`
    /// or `claim/rejected` steps of the same trajectory — BY NAME (P3-D11), so this crate gains
    /// no dependency on `claims`.
    Claim {
        step: StepId,
        claim: String,
        kind: String,
        title: String,
        body: String,
        state: ClaimState,
    },
    Tool {
        call: ToolCallId,
        name: String,
        intent: RenderIntent,
        args: serde_json::Value,
        result: Option<ToolResultBody>,
        call_step: StepId,
        /// When the call was made, for the live `running · … · 12s` line (round 5).
        at: chrono::DateTime<chrono::Utc>,
    },
    /// ONE program under code mode: the `run` call that carried the JS source, every `program/*`
    /// step written from inside it, and the `tool/result` that closed it — one row, the same rule
    /// the `tool/call` + `tool/result` pair obeys. Keyed on the consumer's `RUN_TOOL` name, never
    /// on a render intent (P-CM-D13). See `program.rs`.
    Program {
        call: ToolCallId,
        /// The JS the model wrote. Empty only when the call's args lost it.
        source: String,
        /// Every `program/console` chunk, concatenated in seq order.
        console: String,
        subs: Vec<ProgramSub>,
        /// The `tool/result` of the `run` call.
        result: Option<ToolResultBody>,
        /// The one terminal error the program ended with, if it did.
        error: Option<ProgramError>,
        ops: u64,
        ms: u64,
        call_step: StepId,
        /// When the `run` call was made (round 5, the live line).
        at: chrono::DateTime<chrono::Utc>,
        /// Every step folded into this row, oldest first (the [`Row::Text`] rule).
        parts: Vec<StepId>,
    },
    WakeMark {
        step: StepId,
        wake: WakeId,
        phase: Phase,
        reason: Option<String>,
        /// `wake/end.cause`. `aborted` is what an explicit cancel produces, and only the CAUSE
        /// says whether that cancel was Andrey's Esc or a parent tearing a worker down.
        cause: Option<String>,
    },
    About {
        step: StepId,
        view: AboutView,
    },
    /// A message from Andrey that is WAITING for its turn (round 8): an `inbox/spliced` insert
    /// whose wake has not yet claimed it. Gone the moment a later splice claims or removes the
    /// same message — the `mail/delivered` step then draws it as [`Row::Andrey`].
    Queued {
        step: StepId,
        message: String,
        text: String,
    },
    /// A `draft/message` or `draft/ticket` step, read BY NAME (the TUI brief, D6): the agent
    /// wrote something outward-facing and did NOT send it. Rendered as a card with `copy` and
    /// `open`; never a send.
    Draft {
        step: StepId,
        draft: String,
        /// `message` or `ticket`.
        kind: String,
        audience: String,
        subject: String,
        body: String,
    },
    Other {
        step: StepId,
        kind: StepType,
    },
}

/// Where a claim card stands. Folded from the `claim/accepted` / `claim/rejected` steps that
/// name the same claim id, never asked of the `claims` seam: the pane reads the LEDGER BODY.
#[derive(Clone, Debug, PartialEq)]
pub enum ClaimState {
    /// Proposed and undecided: the card draws its three hit regions.
    Open,
    Accepted {
        edited: bool,
    },
    Rejected {
        reason: String,
    },
}

impl ClaimState {
    /// The word the card shows.
    pub fn word(&self) -> &'static str {
        match self {
            ClaimState::Open => "open",
            ClaimState::Accepted { edited: false } => "accepted",
            ClaimState::Accepted { edited: true } => "accepted (edited)",
            ClaimState::Rejected { .. } => "rejected",
        }
    }

    pub fn is_open(&self) -> bool {
        matches!(self, ClaimState::Open)
    }
}

impl Row {
    /// The step this row was built from. A `Tool` row names its CALL step: the result folded into
    /// it is the same row, which is what "no step is rendered twice" means for a tool call.
    pub fn step(&self) -> &StepId {
        match self {
            Row::Mail { step, .. }
            | Row::Andrey { step, .. }
            | Row::Text { step, .. }
            | Row::Reasoning { step, .. }
            | Row::WakeMark { step, .. }
            | Row::About { step, .. }
            | Row::Draft { step, .. }
            | Row::Queued { step, .. }
            | Row::Claim { step, .. }
            | Row::Other { step, .. } => step,
            Row::Tool { call_step, .. } | Row::Program { call_step, .. } => call_step,
        }
    }

    /// Every durable step folded into this row, oldest first. One step for every row except a
    /// joined `Text`/`Reasoning` group (P5-D14).
    pub fn parts(&self) -> Vec<StepId> {
        match self {
            Row::Text { parts, .. } | Row::Reasoning { parts, .. } | Row::Program { parts, .. } => {
                parts.clone()
            }
            other => vec![other.step().clone()],
        }
    }

    /// A `Tool` row with no result yet: the call is out and the answer has not come back.
    pub fn is_pending_tool(&self) -> bool {
        matches!(self, Row::Tool { result: None, .. })
    }
}

/// PURE: the whole projection of a trajectory into rows. `tool/call` and `tool/result` fold into
/// ONE [`Row::Tool`] by call id; envelope steps (`step/start`, `request/header`, `inbox/spliced`)
/// fold into their neighbours or are dropped.
///
/// TOTAL: every input produces a row or is deliberately dropped, and a body that does not match
/// its declared shape degrades to [`Row::Other`] rather than panicking. A surface that panicked on
/// an unfamiliar step would take the terminal down with it.
pub fn rows_from_steps(steps: &[Step]) -> Vec<Row> {
    let mut out: Vec<Row> = Vec::with_capacity(steps.len());
    // Where each call id's row sits, so a result folds into the call instead of appending.
    let mut by_call: BTreeMap<ToolCallId, usize> = BTreeMap::new();
    // Where each claim id's card sits, so a decision folds into the card instead of appending.
    let mut by_claim: BTreeMap<String, usize> = BTreeMap::new();
    // Where each INNER call id's sub sits: `(row index, sub index)`, so a `program/result` reaches
    // the `ProgramSub` its `program/call` created.
    let mut by_sub: BTreeMap<ToolCallId, (usize, usize)> = BTreeMap::new();
    // P5-D14, the field bug: the durable steps of ONE model step are a SPLIT of one stream, so
    // consecutive `thought/text` (or `thought/reasoning`) steps sharing `(wake, step_index)` are
    // ONE row. `None` means the last row pushed cannot be joined onto — anything else in between
    // (a tool call, a wake mark, a new step index, a new wake) breaks the group.
    let mut open_group: Option<(usize, GroupKey)> = None;
    // Calls whose rendering IS the row that follows them (a `draft_*` call and its draft card,
    // D6): the call and its result draw nothing, or the card would sit under a header saying
    // the same thing.
    let mut hidden_calls: BTreeSet<ToolCallId> = BTreeSet::new();

    for step in steps {
        let kind = step.kind.as_str();
        if ENVELOPE.contains(&kind) || MACHINERY.contains(&kind) {
            continue;
        }
        // Every arm below except the two joining ones ends the open group; the joining arms set
        // it. Spelled once here so a new row type cannot silently keep a group open across itself.
        let was = open_group.take();
        match kind {
            "mail/delivered" => out.push(mail_row(step)),
            "thought/text" | "thought/reasoning" => {
                let text = match body_str(step, "text") {
                    Some(text) => text,
                    None => {
                        out.push(other(step));
                        continue;
                    }
                };
                let key = GroupKey {
                    text: kind == "thought/text",
                    wake: step.wake.clone(),
                    index: body_u32(step, "step_index"),
                };
                match was {
                    // The join, and it is RAW CONCATENATION: the flush boundary is a timer, not a
                    // sentence, and inserting a separator is exactly what put `"I'll run that"`
                    // and `" shell command for you."` on two lines.
                    Some((at, ref k)) if *k == key && at + 1 == out.len() => {
                        match &mut out[at] {
                            Row::Text { parts, text: t, .. }
                            | Row::Reasoning { parts, text: t, .. } => {
                                t.push_str(&text);
                                parts.push(step.id.clone());
                            }
                            _ => {}
                        }
                        open_group = Some((at, key));
                    }
                    _ => {
                        let row = if key.text {
                            Row::Text {
                                step: step.id.clone(),
                                parts: vec![step.id.clone()],
                                wake: key.wake.clone(),
                                index: key.index,
                                text,
                            }
                        } else {
                            Row::Reasoning {
                                step: step.id.clone(),
                                parts: vec![step.id.clone()],
                                wake: key.wake.clone(),
                                index: key.index,
                                text,
                            }
                        };
                        open_group = Some((out.len(), key));
                        out.push(row);
                    }
                }
            }
            // The claim card, read BY NAME (P3-D11).
            "claim/proposed" => match claim_row(step) {
                Some((claim, row)) => {
                    by_claim.insert(claim, out.len());
                    out.push(row);
                }
                None => out.push(other(step)),
            },
            "claim/accepted" | "claim/rejected" => {
                let id = body_str(step, "claim").unwrap_or_default();
                match by_claim.get(&id).copied() {
                    Some(at) => {
                        if let Row::Claim { state, .. } = &mut out[at] {
                            *state = if kind == "claim/accepted" {
                                ClaimState::Accepted {
                                    edited: step
                                        .body
                                        .get("edited")
                                        .and_then(|v| v.as_bool())
                                        .unwrap_or(false),
                                }
                            } else {
                                ClaimState::Rejected {
                                    reason: body_str(step, "reason").unwrap_or_default(),
                                }
                            };
                        }
                    }
                    // A decision whose proposal is not in this window (it paged out). Shown as
                    // itself rather than dropped: the decision is the news.
                    None => out.push(other(step)),
                }
            }
            // Code mode's ONE API tool. Its row is the anchor every `program/*` step folds into,
            // so it is recognised BEFORE the generic tool row (P-CM-D13).
            "tool/call" if step.body.get("name").and_then(|v| v.as_str()) == Some(RUN_TOOL) => {
                match program::program_row(step) {
                    Some((call, row)) => {
                        by_call.insert(call, out.len());
                        out.push(row);
                    }
                    None => out.push(other(step)),
                }
            }
            "program/call" | "program/result" | "program/console" | "program/error" => {
                // TOTAL: a sub-step whose program paged out, or whose body does not match its
                // declared shape, is shown as itself rather than dropped or panicked on.
                if !program::fold_sub(&mut out, &by_call, &mut by_sub, step) {
                    out.push(other(step));
                }
            }
            "tool/call" => match tool_call_row(step) {
                Some((call, Row::Tool { name, .. })) if is_card_call(&name) => {
                    hidden_calls.insert(call);
                }
                Some((call, row)) => {
                    by_call.insert(call, out.len());
                    out.push(row);
                }
                None => out.push(other(step)),
            },
            "tool/result" => {
                let parsed: Option<ToolResultBody> =
                    serde_json::from_value(step.body.as_ref().clone()).ok();
                match parsed {
                    Some(body) if hidden_calls.contains(&body.call) => {}
                    Some(body) => match by_call.get(&body.call).copied() {
                        // The fold: one row for the pair.
                        Some(at) => match &mut out[at] {
                            Row::Tool { result, .. } => *result = Some(body),
                            // The `run` call's own result closes its program row.
                            Row::Program {
                                result, parts, ms, ..
                            } => {
                                *ms = step
                                    .body
                                    .get("value")
                                    .and_then(|v| v.get("ms"))
                                    .and_then(|v| v.as_u64())
                                    .unwrap_or(*ms);
                                *result = Some(body);
                                parts.push(step.id.clone());
                            }
                            _ => {}
                        },
                        // A result whose call is not in this window — the call paged out, or crash
                        // repair synthesised the result. It is still a tool row, with no args.
                        None => {
                            let call = body.call.clone();
                            by_call.insert(call.clone(), out.len());
                            out.push(Row::Tool {
                                call,
                                name: body.name.to_string(),
                                intent: RenderIntent::Generic,
                                args: serde_json::Value::Null,
                                result: Some(body),
                                call_step: step.id.clone(),
                                at: step.at,
                            });
                        }
                    },
                    None => out.push(other(step)),
                }
            }
            // ONE rule per turn (visual audit F2): the turn's start is said by the speaker label
            // on its first words, its end by the `── turn ended · …` rule. `── turn` above the
            // label was a third chrome line saying what the label already says.
            "wake/start" => {}
            // A message sent while a turn ran (round 8): the splice that queued it is the only
            // step it has until its own wake claims it.
            "inbox/spliced" => {
                let op = body_str(step, "op").unwrap_or_default();
                let message = body_str(step, "message").unwrap_or_default();
                match op.as_str() {
                    "insert" => {
                        let payload = step.body.get("payload");
                        let from = payload
                            .and_then(|p| p.get("from"))
                            .and_then(|f| f.get("kind"))
                            .and_then(|k| k.as_str())
                            .unwrap_or("");
                        let text = payload
                            .and_then(|p| p.get("text"))
                            .and_then(|t| t.as_str())
                            .unwrap_or("")
                            .to_string();
                        if from == ANDREY_REF && !message.is_empty() {
                            out.push(Row::Queued {
                                step: step.id.clone(),
                                message,
                                text,
                            });
                        }
                    }
                    _ => out
                        .retain(|r| !matches!(r, Row::Queued { message: m, .. } if *m == message)),
                }
            }
            "draft/message" | "draft/ticket" => match draft_row(step) {
                Some(row) => out.push(row),
                None => out.push(other(step)),
            },
            // The rule only when the ending is NEWS (the TUI brief, D5): a completed turn is
            // already told by the next speaker label, so `── turn ended · completed` after every
            // turn was a second line saying the same thing. An interrupt, an abort, a crash
            // repair — those the reader must not miss, and they keep the rule.
            "wake/end" => {
                let reason = body_str(step, "reason");
                let quiet = matches!(
                    reason.as_deref().map(str::trim),
                    None | Some("") | Some("completed")
                );
                if !quiet {
                    out.push(Row::WakeMark {
                        step: step.id.clone(),
                        wake: step.wake.clone(),
                        phase: Phase::End,
                        reason,
                        cause: body_str(step, "cause"),
                    });
                }
            }
            _ => match about_from_step(step) {
                // `about/line` BY NAME (P3-D11): the pane does not depend on the row that writes it.
                // It is the RAIL's line (and `/agents`'), not the transcript's: echoed here it was a
                // green sentence after every turn that nobody wrote to Andrey (visual audit F2).
                Some(_) => {}
                None => out.push(other(step)),
            },
        }
    }
    out
}

/// What makes two consecutive durable steps ONE row: the same kind of thought, in the same wake,
/// at the same model step index (P5-D14).
#[derive(Clone, Debug, PartialEq)]
struct GroupKey {
    /// `true` for `thought/text`, `false` for `thought/reasoning`. The two never join together.
    text: bool,
    wake: WakeId,
    index: u32,
}

fn claim_row(step: &Step) -> Option<(String, Row)> {
    let claim = body_str(step, "claim")?;
    Some((
        claim.clone(),
        Row::Claim {
            step: step.id.clone(),
            claim,
            kind: body_str(step, "kind").unwrap_or_default(),
            title: body_str(step, "title").unwrap_or_default(),
            body: body_str(step, "body").unwrap_or_default(),
            state: ClaimState::Open,
        },
    ))
}

/// PURE: whether a tool or program row FAILED (a result that is not ok, or a program error).
/// A row with no result yet is neither.
pub fn is_failed_call(row: &Row) -> bool {
    match row {
        Row::Tool { result, .. } => result
            .as_ref()
            .is_some_and(|r| r.outcome != bough_plugin_tools::ToolOutcomeKind::Ok),
        Row::Program { result, error, .. } => {
            error.is_some()
                || result
                    .as_ref()
                    .is_some_and(|r| r.outcome != bough_plugin_tools::ToolOutcomeKind::Ok)
        }
        _ => false,
    }
}

/// PURE: whether a tool or program row SUCCEEDED.
pub fn is_ok_call(row: &Row) -> bool {
    match row {
        Row::Tool { result, .. } => result
            .as_ref()
            .is_some_and(|r| r.outcome == bough_plugin_tools::ToolOutcomeKind::Ok),
        Row::Program { result, error, .. } => {
            error.is_none()
                && result
                    .as_ref()
                    .is_some_and(|r| r.outcome == bough_plugin_tools::ToolOutcomeKind::Ok)
        }
        _ => false,
    }
}

/// One run of failed attempts folded under the call that finally succeeded (round 8).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetryFold {
    /// The first failed row.
    pub start: usize,
    /// One past the last folded row — the successful call's index.
    pub end: usize,
    /// How many failed calls the run holds (its narration rows are folded with them).
    pub attempts: usize,
}

/// The tool a call row is: a typed tool's name, or `program` for a code-mode run.
fn call_name(row: &Row) -> Option<&str> {
    match row {
        Row::Tool { name, .. } => Some(name.as_str()),
        Row::Program { .. } => Some("program"),
        _ => None,
    }
}

/// PURE: the runs of failed calls — with the model's narration between them — that are followed
/// by a successful call OF THE SAME TOOL, so the conversation can fold them under `▸ N failed
/// attempts` instead of leaving `✗` rows and "let me fix the tag" inline forever. A failed run
/// that never succeeded is NOT folded: that failure is the news. A failed `read_file` followed
/// by a successful `write_file` is two different things, not a retry (02-tool-calls).
pub fn retry_folds(rows: &[Row]) -> Vec<RetryFold> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < rows.len() {
        if !is_failed_call(&rows[i]) {
            i += 1;
            continue;
        }
        let start = i;
        let tool = call_name(&rows[i]);
        let mut attempts = 0;
        let mut j = i;
        while j < rows.len() {
            if is_failed_call(&rows[j]) && call_name(&rows[j]) == tool {
                attempts += 1;
                j += 1;
            } else if matches!(rows[j], Row::Text { .. } | Row::Reasoning { .. }) {
                j += 1;
            } else {
                break;
            }
        }
        // `j` is the first row that is neither a failed call of this tool nor narration.
        if j < rows.len() && is_ok_call(&rows[j]) && call_name(&rows[j]) == tool {
            out.push(RetryFold {
                start,
                end: j,
                attempts,
            });
        }
        i = j.max(start + 1);
    }
    out
}

/// PURE: a program that did nothing a reader can act on (round 9) — no inner call, nothing
/// printed, finished without error. Code mode makes the model run one for a plain reply, and
/// `▸ program 0 calls ✓` on every chat-only turn was noise.
pub fn is_empty_program(row: &Row) -> bool {
    matches!(
        row,
        Row::Program { subs, console, result: Some(_), error: None, .. }
            if subs.is_empty() && console.trim().is_empty()
    )
}

/// What the focused lane is WAITING ON FROM ANDREY (round 10): open claims, and whether its
/// last message was a question to him.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Owed {
    pub claims: usize,
    pub question: bool,
}

/// PURE: the lane's open claims, and — with the turn over — whether its last message ends in a
/// question that nothing from Andrey has answered yet. Heuristic by design (no model call):
/// the last agent row is text ending with `?`, the turn is not running, and no Andrey row
/// follows it.
pub fn owed(rows: &[Row], running: bool) -> Owed {
    let claims = rows
        .iter()
        .filter(|r| matches!(r, Row::Claim { state, .. } if state.is_open()))
        .count();
    let question = !running
        && rows
            .iter()
            .rposition(|r| is_agent_row(r) || matches!(r, Row::Andrey { .. } | Row::Queued { .. }))
            .is_some_and(|i| match &rows[i] {
                Row::Text { text, .. } => text.trim_end().ends_with('?'),
                _ => false,
            });
    Owed { claims, question }
}

/// Tools that change files, by name (typed rows and program sub-calls alike).
pub fn changes_files(name: &str) -> bool {
    matches!(name, "patch" | "edit_file" | "write_file")
}

/// PURE: the files a run of agent rows changed, in first-touched order, from every `patch` /
/// `edit_file` / `write_file` call — typed rows and the calls inside programs (round 6): "what
/// did it do to my files" was invisible among the `▸ program …` lines.
pub fn changed_files(rows: &[Row]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |name: &str, args: &serde_json::Value| {
        if changes_files(name) {
            let path = first_string(args);
            if !path.is_empty() && !out.contains(&path) {
                out.push(path);
            }
        }
    };
    for row in rows {
        match row {
            Row::Tool { name, args, .. } => push(name, args),
            Row::Program { subs, .. } => {
                for sub in subs {
                    push(&sub.name, &sub.args);
                }
            }
            _ => {}
        }
    }
    out
}

/// PURE: the live line for an in-flight call at the END of the rows (round 5): the newest agent
/// row, when it is a tool or program with no result yet — `▸ running · bash cargo test · 12s`.
/// `None` when nothing is in flight, so a finished transcript shows no such line.
pub fn running_line(rows: &[Row], now: chrono::DateTime<chrono::Utc>) -> Option<String> {
    let last = rows.iter().rev().find(|r| is_agent_row(r))?;
    let (what, at) = match last {
        Row::Tool {
            name,
            args,
            result: None,
            at,
            ..
        } => (
            format!("{name} {}", first_string(args))
                .trim_end()
                .to_string(),
            *at,
        ),
        Row::Program {
            subs,
            result: None,
            error: None,
            at,
            ..
        } => {
            let current = subs
                .iter()
                .rev()
                .find(|s| s.result.is_none())
                .map(|s| {
                    format!("{} {}", s.name, first_string(&s.args))
                        .trim_end()
                        .to_string()
                })
                .unwrap_or_else(|| "program".to_string());
            (current, *at)
        }
        _ => return None,
    };
    let secs = (now - at).num_seconds().max(0);
    let clock = if secs < 60 {
        format!("{secs}s")
    } else {
        format!("{}m{:02}", secs / 60, secs % 60)
    };
    Some(format!("\u{25b8} running \u{b7} {what} \u{b7} {clock}"))
}

/// The argument a person would name a call by: `command`/`path`/`pattern`/… else the first string.
fn first_string(args: &serde_json::Value) -> String {
    const KEYS: [&str; 8] = [
        "command", "cmd", "path", "file", "pattern", "query", "url", "name",
    ];
    let Some(o) = args.as_object() else {
        return String::new();
    };
    let v = KEYS
        .iter()
        .find_map(|k| o.get(*k).and_then(|v| v.as_str()))
        .or_else(|| o.values().find_map(|v| v.as_str()))
        .unwrap_or("");
    let v = unhandle(v.lines().next().unwrap_or("").trim());
    if v.chars().count() > 40 {
        v.chars().take(39).collect::<String>() + "\u{2026}"
    } else {
        v.to_string()
    }
}

/// PURE: a code-mode file HANDLE — `[README.md#B749]`, the path plus a content hash — read as
/// the path a person would name. Anything else passes through.
pub fn unhandle(v: &str) -> String {
    match v.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
        Some(inner) => inner
            .rsplit_once('#')
            .map(|(p, _)| p)
            .unwrap_or(inner)
            .to_string(),
        None => v.to_string(),
    }
}

/// A tool whose rendering is the CARD its step produces, so its call row is not drawn (D6).
pub fn is_card_call(name: &str) -> bool {
    matches!(name, "draft_message" | "draft_ticket")
}

/// A draft step as a card row; `None` when the body is not a draft's shape.
fn draft_row(step: &Step) -> Option<Row> {
    let kind = step.kind.as_str().strip_prefix("draft/")?.to_string();
    Some(Row::Draft {
        step: step.id.clone(),
        draft: body_str(step, "draft")?,
        audience: body_str(step, "audience").unwrap_or_default(),
        subject: body_str(step, "subject")
            .or_else(|| body_str(step, "title"))
            .unwrap_or_default(),
        body: body_str(step, "body").unwrap_or_default(),
        kind,
    })
}

fn other(step: &Step) -> Row {
    Row::Other {
        step: step.id.clone(),
        kind: step.kind.clone(),
    }
}

fn mail_row(step: &Step) -> Row {
    let from = body_str(step, "from").unwrap_or_default();
    let subject = body_str(step, "subject").unwrap_or_default();
    if from == ANDREY_REF {
        // Andrey's own messages are not "mail from someone": they are the other half of the
        // conversation, and they render as their own row.
        return Row::Andrey {
            step: step.id.clone(),
            text: body_str(step, "summary").unwrap_or(subject),
        };
    }
    let class = match body_str(step, "class").as_deref() {
        Some("wake") => MailClass::Wake,
        _ => MailClass::Ordinary,
    };
    Row::Mail {
        step: step.id.clone(),
        from,
        subject,
        class,
    }
}

fn tool_call_row(step: &Step) -> Option<(ToolCallId, Row)> {
    let call = ToolCallId::new(step.body.get("call")?.as_str()?);
    let name = step.body.get("name")?.as_str()?.to_string();
    let intent = step
        .body
        .get("render")
        .and_then(|v| serde_json::from_value::<RenderIntent>(v.clone()).ok())
        // §9's declared intent is how a surface dispatches; a call that lost it renders generic
        // rather than being guessed at by name.
        .unwrap_or(RenderIntent::Generic);
    let args = step
        .body
        .get("args")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    Some((
        call.clone(),
        Row::Tool {
            call,
            name,
            intent,
            args,
            result: None,
            call_step: step.id.clone(),
            at: step.at,
        },
    ))
}

fn body_str(step: &Step, key: &str) -> Option<String> {
    step.body
        .get(key)
        .and_then(|v| v.as_str())
        .map(String::from)
}

fn body_u32(step: &Step, key: &str) -> u32 {
    step.body
        .get(key)
        .and_then(|v| v.as_u64())
        .unwrap_or(0)
        .min(u32::MAX as u64) as u32
}

/// PURE: the durable text of the trailing [`Row::Text`] of `rows`. Since P5-D14 the flushes of
/// one step index are already ONE row, so this is that row's text — the string the live tail is
/// compared against (P3-D12).
pub fn trailing_durable(rows: &[Row]) -> String {
    rows.iter()
        .rev()
        .find_map(|r| match r {
            Row::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

/// PURE: the row index of the trailing [`Row::Text`], or nothing. At most ONE index since the
/// join (P5-D14): `lines()` chooses between that row's durable text and the live tail, and no
/// longer has to skip earlier flushes of the same step.
pub fn trailing_text_row(rows: &[Row]) -> Option<usize> {
    rows.iter()
        .enumerate()
        .rev()
        .find_map(|(i, r)| matches!(r, Row::Text { .. }).then_some(i))
}

/// The pre-join spelling, kept so the shape of the old call sites stays honest: a `Vec` of at
/// most one index.
pub fn trailing_text_rows(rows: &[Row]) -> Vec<usize> {
    trailing_text_row(rows).into_iter().collect()
}

/// PURE: whether row `i` opens a span of the agent acting, and so wears the agent's name as a
/// label the way Andrey's rows wear `andrey:` (visual audit F2). An agent row after another agent
/// row continues the same span; after anything else — Andrey, mail, a claim card, the end of a
/// turn, the top of the window — it starts one.
pub fn opens_speech(rows: &[Row], i: usize) -> bool {
    let Some(row) = rows.get(i) else {
        return false;
    };
    if !is_agent_row(row) || is_empty_program(row) {
        return false;
    }
    // The previous VISIBLE row: an empty program (drawn as nothing) does not open or continue
    // a span.
    let prev = rows[..i].iter().rev().find(|r| !is_empty_program(r));
    prev.is_none_or(|p| !is_agent_row(p))
}

/// PURE: a row that is the agent acting — its words, its reasoning, its tool calls. A turn that
/// opens with a tool call wears the label on the tool header: the speaker is who ACTS.
pub fn is_agent_row(row: &Row) -> bool {
    matches!(
        row,
        Row::Text { .. }
            | Row::Reasoning { .. }
            | Row::Tool { .. }
            | Row::Program { .. }
            | Row::Draft { .. }
    )
}

/// PURE: the words a [`Row::WakeMark`] shows, in the USER-FACING vocabulary (nit 37).
///
/// The ledger keeps `wake/start` and `wake/end` — REQUIREMENTS §3's step-type map does not move.
/// What moves is the chrome: nobody outside this codebase calls a turn a wake, and the audit's
/// personas read `── wake end · completed` as a machine talking to itself.
pub fn turn_mark_words(phase: &Phase, reason: Option<&str>, cause: Option<&str>) -> String {
    match phase {
        Phase::Start => "turn".to_string(),
        Phase::End => match reason.map(str::trim).filter(|r| !r.is_empty()) {
            // §5 reserves `interrupted` for a PREEMPTED wake and for crash repair: "`interrupted`
            // is the one reason no loop emits" for a user's Esc. What Esc actually produces is
            // `aborted` with `cause: user` — so THAT is the pair the interrupt marker reads from.
            // Rendering it as `turn ended · aborted` is what made B7's marker invisible.
            Some("aborted") if cause.map(str::trim) == Some("user") => {
                "turn interrupted".to_string()
            }
            Some("interrupted") => "turn interrupted".to_string(),
            Some(r) => format!("turn ended \u{b7} {r}"),
            None => "turn ended".to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `ANDREY_REF` is a re-spelling of the sender encoding `agents` owns. The two are pinned
    /// equal here: if the encoding ever changes, Andrey's messages would silently stop rendering
    /// as `Row::Andrey` and lose their accent label, with nothing failing.
    /// The chrome speaks turn/message; the ledger keeps `wake/*`. Both halves pinned here.
    #[test]
    fn turn_marks_never_say_wake() {
        assert_eq!(turn_mark_words(&Phase::Start, None, None), "turn");
        assert_eq!(
            turn_mark_words(&Phase::End, Some("completed"), None),
            "turn ended \u{b7} completed"
        );
        assert_eq!(
            turn_mark_words(&Phase::End, Some("interrupted"), None),
            "turn interrupted"
        );
        assert_eq!(turn_mark_words(&Phase::End, Some("  "), None), "turn ended");
        // B7, the marker Esc actually produces. §5 reserves `interrupted` for a PREEMPTED wake,
        // so a user's Esc lands as `aborted` + `cause: user` — and that pair has to read as an
        // interrupt, or the audit's marker is invisible on the one path a person can take.
        assert_eq!(
            turn_mark_words(&Phase::End, Some("aborted"), Some("user")),
            "turn interrupted"
        );
        // A worker torn down by its spawner is NOT the user interrupting.
        assert_eq!(
            turn_mark_words(&Phase::End, Some("aborted"), Some("parent")),
            "turn ended \u{b7} aborted"
        );
        assert_eq!(
            turn_mark_words(&Phase::End, Some("aborted"), None),
            "turn ended \u{b7} aborted"
        );
        for phase in [Phase::Start, Phase::End] {
            assert!(!turn_mark_words(&phase, Some("done"), None).contains("wake"));
        }
    }

    #[test]
    fn andrey_ref_is_the_spelling_agents_writes() {
        assert_eq!(
            ANDREY_REF,
            bough_plugin_agents::Sender::Andrey.as_ref_str(),
            "the focus pane reads the ledger BODY, so its literal must match what `agents` wrote"
        );
    }
}
