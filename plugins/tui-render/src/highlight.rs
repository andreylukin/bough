//! Invariant: an UNKNOWN extension returns UNSTYLED lines rather than guessing a syntax. A wrong
//! highlight reads as a claim about the code; no highlight reads as what it is.

use std::sync::OnceLock;

use bough_plugin_tui_shell::Theme;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use syntect::easy::HighlightLines;
use syntect::highlighting::Theme as SynTheme;
use syntect::parsing::{SyntaxReference, SyntaxSet};
use two_face::theme::EmbeddedThemeName;

struct Assets {
    syntaxes: SyntaxSet,
    dark: SynTheme,
    light: SynTheme,
}

/// syntect + two-face are loaded ONCE: the syntax dump is megabytes and every tool call would
/// otherwise pay for it.
fn assets() -> &'static Assets {
    static ASSETS: OnceLock<Assets> = OnceLock::new();
    ASSETS.get_or_init(|| {
        let themes = two_face::theme::extra();
        Assets {
            syntaxes: two_face::syntax::extra_no_newlines(),
            dark: themes.get(EmbeddedThemeName::Base16OceanDark).clone(),
            light: themes.get(EmbeddedThemeName::Base16OceanLight).clone(),
        }
    })
}

/// The palette a `Theme` asks for. The role struct carries no name, so the background decides,
/// which is the same thing the terminal shows.
fn syntect_theme(theme: &Theme) -> &'static SynTheme {
    let a = assets();
    if is_dark(theme.bg) {
        &a.dark
    } else {
        &a.light
    }
}

fn is_dark(c: Color) -> bool {
    match c {
        Color::Rgb(r, g, b) => (0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32) < 128.0,
        Color::White | Color::Gray => false,
        _ => true,
    }
}

/// The syntax for an extension or a fence language, or `None` when nothing matches.
fn syntax_for(ext: Option<&str>) -> Option<&'static SyntaxReference> {
    let ext = ext?.trim().trim_start_matches('.');
    if ext.is_empty() {
        return None;
    }
    let set = &assets().syntaxes;
    set.find_syntax_by_extension(ext)
        .or_else(|| set.find_syntax_by_token(ext))
}

/// A reusable highlighter, so a multi-line block keeps its parse state across lines.
pub(crate) struct Highlighter {
    inner: Option<HighlightLines<'static>>,
}

impl Highlighter {
    pub(crate) fn new(ext: Option<&str>, theme: &Theme) -> Highlighter {
        Highlighter {
            inner: syntax_for(ext).map(|s| HighlightLines::new(s, syntect_theme(theme))),
        }
    }

    /// `true` when a syntax was actually found; a caller uses it to keep its own role colour.
    pub(crate) fn active(&self) -> bool {
        self.inner.is_some()
    }

    /// One line (no trailing newline) into styled spans. Unstyled when no syntax matched.
    pub(crate) fn line(&mut self, text: &str, theme: &Theme) -> Vec<Span<'static>> {
        let plain = || {
            vec![Span::styled(
                text.to_string(),
                Style::default().fg(theme.fg),
            )]
        };
        let Some(h) = self.inner.as_mut() else {
            return plain();
        };
        let Ok(ranges) = h.highlight_line(text, &assets().syntaxes) else {
            return plain();
        };
        ranges
            .into_iter()
            .filter(|(_, t)| !t.is_empty())
            .map(|(style, t)| {
                let c = style.foreground;
                Span::styled(
                    t.to_string(),
                    Style::default().fg(Color::Rgb(c.r, c.g, c.b)),
                )
            })
            .collect()
    }
}

/// syntect + two-face, fancy-regex, loaded once through a `OnceLock`. An unknown extension
/// returns unstyled lines rather than guessing.
pub fn highlight(code: &str, ext: Option<&str>, theme: &Theme) -> Vec<Line<'static>> {
    let mut h = Highlighter::new(ext, theme);
    code.split('\n')
        .map(|l| Line::from(h.line(l.strip_suffix('\r').unwrap_or(l), theme)))
        .collect()
}
