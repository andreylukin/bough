//! The bough mark as terminal art: the empty-session splash.
//!
//! GENERATED from `assets/logo-1024.png`: the icon point-sampled onto a
//! character grid and mapped through the ` .:-=+*#` density ramp — ASCII, not
//! block pixels. A half-block render of the same icon was a faithful copy and
//! looked like a smeared photo in a terminal; the ramp keeps the log's rings
//! and the sprout as STRUCTURE.
//!
//! Cells darker than the icon's black plate are left BLANK — the plate is
//! never painted, so the mark sits on the user's terminal background in any
//! theme rather than stamping a black rectangle onto a light one. Each glyph
//! keeps its source colour, so the sprout stays green and the log tan.
//!
//! The rows carry SGR escapes exactly like transcript rows do; `chat.rs` runs
//! them through `ansi::line_from_ansi`, so nothing here is painted raw.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::{accent, display_width, muted};

/// Display columns `ART` occupies.
pub const ART_COLS: usize = 16;

/// Display columns `ART_SMALL` occupies.
pub const ART_SMALL_COLS: usize = 9;

/// The wordmark under the mark.
const WORDMARK: &str = "bough";

/// The tagline for a pane too narrow for the caller's.
const SHORT_TAGLINE: &str = "type to start · ? help";

/// Centre `line` in `width` columns and pad the tail, so the row overwrites
/// whatever the previous frame left in those cells.
fn centered(mut line: Line<'static>, width: usize, painted: usize) -> Line<'static> {
    let left = width.saturating_sub(painted) / 2;
    if left > 0 {
        line.spans.insert(0, Span::raw(" ".repeat(left)));
    }
    let right = width.saturating_sub(left + painted);
    if right > 0 {
        line.spans.push(Span::raw(" ".repeat(right)));
    }
    line
}

fn centered_text(text: &str, width: usize, style: Style) -> Line<'static> {
    let painted = display_width(text);
    centered(
        Line::from(Span::styled(text.to_string(), style)),
        width,
        painted,
    )
}

/// The empty-session splash: the mark, the wordmark, what this thing does, and
/// the two keys that open everything else.
///
/// Returns exactly `height` rows, or `None` when the body is too short or too
/// narrow for anything but the bare tagline — the caller keeps its
/// one-line placeholder for that case rather than painting a clipped logo.
pub fn splash_block(
    width: usize,
    height: usize,
    tagline: &str,
    hint: &str,
) -> Option<Vec<Line<'static>>> {
    let dim = Style::default().add_modifier(Modifier::DIM);
    let mut block: Vec<Line<'static>> = Vec::new();

    // Biggest mark the body can hold, then the small one, then none. The art is
    // the first thing dropped: a mark clipped in half reads as a rendering bug,
    // while the wording alone reads as a deliberately quiet screen.
    let art: Option<(&[&str], usize)> = if height >= ART.len() + 4 && width >= ART_COLS + 2 {
        Some((&ART, ART_COLS))
    } else if height >= ART_SMALL.len() + 4 && width >= ART_SMALL_COLS + 2 {
        Some((&ART_SMALL, ART_SMALL_COLS))
    } else {
        None
    };
    if let Some((rows, cols)) = art {
        for row in rows {
            block.push(centered(crate::ansi::line_from_ansi(row), width, cols));
        }
        block.push(centered_text("", width, dim));
    } else if height < 3 {
        return None;
    }

    // A narrow pane truncated the tagline MID-WORD — "type to start · the agent
    // writes one program p". The short line says the same thing and fits, and a
    // pane too narrow even for that gets nothing from here.
    let tagline = if width >= display_width(tagline) {
        tagline
    } else if width >= display_width(SHORT_TAGLINE) {
        SHORT_TAGLINE
    } else {
        return None;
    };

    block.push(centered_text(
        WORDMARK,
        width,
        Style::default().fg(accent()).add_modifier(Modifier::BOLD),
    ));
    block.push(centered_text(tagline, width, dim));
    if height > block.len() && width >= display_width(hint) {
        block.push(centered_text(hint, width, Style::default().fg(muted())));
    }

    // Centre the block vertically, biased upward: the composer sits directly
    // below, so a mark pinned to the exact middle crowds it.
    let slack = height.saturating_sub(block.len());
    let top = slack / 2;
    let blank = || Line::from(Span::raw(" ".repeat(width)));
    let mut rows: Vec<Line<'static>> = (0..top).map(|_| blank()).collect();
    rows.extend(block);
    while rows.len() < height {
        rows.push(blank());
    }
    Some(rows)
}

