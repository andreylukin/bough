//! Invariant: the palette is STATE and a PURE filter — it never dispatches. The shell owns when
//! it opens (a `/` at line start) and closes, so a filtering list can never send a command by
//! itself (phase ux1 §2.8).
//!
//! Scaffold deviation D1: `lines()` (the drawing half of §2.8) lives in
//! `bough-plugin-tui-shell::palette`, because it needs the shell's `Theme` and this crate cannot
//! depend on the shell without a dependency cycle.

use crossterm::event::KeyEvent;

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
pub fn filter(all: &[CommandInfo], query: &str) -> Vec<Item> {
    let _ = (all, query);
    todo!("WP-5")
}

/// What a key did to the palette.
#[derive(Clone, Debug, PartialEq)]
pub enum PaletteAction {
    None,
    Moved,
    Accept(CommandName),
    Close,
}

/// PURE: Up/Down move, Tab and Enter accept, Esc closes, anything else falls through.
pub fn on_key(p: &mut Palette, key: KeyEvent, n: usize) -> PaletteAction {
    let _ = (p, key, n);
    todo!("WP-5")
}

/// PURE: the notice a command miss produces. Always three parts: what was typed, the nearest
/// known command if there is one ([`crate::CommandError::Unknown`]'s `did_you_mean`), and the way
/// out (B3, M17):
///
/// ```text
/// unknown command `tmp` — did you mean `focus`? · Enter again sends it as a message · /help
/// ```
pub fn miss_notice(typed: &str, did_you_mean: Option<&str>) -> String {
    let _ = (typed, did_you_mean);
    todo!("WP-5")
}
