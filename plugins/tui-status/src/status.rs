//! Invariant: `render` queries NOTHING. The row's listeners assemble a [`StatusView`] and the
//! drawing is a pure function of it (phase ux1 §2.5).
//!
//! And one rule the whole module exists to hold: **the status line is exactly one row.** Nothing
//! here wraps, nothing overflows, and a value the ledger has not recorded renders as `—` rather
//! than as a plausible zero (M24) — this is the most-read chrome in the product, so a fabricated
//! number here is the most expensive lie the surface can tell.

use std::path::{Path, PathBuf};
use std::time::Duration;

use bough_plugin_tui_shell::Theme;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

/// What an unknown value renders as. Never `0`, never a blank.
pub const UNKNOWN: &str = "—";

/// What separates two fields.
pub const SEP: &str = " · ";

/// Everything the line can show.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StatusView {
    /// `"bough 0.x"`.
    pub product: String,
    /// From `ctx.workspace`, NOT from `std::env` (B5).
    pub cwd: Option<PathBuf>,
    /// The user's home, so a cwd inside it renders as `~/…`. Set by the row from `bough-util`;
    /// kept on the view so `fields` and `status_line` measure the SAME string (phase ux1 §2.5,
    /// deviation D-ux1-4a).
    pub home: Option<PathBuf>,
    /// `StatusConfig::cwd_max`, on the view for the same reason.
    pub cwd_max: u16,
    /// Last `request/header.call.model`.
    pub model: Option<String>,
    /// `100 - 100 * projection_tokens / budget`.
    pub context_left: Option<u8>,
    /// Σ `usage/round.cost_usd` for this home. `None` renders as `—`, never as `$0.00`.
    pub cost_usd: Option<f64>,
    pub running: bool,
    pub elapsed: Option<Duration>,
    pub spinner_frame: char,
    pub hints: Vec<(String, String)>,
}

/// A field of the line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Product,
    Cwd,
    Model,
    Context,
    Cost,
    Elapsed,
    Hints,
}

/// The order fields are RENDERED in, left to right.
pub const RENDER_ORDER: [Field; 7] = [
    Field::Product,
    Field::Cwd,
    Field::Model,
    Field::Context,
    Field::Cost,
    Field::Elapsed,
    Field::Hints,
];

/// The order fields are DROPPED in as the width shrinks — first entry goes first.
///
/// The reasoning, which is the whole of the design: the hints are learnable and therefore the
/// cheapest thing to lose; the cwd is the longest field and a user in a narrow terminal usually
/// knows where they are; money and context are numbers you glance at, not act on; the model
/// matters more; and the LAST two things to go are the spinner — the only thing on screen saying
/// the harness is alive (M32) — and the product's own name.
pub const DROP_ORDER: [Field; 7] = [
    Field::Hints,
    Field::Cwd,
    Field::Cost,
    Field::Context,
    Field::Model,
    Field::Elapsed,
    Field::Product,
];

/// The drop order in force for this view. While a turn is RUNNING the hints are the last thing
/// to go rather than the first: `esc interrupt` is the stop key, and the audit's blocker was not
/// that Esc did nothing but that nobody was ever told it was there (phase ux1 §2.4). An idle
/// screen can afford to teach; a running one has to.
pub fn drop_order(v: &StatusView) -> [Field; 7] {
    if v.running {
        [
            Field::Cwd,
            Field::Cost,
            Field::Context,
            Field::Model,
            Field::Hints,
            Field::Elapsed,
            Field::Product,
        ]
    } else {
        DROP_ORDER
    }
}

