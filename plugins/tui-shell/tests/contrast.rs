//! V9 of phase ux1 §2.5: legibility is a measured number. Both palettes, every foreground role,
//! ≥4.5:1 — and when one regresses the failure NAMES it, because "a ratio was too small" is not a
//! bug report.

use bough_plugin_tui_shell::contrast::{audit, ratio, MIN_RATIO};
use bough_plugin_tui_shell::{Theme, ThemeName};

#[test]
fn every_foreground_role_of_both_themes_clears_wcag_aa() {
    for name in [ThemeName::Dark, ThemeName::Light] {
        let theme = Theme::of(name);
        let bad: Vec<String> = audit(&theme)
            .into_iter()
            .filter(|(_, r)| *r < MIN_RATIO)
            .map(|(role, r)| format!("{name:?}.{role} = {r:.2}:1"))
            .collect();
        assert!(
            bad.is_empty(),
            "these roles are below {MIN_RATIO}:1 — {}",
            bad.join(", ")
        );
    }
}

#[test]
fn the_two_roles_the_audit_moved_are_the_two_that_regressed() {
    // M22: `hint` was #565f89 (2.8:1) and `dim` was #707680 (3.7:1). The test pins that the OLD
    // values would fail, so a revert cannot pass this suite quietly.
    let theme = Theme::of(ThemeName::Dark);
    let m = theme.measure_bg;
    assert!(ratio(ratatui::style::Color::Rgb(0x56, 0x5f, 0x89), theme.bg, m) < MIN_RATIO);
    assert!(ratio(ratatui::style::Color::Rgb(0x70, 0x76, 0x80), theme.bg, m) < MIN_RATIO);
    let by_name: std::collections::HashMap<_, _> = audit(&theme).into_iter().collect();
    assert!(by_name["hint"] >= MIN_RATIO);
    assert!(by_name["dim"] >= MIN_RATIO);
}

#[test]
fn selection_stays_readable_under_the_body_colour() {
    // Not part of `audit` (it is a background), but a selection nobody can read is the same bug.
    for name in [ThemeName::Dark, ThemeName::Light] {
        let t = Theme::of(name);
        assert!(
            ratio(t.fg, t.sel_bg, t.measure_bg) >= MIN_RATIO,
            "{name:?}: body text on the selection background"
        );
    }
}
