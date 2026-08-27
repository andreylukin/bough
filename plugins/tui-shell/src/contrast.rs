//! Invariant: a colour role's legibility is a NUMBER, measured from the theme, not a matter of
//! taste (phase ux1 §2.5, V9). Every function here is PURE.

use ratatui::style::Color;

use crate::theme::Theme;

/// WCAG relative luminance of an sRGB colour. [`Color::Reset`] resolves to [`Theme::measure_bg`].
pub fn luminance(c: Color, measure_bg: Color) -> f64 {
    let _ = (c, measure_bg);
    todo!("WP-4")
}

/// WCAG 2.1 contrast ratio, `1.0..=21.0`.
pub fn ratio(fg: Color, bg: Color, measure_bg: Color) -> f64 {
    let _ = (fg, bg, measure_bg);
    todo!("WP-4")
}

/// Every foreground role of a theme against its background, by name. The V9 test reads this.
pub fn audit(theme: &Theme) -> Vec<(&'static str, f64)> {
    let _ = theme;
    todo!("WP-4")
}