/// PURE: the text of one field, or `None` when the view has no such field at all.
///
/// A field the view KNOWS is missing (no cost yet) still renders — as [`UNKNOWN`]. A field that
/// does not apply (the elapsed clock while nothing runs) is absent.
pub fn field_text(v: &StatusView, f: Field) -> Option<String> {
    match f {
        Field::Product => {
            if v.product.is_empty() {
                None
            } else {
                Some(v.product.clone())
            }
        }
        Field::Cwd => v
            .cwd
            .as_ref()
            .map(|p| elide_path(p, v.home.as_deref(), v.cwd_max)),
        Field::Model => Some(
            v.model
                .as_deref()
                .map(short_model)
                .unwrap_or_else(|| UNKNOWN.to_string()),
        ),
        Field::Context => Some(match v.context_left {
            Some(pct) => format!("{pct}% ctx"),
            None => format!("{UNKNOWN} ctx"),
        }),
        Field::Cost => Some(match v.cost_usd {
            Some(c) => money(c),
            None => UNKNOWN.to_string(),
        }),
        Field::Elapsed => {
            if !v.running {
                return None;
            }
            let frame = if v.spinner_frame == '\0' {
                ' '
            } else {
                v.spinner_frame
            };
            Some(match v.elapsed {
                Some(d) => format!("{frame} {}", clock(d)),
                None => format!("{frame} running"),
            })
        }
        Field::Hints => {
            if v.hints.is_empty() {
                return None;
            }
            Some(
                v.hints
                    .iter()
                    .map(|(k, m)| format!("{k} {m}"))
                    .collect::<Vec<_>>()
                    .join(SEP),
            )
        }
    }
}

/// PURE: a dollar amount that never rounds a real cost to `$0.00`.
pub fn money(c: f64) -> String {
    if c > 0.0 && c < 0.01 {
        format!("${c:.4}")
    } else {
        format!("${c:.2}")
    }
}

/// PURE: an elapsed clock, seconds under a minute and `m`+`s` above it.
pub fn clock(d: Duration) -> String {
    let s = d.as_secs();
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else {
        format!("{}h{:02}m", s / 3600, (s % 3600) / 60)
    }
}

/// PURE: the model id as a person reads it. A release id carries a trailing snapshot date that
/// tells the reader nothing and costs nine cells on the one row that also has to fit the cwd, the
/// cost and the key hints — so it goes, and nothing else does: the family and the version are the
/// parts anyone says out loud.
pub fn short_model(id: &str) -> String {
    let trimmed = id.strip_suffix(|_: char| false).unwrap_or(id);
    match trimmed.rsplit_once('-') {
        Some((head, tail)) if tail.len() == 8 && tail.chars().all(|c| c.is_ascii_digit()) => {
            head.to_string()
        }
        _ => trimmed.to_string(),
    }
}

/// Display width of a string, in terminal cells.
fn cells(s: &str) -> usize {
    s.chars().count()
}

/// PURE: the fields that survive at `width`, in RENDER order. Nothing overflows, nothing wraps —
/// the status line is exactly one row (M9).
pub fn fields(v: &StatusView, width: u16) -> Vec<Field> {
    let present: Vec<(Field, usize)> = RENDER_ORDER
        .iter()
        .filter_map(|f| field_text(v, *f).map(|t| (*f, cells(&t))))
        .collect();
    let mut kept: Vec<Field> = present.iter().map(|(f, _)| *f).collect();
    let total = |kept: &Vec<Field>| -> usize {
        if kept.is_empty() {
            return 0;
        }
        let text: usize = present
            .iter()
            .filter(|(f, _)| kept.contains(f))
            .map(|(_, n)| *n)
            .sum();
        // `SEP.chars().count()`, never `SEP.len()`: the separator is FOUR bytes and THREE cells,
        // and measuring in bytes is how a line that "fits" overflows by one cell per field.
        text + cells(SEP) * (kept.len() - 1)
    };
    for f in drop_order(v) {
        if total(&kept) <= width as usize {
            break;
        }
        kept.retain(|k| *k != f);
    }
    // Even one field can be too wide for a very narrow terminal; `status_line` clips, and `fields`
    // reports what it kept, which is at most one field in that case.
    kept
}

/// PURE: `(view, width, theme) -> Line`. Every span names a theme ROLE — never a literal colour.
///
/// The hints are pushed to the RIGHT edge when there is room for them, because a hint list that
/// moves as the cwd changes length is a hint list nobody's eye can find.
pub fn status_line(v: &StatusView, width: u16, theme: &Theme) -> Line<'static> {
    let kept = fields(v, width);
    let sep = Style::default().fg(theme.dim);
    let mut left: Vec<Span<'static>> = Vec::new();
    let mut right: Vec<Span<'static>> = Vec::new();
    for f in kept.iter() {
        let Some(text) = field_text(v, *f) else {
            continue;
        };
        let style = Style::default().fg(role(*f, v, theme));
        if *f == Field::Hints {
            right.push(Span::styled(text, style));
        } else {
            if !left.is_empty() {
                left.push(Span::styled(SEP.to_string(), sep));
            }
            left.push(Span::styled(text, style));
        }
    }
    let used: usize = left
        .iter()
        .chain(right.iter())
        .map(|s| cells(&s.content))
        .sum();
    let mut out = left;
    if !right.is_empty() {
        let pad = (width as usize).saturating_sub(used).max(cells(SEP));
        out.push(Span::styled(" ".repeat(pad), sep));
        out.extend(right);
    }
    clip_to(Line::from(out), width)
}

