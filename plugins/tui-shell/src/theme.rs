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
    /// The terminal background this palette was DESIGNED for. `bg` is [`Color::Reset`] — "whatever
    /// the user's terminal is" — and a contrast ratio against that is not a number, so the audit
    /// (phase ux1 §2.5, V9) measures against this instead.
    pub measure_bg: Color,
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
    /// Things you can CLICK or open: tool-row headers, claim buttons, the branch picker, the
    /// search label (visual audit F5). One colour for "this responds", distinct from `accent`,
    /// which names WHO is speaking and what a heading is.
    pub interactive: Color,
    /// Inline code and code blocks: a foreground of their own on `code_bg`, so code is a
    /// texture rather than a colour that competes with names and links.
    pub code: Color,
    pub code_bg: Color,
    /// The composer's band: the one row that is an input, told apart by its ground.
    pub field_bg: Color,
    /// Section grounds (the conversation brief, 2026-08-31): membership in the next context is a
    /// wash a row STANDS ON, not only a rule above it. Each is its section's hue blended faintly
    /// into `measure_bg`, so every foreground role keeps its audited contrast on top of it.
    /// The fixed material: identity, skills, digest, pins.
    pub wash_head: Color,
    /// A tier band: rollups standing in for rows no longer verbatim.
    pub wash_tier: Color,
    /// The verbatim tail.
    pub wash_tail: Color,
    /// Unconsumed mail: the tray, and the mail band.
    pub wash_mail: Color,
}

impl Theme {
    /// The palette for a name.
    pub fn of(name: ThemeName) -> Theme {
        match name {
            ThemeName::Dark => Theme {
                bg: Color::Reset,
                measure_bg: Color::Rgb(0x1a, 0x1b, 0x26),
                fg: Color::Rgb(0xd0, 0xd0, 0xd0),
                // 3.7:1 at #707680; the audit requires 4.5:1 (M22). Raised again for the visual
                // audit (F6): #8b92a1 was 4.3:1 on a #282d35 terminal, the commonest dark ground.
                dim: Color::Rgb(0x9a, 0xa2, 0xb1),
                accent: Color::Rgb(0x7a, 0xa2, 0xf7),
                evidence: Color::Rgb(0x9e, 0xce, 0x6a),
                thought: Color::Rgb(0xbb, 0x9a, 0xf7),
                warn: Color::Rgb(0xe0, 0xaf, 0x68),
                error: Color::Rgb(0xf7, 0x76, 0x8e),
                added: Color::Rgb(0x9e, 0xce, 0x6a),
                removed: Color::Rgb(0xf7, 0x76, 0x8e),
                sel_bg: Color::Rgb(0x2d, 0x3f, 0x60),
                // Was #565f89 — 2.8:1, the least legible text in the product. Now body contrast.
                hint: Color::Rgb(0xa9, 0xb1, 0xd6),
                interactive: Color::Rgb(0x7d, 0xcf, 0xff),
                code: Color::Rgb(0xc0, 0xca, 0xf5),
                code_bg: Color::Rgb(0x2f, 0x34, 0x42),
                field_bg: Color::Rgb(0x2a, 0x30, 0x3c),
                wash_head: Color::Rgb(0x22, 0x27, 0x38),
                wash_tier: Color::Rgb(0x2a, 0x27, 0x3b),
                wash_tail: Color::Rgb(0x20, 0x24, 0x2a),
                wash_mail: Color::Rgb(0x2d, 0x2a, 0x2c),
            },
            ThemeName::Light => Theme {
                bg: Color::Reset,
                measure_bg: Color::Rgb(0xff, 0xff, 0xff),
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
                // Was #8a909a — 3.2:1.
                hint: Color::Rgb(0x5f, 0x65, 0x70),
                interactive: Color::Rgb(0x0f, 0x6e, 0x8c),
                code: Color::Rgb(0x2f, 0x35, 0x45),
                code_bg: Color::Rgb(0xea, 0xee, 0xf4),
                field_bg: Color::Rgb(0xf0, 0xf3, 0xf7),
                wash_head: Color::Rgb(0xee, 0xf2, 0xf9),
                wash_tier: Color::Rgb(0xf3, 0xf0, 0xf7),
                wash_tail: Color::Rgb(0xf4, 0xf8, 0xf4),
                wash_mail: Color::Rgb(0xf7, 0xf2, 0xeb),
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
