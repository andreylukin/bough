//! Invariant: the projection of a trajectory into rows is PURE and TOTAL. `tool/call` and
//! `tool/result` fold into ONE row by call id; envelope steps fold into their neighbours or are
//! dropped; and an UNKNOWN step type renders as `Other` and never panics — the step-type map is
//! merge-extensible (§3), so a renderer will meet types it does not own.

use std::collections::BTreeMap;

use bough_plugin_agents::Phase;
use bough_plugin_ledger::vocabulary::MailClass;
use bough_plugin_ledger::{Step, StepId, StepType, WakeId};
use bough_plugin_llm::ToolCallId;
use bough_plugin_tools::{RenderIntent, ToolResultBody};
use bough_plugin_tui_render::{about_from_step, AboutView};

/// The `from` ref Andrey's own mail carries (`Sender::Andrey::as_ref_str`). Spelled here rather
/// than imported as a constant because `agents` exposes it only through the enum, and the focus
/// pane reads the LEDGER BODY, not a live `Message`.
pub const ANDREY_REF: &str = "andrey";

/// Step types that carry the machinery of a wake rather than anything Andrey reads. They are
/// dropped: `step/start` and `step/end` bracket rows that are already shown in order,
/// `request/header` is the reconstruction anchor, and `inbox/spliced` is the durable twin of the
/// `mail/delivered` row right next to it (rendering both would show one message twice).
pub const ENVELOPE: &[&str] = &[
    "step/start",
    "step/end",
    "request/header",
    "inbox/spliced",
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
    },
    WakeMark {
        step: StepId,
        wake: WakeId,
        phase: Phase,
        reason: Option<String>,
    },
    About {
        step: StepId,
        view: AboutView,
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
            | Row::Claim { step, .. }
            | Row::Other { step, .. } => step,
            Row::Tool { call_step, .. } => call_step,
        }
    }

    /// Every durable step folded into this row, oldest first. One step for every row except a
    /// joined `Text`/`Reasoning` group (P5-D14).
    pub fn parts(&self) -> Vec<StepId> {
        match self {
            Row::Text { parts, .. } | Row::Reasoning { parts, .. } => parts.clone(),
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
    // P5-D14, the field bug: the durable steps of ONE model step are a SPLIT of one stream, so
    // consecutive `thought/text` (or `thought/reasoning`) steps sharing `(wake, step_index)` are
    // ONE row. `None` means the last row pushed cannot be joined onto — anything else in between
    // (a tool call, a wake mark, a new step index, a new wake) breaks the group.
    let mut open_group: Option<(usize, GroupKey)> = None;

    for step in steps {
        let kind = step.kind.as_str();
        if ENVELOPE.contains(&kind) {
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
            "tool/call" => match tool_call_row(step) {
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
                    Some(body) => match by_call.get(&body.call).copied() {
                        // The fold: one row for the pair.
                        Some(at) => {
                            if let Row::Tool { result, .. } = &mut out[at] {
                                *result = Some(body);
                            }
                        }
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
                            });
                        }
                    },
                    None => out.push(other(step)),
                }
            }
            "wake/start" => out.push(Row::WakeMark {
                step: step.id.clone(),
                wake: step.wake.clone(),
                phase: Phase::Start,
                reason: body_str(step, "urgency"),
            }),
            "wake/end" => out.push(Row::WakeMark {
                step: step.id.clone(),
                wake: step.wake.clone(),
                phase: Phase::End,
                reason: body_str(step, "reason"),
            }),
            _ => match about_from_step(step) {
                // `about/line` BY NAME (P3-D11): the pane does not depend on the row that writes it.
                Some(view) => out.push(Row::About {
                    step: step.id.clone(),
                    view,
                }),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `ANDREY_REF` is a re-spelling of the sender encoding `agents` owns. The two are pinned
    /// equal here: if the encoding ever changes, Andrey's messages would silently stop rendering
    /// as `Row::Andrey` and lose their accent label, with nothing failing.
    #[test]
    fn andrey_ref_is_the_spelling_agents_writes() {
        assert_eq!(
            ANDREY_REF,
            bough_plugin_agents::Sender::Andrey.as_ref_str(),
            "the focus pane reads the ledger BODY, so its literal must match what `agents` wrote"
        );
    }
}
