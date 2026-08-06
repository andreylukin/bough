//! The status line BELOW the composer (App.tsx::StatusLine + the ChatMeter
//! facts from Chat.tsx): one dim `meter_line` row — workspace, model, cost,
//! context, live-work counts, the `? help` hint, the `←` out-chip.
//!
//! WAVE-1 NOTE: `meter_line` is format.ts territory (row 1.34); the port lives
//! here `pub(crate)` until format.rs lands, wording and degradation ladder
//! ported verbatim from `src/tui/format.ts::meterLine`.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::{display_width, fmt_tokens, fmt_usd};

/// Below this the context chip raises its voice (format.ts::CTX_WARN_PCT).
pub const CTX_WARN_PCT: i64 = 20;

/// Whole-percent usable context left. None when the limit is unknown — an
/// invented percentage is worse than no chip (format.ts::ctxPctLeft).
pub fn ctx_pct_left(context_tokens: i64, context_limit: Option<i64>) -> Option<i64> {
    let limit = context_limit?;
    if limit <= 0 {
        return None;
    }
    let pct = ((1.0 - context_tokens as f64 / limit as f64) * 100.0).floor() as i64;
    Some(pct.clamp(0, 100))
}

/// The status-line facts (Chat.tsx::ChatMeter). All optional: every absent
/// field degrades to silence, never to a fake value.
#[derive(Default)]
pub struct ChatMeter {
    pub model: Option<String>,
    /// Thinking depth when it is not the default.
    pub effort: Option<String>,
    pub cost_usd: Option<f64>,
    pub context_tokens: Option<i64>,
    pub context_limit: Option<i64>,
    /// Where the turn runs, already shortened. Leads the line.
    pub workspace: Option<String>,
    /// The branch those edits land on.
    pub branch: Option<String>,
    /// Background shells still running — nothing may run with no pixels on screen.
    pub shells: Option<i64>,
    /// Delegated agents and workflow runs still going.
    pub agents: Option<i64>,
    pub runs: Option<i64>,
    /// Append the `? help` hint. The chat sets it; other surfaces need not.
    pub help: bool,
    /// Spawned by another conversation, so `←` goes back.
    pub out: bool,
}

/// format.ts::meterLine — join order and the degradation ladder, verbatim.
pub(crate) fn meter_line(m: &ChatMeter, width: Option<usize>) -> String {
    // Workspace FIRST and at the bottom of the screen, next to the composer.
    let place = |dir: &str| -> String {
        if !dir.is_empty() {
            if let Some(branch) = m.branch.as_deref() {
                return format!("{dir}@{branch}");
            }
        }
        dir.to_string()
    };
    let workspace = place(m.workspace.as_deref().unwrap_or(""));
    // The effort rides the model token: it is a property OF the model choice.
    let model = match (&m.model, &m.effort) {
        (Some(model), Some(effort)) => format!("{model} · {effort}"),
        (Some(model), None) => model.clone(),
        _ => String::new(),
    };
    let cost = match m.cost_usd {
        Some(c) if c > 0.0 => fmt_usd(c),
        _ => String::new(),
    };
    let context = match m.context_tokens {
        Some(t) if t > 0 => match ctx_pct_left(t, m.context_limit) {
            None => format!("{} ctx", fmt_tokens(t)),
            // The chip is the ONLY warning before a turn fails on overflow,
            // and when it warns it says the way OUT.
            Some(pct) if pct <= CTX_WARN_PCT => format!("⚠ {pct}% ctx left — /compact"),
            Some(pct) => format!("{pct}% ctx left"),
        },
        _ => String::new(),
    };
    let counted = |n: Option<i64>, glyph: &str, word: &str| -> String {
        match n {
            Some(n) if n > 0 => {
                format!("{glyph} {n} {word}{}", if n == 1 { "" } else { "s" })
            }
            _ => String::new(),
        }
    };
    let shells = counted(m.shells, "⚙", "shell");
    let agents = counted(m.agents, "◆", "agent");
    let runs = counted(m.runs, "⧉", "run");
    // Glyph-and-number, for widths where the spelled-out words do not fit.
    // What is running must survive degradation.
    let live_bit = |n: Option<i64>, glyph: &str| -> String {
        match n {
            Some(n) if n > 0 => format!("{glyph}{n}"),
            _ => String::new(),
        }
    };
    let live = [
        live_bit(m.shells, "⚙"),
        live_bit(m.agents, "◆"),
        live_bit(m.runs, "⧉"),
    ]
    .into_iter()
    .filter(|s| !s.is_empty())
    .collect::<Vec<_>>()
    .join(" ");
    let help = if m.help { "? help" } else { "" }.to_string();
    let out = if m.out { "← back" } else { "" }.to_string();
    let join = |bits: &[&str]| -> String {
        bits.iter()
            .filter(|b| !b.is_empty())
            .copied()
            .collect::<Vec<_>>()
            .join(" · ")
    };

    let full = join(&[
        &workspace, &model, &cost, &context, &shells, &agents, &runs, &out, &help,
    ]);
    let Some(w) = width else { return full };
    if display_width(&full) <= w {
        return full;
    }

    // Too narrow for everything. Degrade in priority order instead of wrapping
    // onto a second row. `out` rides with `help` down the ladder: the chip that
    // says how to LEAVE a conversation you did not open on purpose is worth
    // more than a token count.
    let ws = m.workspace.as_deref().unwrap_or("");
    let base_name = ws.trim_end_matches('/').rsplit('/').next().unwrap_or("");
    let base = place(base_name);
    for candidate in [
        join(&[
            &base, &model, &cost, &context, &shells, &agents, &runs, &out, &help,
        ]),
        join(&[
            &model, &cost, &context, &shells, &agents, &runs, &out, &help,
        ]),
        join(&[&cost, &context, &live, &out, &help]),
        join(&[&cost, &context, &out, &live]),
        join(&[&context, &live]),
        join(&[&context]),
    ] {
        if display_width(&candidate) <= w {
            return candidate;
        }
    }
    // Last resort: hard truncate (v1 strings carry no escapes).
    let mut out_s = String::new();
    let mut used = 0usize;
    for ch in full.chars() {
        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + cw > w.saturating_sub(1) {
            break;
        }
        out_s.push(ch);
        used += cw;
    }
    out_s.push('…');
    out_s
}

