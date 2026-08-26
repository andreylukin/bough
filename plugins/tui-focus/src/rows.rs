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
        step: StepId,
        wake: WakeId,
        index: u32,
        text: String,
    },
    Reasoning {
        step: StepId,
        text: String,
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
            | Row::Other { step, .. } => step,
            Row::Tool { call_step, .. } => call_step,
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

    for step in steps {
        let kind = step.kind.as_str();
        if ENVELOPE.contains(&kind) {
            continue;
        }
        match kind {
            "mail/delivered" => out.push(mail_row(step)),
            "thought/text" => out.push(match body_str(step, "text") {
                Some(text) => Row::Text {
                    step: step.id.clone(),
                    wake: step.wake.clone(),
                    index: body_u32(step, "step_index"),
                    text,
                },
                None => other(step),
            }),
            "thought/reasoning" => out.push(match body_str(step, "text") {
                Some(text) => Row::Reasoning {
                    step: step.id.clone(),
                    text,
                },
                None => other(step),
            }),
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

/// PURE: the durable text of the trailing step of `rows` — every `thought/text` row of the LAST
/// step index, concatenated in order. That is the string the live tail is compared against
/// (P3-D12), because `flush_text` drains its accumulator and the flushes concatenate.
pub fn trailing_durable(rows: &[Row]) -> String {
    let Some((wake, index)) = rows.iter().rev().find_map(|r| match r {
        Row::Text { wake, index, .. } => Some((wake.clone(), *index)),
        _ => None,
    }) else {
        return String::new();
    };
    rows.iter()
        .filter_map(|r| match r {
            Row::Text {
                wake: w,
                index: i,
                text,
                ..
            } if *w == wake && *i == index => Some(text.as_str()),
            _ => None,
        })
        .collect()
}
