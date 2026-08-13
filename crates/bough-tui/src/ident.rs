//! What a conversation is CALLED in a tab bar, and how it stays recognisable.
//!
//! THE PROBLEM: a dozen bough windows whose tabs all read `bough · Fix pa…`.
//! The app name is the same in every one of them, so putting it first spends
//! the whole (tiny) width budget on the one part that discriminates nothing.
//! Everything here is ordered by how much it tells you apart from the tab next
//! to it: kind mark, then name, then handle, then — last — "bough".
//!
//! THE TWO NAMES: `name` is semantic and DRIFTS (the cheap tier writes it, a
//! human may rewrite it, the work turns out to be about something else). The
//! `handle` is a deterministic adjective-noun derived from the session id: it
//! means nothing, it is stable forever, and it is what survives truncation to
//! a multiplexer's ~14 columns. A name that silently becomes a lie is worse
//! than a name that never meant anything (claude-code#46852), so the narrow
//! slot gets the handle.
//!
//! WHY A PETNAME AND NOT AN ID: adjacent handles have lexical distance —
//! `brisk-heron` and `plain-otter` cannot be confused the way `s01O97i4` and
//! `s0lO97i4` can, and you can say one out loud.
//!
//! Everything in this module is pure. `app.rs` supplies the parts, `term.rs`
//! writes the result.

/// Adjectives, ≤6 chars so `adj-noun` fits a tmux window name.
const ADJECTIVES: [&str; 64] = [
    "amber", "brisk", "calm", "clear", "brave", "bright", "bold", "civil", "crisp", "damp", "deft",
    "dim", "dry", "eager", "early", "even", "fair", "fine", "firm", "fleet", "fond", "free",
    "gentle", "glad", "grand", "grave", "green", "hardy", "hollow", "humble", "idle", "keen",
    "kind", "level", "light", "loyal", "lucid", "mild", "neat", "noble", "odd", "pale", "patient",
    "plain", "prime", "proud", "quick", "quiet", "rapid", "rare", "ripe", "rough", "royal", "sage",
    "sharp", "silent", "slow", "small", "solid", "spare", "stark", "steady", "still", "swift",
];

/// Nouns, likewise short. Concrete things, so two handles are easy to hold
/// apart in memory — the point of a petname is recall, not entropy.
const NOUNS: [&str; 64] = [
    "acorn", "anvil", "arbor", "badger", "beacon", "birch", "bramble", "canyon", "cedar", "cinder",
    "clover", "comet", "coral", "cove", "crane", "dune", "ember", "falcon", "fern", "finch",
    "forge", "gale", "granite", "harbor", "hazel", "heron", "hollow", "isle", "ivy", "juniper",
    "kestrel", "lantern", "ledge", "lichen", "marsh", "meadow", "mesa", "moss", "otter", "pebble",
    "pine", "plover", "quarry", "quill", "raven", "reef", "ridge", "rill", "sable", "shale",
    "sparrow", "spruce", "summit", "thicket", "thorn", "tide", "trellis", "vale", "vane", "willow",
    "wren", "yarrow", "zenith", "zephyr",
];

/// FNV-1a. A hash, not a cryptographic one — it only has to spread ids evenly
/// across 4096 buckets, and it has to give the SAME answer on every machine
/// and every release, which rules out `DefaultHasher` (explicitly unstable).
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x1000_0000_01b3);
    }
    h
}

/// The stable adjective-noun handle for a session id. Deterministic: the same
/// id yields the same handle on any machine, forever, with no stored state.
pub fn handle(session_id: &str) -> String {
    let h = fnv1a(session_id.as_bytes());
    let adj = ADJECTIVES[(h % ADJECTIVES.len() as u64) as usize];
    let noun = NOUNS[((h / ADJECTIVES.len() as u64) % NOUNS.len() as u64) as usize];
    format!("{adj}-{noun}")
}

/// What a tab bar can carry about one conversation.
///
/// `glyph` and `mark` are the SAME vocabulary the tree paints
/// (`panel::tree::kind_glyph` / `status_mark`) — one set of symbols across the
/// tree, the rail and the terminal, so a mark learned in one is readable in
/// all three.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ident {
    /// Session-kind mark: `●` root, `⑂` fork, `◆` subagent, …
    pub glyph: &'static str,
    /// The semantic title. May be empty (never named) and may change.
    pub name: String,
    /// The stable petname handle.
    pub handle: String,
    /// Outcome mark: `✗`, `✓`, `⋯`, `◼`. State, not identity.
    pub mark: Option<&'static str>,
}

