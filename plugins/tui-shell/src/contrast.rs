//! Invariant: a colour role's legibility is a NUMBER, measured from the theme, not a matter of
//! taste (phase ux1 §2.5, V9). Every function here is PURE.
//!
//! `Theme::bg` is [`Color::Reset`] — whatever the user's terminal happens to be — so a ratio
//! against it is not a number. Everything here measures against [`Theme::measure_bg`], the
//! background the palette was designed for, which is why that field exists.

use ratatui::style::Color;

use crate::theme::Theme;

/// The WCAG minimum for body text. A role under this is a bug, not a preference.
pub const MIN_RATIO: f64 = 4.5;

/// The sRGB components of a colour, resolving [`Color::Reset`] to `measure_bg` and the sixteen
/// ANSI names to their xterm defaults. TOTAL: every `Color` has components.
fn rgb(c: Color, measure_bg: Color) -> (u8, u8, u8) {
    match c {
        Color::Rgb(r, g, b) => (r, g, b),
        Color::Reset => match measure_bg {
            // One level of resolution only: a `measure_bg` of `Reset` is black, which is the
            // conservative reading of "unknown dark terminal".
            Color::Rgb(r, g, b) => (r, g, b),
            other if other != Color::Reset => rgb(other, Color::Rgb(0, 0, 0)),
            _ => (0, 0, 0),
        },
        Color::Black => (0, 0, 0),
        Color::Red => (0xcd, 0x00, 0x00),
        Color::Green => (0x00, 0xcd, 0x00),
        Color::Yellow => (0xcd, 0xcd, 0x00),
        Color::Blue => (0x00, 0x00, 0xee),
        Color::Magenta => (0xcd, 0x00, 0xcd),
        Color::Cyan => (0x00, 0xcd, 0xcd),
        Color::Gray => (0xe5, 0xe5, 0xe5),
        Color::DarkGray => (0x7f, 0x7f, 0x7f),
        Color::LightRed => (0xff, 0x00, 0x00),
        Color::LightGreen => (0x00, 0xff, 0x00),
        Color::LightYellow => (0xff, 0xff, 0x00),
        Color::LightBlue => (0x5c, 0x5c, 0xff),
        Color::LightMagenta => (0xff, 0x00, 0xff),
        Color::LightCyan => (0x00, 0xff, 0xff),
        Color::White => (0xff, 0xff, 0xff),
        Color::Indexed(i) => indexed(i),
    }
}

/// The xterm 256-colour cube, so an `Indexed` role still measures.
fn indexed(i: u8) -> (u8, u8, u8) {
    match i {
        0..=15 => {
            const BASE: [(u8, u8, u8); 16] = [
                (0, 0, 0),
                (0xcd, 0, 0),
                (0, 0xcd, 0),
                (0xcd, 0xcd, 0),
                (0, 0, 0xee),
                (0xcd, 0, 0xcd),
                (0, 0xcd, 0xcd),
                (0xe5, 0xe5, 0xe5),
                (0x7f, 0x7f, 0x7f),
                (0xff, 0, 0),
                (0, 0xff, 0),
                (0xff, 0xff, 0),
                (0x5c, 0x5c, 0xff),
                (0xff, 0, 0xff),
                (0, 0xff, 0xff),
                (0xff, 0xff, 0xff),
            ];
            BASE[i as usize]
        }
        16..=231 => {
            const STEP: [u8; 6] = [0, 95, 135, 175, 215, 255];
            let n = i as usize - 16;
            (STEP[n / 36], STEP[(n / 6) % 6], STEP[n % 6])
        }
        _ => {
            let v = 8 + (i as u16 - 232) * 10;
            let v = v.min(255) as u8;
            (v, v, v)
        }
    }
}

/// One sRGB channel, linearised.
fn channel(v: u8) -> f64 {
    let c = v as f64 / 255.0;
    if c <= 0.03928 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// WCAG relative luminance of an sRGB colour. [`Color::Reset`] resolves to [`Theme::measure_bg`].
pub fn luminance(c: Color, measure_bg: Color) -> f64 {
    let (r, g, b) = rgb(c, measure_bg);
    0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b)
}

/// WCAG 2.1 contrast ratio, `1.0..=21.0`.
pub fn ratio(fg: Color, bg: Color, measure_bg: Color) -> f64 {
    let (a, b) = (luminance(fg, measure_bg), luminance(bg, measure_bg));
    let (hi, lo) = if a >= b { (a, b) } else { (b, a) };
    (hi + 0.05) / (lo + 0.05)
}

/// Every foreground role of a theme against its background, BY NAME — the name is what makes the
/// V9 failure message say which role regressed instead of "a number was too small".
///
/// `sel_bg` is not here: it is a background, and it is measured against `fg` by the caller that
/// cares. `measure_bg` is not a role.
pub fn audit(theme: &Theme) -> Vec<(&'static str, f64)> {
    let m = theme.measure_bg;
    let r = |c: Color| ratio(c, theme.bg, m);
    vec![
        ("fg", r(theme.fg)),
        ("dim", r(theme.dim)),
        ("accent", r(theme.accent)),
        ("evidence", r(theme.evidence)),
        ("thought", r(theme.thought)),
        ("warn", r(theme.warn)),
        ("error", r(theme.error)),
        ("added", r(theme.added)),
        ("removed", r(theme.removed)),
        ("hint", r(theme.hint)),
        ("interactive", r(theme.interactive)),
        // Code sits on its own ground, so it is measured against that ground.
        ("code", ratio(theme.code, theme.code_bg, m)),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_two_wcag_anchors_come_out_exact() {
        let m = Color::Rgb(0, 0, 0);
        let white = Color::Rgb(0xff, 0xff, 0xff);
        let black = Color::Rgb(0, 0, 0);
        assert!((ratio(white, black, m) - 21.0).abs() < 1e-9);
        assert!((ratio(white, white, m) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn reset_is_measured_as_the_palettes_own_background() {
        let t = Theme::of(crate::theme::ThemeName::Dark);
        assert_eq!(t.bg, Color::Reset);
        let direct = ratio(t.fg, t.measure_bg, t.measure_bg);
        let through_reset = ratio(t.fg, t.bg, t.measure_bg);
        assert!((direct - through_reset).abs() < 1e-12);
    }

    #[test]
    fn the_ratio_is_symmetric() {
        let m = Color::Rgb(0x1a, 0x1b, 0x26);
        let a = Color::Rgb(0x7a, 0xa2, 0xf7);
        let b = Color::Rgb(0x1a, 0x1b, 0x26);
        assert!((ratio(a, b, m) - ratio(b, a, m)).abs() < 1e-12);
    }
}
