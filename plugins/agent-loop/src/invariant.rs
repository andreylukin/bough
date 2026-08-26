//! §0.2 runtime invariants for `bough-plugin-agent-loop`:
//!
//! 1. **The request the adapter was handed RECONSTRUCTS from the ledger, byte for byte.** This is
//!    §0.2's "model-visible ⟺ ledgered" made checkable: the loop records each request it sent
//!    (bounded, last N wakes) and the check rebuilds it from the wake's own steps and compares.
//!    A side-channel message — anything that reached the model without a step — is exactly what
//!    it catches.
//! 2. **Unconsumed ordinary mail at any `wake_end` implies a scheduled drain wake** (§5's
//!    standing invariant).
//! 3. **Every `wake/start` has a `wake/end`, or is the live one.**
//!
//! P2-D18: the reconstruction evaluator is a PURE FUNCTION here, imported by
//! `agent-loop-scripted` for its own invariant. Two recorders, one evaluator, so the copies
//! cannot drift.

use std::collections::BTreeSet;

use bough_kernel::{Cadence, Context, FiberUid, InvariantSpec, InvariantViolation};
use bough_plugin_ledger::{Ledger, Seq, Step, StepQuery, WakeId};
use bough_plugin_llm::LlmRequest;
use parking_lot::Mutex;

/// How many sent requests are kept. A protocol bound on a debug buffer, not a deployment value.
const RECORD_CAP: usize = 256;

/// One request as it was actually handed to an adapter.
#[derive(Clone, Debug)]
pub struct SentRequest {
    pub fiber: FiberUid,
    pub wake: WakeId,
    pub step_index: u32,
    pub request: LlmRequest,
}

static RECORDED: Mutex<Vec<SentRequest>> = Mutex::new(Vec::new());

/// Record one sent request. Called by the loop, and by `agent-loop-scripted`.
pub fn record(sent: SentRequest) {
    let mut log = RECORDED.lock();
    if log.len() >= RECORD_CAP {
        log.remove(0);
    }
    log.push(sent);
}

/// Forget everything recorded for `fiber` (registered as an inverse by `apply`, so a reload
/// starts clean).
pub fn forget(fiber: FiberUid) {
    RECORDED.lock().retain(|s| s.fiber != fiber);
}

/// Everything recorded so far, oldest first.
pub fn seen() -> Vec<SentRequest> {
    RECORDED.lock().clone()
}

/// THE shared evaluator (P2-D18): rebuild each recorded request from the wake's steps and compare.
///
/// The comparison is over what the model was SHOWN: the messages (byte for byte, through
/// [`crate::transcript::rebuild`]) and the system prefix (through the `projection_digest` anchor
/// the wake's newest `request/header` carries). A message that reached the model without a step
/// cannot be rebuilt, so it is reported.
pub fn evaluate_reconstruction(sent: &[SentRequest], steps: &[Step]) -> Result<(), String> {
    for s in sent {
        let claimed = claimed_ranges(steps, &s.wake);
        let of_wake = crate::transcript::steps_for_wake(steps, &s.wake, &claimed);
        if of_wake.is_empty() {
            // A recorded request whose wake left no steps at all is itself the violation: the
            // request was model-visible and the ledger has nothing to rebuild it from.
            return Err(format!(
                "wake {} step {} sent a request but appended no steps",
                s.wake, s.step_index
            ));
        }
        let as_of = step_start_seq(&of_wake, s.step_index);
        let rebuilt = crate::transcript::rebuild(&of_wake, as_of);
        if rebuilt != s.request.messages {
            return Err(format!(
                "wake {} step {}: the request does not reconstruct from the ledger\n  sent:      {:?}\n  rebuilt:   {:?}",
                s.wake, s.step_index, s.request.messages, rebuilt
            ));
        }
        if let Some(expected) = projection_digest(&of_wake, s.step_index) {
            let actual = crate::request::digest(s.request.system.as_deref().unwrap_or(""));
            if actual != expected {
                return Err(format!(
                    "wake {} step {}: the system prefix does not match the request/header's projection_digest",
                    s.wake, s.step_index
                ));
            }
        }
    }
    Ok(())
}

/// The claimed seq ranges a wake recorded on its `wake/start`.
fn claimed_ranges(steps: &[Step], wake: &WakeId) -> Vec<bough_plugin_ledger::SeqRange> {
    steps
        .iter()
        .filter(|s| &s.wake == wake && s.kind.as_str() == "wake/start")
        .filter_map(|s| s.body.get("claimed").cloned())
        .filter_map(|v| serde_json::from_value::<Vec<bough_plugin_ledger::SeqRange>>(v).ok())
        .flatten()
        .collect()
}

