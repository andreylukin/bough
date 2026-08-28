//! Invariant: both renderers are TOTAL. A share of 0.0 and a share of 1.0 both draw a bar, and an
//! inactive signal draws `∅` — never `0.00`, which would read as "no rejections" (§16).

use bough_plugin_drift_watch::DriftFlag;

use crate::dash::{claim_cell, DashRow};

/// The glyph an inactive or uncomputable signal renders as.
pub const UNKNOWN: &str = "\u{2205}";

/// The filled cell of a bar.
const FULL: char = '\u{2588}';
/// The empty cell of a bar.
const EMPTY: char = '\u{2591}';

/// PURE: the name one flag is written under. The flag column is generated from this, so a new
/// `DriftFlag` cannot reach the screen as a blank (the match is exhaustive on purpose).
pub fn flag_name(flag: DriftFlag) -> &'static str {
    match flag {
        DriftFlag::ThoughtLengthUnstable => "thought-length",
        DriftFlag::ToolUseCollapsed => "tool-use",
        DriftFlag::ClaimsMostlyRejected => "claims-rejected",
        DriftFlag::TooFewSamples => "too-few-samples",
    }
}

/// PURE: the flag column. Empty when nothing is flagged — a dash there would read as a flag whose
/// name nobody wrote down.
pub fn flag_column(flags: &[DriftFlag]) -> String {
    flags
        .iter()
        .map(|f| flag_name(*f))
        .collect::<Vec<_>>()
        .join(" ")
}

/// PURE: the rendered line, clipped to `cols`.
///
/// `verdict │ agent │ samples │ thought cv │ tool entropy │ top-tool bar │ claims │ flags`.
/// Clipping is by CHARACTER, and the whole line is one `String`, so a narrow terminal loses the
/// right-hand columns rather than wrapping the row into two.
pub fn line(r: &DashRow, cols: u16, bar_cols: u16) -> String {
    let top = r.top_tools.first();
    let bar_cell = bar(top.map(|t| t.share).unwrap_or(0.0), bar_cols);
    let top_name = top.map(|t| t.tool.as_str()).unwrap_or(UNKNOWN);
    let flags = flag_column(&r.flags);
    let mut line = format!(
        "{g} {agent:<10} n={n:<5} cv={cv:>5.2} H={h:>4.2} {bar} {top} claims={claims}",
        g = r.verdict.glyph(),
        agent = r.agent.to_string(),
        n = r.samples,
        cv = r.thought_cv,
        h = r.tool_entropy,
        bar = bar_cell,
        top = top_name,
        claims = claim_cell(&r.claim_rejection),
    );
    if !flags.is_empty() {
        line.push_str("  ");
        line.push_str(&flags);
    }
    clip(&line, cols)
}

/// PURE: clip to `cols` CHARACTERS. Total: `cols == 0` is the empty string.
pub fn clip(text: &str, cols: u16) -> String {
    text.chars().take(cols as usize).collect()
}

/// PURE: `share` as a `cols`-wide bar. Total: 0.0 and 1.0 both render, and a share outside
/// `[0.0, 1.0]` (or a `NaN` that reached here from an arithmetic nobody expected) is CLAMPED
/// rather than panicking in a frame.
pub fn bar(share: f64, cols: u16) -> String {
    let cols = cols as usize;
    if cols == 0 {
        return String::new();
    }
    let share = if share.is_nan() {
        0.0
    } else {
        share.clamp(0.0, 1.0)
    };
    let filled = (share * cols as f64).round() as usize;
    let filled = filled.min(cols);
    let mut out = String::with_capacity(cols);
    for _ in 0..filled {
        out.push(FULL);
    }
    for _ in filled..cols {
        out.push(EMPTY);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dash::Verdict;
    use bough_plugin_drift_watch::{SignalState, ToolShare};
    use bough_plugin_ledger::AgentName;

    fn row(flags: Vec<DriftFlag>) -> DashRow {
        DashRow {
            agent: AgentName::new("sol"),
            samples: 42,
            thought_cv: 0.31,
            tool_entropy: 0.87,
            top_tools: vec![ToolShare {
                tool: "bash".into(),
                calls: 21,
                share: 0.5,
            }],
            claim_rejection: SignalState::Inactive {
                since: "no claim in the window has been decided".into(),
            },
            verdict: if flags.is_empty() {
                Verdict::Steady
            } else {
                Verdict::Flagged
            },
            flags,
        }
    }

    #[test]
    fn a_zero_share_bar_and_a_full_bar_both_render() {
        // TOTAL: neither end of the range is a special case that draws nothing.
        assert_eq!(bar(0.0, 4).chars().count(), 4);
        assert_eq!(bar(1.0, 4).chars().count(), 4);
        assert_eq!(bar(0.0, 4), "\u{2591}\u{2591}\u{2591}\u{2591}");
        assert_eq!(bar(1.0, 4), "\u{2588}\u{2588}\u{2588}\u{2588}");
        assert_eq!(bar(0.5, 4), "\u{2588}\u{2588}\u{2591}\u{2591}");
        // …and outside the range, and with no columns at all.
        assert_eq!(bar(-1.0, 3), bar(0.0, 3));
        assert_eq!(bar(9.0, 3), bar(1.0, 3));
        assert_eq!(bar(f64::NAN, 3), bar(0.0, 3));
        assert_eq!(bar(0.5, 0), "");
    }

    #[test]
    fn a_line_is_clipped_to_cols() {
        let r = row(vec![DriftFlag::ToolUseCollapsed]);
        let full = line(&r, u16::MAX, 8);
        assert!(full.chars().count() > 20, "the wide line is the whole row");
        for cols in [0u16, 1, 7, 20, 40] {
            let l = line(&r, cols, 8);
            assert!(
                l.chars().count() <= cols as usize,
                "cols={cols} produced {} chars",
                l.chars().count()
            );
            // Clipping is a PREFIX of the wide line: a narrow terminal loses columns, it does not
            // get a different row.
            assert!(full.starts_with(&l), "cols={cols}");
        }
        assert_eq!(line(&r, 0, 8), "");
    }

    #[test]
    fn the_flag_column_names_every_flag() {
        let all = [
            DriftFlag::ThoughtLengthUnstable,
            DriftFlag::ToolUseCollapsed,
            DriftFlag::ClaimsMostlyRejected,
            DriftFlag::TooFewSamples,
        ];
        let col = flag_column(&all);
        for f in all {
            assert!(col.contains(flag_name(f)), "{f:?} is not named in {col:?}");
            assert!(!flag_name(f).is_empty());
        }
        // Every name is distinct: two flags that render the same word are one flag on screen.
        let mut names: Vec<&str> = all.iter().map(|f| flag_name(*f)).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), all.len());
        // Nothing flagged is nothing said.
        assert_eq!(flag_column(&[]), "");
        // …and the line carries the column.
        assert!(line(&row(all.to_vec()), u16::MAX, 8).contains("tool-use"));
    }

    #[test]
    fn an_inactive_claim_never_renders_as_a_zero() {
        let l = line(&row(vec![]), u16::MAX, 8);
        assert!(l.contains(&format!("claims={UNKNOWN}")), "{l}");
        assert!(!l.contains("claims=0.00"), "{l}");
    }
}
