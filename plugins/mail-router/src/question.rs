//! Invariant (§4): ambiguous routing becomes a leader QUESTION, never a guess. The question is a
//! `leader/question` step on the unsorted trajectory, routed at [`MailClass::Wake`] with the
//! `class:ask` ref so it reactivates a dormant leader (P5-D3).

use bough_plugin_ledger::Ref;

use crate::envelope::{Envelope, Question};

/// The ref every leader question carries.
pub const ASK_CLASS_REF: &str = "class:ask";

/// PURE: the envelope a [`Question`] is routed as.
pub fn envelope_for(_q: &Question) -> Envelope {
    todo!("WP-1: wake-class envelope carrying `class:ask` plus the question's own refs")
}

/// The `class:ask` ref as a [`Ref`].
pub fn ask_ref() -> Ref {
    Ref::new(ASK_CLASS_REF)
}