/// The seq the request of `step_index` was assembled at: its `step/start`. Everything the model
/// saw was already committed by then, and everything after it is that step's own output.
fn step_start_seq(of_wake: &[Step], step_index: u32) -> Option<Seq> {
    of_wake
        .iter()
        .filter(|s| s.kind.as_str() == "step/start")
        .filter(|s| s.body.get("index").and_then(|v| v.as_u64()) == Some(step_index as u64))
        .map(|s| s.seq)
        .next_back()
}

/// The `projection_digest` of the header appended FOR THIS STEP, if one was.
///
/// §5 appends a header only when it changes, so a later step usually has none — and the previous
/// step's digest describes a projection that has since grown. The anchor is therefore read only
/// when it belongs to this step, and a step without one is checked on its messages alone (its
/// system prefix is still reproducible, from the newest header's `as_of` plus the assembler).
fn projection_digest(of_wake: &[Step], step_index: u32) -> Option<String> {
    let start = of_wake
        .iter()
        .filter(|s| s.kind.as_str() == "step/start")
        .filter(|s| s.body.get("index").and_then(|v| v.as_u64()) == Some(step_index as u64))
        .map(|s| s.seq)
        .next_back()?;
    let next_start = of_wake
        .iter()
        .filter(|s| s.kind.as_str() == "step/start" && s.seq > start)
        .map(|s| s.seq)
        .next();
    of_wake
        .iter()
        .filter(|s| s.kind.as_str() == "request/header")
        .filter(|s| s.seq > start && next_start.map(|n| s.seq < n).unwrap_or(true))
        .filter_map(|s| {
            s.body
                .get("projection_digest")
                .and_then(|v| v.as_str())
                .map(String::from)
        })
        .next_back()
}

/// The standing mail invariant, as a pure function (§5).
pub fn evaluate_mail(steps: &[Step], drain_scheduled: bool) -> Result<(), String> {
    let consumed = crate::mail::consumed_union(steps);
    let unconsumed = crate::mail::unconsumed(steps, &consumed);
    let ordinary = unconsumed
        .iter()
        .filter(|s| crate::mail::is_ordinary(s))
        .count();
    if crate::mail::standing_invariant_holds(ordinary, drain_scheduled) {
        Ok(())
    } else {
        Err(format!(
            "{ordinary} unconsumed ordinary mail step(s) and no drain wake scheduled"
        ))
    }
}

/// Every `wake/start` closed, or is the live one.
pub fn evaluate_wake_pairing(steps: &[Step], live: Option<&WakeId>) -> Result<(), String> {
    let started: BTreeSet<&str> = steps
        .iter()
        .filter(|s| s.kind.as_str() == "wake/start")
        .map(|s| s.wake.as_str())
        .collect();
    let ended: BTreeSet<&str> = steps
        .iter()
        .filter(|s| s.kind.as_str() == "wake/end")
        .map(|s| s.wake.as_str())
        .collect();
    let open: Vec<&str> = started
        .difference(&ended)
        .copied()
        .filter(|w| live.map(|l| l.as_str() != *w).unwrap_or(true))
        .collect();
    if open.is_empty() {
        Ok(())
    } else {
        Err(format!("wake(s) opened and never closed: {open:?}"))
    }
}

/// The specs `AgentLoopPlugin::invariants` returns.
pub fn specs() -> Vec<InvariantSpec> {
    vec![
        InvariantSpec {
            name: "every_request_reconstructs_from_the_ledger",
            plugin: crate::PLUGIN_NAME,
            cadence: Cadence::OnQuiesce,
            check: |ctx| Box::pin(check_reconstruction(ctx)),
        },
        InvariantSpec {
            name: "unconsumed_ordinary_mail_implies_a_scheduled_drain_wake",
            plugin: crate::PLUGIN_NAME,
            cadence: Cadence::OnQuiesce,
            check: |ctx| Box::pin(check_mail(ctx)),
        },
    ]
}

/// Every step in the store, oldest first. The checks are bounded by the recorded requests, not by
/// the ledger, so this is a read of a test-sized tree in the dev/test profiles the runner runs in.
async fn all_steps(ctx: &Context) -> Result<Vec<Step>, String> {
    let ledger = ctx
        .try_get::<Ledger>()
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "no ledger bound".to_string())?;
    ledger
        .0
        .steps(&StepQuery::default())
        .await
        .map_err(|e| e.to_string())
}

fn violation(name: &'static str, ctx: &Context, detail: String) -> InvariantViolation {
    InvariantViolation {
        invariant: name,
        plugin: crate::PLUGIN_NAME,
        entry: ctx.entry_id().clone(),
        detail,
    }
}

async fn check_reconstruction(ctx: Context) -> Result<(), InvariantViolation> {
    // Partition by fiber (the way `agents` and `tools` do): the recorder is a process-wide static
    // and a second kernel in the same test binary would otherwise inherit the first tree's
    // requests and check them against a store that never held them.
    let mine = ctx.fiber_uid();
    let sent: Vec<SentRequest> = seen().into_iter().filter(|s| s.fiber == mine).collect();
    if sent.is_empty() {
        return Ok(());
    }
    let steps = all_steps(&ctx)
        .await
        .map_err(|d| violation("every_request_reconstructs_from_the_ledger", &ctx, d))?;
    evaluate_reconstruction(&sent, &steps)
        .map_err(|d| violation("every_request_reconstructs_from_the_ledger", &ctx, d))
}

