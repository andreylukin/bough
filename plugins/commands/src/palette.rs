//! Invariant: the palette is STATE and a PURE filter — it never dispatches. The shell owns when
//! it opens (a `/` at line start) and closes, so a filtering list can never send a command by
//! itself (phase ux1 §2.8).
//!
//! Scaffold deviation D1: `lines()` (the drawing half of §2.8) lives in
//! `bough-plugin-tui-shell::palette`, because it needs the shell's `Theme` and this crate cannot
//! depend on the shell without a dependency cycle.
//!
//! WP-5 deviation D2: `on_key` takes the FILTERED ITEMS, not their count. The scaffold's
//! `n: usize` cannot produce `Accept(CommandName)` — there is no name in a count — and `Tab`
//! could not complete for the same reason. The items are what the caller already has.
//!
//! WP-5 deviation D3: `PaletteAction` gains `Complete(CommandName)`. §2.8's prose says "Tab and
//! Enter accept", the work package says "`Tab` completes without accepting"; the work package
//! wins, and completing needs its own answer so the shell knows to rewrite the draft rather than
//! to dispatch.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::{CommandInfo, CommandName};

/// The `/` palette. State only.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Palette {
    pub open: bool,
    pub query: String,
    pub selected: usize,
}

/// One row of the palette.
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub name: CommandName,
    pub usage: String,
    pub summary: String,
}

/// PURE: prefix matches first, then substring, each group alphabetical. Stable, so the selection
/// does not jump under the user as they type.
///
/// The order depends on the NAME alone, never on registration order or on how long the query is,
/// so a growing query only ever REMOVES rows: the row under the cursor cannot be swapped out from
/// under a typist mid-word.
pub fn filter(all: &[CommandInfo], query: &str) -> Vec<Item> {
    let q = query.trim().trim_start_matches('/').to_lowercase();
    let mut prefix: Vec<&CommandInfo> = Vec::new();
    let mut substring: Vec<&CommandInfo> = Vec::new();
    for info in all {
        let name = info.name.as_str().to_lowercase();
        if q.is_empty() || name.starts_with(&q) {
            prefix.push(info);
        } else if name.contains(&q) {
            substring.push(info);
        }
    }
    let by_name = |a: &&CommandInfo, b: &&CommandInfo| a.name.as_str().cmp(b.name.as_str());
    prefix.sort_by(by_name);
    substring.sort_by(by_name);
    prefix
        .into_iter()
        .chain(substring)
        .map(|info| Item {
            name: info.name.clone(),
            usage: info.usage.clone(),
            summary: info.summary.clone(),
        })
        .collect()
}

/// What a key did to the palette.
#[derive(Clone, Debug, PartialEq)]
pub enum PaletteAction {
    None,
    Moved,
    /// `Tab`: put this name in the composer and leave the palette open (D3).
    Complete(CommandName),
    /// `Enter`: dispatch this name.
    Accept(CommandName),
    Close,
}

/// PURE: Up/Down move (wrapping at BOTH ends), Tab completes, Enter accepts, Esc closes, anything
/// else falls through to the composer.
///
/// An empty list answers `None` to every key but `Esc`: there is nothing to move to, nothing to
/// complete and nothing to accept, and a palette that swallowed Enter over no items would eat a
/// message.
pub fn on_key(p: &mut Palette, key: KeyEvent, items: &[Item]) -> PaletteAction {
    if key.code == KeyCode::Esc {
        p.open = false;
        p.selected = 0;
        return PaletteAction::Close;
    }
    if items.is_empty() {
        p.selected = 0;
        return PaletteAction::None;
    }
    if p.selected >= items.len() {
        p.selected = items.len() - 1;
    }
    let n = items.len();
    match key.code {
        KeyCode::Up => {
            p.selected = (p.selected + n - 1) % n;
            PaletteAction::Moved
        }
        KeyCode::Down => {
            p.selected = (p.selected + 1) % n;
            PaletteAction::Moved
        }
        KeyCode::BackTab => {
            p.selected = (p.selected + n - 1) % n;
            PaletteAction::Moved
        }
        KeyCode::Tab => PaletteAction::Complete(items[p.selected].name.clone()),
        KeyCode::Enter
            if !key
                .modifiers
                .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) =>
        {
            p.open = false;
            PaletteAction::Accept(items[p.selected].name.clone())
        }
        _ => PaletteAction::None,
    }
}

/// The escape hatch a miss offers, spelled once.
pub const SEND_AS_MESSAGE: &str = "Enter again sends it as a message";

