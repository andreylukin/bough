//! Invariant (§4): ambiguous routing becomes a leader QUESTION, never a guess. The question is a
//! `leader/question` step on the unsorted trajectory, routed at [`MailClass::Wake`] with the
//! `class:ask` ref so it reactivates a dormant leader (P5-D3).

use bough_plugin_agents::{MailClass, Sender};
use bough_plugin_ledger::Ref;

use crate::envelope::{Envelope, Question};

/// The ref every leader question carries.
pub const ASK_CLASS_REF: &str = "class:ask";

/// PURE: the envelope a [`Question`] is routed as.
///
/// The question's OWN refs ride along, so a leader that routes on `repo:bough` still receives a
/// `repo:bough` question by ordinary matching; `class:ask` is added on top, which is what makes
/// the delivery able to REACTIVATE a dormant leader rather than merely queue for it.
pub fn envelope_for(q: &Question) -> Envelope {
    let mut refs = q.refs.clone();
    refs.insert(ask_ref());
    Envelope {
        from: Sender::System("mail-router"),
        class: MailClass::Wake,
        subject: format!("question from `{}`", q.asked_by),
        summary: q.about.clone(),
        text: if q.options.is_empty() {
            q.about.clone()
        } else {
            format!("{}\n\n{}", q.about, q.options.join("\n"))
        },
        cites: q.cites.clone(),
        refs,
        at: q.at,
    }
}

/// The `class:ask` ref as a [`Ref`].
pub fn ask_ref() -> Ref {
    Ref::new(ASK_CLASS_REF)
}