/// One dim row; renders nothing when there is nothing to say (StatusLine).
pub fn render_status(m: &ChatMeter, area: Rect, buf: &mut Buffer) {
    let text = meter_line(m, Some(area.width as usize));
    if text.is_empty() {
        return;
    }
    let line = Line::from(Span::styled(
        text,
        Style::default().add_modifier(Modifier::DIM),
    ));
    buf.set_line(area.x, area.y, &line, area.width);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_in_order_and_effort_rides_the_model() {
        let m = ChatMeter {
            workspace: Some("~/repos/bough".into()),
            branch: Some("main".into()),
            model: Some("claude-sonnet".into()),
            effort: Some("high".into()),
            cost_usd: Some(0.042),
            context_tokens: Some(50_000),
            context_limit: Some(200_000),
            shells: Some(1),
            agents: Some(2),
            help: true,
            ..Default::default()
        };
        assert_eq!(
            meter_line(&m, None),
            "~/repos/bough@main · claude-sonnet · high · $0.042 · 75% ctx left · ⚙ 1 shell · ◆ 2 agents · ? help"
        );
    }

    #[test]
    fn unknown_context_limit_shows_tokens_never_an_invented_percentage() {
        let m = ChatMeter {
            context_tokens: Some(1234),
            ..Default::default()
        };
        assert_eq!(meter_line(&m, None), "1.2k ctx");
    }

    #[test]
    fn low_context_warns_and_names_the_way_out() {
        let m = ChatMeter {
            context_tokens: Some(190_000),
            context_limit: Some(200_000),
            ..Default::default()
        };
        assert_eq!(meter_line(&m, None), "⚠ 5% ctx left — /compact");
    }

    #[test]
    fn degrades_to_the_basename_then_to_the_live_glyphs() {
        let m = ChatMeter {
            workspace: Some("/very/long/absolute/path/to/the/workspace".into()),
            model: Some("claude-sonnet".into()),
            context_tokens: Some(50_000),
            context_limit: Some(200_000),
            shells: Some(2),
            help: true,
            ..Default::default()
        };
        // Wide enough for the basename ladder rung.
        let narrow = meter_line(&m, Some(70));
        assert!(narrow.starts_with("workspace ·"), "{narrow}");
        // The live glyphs survive further degradation.
        let tiny = meter_line(&m, Some(28));
        assert!(tiny.contains("⚙2"), "{tiny}");
    }

    #[test]
    fn out_chip_and_binding_share_the_condition() {
        let m = ChatMeter {
            out: true,
            help: true,
            ..Default::default()
        };
        assert_eq!(meter_line(&m, None), "← back · ? help");
    }
}
