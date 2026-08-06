//! Long pastes, and WHERE IN THE DRAFT they belong (port of `src/tui/paste.ts`).
//!
//! A paste over [`QUEUE_ABOVE_CHARS`] is not inlined — a 400-line stack trace in
//! the composer buries the sentence being written and pushes the transcript off
//! the screen. It is held aside and shown as one compact row instead.
//!
//! What did not work is that the held text was appended to the END of the
//! message at submit, in the order it was pasted. "Compare `<paste>` with
//! `<paste>` and explain the difference" came out as the two sentences first and
//! both pastes afterwards, in an order the user could not influence and had no
//! way to see. Position is meaning.
//!
//! So a paste leaves a MARK where the cursor was, and the mark is what the draft
//! carries: `[Pasted text #1]`, the same text the chip row shows. Three
//! properties follow from making the draft the record rather than a parallel
//! list of offsets:
//!
//!   - **Edits cannot desynchronize it** — a mark moves with the text it sits in.
//!   - **Deleting the mark drops the paste.** That is the removal gesture.
//!   - **Order is the draft's order**, because expansion follows the marks and
//!     not the queue.
//!
//! An ordinal is a stable NAME, never a position. A mark naming a paste that
//! does not exist — one the user typed themselves — is left exactly as written.
//!
//! PURE. Strings in, strings out: no state, no terminal.

use std::sync::LazyLock;

use regex::{Captures, Regex};

/// Above this many characters a paste is held aside instead of inlined.
///
/// Low on purpose. The cost of holding a paste is one row and a mark; the cost
/// of inlining one is a composer the user cannot see past, and that asymmetry
/// starts well before a paste is what anyone would call large.
pub const QUEUE_ABOVE_CHARS: usize = 50;

/// The mark a held paste leaves in the draft. Matches the chip row's own label.
pub fn paste_mark(ordinal: usize) -> String {
    format!("[Pasted text #{ordinal}]")
}

/// Global, because a draft may hold several — and the same one more than once.
static MARK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[Pasted text #(\d+)\]").unwrap());

/// The message as it will actually be sent: every mark replaced by its paste.
///
/// A paste nobody refers to is dropped — its mark was deleted, which is how a
/// held paste is thrown away. A mark with no paste behind it is left verbatim.
pub fn expand_pastes(text: &str, pastes: &[String]) -> String {
    MARK.replace_all(text, |caps: &Captures| {
        let ordinal: usize = caps[1].parse().unwrap_or(0);
        match ordinal.checked_sub(1).and_then(|i| pastes.get(i)) {
            Some(paste) => paste.clone(),
            None => caps[0].to_string(),
        }
    })
    .into_owned()
}

// ---------------------------------------------------------------------------
// Tests — ports of `src/tui/paste.test.ts`
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "aaa\nbbb";
    const B: &str = "console.error('boom')";

    fn both() -> Vec<String> {
        vec![A.to_string(), B.to_string()]
    }

    #[test]
    fn a_mark_expands_where_it_sits_not_at_the_end() {
        let draft = format!("compare {} with {} and explain", paste_mark(1), paste_mark(2));
        let out = expand_pastes(&draft, &both());
        assert_eq!(out, format!("compare {A} with {B} and explain"));
        // The whole point: the pastes are INSIDE the sentence, not trailing it.
        assert!(out.ends_with("and explain"));
    }

    #[test]
    fn the_drafts_order_wins_over_the_queues() {
        // #1 was pasted first; the user moved it after #2. What the draft says goes.
        let draft = format!("{} then {}", paste_mark(2), paste_mark(1));
        assert_eq!(expand_pastes(&draft, &both()), format!("{B} then {A}"));
    }

    #[test]
    fn deleting_a_mark_drops_its_paste_that_is_the_removal_gesture() {
        assert_eq!(
            expand_pastes(&format!("only {}", paste_mark(2)), &both()),
            format!("only {B}")
        );
        assert_eq!(expand_pastes("nothing held", &both()), "nothing held");
    }

    #[test]
    fn an_ordinal_is_a_name_not_a_position() {
        // #1's mark is gone; #2 is still #2 in both the draft and the row.
        let three = vec![A.to_string(), B.to_string(), "third".to_string()];
        assert_eq!(
            expand_pastes(&format!("keep {}", paste_mark(2)), &three),
            format!("keep {B}")
        );
    }

    #[test]
    fn a_mark_repeated_is_a_paste_repeated() {
        let draft = format!("{} vs {}", paste_mark(1), paste_mark(1));
        assert_eq!(expand_pastes(&draft, &[A.to_string()]), format!("{A} vs {A}"));
    }

    #[test]
    fn a_mark_with_no_paste_behind_it_is_left_exactly_as_written() {
        // Typed by hand, or left over from a message already sent.
        let draft = format!("see {}", paste_mark(7));
        assert_eq!(expand_pastes(&draft, &[A.to_string()]), draft);
    }

    #[test]
    fn the_mark_is_the_chips_own_label_so_the_draft_reads_like_the_row() {
        assert_eq!(paste_mark(1), "[Pasted text #1]");
    }

    #[test]
    fn the_hold_threshold_is_deliberately_low() {
        assert_eq!(QUEUE_ABOVE_CHARS, 50);
    }
}
