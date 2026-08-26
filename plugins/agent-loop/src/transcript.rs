//! Invariant (P2-D19): the loop builds every request FROM THE LEDGER. This module is the pure
//! fold `steps -> Vec<LlmMessage>`, used by the loop to make a request AND by the invariant to
//! reconstruct it — so "model-visible ⟺ ledgered" is true by construction rather than by
//! discipline, and a side-channel message cannot survive a reconstruction.

use bough_plugin_ledger::{Seq, Step};
use bough_plugin_llm::LlmMessage;

/// Fold a wake's own steps into the messages the model is shown, up to `as_of`.
///
/// Pure: no clock, no ledger handle, no in-memory conversation. WP-4.
pub fn rebuild(_steps: &[Step], _as_of: Option<Seq>) -> Vec<LlmMessage> {
    todo!("WP-4: mail/delivered + thought/* + tool/call + tool/result -> messages, in seq order")
}