/// The theme role a field draws in.
fn role(f: Field, v: &StatusView, theme: &Theme) -> ratatui::style::Color {
    match f {
        Field::Product => theme.accent,
        Field::Cwd => theme.fg,
        Field::Model => theme.fg,
        // A context budget running out is the one number on this line that is a WARNING.
        Field::Context => match v.context_left {
            Some(pct) if pct <= 10 => theme.error,
            Some(pct) if pct <= 25 => theme.warn,
            _ => theme.dim,
        },
        Field::Cost => theme.dim,
        Field::Elapsed => theme.accent,
        Field::Hints => theme.hint,
    }
}

/// PURE: hard-clip a line to `width`. The last defence of "exactly one row": whatever the fields
/// decided, nothing leaves this module wider than the terminal.
pub fn clip_to(line: Line<'static>, width: u16) -> Line<'static> {
    let width = width as usize;
    let total: usize = line.spans.iter().map(|s| cells(&s.content)).sum();
    if total <= width {
        return line;
    }
    let mut out: Vec<Span<'static>> = Vec::new();
    let mut used = 0usize;
    for span in line.spans.into_iter() {
        if used >= width {
            break;
        }
        let n = cells(&span.content);
        if used + n <= width {
            used += n;
            out.push(span);
        } else {
            let text: String = span.content.chars().take(width - used).collect();
            used = width;
            out.push(Span::styled(text, span.style));
        }
    }
    Line::from(out)
}

/// PURE: a path elided in the MIDDLE (`~/repos/bou…/ux/cwd`), never at the end — the last
/// component is the one a user checks (B5).
///
/// `max` of `0` means "no cap": the caller has already decided the line has room.
pub fn elide_path(p: &Path, home: Option<&Path>, max: u16) -> String {
    let full = match home.and_then(|h| p.strip_prefix(h).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_string(),
        Some(rest) => format!("~/{}", rest.display()),
        None => p.display().to_string(),
    };
    let max = max as usize;
    if max == 0 || cells(&full) <= max {
        return full;
    }
    // The last component, with its separator, is what survives.
    let tail = match full.rfind('/') {
        Some(i) => full[i..].to_string(),
        None => return keep_tail(&full, max),
    };
    let tail_cells = cells(&tail);
    if tail_cells + 2 > max {
        // No room for a head at all: keep the tail, which is the component that matters.
        return keep_tail(&full, max);
    }
    let head_room = max - 1 - tail_cells;
    let head: String = full.chars().take(head_room).collect();
    format!("{head}…{tail}")
}

/// The last `max` cells of a string, marked as cut.
fn keep_tail(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if cells(s) <= max {
        return s.to_string();
    }
    let keep = max - 1;
    let start = cells(s) - keep;
    let tail: String = s.chars().skip(start).collect();
    format!("…{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unknown_cost_is_a_dash_and_never_a_zero() {
        let v = StatusView {
            cost_usd: None,
            ..Default::default()
        };
        assert_eq!(field_text(&v, Field::Cost).as_deref(), Some(UNKNOWN));
        let v = StatusView {
            cost_usd: Some(0.0),
            ..Default::default()
        };
        assert_eq!(field_text(&v, Field::Cost).as_deref(), Some("$0.00"));
    }

    #[test]
    fn a_cost_under_a_cent_is_not_rounded_away() {
        assert_eq!(money(0.0042), "$0.0042");
        assert_eq!(money(1.5), "$1.50");
    }

    #[test]
    fn the_elapsed_field_exists_only_while_something_runs() {
        let mut v = StatusView::default();
        assert_eq!(field_text(&v, Field::Elapsed), None);
        v.running = true;
        v.spinner_frame = '⠋';
        v.elapsed = Some(Duration::from_secs(75));
        assert_eq!(field_text(&v, Field::Elapsed).as_deref(), Some("⠋ 1m15s"));
    }
}