async fn check_mail(ctx: Context) -> Result<(), InvariantViolation> {
    let steps = all_steps(&ctx).await.map_err(|d| {
        violation(
            "unconsumed_ordinary_mail_implies_a_scheduled_drain_wake",
            &ctx,
            d,
        )
    })?;
    // At quiesce nothing is in flight, so a drain wake being "scheduled" is the driver's live
    // state: `crate::driver::any_drain_scheduled()` reads it across the live drivers.
    evaluate_mail(&steps, crate::driver::any_drain_scheduled()).map_err(|d| {
        violation(
            "unconsumed_ordinary_mail_implies_a_scheduled_drain_wake",
            &ctx,
            d,
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{delivered, step, wake_end, wake_of};
    use bough_plugin_llm::{CallConfig, LlmContentBlock, LlmMessage, LlmRole};

    fn call() -> CallConfig {
        CallConfig {
            model: "haiku".into(),
            max_tokens: 8,
            effort: None,
            tool_choice_none: false,
            meta: Default::default(),
        }
    }

    fn wake_with_one_step() -> (WakeId, Vec<Step>) {
        let w = wake_of("w1");
        let steps = vec![
            step(1, &w, "wake/start", serde_json::json!({ "claimed": [] })),
            step(
                2,
                &w,
                "mail/delivered",
                serde_json::json!({ "class": "wake", "from": "andrey", "subject": "s", "summary": "do it" }),
            ),
            step(3, &w, "step/start", serde_json::json!({ "index": 0 })),
        ];
        (w, steps)
    }

    fn sent_for(w: &WakeId, steps: &[Step]) -> SentRequest {
        let messages = crate::transcript::rebuild(steps, Some(Seq(3)));
        SentRequest {
            fiber: FiberUid(1),
            wake: w.clone(),
            step_index: 0,
            request: LlmRequest {
                model: "haiku".into(),
                system: None,
                system_volatile: None,
                messages,
                tools: vec![],
                call: call(),
            },
        }
    }

    #[test]
    fn a_matching_pair_is_clean() {
        let (w, steps) = wake_with_one_step();
        let sent = sent_for(&w, &steps);
        evaluate_reconstruction(&[sent], &steps).expect("a request built from the ledger rebuilds");
    }

    #[test]
    fn a_digest_mismatch_is_a_violation() {
        let (w, mut steps) = wake_with_one_step();
        let sent = sent_for(&w, &steps);
        steps.push(step(
            4,
            &w,
            "request/header",
            serde_json::json!({ "prompt_ver": "p", "sections": [], "tools": [], "call": {},
                                "composition": "c", "projection_digest": "deadbeef" }),
        ));
        let err = evaluate_reconstruction(&[sent], &steps)
            .expect_err("a system prefix that is not the ledgered projection is a violation");
        assert!(err.contains("projection_digest"), "{err}");
    }

    #[test]
    fn a_side_channel_message_is_a_violation() {
        let (w, steps) = wake_with_one_step();
        let mut sent = sent_for(&w, &steps);
        // Exactly what a `llm/stream` listener appending to `messages` would do.
        sent.request.messages.push(LlmMessage {
            role: LlmRole::User,
            content: vec![LlmContentBlock::Text {
                text: "planted".into(),
            }],
        });
        let err = evaluate_reconstruction(&[sent], &steps).expect_err("a side channel is caught");
        assert!(err.contains("does not reconstruct"), "{err}");
    }

    #[test]
    fn unconsumed_ordinary_mail_without_a_drain_is_a_violation() {
        let w = wake_of("w1");
        let steps = vec![
            delivered(1, &w, "ordinary", "a push"),
            wake_end(2, &w, "completed", &[]),
        ];
        assert!(evaluate_mail(&steps, true).is_ok(), "a drain is scheduled");
        let err = evaluate_mail(&steps, false).expect_err("no drain scheduled");
        assert!(err.contains("unconsumed ordinary mail"), "{err}");
        // Consumed mail is not owed a drain.
        let consumed = vec![
            delivered(1, &w, "ordinary", "a push"),
            wake_end(2, &w, "completed", &[(1, 1)]),
        ];
        assert!(evaluate_mail(&consumed, false).is_ok());
    }

    #[test]
    fn an_unclosed_wake_is_reported_unless_it_is_the_live_one() {
        let w = wake_of("w1");
        let steps = vec![step(1, &w, "wake/start", serde_json::json!({}))];
        assert!(evaluate_wake_pairing(&steps, None).is_err());
        assert!(evaluate_wake_pairing(&steps, Some(&w)).is_ok());
    }
}