/// The mark at full size: 16 columns, 24 rows.
pub const ART: [&str; 24] = [
    "\x1b[0m         \x1b[0m\x1b[38;2;32;67;53m.\x1b[0m\x1b[0m",
    "\x1b[0m         \x1b[0m\x1b[38;2;78;201;143m--\x1b[0m  \x1b[0m\x1b[38;2;80;207;147m-\x1b[0m\x1b[0m",
    "\x1b[0m         \x1b[0m\x1b[38;2;78;201;143m--\x1b[0m\x1b[38;2;74;191;136m-\x1b[0m\x1b[38;2;78;201;143m--\x1b[0m\x1b[0m",
    "\x1b[0m          \x1b[0m\x1b[38;2;78;201;143m--\x1b[0m\x1b[38;2;81;209;148m-\x1b[0m\x1b[0m",
    "\x1b[0m         \x1b[0m\x1b[38;2;78;201;143m--\x1b[0m\x1b[0m",
    "\x1b[0m        \x1b[0m\x1b[38;2;78;201;143m--\x1b[0m\x1b[0m",
    "\x1b[0m        \x1b[0m\x1b[38;2;78;201;143m-\x1b[0m\x1b[38;2;80;207;147m-\x1b[0m\x1b[0m",
    "\x1b[0m       \x1b[0m\x1b[38;2;81;209;148m-\x1b[0m\x1b[38;2;78;201;143m-\x1b[0m\x1b[0m",
    "\x1b[0m  \x1b[0m\x1b[38;2;47;46;41m.\x1b[0m\x1b[38;2;238;219;166m+\x1b[0m\x1b[38;2;202;168;87m=\x1b[0m\x1b[38;2;194;156;69m--\x1b[0m\x1b[38;2;78;201;143m--\x1b[0m\x1b[38;2;194;156;69m-\x1b[0m\x1b[38;2;193;155;67m-\x1b[0m\x1b[38;2;206;173;96m=\x1b[0m\x1b[38;2;243;223;168m+\x1b[0m\x1b[0m",
    "\x1b[0m \x1b[0m\x1b[38;2;236;217;163m+\x1b[0m\x1b[38;2;193;155;67m-\x1b[0m\x1b[38;2;225;201;139m=\x1b[0m\x1b[38;2;192;154;65m-\x1b[0m\x1b[38;2;193;154;66m-\x1b[0m\x1b[38;2;212;181;108m=\x1b[0m\x1b[38;2;78;201;143m-\x1b[0m\x1b[38;2;76;202;144m-\x1b[0m\x1b[38;2;213;184;112m=\x1b[0m\x1b[38;2;193;154;66m-\x1b[0m\x1b[38;2;194;155;68m-\x1b[0m\x1b[38;2;199;163;80m=\x1b[0m\x1b[38;2;192;153;65m-\x1b[0m\x1b[38;2;236;217;163m+\x1b[0m\x1b[0m",
    "\x1b[0m\x1b[38;2;236;217;163m+\x1b[0m\x1b[38;2;192;153;65m-\x1b[0m\x1b[38;2;236;217;163m+\x1b[0m\x1b[38;2;193;155;67m-\x1b[0m\x1b[38;2;237;218;164m+\x1b[0m\x1b[38;2;238;220;167m+\x1b[0m\x1b[38;2;192;153;64m-\x1b[0m\x1b[38;2;78;201;143m-\x1b[0m\x1b[38;2;80;200;142m-\x1b[0m\x1b[38;2;200;164;81m=\x1b[0m\x1b[38;2;197;161;76m-\x1b[0m\x1b[38;2;236;217;163m+\x1b[0m\x1b[38;2;192;153;65m-\x1b[0m\x1b[38;2;237;218;165m+\x1b[0m\x1b[38;2;233;214;158m+\x1b[0m\x1b[38;2;236;217;163m+\x1b[0m",
    "\x1b[0m\x1b[38;2;235;215;159m+\x1b[0m\x1b[38;2;236;216;162m+\x1b[0m\x1b[38;2;193;154;66m-\x1b[0m\x1b[38;2;208;177;101m=\x1b[0m\x1b[38;2;195;158;73m-\x1b[0m\x1b[38;2;194;156;69m-\x1b[0m\x1b[38;2;192;153;65m-\x1b[0m\x1b[38;2;212;182;109m=\x1b[0m\x1b[38;2;207;175;98m=\x1b[0m\x1b[38;2;192;154;66m-\x1b[0m\x1b[38;2;223;198;133m=\x1b[0m\x1b[38;2;193;154;66m-\x1b[0m\x1b[38;2;237;219;166m+\x1b[0m\x1b[38;2;193;155;66m-\x1b[0m\x1b[38;2;236;217;163m+\x1b[0m\x1b[38;2;220;194;128m=\x1b[0m",
    "\x1b[0m\x1b[38;2;217;180;95m=\x1b[0m\x1b[38;2;216;179;93m=\x1b[0m\x1b[38;2;237;218;165m+\x1b[0m\x1b[38;2;203;168;89m=\x1b[0m\x1b[38;2;194;156;69m-\x1b[0m\x1b[38;2;194;156;70m-\x1b[0m\x1b[38;2;206;173;95m=\x1b[0m\x1b[38;2;199;164;81m=\x1b[0m\x1b[38;2;201;166;84m=\x1b[0m\x1b[38;2;205;173;94m=\x1b[0m\x1b[38;2;192;154;65m-\x1b[0m\x1b[38;2;194;156;69m-\x1b[0m\x1b[38;2;227;203;142m=\x1b[0m\x1b[38;2;238;220;167m+\x1b[0m\x1b[38;2;194;156;69m--\x1b[0m",
    "\x1b[0m\x1b[38;2;217;180;95m====\x1b[0m\x1b[38;2;217;179;94m=\x1b[0m\x1b[38;2;220;186;106m=\x1b[0m\x1b[38;2;229;204;140m=\x1b[0m\x1b[38;2;235;214;157m+\x1b[0m\x1b[38;2;234;213;155m+\x1b[0m\x1b[38;2;229;203;137m=\x1b[0m\x1b[38;2;218;181;97m=\x1b[0m\x1b[38;2;217;180;94m=\x1b[0m\x1b[38;2;194;156;69m----\x1b[0m",
    "\x1b[0m\x1b[38;2;217;180;95m============\x1b[0m\x1b[38;2;194;156;69m----\x1b[0m",
    "\x1b[0m\x1b[38;2;217;180;95m============\x1b[0m\x1b[38;2;194;156;69m----\x1b[0m",
    "\x1b[0m\x1b[38;2;217;180;95m============\x1b[0m\x1b[38;2;194;156;69m----\x1b[0m",
    "\x1b[0m\x1b[38;2;217;180;95m============\x1b[0m\x1b[38;2;194;156;69m----\x1b[0m",
    "\x1b[0m\x1b[38;2;217;180;95m============\x1b[0m\x1b[38;2;194;156;69m----\x1b[0m",
    "\x1b[0m\x1b[38;2;217;180;95m============\x1b[0m\x1b[38;2;194;156;69m----\x1b[0m",
    "\x1b[0m\x1b[38;2;217;180;95m============\x1b[0m\x1b[38;2;194;156;69m----\x1b[0m",
    "\x1b[0m\x1b[38;2;217;180;95m============\x1b[0m\x1b[38;2;194;156;69m---\x1b[0m\x1b[38;2;195;156;69m-\x1b[0m",
    "\x1b[0m \x1b[0m\x1b[38;2;217;180;95m===========\x1b[0m\x1b[38;2;194;156;69m---\x1b[0m\x1b[0m",
    "\x1b[0m   \x1b[0m\x1b[38;2;221;183;97m=\x1b[0m\x1b[38;2;217;180;95m======\x1b[0m\x1b[38;2;210;172;86m=\x1b[0m\x1b[38;2;194;156;69m-\x1b[0m\x1b[38;2;189;152;68m-\x1b[0m\x1b[0m",
];

