//! A literal theme, so the tests name colour ROLES exactly as a pane does. `Theme::of` belongs to
//! `tui-shell` (WP-2); the render tests must not wait on a palette to assert on a role.
use bough_plugin_tui_shell::{Theme, ThemeName};
use ratatui::style::Color;

pub fn theme() -> Theme {
    Theme {
        bg: Color::Rgb(16, 16, 20),
        // phase ux1 §2.5: the background the palette is MEASURED against (`bg` may be `Reset`).
        measure_bg: Color::Rgb(16, 16, 20),
        fg: Color::Rgb(220, 220, 220),
        dim: Color::Rgb(120, 120, 120),
        accent: Color::Rgb(120, 180, 255),
        evidence: Color::Rgb(150, 200, 150),
        thought: Color::Rgb(160, 140, 200),
        warn: Color::Rgb(230, 190, 80),
        error: Color::Rgb(230, 90, 90),
        added: Color::Rgb(90, 200, 120),
        removed: Color::Rgb(220, 100, 100),
        sel_bg: Color::Rgb(40, 60, 90),
        hint: Color::Rgb(100, 100, 100),
    }
}

#[allow(dead_code)]
pub fn theme_name() -> ThemeName {
    ThemeName::Dark
}

/// Every span's text, concatenated.
#[allow(dead_code)]
pub fn text(line: &ratatui::text::Line<'_>) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

/// The colours actually used on a line, in order.
#[allow(dead_code)]
pub fn colors(line: &ratatui::text::Line<'_>) -> Vec<Option<Color>> {
    line.spans.iter().map(|s| s.style.fg).collect()
}