/// PURE: the notice a command miss produces. Always three parts: what was typed, the nearest
/// known command if there is one ([`crate::CommandError::Unknown`]'s `did_you_mean`), and the way
/// out (B3, M17):
///
/// ```text
/// unknown command `tmp` — did you mean `/focus`? · Enter again sends it as a message · try /help
/// ```
///
/// The TYPED TEXT is carried whole and verbatim, because the notice is the receipt for text the
/// composer is still holding: a notice that paraphrased it would read as if the text were gone.
pub fn miss_notice(typed: &str, did_you_mean: Option<&str>) -> String {
    let mut out = format!("unknown command `{}`", typed.trim());
    if let Some(near) = did_you_mean {
        let near = near.trim_start_matches('/');
        out.push_str(&format!(" — did you mean `/{near}`?"));
    }
    out.push_str(&format!(" · {SEND_AS_MESSAGE} · try /help"));
    out
}

/// PURE: a command's output with the line that produced it echoed above it (M18).
///
/// A pane that shows only the answer makes `/agents` and `/drift` look like the same anonymous
/// block of text; the echo is what turns a notice into a transcript entry.
pub fn echoed(raw: &str, text: &str) -> String {
    let raw = raw.trim();
    if text.trim().is_empty() {
        // M27: "every command renders output OR A REASON". Echoing the bare line for an empty
        // answer made a complete no-op indistinguishable from a command that worked — and it made
        // the two shell-use bullets that decide "rendered something" by diffing the screen pass
        // for a command that renders nothing, because its own name appeared in the notice band.
        return format!("{raw}\n{NO_OUTPUT}");
    }
    format!("{raw}\n{text}")
}

/// What a command that answered with nothing at all says instead of nothing at all. A test
/// asserting M27 looks for the ABSENCE of this string, which is a check that can fail.
pub const NO_OUTPUT: &str = "(no output \u{2014} this command did nothing)";

/// The words no user-facing command summary may use: this tree's INTERNAL vocabulary, which the
/// audit caught leaking into `/help` (M16). The ledger's step types keep these names; a sentence
/// shown to a human does not get them.
pub const HOUSE_WORDS: [&str; 5] = ["tree", "lane", "mail", "wake", "distil"];

/// PURE: `Some(word)` when a summary uses a house word, `None` when it reads as plain language.
///
/// Prefix matching on whole words, so `lanes`, `mailbox`, `wakes` and `distilled` are caught too.
/// Word-boundary, so `awake` and `email` are not: they are ordinary English.
pub fn house_word(summary: &str) -> Option<&'static str> {
    for word in summary.split(|c: char| !c.is_alphanumeric()) {
        let word = word.to_lowercase();
        if let Some(found) = HOUSE_WORDS.iter().find(|w| word.starts_with(*w)) {
            return Some(found);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CommandScope;

    fn info(name: &str) -> CommandInfo {
        CommandInfo {
            name: CommandName::new(name),
            summary: format!("do {name}"),
            usage: format!("/{name}"),
            scope: CommandScope::Global,
        }
    }

    #[test]
    fn a_house_word_is_caught_and_plain_language_is_not() {
        assert_eq!(house_word("mail keeps queuing"), Some("mail"));
        assert_eq!(house_word("which lanes are asleep"), Some("lane"));
        assert_eq!(house_word("distil the evidence"), Some("distil"));
        assert_eq!(house_word("tear the tree down and leave"), Some("tree"));
        assert_eq!(house_word("close bough"), None);
        // `awake` and `email` are English, not house words.
        assert_eq!(house_word("wait until it is awake"), None);
    }

    #[test]
    fn the_echo_puts_the_typed_line_first() {
        assert_eq!(echoed("/agents ", "sol  idle"), "/agents\nsol  idle");
        // M27: a command that answered with nothing SAYS so. The bare-line form made "worked
        // silently" and "did nothing at all" the same pixels.
        assert_eq!(echoed("/quit", "  "), format!("/quit\n{NO_OUTPUT}"));
    }

    #[test]
    fn an_empty_palette_swallows_nothing_but_escape() {
        let mut p = Palette {
            open: true,
            query: "zz".into(),
            selected: 3,
        };
        let key = |c| KeyEvent::new(c, KeyModifiers::NONE);
        assert_eq!(
            on_key(&mut p, key(KeyCode::Enter), &[]),
            PaletteAction::None
        );
        assert!(p.open);
        assert_eq!(on_key(&mut p, key(KeyCode::Esc), &[]), PaletteAction::Close);
        assert!(!p.open);
    }

    #[test]
    fn filter_is_stable_as_the_query_grows() {
        let all = vec![info("focus"), info("dormant"), info("drift"), info("quit")];
        let wide: Vec<String> = filter(&all, "d")
            .iter()
            .map(|i| i.name.to_string())
            .collect();
        assert_eq!(wide, ["dormant", "drift"]);
        let narrow: Vec<String> = filter(&all, "dr")
            .iter()
            .map(|i| i.name.to_string())
            .collect();
        assert_eq!(narrow, ["drift"]);
    }
}