/// The same mark for a body that cannot hold 24 rows: 9 columns, 15 rows.
pub const ART_SMALL: [&str; 15] = [
    "\x1b[0m     \x1b[0m\x1b[38;2;70;176;126m-\x1b[0m\x1b[0m",
    "\x1b[0m     \x1b[0m\x1b[38;2;78;201;143m-\x1b[0m\x1b[38;2;26;52;43m.\x1b[0m\x1b[38;2;78;201;143m-\x1b[0m\x1b[0m",
    "\x1b[0m     \x1b[0m\x1b[38;2;79;203;144m-\x1b[0m\x1b[38;2;78;201;143m-\x1b[0m\x1b[0m",
    "\x1b[0m    \x1b[0m\x1b[38;2;36;80;62m.\x1b[0m\x1b[38;2;78;201;143m-\x1b[0m\x1b[0m",
    "\x1b[0m    \x1b[0m\x1b[38;2;78;201;143m-\x1b[0m\x1b[0m",
    "\x1b[0m \x1b[0m\x1b[38;2;237;219;166m+\x1b[0m\x1b[38;2;194;156;69m-\x1b[0m\x1b[38;2;196;159;73m-\x1b[0m\x1b[38;2;78;201;143m-\x1b[0m\x1b[38;2;193;155;67m-\x1b[0m\x1b[38;2;193;154;66m-\x1b[0m\x1b[38;2;244;224;168m+\x1b[0m\x1b[0m",
    "\x1b[0m\x1b[38;2;199;163;80m=\x1b[0m\x1b[38;2;223;197;132m=\x1b[0m\x1b[38;2;227;204;142m=\x1b[0m\x1b[38;2;237;219;166m+\x1b[0m\x1b[38;2;78;201;143m-\x1b[0m\x1b[38;2;231;210;151m+\x1b[0m\x1b[38;2;237;219;165m+\x1b[0m\x1b[38;2;236;217;163m++\x1b[0m",
    "\x1b[0m\x1b[38;2;236;217;163m+\x1b[0m\x1b[38;2;192;154;65m-\x1b[0m\x1b[38;2;193;155;67m-\x1b[0m\x1b[38;2;208;176;100m=\x1b[0m\x1b[38;2;199;163;80m=\x1b[0m\x1b[38;2;198;162;78m-\x1b[0m\x1b[38;2;202;168;87m=\x1b[0m\x1b[38;2;194;156;69m-\x1b[0m\x1b[38;2;238;220;167m+\x1b[0m",
    "\x1b[0m\x1b[38;2;217;180;95m==\x1b[0m\x1b[38;2;216;178;92m=\x1b[0m\x1b[38;2;229;204;140m=\x1b[0m\x1b[38;2;235;214;158m+\x1b[0m\x1b[38;2;225;196;124m=\x1b[0m\x1b[38;2;217;180;94m=\x1b[0m\x1b[38;2;194;156;69m--\x1b[0m",
    "\x1b[0m\x1b[38;2;217;180;95m=======\x1b[0m\x1b[38;2;194;156;69m--\x1b[0m",
    "\x1b[0m\x1b[38;2;217;180;95m=======\x1b[0m\x1b[38;2;194;156;69m--\x1b[0m",
    "\x1b[0m\x1b[38;2;217;180;95m=======\x1b[0m\x1b[38;2;194;156;69m--\x1b[0m",
    "\x1b[0m\x1b[38;2;217;180;95m=======\x1b[0m\x1b[38;2;194;156;69m--\x1b[0m",
    "\x1b[0m\x1b[38;2;217;180;95m=======\x1b[0m\x1b[38;2;194;156;69m--\x1b[0m",
    "\x1b[0m \x1b[0m\x1b[38;2;217;180;95m=====\x1b[0m\x1b[38;2;194;156;69m-\x1b[0m\x1b[38;2;196;157;69m-\x1b[0m\x1b[0m",
];