impl Ident {
    /// For the terminal window title, where there is room to be legible.
    ///
    /// "bough" goes LAST: it is the one word shared by every bough tab, so it
    /// is the one word worth losing first to truncation.
    pub fn long(&self) -> String {
        let head = if self.name.trim().is_empty() {
            self.handle.clone()
        } else {
            format!("{} · {}", self.name.trim(), self.handle)
        };
        let mut out = format!("{} {head}", self.glyph);
        if let Some(mark) = self.mark {
            out.push(' ');
            out.push_str(mark);
        }
        out.push_str(" · bough");
        out
    }

    /// The body of a desktop notification.
    ///
    /// WHY IT NAMES THE CONVERSATION: a notification body is not only a banner.
    /// cmux reads the most recent one onto its sidebar row for that workspace,
    /// so with a dozen agents running, "bough finished a turn" renders a dozen
    /// identical lines that say nothing — the exact complaint that motivated
    /// cmux's notification system in the first place. Naming the conversation
    /// makes the banner AND the row answer "which one".
    pub fn notice(&self, what: &str) -> String {
        let who = if self.name.trim().is_empty() {
            self.handle.clone()
        } else {
            format!("{} · {}", self.name.trim(), self.handle)
        };
        format!("{} {who} — {what}", self.glyph)
    }

    /// For a tmux window name or zellij tab, which get ~14 columns.
    ///
    /// The HANDLE, not the name: this slot is too narrow to hold a semantic
    /// title anyway, and a clipped stale title reads as a wrong answer where a
    /// handle reads as an address. The mark rides along because "which of
    /// these twelve is broken" is the question a tab bar is scanned for.
    pub fn short(&self) -> String {
        match self.mark {
            Some(mark) => format!("{} {} {mark}", self.glyph, self.handle),
            None => format!("{} {}", self.glyph, self.handle),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_handle_is_stable_for_an_id_and_differs_between_ids() {
        // Pinned literals, not just `a == a`: the handle is an ADDRESS a human
        // learns, so a change to the hash or the word lists is a breaking
        // change and has to fail here.
        // Ids differing in ONE character land on unrelated words — the point of
        // a petname over an id is that `light-fern` and `free-heron` cannot be
        // misread for each other the way `…4e10` and `…4e11` can.
        assert_eq!(handle("019423b1-7c2e-7a3f-9d81-0f5a2b6c4e10"), "light-fern");
        assert_eq!(handle("019423b1-7c2e-7a3f-9d81-0f5a2b6c4e11"), "free-heron");
        assert_eq!(handle(""), "mild-coral");
        // Same input, same answer — no clock, no RNG, no stored state.
        assert_eq!(handle("session-a"), handle("session-a"));
        assert_ne!(handle("session-a"), handle("session-b"));
    }

    #[test]
    fn handles_are_short_enough_for_a_multiplexer_tab() {
        for id in ["a", "session-42", "019423b1-7c2e-7a3f-9d81-0f5a2b6c4e10"] {
            assert!(handle(id).len() <= 14, "{id}");
        }
    }

    fn ident(name: &str, mark: Option<&'static str>) -> Ident {
        Ident {
            glyph: "●",
            name: name.into(),
            handle: "brisk-heron".into(),
            mark,
        }
    }

    #[test]
    fn the_long_title_leads_with_what_discriminates_and_ends_with_bough() {
        assert_eq!(
            ident("Fix parser", None).long(),
            "● Fix parser · brisk-heron · bough"
        );
        assert_eq!(
            ident("Fix parser", Some("✗")).long(),
            "● Fix parser · brisk-heron ✗ · bough"
        );
        // Never named: the handle stands in rather than leaving a hole.
        assert_eq!(ident("", None).long(), "● brisk-heron · bough");
        assert_eq!(ident("   ", None).long(), "● brisk-heron · bough");
    }

    #[test]
    fn the_short_title_carries_the_handle_because_the_name_would_be_clipped() {
        assert_eq!(ident("Fix the parser bug", None).short(), "● brisk-heron");
        assert_eq!(
            ident("Fix the parser bug", Some("✗")).short(),
            "● brisk-heron ✗"
        );
    }

    #[test]
    fn a_notification_says_which_conversation_it_is_about() {
        assert_eq!(
            ident("Fix parser", None).notice("turn finished"),
            "● Fix parser · brisk-heron — turn finished"
        );
        assert_eq!(
            ident("", None).notice("turn finished"),
            "● brisk-heron — turn finished"
        );
    }

    #[test]
    fn no_title_ever_carries_a_spinner_frame() {
        // The regression this module exists for: an animated title is rewritten
        // every frame, which clobbers any name the user or multiplexer set and
        // spawns a rename process per tick (claude-code#55397). Animation is
        // OSC 9;4's job. Neither form may contain a braille frame.
        let long = ident("Fix parser", Some("⋯")).long();
        let short = ident("Fix parser", Some("⋯")).short();
        for frame in crate::term::TITLE_SPINNER {
            assert!(!long.contains(frame), "{long}");
            assert!(!short.contains(frame), "{short}");
        }
    }
}
