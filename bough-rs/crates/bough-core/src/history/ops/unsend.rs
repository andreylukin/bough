//! The take-back (port of `src/history/unsend.ts`): dropping a message the
//! user retracts seconds after sending it, IN PLACE, on the conversation they
//! sent it in.
//!
//! WHY THIS IS NOT A FORK. Every other operation in this directory branches,
//! and that is right for every one of them: an edit to a turn from ten minutes
//! ago is a second attempt at a piece of work. Escape within the take-back
//! window is not that. It is the user saying the message never should have
//! left — a typo, the wrong conversation — and answering it with a branch
//! produced a sibling session, a `⑂` in the tree, and a conversation the user
//! has to learn to ignore, for a message that existed for three seconds.
//!
//! SO THE RULES ARE NARROW, and they are what makes deleting rows defensible
//! in a system whose whole premise is that history is a tree:
//!
//!   - Only the session's OWN messages. Ancestor history belongs to another
//!     session's rows, exactly as for a fork, and the answer is the same 400.
//!   - Only a USER message. The model's turns are not the user's to retract.
//!   - Only the LAST user message. Anything earlier is settled history with
//!     answers built on top of it — reaching back into it is what fork is for.
//!
//! WHAT GOES WITH IT: the message and everything AFTER it, which in practice
//! is the partial answer the retracted message provoked. Keeping that would
//! leave a reply to a question nobody can see.
//!
//! THE RUNNING TURN IS STOPPED FIRST, here rather than in the client. Nobody
//! takes a message back and still wants to pay for the answer, and doing both
//! halves in one place removes the race: two calls from a client can
//! interleave with the runner's own writes, one route cannot. The abort does
//! not block, and it does not need to — the runner's late writes are UPDATEs
//! against rows that are gone, which SQLite answers by changing nothing, and
//! its late events name a message no client still holds.

use serde::Serialize;

use crate::errors::BoughError;
use crate::schema::parts::{Message, Role};
use crate::types::AppCtx;

use super::seed::with_db;

/// What the client gets back: enough to put the text in the composer and say so.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UnsendResult {
    pub session_id: String,
    /// The retracted message's text, for the composer it is going back into.
    pub text: String,
    /// Every message id removed — the retracted one, then whatever followed it.
    pub removed: Vec<String>,
    /// True when a turn was running and has been signalled to stop.
    pub interrupted: bool,
}

/// The plain text of a user message, which is all a composer can hold.
fn text_of(message: &Message) -> String {
    message
        .parts
        .iter()
        .filter_map(|p| match p {
            crate::schema::parts::Part::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<String>()
        .trim()
        .to_string()
}

fn role_str(role: Role) -> &'static str {
    match role {
        Role::User => "user",
        Role::Supervisor => "supervisor",
        Role::System => "system",
    }
}

/// Retract `at_message_id` and everything after it.
///
/// Every refusal names the operation that DOES work, because the caller
/// reaching this with the wrong message is a client one release out of step,
/// and "400" on its own leaves the user with a key that silently does nothing.
pub fn unsend(
    ctx: &AppCtx,
    session_id: &str,
    at_message_id: &str,
) -> Result<UnsendResult, BoughError> {
    let session = with_db(&ctx.db, |d| d.get_session(session_id))?
        .ok_or_else(|| BoughError::not_found(format!("no session {session_id}")))?;

    let own = with_db(&ctx.db, |d| d.messages_for(&session.id))?;
    let Some(target) = own.iter().find(|m| m.id == at_message_id) else {
        // Either it never existed, or it is an ancestor's — one sentence
        // covers both, because from here they are the same fact: this session
        // does not own that row.
        return Err(BoughError::bad_request(format!(
            "message {at_message_id} is not one of this session's own messages, so it \
             cannot be taken back here — fork the session that owns it instead"
        )));
    };
    if target.role != Role::User {
        return Err(BoughError::bad_request(format!(
            "only a user message can be taken back; {at_message_id} is a {} message — \
             fork at it to branch away from what the model said",
            role_str(target.role)
        )));
    }
    let last_user = own.iter().rev().find(|m| m.role == Role::User);
    if last_user.map(|m| m.id.as_str()) != Some(target.id.as_str()) {
        return Err(BoughError::bad_request(format!(
            "{at_message_id} is not the most recent thing you said, and taking it back \
             would drop the turns built on top of it — fork at it to say it differently \
             and keep this conversation intact"
        )));
    }

    // Stop first, delete second: a turn signalled after its message is gone
    // would spend the round it is in the middle of for an answer to a
    // retracted question.
    let interrupted = ctx.turn_registry.interrupt(&session.id);
    let text = text_of(target);
    let target_id = target.id.clone();
    let removed = with_db(&ctx.db, |d| d.delete_messages_from(&session.id, &target_id))?;

    Ok(UnsendResult { session_id: session.id, text, removed, interrupted })
}
