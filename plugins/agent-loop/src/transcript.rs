//! Invariant (P2-D19): the loop builds every request FROM THE LEDGER. This module is the pure
//! fold `steps -> Vec<LlmMessage>`, used by the loop to make a request AND by the invariant to
//! reconstruct it — so "model-visible ⟺ ledgered" is true by construction rather than by
//! discipline, and a side-channel message cannot survive a reconstruction.

use bough_plugin_ledger::{SeqRange, Step, WakeId};
use bough_plugin_llm::{LlmContentBlock, LlmMessage, LlmRole};

/// The step kinds this fold reads. Anything else in the wake (`wake/start`, `step/start`,
/// `request/header`, `inbox/spliced`, …) is bookkeeping and is deliberately invisible to the
/// model: it carries no model-visible content, so including it would make the reconstruction
/// disagree with what was sent.
const MAIL: &str = "mail/delivered";
const TEXT: &str = "thought/text";
const REASONING: &str = "thought/reasoning";
const TOOL_CALL: &str = "tool/call";
const TOOL_RESULT: &str = "tool/result";
const JOT: &str = "wake/jot";
/// The grace step's instruction. Model-visible, therefore ledgered, therefore folded here.
const GRACE: &str = "wake/grace-prompt";

/// The steps one wake's request is built from, in seq order: the wake's OWN steps plus the
/// delivered mail its `wake/start` claimed (mail is delivered under whatever wake delivered it,
/// so a claim is the only durable link between a wake and the mail it answers).
///
/// Pure over `all`: the caller does the reading.
pub fn steps_for_wake(all: &[Step], wake: &WakeId, claimed: &[SeqRange]) -> Vec<Step> {
    let mut out: Vec<Step> = all
        .iter()
        .filter(|s| &s.wake == wake || claimed.iter().any(|r| r.contains(s.seq)))
        .cloned()
        .collect();
    out.sort_by_key(|s| s.seq);
    out
}

/// Fold a wake's own steps into the messages the model is shown, up to `as_of`.
///
/// Pure: no clock, no ledger handle, no in-memory conversation. Consecutive blocks of one role
/// coalesce into one message, which is what an adapter would do anyway and what makes the fold
/// canonical (two different flush cadences of the same text still produce two `thought/text`
/// steps, and therefore two blocks — the coalescing is over ROLE, never over content).
pub fn rebuild(steps: &[Step], as_of: Option<bough_plugin_ledger::Seq>) -> Vec<LlmMessage> {
    let mut blocks: Vec<(LlmRole, LlmContentBlock)> = Vec::new();
    let mut ordered: Vec<&Step> = steps
        .iter()
        .filter(|s| as_of.map(|c| s.seq <= c).unwrap_or(true))
        .collect();
    ordered.sort_by_key(|s| s.seq);

    for step in ordered {
        let body = step.body.as_ref();
        match step.kind.as_str() {
            MAIL => {
                let subject = str_field(body, "subject");
                let summary = str_field(body, "summary");
                let from = str_field(body, "from");
                let text = if subject.is_empty() {
                    format!("[mail from {from}]\n{summary}")
                } else {
                    format!("[mail from {from}] {subject}\n{summary}")
                };
                blocks.push((LlmRole::User, LlmContentBlock::Text { text }));
            }
            JOT => {
                let state = str_field(body, "state");
                let hint = str_field(body, "resume_hint");
                blocks.push((
                    LlmRole::User,
                    LlmContentBlock::Text {
                        text: format!("[checkpoint] {state}\n[resume] {hint}"),
                    },
                ));
            }
            GRACE => blocks.push((
                LlmRole::User,
                LlmContentBlock::Text {
                    text: str_field(body, "text"),
                },
            )),
            TEXT => blocks.push((
                LlmRole::Assistant,
                LlmContentBlock::Text {
                    text: str_field(body, "text"),
                },
            )),
            REASONING => blocks.push((
                LlmRole::Assistant,
                LlmContentBlock::Reasoning {
                    text: str_field(body, "text"),
                    meta: body.get("meta").filter(|v| !v.is_null()).cloned(),
                },
            )),
            TOOL_CALL => blocks.push((
                LlmRole::Assistant,
                LlmContentBlock::ToolUse {
                    id: str_field(body, "call"),
                    name: str_field(body, "name"),
                    input: body.get("args").cloned().unwrap_or(serde_json::Value::Null),
                },
            )),
            TOOL_RESULT => {
                let outcome = str_field(body, "outcome");
                blocks.push((
                    LlmRole::User,
                    LlmContentBlock::ToolResult {
                        tool_use_id: str_field(body, "call"),
                        content: str_field(body, "content"),
                        is_error: outcome != "ok",
                    },
                ));
            }
            _ => {}
        }
    }

    let mut out: Vec<LlmMessage> = Vec::new();
    for (role, block) in blocks {
        match out.last_mut() {
            Some(m) if m.role == role => m.content.push(block),
            _ => out.push(LlmMessage {
                role,
                content: vec![block],
            }),
        }
    }
    out
}

