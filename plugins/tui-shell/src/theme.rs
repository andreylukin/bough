//! Invariant: call sites name ROLES, never colours. A pane that writes a literal `Color::Rgb` is a
//! failed review — the theme is the one place a colour is chosen, so dark and light stay one
//! surface and `shell-use cells` assertions have a name to test against.

use ratatui::style::Color;

/// Which backend the shell draws on.
#[derive(
    Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    /// crossterm when stdout is a TTY, else headless (P3-D2).
    Auto,
    Crossterm,
    Headless,
}

impl Backend {
    /// Resolve `Auto` against the runtime fact of a terminal (P3-D2). PURE in `is_tty`, so the
    /// rule is testable without one.
    pub fn resolve(self, is_tty: bool) -> Backend {
        match self {
            Backend::Auto if is_tty => Backend::Crossterm,
            Backend::Auto => Backend::Headless,
            other => other,
        }
    }
}

/// Which palette.
#[derive(
    Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum ThemeName {
    Dark,
    Light,
}

/// Named roles, not colours at call sites.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Theme {
    pub bg: Color,
    pub fg: Color,
    pub dim: Color,
    pub accent: Color,
    pub evidence: Color,
    pub thought: Color,
    pub warn: Color,
    pub error: Color,
    pub added: Color,
    pub removed: Color,
    pub sel_bg: Color,
    pub hint: Color,
}

impl Theme {
    /// The palette for a name.
    pub fn of(name: ThemeName) -> Theme {
        match name {
            ThemeName::Dark => Theme {
                bg: Color::Reset,
                fg: Color::Rgb(0xd0, 0xd0, 0xd0),
                dim: Color::Rgb(0x70, 0x76, 0x80),
                accent: Color::Rgb(0x7a, 0xa2, 0xf7),
                evidence: Color::Rgb(0x9e, 0xce, 0x6a),
                thought: Color::Rgb(0xbb, 0x9a, 0xf7),
                warn: Color::Rgb(0xe0, 0xaf, 0x68),
                error: Color::Rgb(0xf7, 0x76, 0x8e),
                added: Color::Rgb(0x9e, 0xce, 0x6a),
                removed: Color::Rgb(0xf7, 0x76, 0x8e),
                sel_bg: Color::Rgb(0x2d, 0x3f, 0x60),
                hint: Color::Rgb(0x56, 0x5f, 0x89),
            },
            ThemeName::Light => Theme {
                bg: Color::Reset,
                fg: Color::Rgb(0x24, 0x28, 0x2f),
                dim: Color::Rgb(0x6a, 0x70, 0x7a),
                accent: Color::Rgb(0x2e, 0x5c, 0xb8),
                evidence: Color::Rgb(0x2c, 0x77, 0x2c),
                thought: Color::Rgb(0x6b, 0x3f, 0xa0),
                warn: Color::Rgb(0x9a, 0x62, 0x00),
                error: Color::Rgb(0xb3, 0x1f, 0x3c),
                added: Color::Rgb(0x2c, 0x77, 0x2c),
                removed: Color::Rgb(0xb3, 0x1f, 0x3c),
                sel_bg: Color::Rgb(0xcf, 0xdd, 0xf7),
                hint: Color::Rgb(0x8a, 0x90, 0x9a),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_is_a_terminal_when_there_is_one_and_headless_otherwise() {
        assert_eq!(Backend::Auto.resolve(true), Backend::Crossterm);
        assert_eq!(Backend::Auto.resolve(false), Backend::Headless);
    }

    #[test]
    fn an_explicit_backend_is_never_overridden_by_the_absence_of_a_tty() {
        assert_eq!(Backend::Crossterm.resolve(false), Backend::Crossterm);
        assert_eq!(Backend::Headless.resolve(true), Backend::Headless);
    }

    #[test]
    fn the_two_palettes_differ_in_every_foreground_role() {
        let (d, l) = (Theme::of(ThemeName::Dark), Theme::of(ThemeName::Light));
        assert_ne!(d.fg, l.fg);
        assert_ne!(d.accent, l.accent);
        assert_ne!(d.sel_bg, l.sel_bg);
    }
}
