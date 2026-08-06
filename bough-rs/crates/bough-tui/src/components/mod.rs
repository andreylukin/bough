//! TUI components — wave-1 (row 1.39) subset: chat transcript, composer,
//! status row. Every component is a pure render of props (the App.tsx
//! invariant: "this file contains no logic worth testing" holds for the
//! composition root because the components and helpers here carry the logic).
//!
//! WAVE-1 NOTE ON HELPERS: `fmt_tokens`/`fmt_usd`/`fmt_duration`/`SPINNER` and
//! the palette constants below belong to `tui/format.rs` and `tui/theme.rs`
//! per the specs (rows 1.34 / wave 2). Those modules are unported stubs, so
//! the minimal subset lives here as `pub(crate)` items — private to the crate
//! precisely so the eventual `format.rs`/`theme.rs` ports can take them over
//! without a public-API break. Wording and math are ported verbatim from
//! `src/tui/format.ts`.

pub mod ask;
pub mod chat;
pub mod composer;
pub mod help;
pub mod job_output;
pub mod panel;
pub mod rail;
pub mod status;

use ratatui::style::Color;

// ---- FALLBACK palette (theme.ts::FALLBACK, contrast-checked) ---------------
// v1 ships the fallback palette only (PORT_PLAN wave-1 stub note). Token
// mapping follows theme.ts::palette: accent=green, warn=amber, error=red,
// info=blue, border=hairline.

pub(crate) const ACCENT: Color = Color::Rgb(0x4e, 0xc9, 0x8f); // green #4ec98f
pub(crate) const WARN: Color = Color::Rgb(0xd9, 0xb4, 0x5f); // amber #d9b45f
#[allow(dead_code)] // error tone lands with the panel tabs (wave 2)
pub(crate) const ERROR: Color = Color::Rgb(0xe2, 0x77, 0x6e); // red #e2776e
pub(crate) const INFO: Color = Color::Rgb(0x5c, 0x88, 0xc9); // blue #5c88c9
/// Panel borders and hairline separators (theme.ts::palette.border = hairline).
pub(crate) const BORDER: Color = Color::Rgb(0x66, 0x6d, 0x79); // hairline #666d79
/// Bordered containers: the panel, cards, pickers. A RAISED surface — it must
/// be PAINTED, or a preset whose whole note is "deeper surfaces" changes a
/// border colour and leaves the panel transparent over the transcript.
pub(crate) const PANEL: Color = Color::Rgb(0x14, 0x16, 0x1a); // panel #14161a
pub(crate) const BG: Color = Color::Rgb(0x0e, 0x10, 0x13); // bg #0e1013
pub(crate) const PANEL_INSET: Color = Color::Rgb(0x1f, 0x23, 0x29); // panelInset #1f2329
pub(crate) const MUTED: Color = Color::Rgb(0x9a, 0xa1, 0xac); // muted #9aa1ac

// ---- number/time wording (format.ts, verbatim) ------------------------------

/// Braille spinner frames. Ten of them, so the phase reads as motion, not a glitch.
pub(crate) const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Ticks per spinner cycle (format.ts::SPINNER_MS).
pub const SPINNER_MS: u64 = 120;

/// 1234 → "1.2k", 12345 → "12k" (format.ts::fmtTokens).
pub(crate) fn fmt_tokens(n: i64) -> String {
    if n >= 1000 {
        let k = n as f64 / 1000.0;
        if n >= 10_000 {
            format!("{k:.0}k")
        } else {
            format!("{k:.1}k")
        }
    } else {
        format!("{n}")
    }
}

/// 1.234 → "$1.23", 0.0042 → "$0.004" — sub-dollar spend keeps a visible digit.
pub(crate) fn fmt_usd(n: f64) -> String {
    if n >= 1.0 {
        format!("${n:.2}")
    } else if n >= 0.001 {
        format!("${n:.3}")
    } else {
        format!("${n:.4}")
    }
}

/// "9s", "1m04s", "1h02m" (format.ts::fmtDuration).
pub(crate) fn fmt_duration(ms: i64) -> String {
    let total = (ms / 1000).max(0);
    if total < 60 {
        return format!("{total}s");
    }
    let mins = total / 60;
    let secs = total % 60;
    if mins < 60 {
        return format!("{mins}m{secs:02}s");
    }
    format!("{}h{:02}m", mins / 60, mins % 60)
}

/// Display columns of a string. v1 strings carry no escapes (ansi.rs is the
/// row-1.34 port); CJK wide glyphs count 2 via unicode-width.
pub(crate) fn display_width(s: &str) -> usize {
    unicode_width::UnicodeWidthStr::width(s)
}

/// Pads to exactly `w` display columns AND flattens `\r?\n` → `" "` — one
/// string that paints two rows shifts every pinned region below it
/// (Message.tsx::padRow, the row-hygiene backstop).
pub(crate) fn pad_row(text: &str, w: usize) -> String {
    let flat = text.replace("\r\n", " ").replace('\n', " ");
    let width = display_width(&flat);
    if width >= w {
        flat
    } else {
        let mut out = flat;
        out.push_str(&" ".repeat(w - width));
        out
    }
}

/// [`pad_row`] for a string that CARRIES ANSI (the rail, the job view): the
/// escapes are zero-width, so the padding is measured over `ansi::width` and
/// not over the bytes. Measuring the escapes as text under-pads the row, and
/// an under-padded row keeps the tail of the longer one that was there before
/// it — on the two surfaces that redraw every second.
pub(crate) fn pad_row_ansi(text: &str, w: usize) -> String {
    let flat = text.replace("\r\n", " ").replace('\n', " ");
    let width = crate::ansi::width(&flat);
    if width >= w {
        flat
    } else {
        let mut out = flat;
        out.push_str(&" ".repeat(w - width));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fmt_tokens_matches_ts() {
        assert_eq!(fmt_tokens(999), "999");
        assert_eq!(fmt_tokens(1234), "1.2k");
        assert_eq!(fmt_tokens(12345), "12k");
    }

    #[test]
    fn fmt_usd_keeps_a_visible_digit_sub_dollar() {
        assert_eq!(fmt_usd(1.234), "$1.23");
        assert_eq!(fmt_usd(0.0042), "$0.004");
        assert_eq!(fmt_usd(0.0004), "$0.0004");
    }

    #[test]
    fn fmt_duration_seconds_survive_past_a_minute() {
        assert_eq!(fmt_duration(9_000), "9s");
        assert_eq!(fmt_duration(64_000), "1m04s");
        assert_eq!(fmt_duration(3_720_000), "1h02m");
    }

    #[test]
    fn pad_row_flattens_newlines_and_pads_to_width() {
        assert_eq!(pad_row("a\nb", 5), "a b  ");
        assert_eq!(pad_row("wide", 2), "wide"); // never truncates — the renderer clips
    }
}
