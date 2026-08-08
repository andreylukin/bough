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
pub mod splash;
pub mod status;

use ratatui::style::Color;

// ---- the LIVE palette (theme.rs) -------------------------------------------
// These were `const`s carrying the FALLBACK hexes, which is how the theme
// picker came to preview, keep and persist a palette that changed no pixel:
// every surface painted a hue that could not move. They are functions now, and
// each one reads `theme::colors()` — the snapshot `apply_theme` refreshes — so
// a preview repaints the PRODUCT and not a swatch. Token mapping follows
// theme.rs: accent=green, warn=amber, error=red, info=blue, border=hairline.
// A theme that fails to load leaves the palette on FALLBACK, so a bad fetch
// degrades to the built-ins rather than to a half-painted screen.

pub(crate) fn accent() -> Color {
    crate::theme::colors().accent
}
pub(crate) fn warn() -> Color {
    crate::theme::colors().warn
}
pub(crate) fn error() -> Color {
    crate::theme::colors().error
}
pub(crate) fn info() -> Color {
    crate::theme::colors().info
}
/// Panel borders and hairline separators (theme.rs::palette.border = hairline).
pub(crate) fn border() -> Color {
    crate::theme::colors().border
}
/// Bordered containers: the panel, cards, pickers. A RAISED surface — it must
/// be PAINTED, or a preset whose whole note is "deeper surfaces" changes a
/// border colour and leaves the panel transparent over the transcript.
pub(crate) fn panel() -> Color {
    crate::theme::colors().panel
}
pub(crate) fn bg() -> Color {
    crate::theme::colors().bg
}
pub(crate) fn panel_inset() -> Color {
    crate::theme::colors().panel_inset
}
pub(crate) fn muted() -> Color {
    crate::theme::colors().muted
}

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
