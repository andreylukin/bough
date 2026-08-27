//! Invariant: NO WRAPPED LINE IS EVER STORED (phase ux1 §2.6). The ledger holds the text, the row
//! holds the text, and wrapping plus markdown happen in `render`, against the width of the frame
//! being painted — so a chunk boundary cannot survive a repaint, a resize or a relaunch (M10,
//! M19, nit 39). Every function here is PURE.

use bough_plugin_tui_shell::Theme;
use ratatui::text::Line;

/// One block of an accumulated markdown document.
#[derive(Clone, Debug, PartialEq)]
pub enum Block {
    Heading { level: u8, text: String },
    Para(String),
    Item { level: u8, marker: String, text: String },
    Code { lang: Option<String>, body: String },
    Table { head: Vec<String>, rows: Vec<Vec<String>> },
    Quote(String),
    Rule,
}

/// PURE and TOTAL: any string is a document. Unterminated fences, half-written tables and a
/// heading with no blank line after it all parse — the parser runs on a LIVE tail.
pub fn blocks(doc: &str) -> Vec<Block> {
    let _ = doc;
    todo!("WP-3")
}

/// PURE: blocks to styled lines at `width`. Items hang-indent to the text after the marker
/// (nit 34); tables lay out to their widest cell and scroll-clip, never wrap a cell; code goes
/// through [`crate::highlight`].
pub fn render(blocks: &[Block], width: u16, theme: &Theme) -> Vec<Line<'static>> {
    let _ = (blocks, width, theme);
    todo!("WP-3")
}

/// The whole path in one call, which is what the pane uses.
pub fn document(doc: &str, width: u16, theme: &Theme) -> Vec<Line<'static>> {
    render(&blocks(doc), width, theme)
}
