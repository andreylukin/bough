//! Invariant: this module IS §5's wake flow, in the drawn order, and nothing else in the tree
//! runs a wake. Every numbered step below is a durable ledger append or a named dispatch; a
//! plugin failure ends the WAKE and not the loop.
//!
//! ```text
//!  1. urgency decides the wake            9. llm.stream(..) through `llm/stream`
//!  2. append `wake/start`                10. tools.execute(..) — the three-stage pipeline
//!  3. cell.claim(..)                     11. append `step/end`
//!  4. waterfall `agent/pre-step`         12. on failure: waterfall `agent/request-error`
//!  5. append `step/start`                13. another step, if tools or next-step input owe one
//!  6. projection.assemble + transcript   14. serial `agent/wake-stopping`, then re-read the inbox
//!  7. append `request/header` on change  15. append `wake/end`
//!  8. waterfall `agent/request`          16. completed only: parallel `agent/wake-end`
//!                                        17. the standing invariant
//! ```

use bough_plugin_agents::AgentCell;
use bough_plugin_ledger::vocabulary::WakeEndReason;
use bough_plugin_ledger::{SeqRange, WakeId};
use bough_plugin_llm::WakeKind;

use crate::LoopConfig;

/// Why one wake is running and what it claimed.
#[derive(Clone, Debug)]
pub struct WakeSpec {
    pub wake: WakeId,
    pub kind: WakeKind,
    pub urgency: crate::mail::Urgency,
    /// The message whose arrival triggered it, if any.
    pub trigger: Option<bough_plugin_agents::MessageId>,
}

/// How one wake ended.
#[derive(Clone, Debug, PartialEq)]
pub struct WakeEnded {
    pub reason: WakeEndReason,
    /// Set when `reason` is `aborted`.
    pub cause: Option<bough_plugin_agents::CancelCause>,
    /// The consumed seqs, which the union at §5 is taken over.
    pub consumed: Vec<SeqRange>,
    pub steps: u32,
}

/// Run one wake, start to finish. The whole of §5's diagram lives here.
///
/// WP-4.
pub async fn run_wake(_cell: &AgentCell, _spec: WakeSpec, _cfg: &LoopConfig) -> WakeEnded {
    todo!("WP-4: the seventeen steps, in order")
}