fn str_field(body: &serde_json::Value, key: &str) -> String {
    body.get(key)
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{step, wake_of};
    use bough_plugin_ledger::Seq;

    #[test]
    fn a_wake_folds_into_alternating_messages_in_seq_order() {
        let w = wake_of("w1");
        let steps = vec![
            step(1, &w, "wake/start", serde_json::json!({})),
            step(
                2,
                &w,
                "mail/delivered",
                serde_json::json!({ "from": "andrey", "subject": "s", "summary": "do it" }),
            ),
            step(3, &w, "step/start", serde_json::json!({ "index": 0 })),
            step(
                4,
                &w,
                "thought/text",
                serde_json::json!({ "text": "on it", "step_index": 0 }),
            ),
            step(
                5,
                &w,
                "tool/call",
                serde_json::json!({ "call": "c1", "name": "bash", "args": { "cmd": "ls" } }),
            ),
            step(
                6,
                &w,
                "tool/result",
                serde_json::json!({ "call": "c1", "name": "bash", "outcome": "ok", "content": "a\nb" }),
            ),
        ];
        let msgs = rebuild(&steps, None);
        assert_eq!(msgs.len(), 3, "user, assistant, user: {msgs:?}");
        assert_eq!(msgs[0].role, LlmRole::User);
        assert_eq!(msgs[1].role, LlmRole::Assistant);
        // The assistant turn coalesces its text and its tool call into ONE message.
        assert_eq!(msgs[1].content.len(), 2);
        assert!(matches!(
            msgs[2].content[0],
            LlmContentBlock::ToolResult {
                is_error: false,
                ..
            }
        ));
        // Bookkeeping steps contribute nothing model-visible.
        assert!(!format!("{msgs:?}").contains("wake/start"));
    }

    #[test]
    fn as_of_cuts_the_fold_at_a_seq() {
        let w = wake_of("w1");
        let steps = vec![
            step(
                1,
                &w,
                "thought/text",
                serde_json::json!({ "text": "one", "step_index": 0 }),
            ),
            step(
                2,
                &w,
                "thought/text",
                serde_json::json!({ "text": "two", "step_index": 1 }),
            ),
        ];
        let cut = rebuild(&steps, Some(Seq(1)));
        assert_eq!(cut.len(), 1);
        assert_eq!(cut[0].content.len(), 1);
        assert_eq!(rebuild(&steps, Some(Seq(2)))[0].content.len(), 2);
    }

    #[test]
    fn a_wakes_steps_are_its_own_plus_the_mail_it_claimed() {
        let mine = wake_of("w2");
        let elsewhere = wake_of("w0");
        let all = vec![
            step(
                7,
                &elsewhere,
                "mail/delivered",
                serde_json::json!({ "from": "gh", "subject": "ci", "summary": "red" }),
            ),
            step(
                8,
                &elsewhere,
                "mail/delivered",
                serde_json::json!({ "from": "gh", "subject": "other", "summary": "ignore me" }),
            ),
            step(
                9,
                &mine,
                "thought/text",
                serde_json::json!({ "text": "hi", "step_index": 0 }),
            ),
        ];
        let claimed = vec![SeqRange {
            from: Seq(7),
            to: Seq(7),
        }];
        let picked = steps_for_wake(&all, &mine, &claimed);
        assert_eq!(picked.len(), 2, "the claimed mail and my own step");
        let text = format!("{:?}", rebuild(&picked, None));
        assert!(text.contains("red"));
        assert!(!text.contains("ignore me"), "unclaimed mail stays out");
    }
}
