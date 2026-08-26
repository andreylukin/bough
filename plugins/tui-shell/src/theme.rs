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
    pub fn of(_name: ThemeName) -> Theme {
        todo!("WP-2")
    }
}
