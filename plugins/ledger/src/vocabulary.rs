//! Invariant: model-visible ⟺ ledgered (§0.2, §3) — every model-visible input has a step TYPE
//! here, with a schema. These sixteen bodies are the Phase 1 vocabulary; `wake/*`, `step/*`,
//! `request/header`, `inbox/spliced`, `mail/delivered`, `rollup/sealed`, `pin/*`, `claim/*` and
//! `action/*` are STEP TYPES, not events (§2.6). Phase 1 writes them from tests only; `agent-loop`
//! writes them for real in Phase 2.

use crate::id::{ActionId, IdemKey, Ref, RollupId, Seq, StepId, TrajId};
use crate::step::SeqRange;

/// Why a wake was scheduled.
#[derive(
    Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum Urgency {
    Immediate,
    Coalesced,
    Scheduled,
    Catchup,
}

/// How a wake ended.
#[derive(
    Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum WakeEndReason {
    Completed,
    Aborted,
    Error,
    MaxTokens,
    Interrupted,
}

/// `wake/start` — Thought.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct WakeStart {
    pub urgency: Urgency,
    /// The step whose arrival triggered this wake, if any.
    #[serde(default)]
    pub trigger: Option<StepId>,
    /// Seq ranges this wake claimed before running.
    #[serde(default)]
    pub claimed: Vec<SeqRange>,
}

/// `wake/end` — Thought. `consumed` is the union §5 unions order-independently.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct WakeEnd {
    pub reason: WakeEndReason,
    #[serde(default)]
    pub cause: Option<String>,
    #[serde(default)]
    pub consumed: Vec<SeqRange>,
}

/// `step/start` — Thought.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct StepStart {
    pub index: u32,
}

/// Outcome of one step within a wake.
#[derive(
    Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum StepOutcome {
    Ok,
    Error,
    /// §2.9 / P2-D15: the step was cut short by a joining Andrey message before its first
    /// streamed token and will be re-run. The honest outcome of a restart, so a reader can tell
    /// it from a failure.
    Restarted,
}

/// `step/end` — Thought.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct StepEnd {
    pub index: u32,
    pub outcome: StepOutcome,
    #[serde(default)]
    pub detail: Option<String>,
}

/// `request/header` — Thought. What the model was actually shown, so a request is reconstructible.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct RequestHeader {
    pub prompt_ver: String,
    /// The ledger high-water the projection was assembled at (P2-D20). Re-assembling with
    /// `as_of = this` reproduces the exact bytes the model was shown, however much has been
    /// appended since.
    pub as_of: Seq,
    /// The token budget the projection was assembled under.
    pub budget: usize,
    /// sha256 of `Assembled::to_text()`, hex.
    pub projection_digest: String,
    /// The projection sections that made up the context, in rendered order.
    pub sections: Vec<String>,
    /// The step of the wake this header describes. Not part of the change comparison (it moves
    /// every step); it is the join key a consumer needs to say WHICH request the header is for.
    #[serde(default)]
    pub step_index: u32,
    /// The tool NAMES offered, in order.
    pub tools: Vec<String>,
    /// sha256 of the canonical JSON of the tool DEFINITIONS, hex. §5 says the header records the
    /// tool schemas; names alone missed a scoped tool shadowing its same-named global twin with
    /// a different schema, and the reconstruction anchor was then incomplete.
    #[serde(default)]
    pub tools_digest: String,
    pub call: serde_json::Value,
    pub composition: String,
}

/// What an inbox splice does to the queue.
#[derive(
    Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum SpliceOp {
    Insert,
    Claim,
    Discard,
}

/// Where a splice lands.
#[derive(
    Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum SpliceTarget {
    NextWake,
    NextStep,
}

/// `inbox/spliced` — Thought.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct InboxSpliced {
    pub message: String,
    pub op: SpliceOp,
    pub target: SpliceTarget,
    /// Whether the splice wakes the agent.
    pub wake: bool,
}

/// Whether a piece of mail is itself a wake reason.
#[derive(
    Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum MailClass {
    Wake,
    Ordinary,
}

/// `mail/delivered` — EVIDENCE, so it must carry cites.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct MailDelivered {
    pub class: MailClass,
    pub from: Ref,
    pub subject: String,
    pub summary: String,
    #[serde(default)]
    pub refs: Vec<Ref>,
}

/// `rollup/sealed` — EVIDENCE. Phase 4 writes these; Phase 1 plants them in tests.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct RollupSealed {
    pub rollup: RollupId,
    pub kind: crate::rows::RollupKind,
    pub tier: u8,
    pub from_seq: Seq,
    pub to_seq: Seq,
    pub prompt_ver: String,
}

/// `pin/set` — Either. A pin rides every projection verbatim, regardless of age (§5).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct PinSet {
    pub title: String,
    pub text: String,
    /// Pins this one replaces. The relief valve §3 names.
    #[serde(default)]
    pub supersedes: Vec<StepId>,
}

/// `pin/retire` — Thought. Withdrawal with no replacement (P1-D4).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct PinRetire {
    pub retires: Vec<StepId>,
    pub reason: String,
}

/// `claim/proposed` — Thought.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ClaimProposed {
    pub claim: String,
    pub kind: String,
    pub title: String,
    pub body: String,
}

/// `claim/accepted` — EVIDENCE.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ClaimAccepted {
    pub claim: String,
    pub proposal: StepId,
    pub edited: bool,
}

/// `claim/rejected` — Thought.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ClaimRejected {
    pub claim: String,
    pub proposal: StepId,
    pub reason: String,
}

/// `action/intent` — Thought. Storage only in Phase 1 (P1-D11).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ActionIntent {
    pub action: ActionId,
    pub idem_key: IdemKey,
    pub kind: String,
    pub target: String,
    pub payload_digest: String,
}

/// How an action finished.
#[derive(
    Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum ActionOutcome {
    Done,
    Failed,
}

/// `action/done` — EVIDENCE.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ActionDone {
    pub action: ActionId,
    pub status: ActionOutcome,
    #[serde(default)]
    pub artifact: Option<String>,
}

/// `fork/end-seed` — Thought. The child trajectory's first live step (§3's fork rule).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ForkEndSeed {
    pub parent: TrajId,
    pub at_seq: Seq,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// §2.7 item 1: the three new `RequestHeader` fields are what make V4 a reconstruction rather
    /// than a hash comparison, so they must survive a round trip intact.
    #[test]
    fn request_header_round_trips_its_reconstruction_anchor() {
        let h = RequestHeader {
            prompt_ver: "p1".into(),
            as_of: Seq(42),
            budget: 8000,
            projection_digest: "abc123".into(),
            sections: vec!["identity".into(), "tail".into()],
            step_index: 2,
            tools: vec!["bash".into()],
            tools_digest: "d00d".into(),
            call: serde_json::json!({ "model": "claude-haiku-4-5-20251001" }),
            composition: "fp".into(),
        };
        let text = serde_json::to_string(&h).expect("serialises");
        assert_eq!(
            serde_json::from_str::<RequestHeader>(&text).expect("parses"),
            h
        );
    }

    /// The addition is to a BODY, not to the envelope: `LEDGER_FORMAT_VERSION` and
    /// `envelope_fingerprint()` must not move (§3, §2.7 item 1).
    #[test]
    fn the_new_fields_do_not_move_the_envelope() {
        assert_eq!(crate::id::LEDGER_FORMAT_VERSION, 1);
        assert_eq!(
            crate::id::envelope_fingerprint(),
            "824283423bd318f3864d3c9af1446268652aad0886c8e8938c92b8b7ccd89f92"
        );
    }

    /// §2.7 item 2: `restarted` is a spelling of its own, distinguishable from `error`.
    #[test]
    fn a_restarted_step_is_its_own_outcome() {
        assert_eq!(
            serde_json::to_string(&StepOutcome::Restarted).unwrap(),
            "\"restarted\""
        );
        assert_ne!(StepOutcome::Restarted, StepOutcome::Error);
    }
}
