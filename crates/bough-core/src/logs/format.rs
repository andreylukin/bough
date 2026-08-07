//! Port of `src/logs/format.ts` — the three renderings of an `Analysis`, and
//! the shared vocabulary between them.
//!
//! ONE MODULE, THREE FORMATS, because they are three views of one decision about
//! what matters — and split across three files they drift, which shows up as a
//! `--json` field the `--llm` view stopped reporting and nobody noticed.
//!
//!   `--llm`   a language model reading the output inside a turn. Optimizes for
//!             tokens and for unambiguous structure.
//!   `--human` a person at a terminal. Optimizes for scanning.
//!   `--json`  a program. `Analysis` verbatim, so the contract is `types.rs`.
//!
//! WHAT NONE OF THEM DO. No format carries a footer advertising anything. "The
//! `--llm` output is fed into a model's context window on every invocation;
//! every line that is not about the log is a line that displaces one that is."
//!
//! NUMBERS ARE NEVER SILENTLY ROUNDED INTO A LIE. Approximate values are marked
//! `~`, untrustworthy rankings are omitted rather than shown with a caveat
//! nobody reads, and truncation is stated in the header.

use super::anomaly::{fmt, to_fixed};
use super::types::{Analysis, Pattern, Severity, VarKind, VarSummary};

// ---------------------------------------------------------------------------
// Shared
// ---------------------------------------------------------------------------

/// `1234567` → `1,234,567`. `toLocaleString("en-US")`, done by hand.
pub fn n(v: u64) -> String {
    let digits = v.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// A share as a percentage with just enough precision to distinguish small ones.
fn pct(share: f64) -> String {
    let p = share * 100.0;
    if p >= 10.0 {
        return format!("{}%", to_fixed(p, 0));
    }
    if p >= 1.0 {
        return format!("{}%", to_fixed(p, 1));
    }
    format!("{}%", to_fixed(p, 2))
}

/// Civil date from a day count since the epoch (Howard Hinnant's algorithm).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m as u32, d as u32)
}

/// `new Date(ms).toISOString()`.
pub fn iso_stamp(ms: i64) -> String {
    let days = ms.div_euclid(86_400_000);
    let rem = ms.rem_euclid(86_400_000);
    let (y, mo, d) = civil_from_days(days);
    let h = rem / 3_600_000;
    let mi = (rem % 3_600_000) / 60_000;
    let s = (rem % 60_000) / 1000;
    let milli = rem % 1000;
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}.{milli:03}Z")
}

/// An epoch millisecond as a compact UTC stamp:
/// `toISOString().replace("T", " ").replace(/\.\d+Z$/, "Z")`.
fn stamp(ms: i64) -> String {
    let iso = iso_stamp(ms);
    let (head, _) = iso.split_at(iso.len() - 5); // drop ".mmmZ"
    format!("{}Z", head.replacen('T', " ", 1))
}

/// One variable slot as a single line of text, without any leading indent.
///
/// Shared by `--llm` and `--human` because deciding WHICH facts about a slot are
/// worth a line is the substantive choice, and it should not be made twice. The
/// order is fixed — identity, then spread, then magnitude.
fn slot_line(v: &VarSummary) -> String {
    let mut parts: Vec<String> = vec![format!("slot {}", v.slot), v.kind.as_str().to_string()];

    if v.kind == VarKind::Id {
        // For an identifier the only interesting fact is that it does not
        // repeat, and the top values were suppressed upstream precisely so
        // nothing here implies otherwise.
        parts.push(format!("~{} distinct / {}", n(v.unique), n(v.count)));
    } else if v.unique == 1 && v.top.as_ref().and_then(|t| t.first()).is_some() {
        let first = v.top.as_ref().and_then(|t| t.first()).expect("checked");
        parts.push(format!("always {}", first.value));
    } else {
        parts.push(format!("{} unique", n(v.unique)));
        if let Some(top) = v.top.as_ref().filter(|t| !t.is_empty()) {
            parts.push(
                top.iter()
                    .map(|t| format!("{} ({})", t.value, pct(t.share)))
                    .collect::<Vec<_>>()
                    .join(", "),
            );
        }
    }

    if let Some(num) = &v.numeric {
        let u = num.unit.as_deref();
        parts.push(format!(
            "p50={} p90={} p99={} max={}",
            fmt(num.p50, u),
            fmt(num.p90, u),
            fmt(num.p99, u),
            fmt(num.max, u)
        ));
    }
    parts.join("  ")
}

// ---------------------------------------------------------------------------
// LLM
// ---------------------------------------------------------------------------

/// Compact markdown, ordered so the first thing read is the most consequential.
///
/// PROBLEMS FIRST IS THE WHOLE LAYOUT. A model reads top to bottom and weights
/// early content more heavily, so putting a 96%-of-traffic INFO pattern first
/// would spend the most valuable position in the context on the least actionable
/// fact in the file.
pub fn to_llm(a: &Analysis) -> String {
    let mut out: Vec<String> = Vec::new();

    let mut header = vec![format!(
        "# {} lines → {} patterns",
        n(a.lines),
        n(a.pattern_count as u64)
    )];
    if let Some(span) = a.time_span {
        header.push(format!("span {} … {}", stamp(span.from), stamp(span.to)));
    }
    if a.patterns.len() < a.pattern_count {
        header.push(format!("showing top {}", a.patterns.len()));
    }
    out.push(header.join(" · "));
    if a.truncated {
        out.push(
            "> NOTE: the cluster cap was reached and rare patterns were evicted; counts are lower bounds."
                .to_string(),
        );
    }
    out.push(String::new());

    let severe: Vec<&Pattern> = a
        .patterns
        .iter()
        .filter(|p| matches!(p.severity, Severity::Error | Severity::Fatal))
        .collect();
    let rest: Vec<&Pattern> = a
        .patterns
        .iter()
        .filter(|p| !matches!(p.severity, Severity::Error | Severity::Fatal))
        .collect();

    if !severe.is_empty() {
        out.push(format!("## Problems ({})", severe.len()));
        out.push(String::new());
        for p in &severe {
            out.extend(llm_pattern(p, true));
        }
    }
    if !rest.is_empty() {
        out.push(format!("## Everything else ({})", rest.len()));
        out.push(String::new());
        for p in &rest {
            out.extend(llm_pattern(p, false));
        }
    }

    if !a.correlations.is_empty() {
        out.push("## Related".to_string());
        out.push(String::new());
        // Phrased as observations, never as causes: co-occurrence cannot
        // distinguish "A caused B" from "C caused both", and a model reading
        // this will act on whatever verb it is given.
        for c in &a.correlations {
            out.push(format!("- {}", c.detail));
        }
        out.push(String::new());
    }

    format!("{}\n", out.join("\n").trim_end())
}

fn llm_pattern(p: &Pattern, with_example: bool) -> Vec<String> {
    let mut out = Vec::new();
    out.push(format!(
        "### #{} [{}] {} lines ({})",
        p.id,
        p.severity.as_str().to_uppercase(),
        n(p.count),
        pct(p.share)
    ));
    out.push("```".to_string());
    out.push(p.template.clone());
    out.push("```".to_string());
    for v in &p.vars {
        // Slots that never varied are dropped from the LLM view. They are
        // constants of the log statement, not variables of it, and one line each
        // is a real cost in a format whose entire premise is that it is cheap to
        // read.
        if v.unique == 1 && v.numeric.is_none() {
            continue;
        }
        out.push(format!("- {}", slot_line(v)));
    }
    for an in &p.anomalies {
        out.push(format!("- ⚠ {}", an.detail));
    }
    if with_example {
        if let Some(example) = p.examples.first() {
            out.push(format!("- e.g. `{example}`"));
        }
    }
    out.push(String::new());
    out
}

// ---------------------------------------------------------------------------
// JSON
// ---------------------------------------------------------------------------

/// `Analysis` verbatim. The contract is `types.rs`; nothing is invented here.
pub fn to_json(a: &Analysis) -> String {
    format!(
        "{}\n",
        serde_json::to_string_pretty(a).unwrap_or_else(|_| "{}".to_string())
    )
}

// ---------------------------------------------------------------------------
// Human
// ---------------------------------------------------------------------------

mod ansi {
    pub const RESET: &str = "\u{1b}[0m";
    pub const DIM: &str = "\u{1b}[2m";
    pub const BOLD: &str = "\u{1b}[1m";
    pub const RED: &str = "\u{1b}[31m";
    pub const YELLOW: &str = "\u{1b}[33m";
    pub const BLUE: &str = "\u{1b}[34m";
    pub const GREEN: &str = "\u{1b}[32m";
    pub const GREY: &str = "\u{1b}[90m";
}

fn severity_colour(s: Severity) -> String {
    match s {
        Severity::Fatal => format!("{}{}", ansi::RED, ansi::BOLD),
        Severity::Error => ansi::RED.to_string(),
        Severity::Warn => ansi::YELLOW.to_string(),
        Severity::Info => ansi::BLUE.to_string(),
        Severity::Debug => ansi::GREY.to_string(),
    }
}

/// Terminal output for a person.
///
/// `colour` is a parameter rather than something detected here, because
/// detection needs a TTY probe and this module is pure — the CLI decides, this
/// renders.
pub fn to_human(a: &Analysis, colour: bool, width: usize) -> String {
    let c = |code: &str, s: &str| -> String {
        if colour {
            format!("{code}{s}{}", ansi::RESET)
        } else {
            s.to_string()
        }
    };
    let mut out: Vec<String> = Vec::new();

    let head = format!(
        "{} lines → {} patterns",
        n(a.lines),
        n(a.pattern_count as u64)
    );
    let reduction = if a.lines > 0 {
        1.0 - a.pattern_count as f64 / a.lines as f64
    } else {
        0.0
    };
    out.push(format!(
        "{}{}",
        c(ansi::BOLD, &head),
        c(ansi::DIM, &format!("  ({} reduction)", pct(reduction)))
    ));
    if let Some(span) = a.time_span {
        out.push(c(
            ansi::DIM,
            &format!("{} … {}", stamp(span.from), stamp(span.to)),
        ));
    }
    if a.truncated {
        out.push(c(
            ansi::YELLOW,
            "cluster cap reached — rare patterns evicted, counts are lower bounds",
        ));
    }
    out.push(String::new());

    // The bar is scaled to the LARGEST pattern shown, not to the total, so the
    // smaller patterns remain visible.
    let peak = a.patterns.iter().map(|p| p.count).max().unwrap_or(1).max(1);
    let bar_width = width.saturating_sub(56).clamp(10, 24);

    for p in &a.patterns {
        let sev = format!("{:<5}", p.severity.as_str().to_uppercase());
        let filled = ((p.count as f64 / peak as f64) * bar_width as f64 + 0.5).floor() as usize;
        let filled = filled.max(1).min(bar_width);
        let bar = format!(
            "{}{}",
            "█".repeat(filled),
            c(ansi::DIM, &"·".repeat(bar_width - filled))
        );
        out.push(format!(
            "{} {} {} {:>9} {}",
            c(ansi::DIM, &format!("#{:>2}", p.id)),
            c(&severity_colour(p.severity), &sev),
            bar,
            n(p.count),
            c(ansi::DIM, &format!("({})", pct(p.share)))
        ));
        out.push(format!("    {}", c(ansi::BOLD, &p.template)));
        for v in &p.vars {
            if v.unique == 1 && v.numeric.is_none() {
                continue;
            }
            out.push(c(ansi::DIM, &format!("      {}", slot_line(v))));
        }
        for an in &p.anomalies {
            out.push(format!("      {} {}", c(ansi::YELLOW, "⚠"), an.detail));
        }
        if let Some(example) = p.examples.first() {
            out.push(c(
                ansi::GREY,
                &format!("      e.g. {}", truncate(example, width.saturating_sub(12))),
            ));
        }
        out.push(String::new());
    }

    if !a.correlations.is_empty() {
        out.push(c(ansi::BOLD, "Related"));
        for cr in &a.correlations {
            out.push(format!("  {} {}", c(ansi::GREEN, "↔"), cr.detail));
        }
        out.push(String::new());
    }

    out.join("\n")
}

/// Cut to width with an ellipsis, so one long line cannot wreck the layout.
fn truncate(s: &str, width: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if width < 8 || chars.len() <= width {
        return s.to_string();
    }
    let mut out: String = chars[..width - 1].iter().collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logs::analyze::{analyze, AnalyzeOptions};

    fn sample() -> Analysis {
        // The same 65-line fixture the CLI suite uses.
        let mut lines: Vec<String> = Vec::new();
        let base = crate::logs::timestamp::date_utc_ms(2024, 0, 15, 14, 0, 0, 0);
        for i in 0..60i64 {
            lines.push(format!(
                "{} INFO Request from 10.0.1.{} completed in {}ms status=200",
                iso_stamp(base + i * 1000),
                i % 4,
                20 + (i % 30)
            ));
        }
        for i in 0..5i64 {
            lines.push(format!(
                "{} ERROR Timeout connecting to 10.0.9.{i} after {}ms",
                iso_stamp(base + i * 1000),
                5000 + i
            ));
        }
        analyze(lines, AnalyzeOptions::default())
    }

    #[test]
    fn thousands_separators_match_en_us() {
        assert_eq!(n(0), "0");
        assert_eq!(n(999), "999");
        assert_eq!(n(1000), "1,000");
        assert_eq!(n(1_234_567), "1,234,567");
    }

    #[test]
    fn the_percentage_ladder_keeps_small_shares_distinguishable() {
        assert_eq!(pct(0.923), "92%");
        assert_eq!(pct(0.0512), "5.1%");
        assert_eq!(pct(0.0004), "0.04%");
    }

    #[test]
    fn a_stamp_is_a_compact_utc_instant() {
        let ms = crate::logs::timestamp::date_utc_ms(2024, 0, 15, 14, 22, 1, 0);
        assert_eq!(iso_stamp(ms), "2024-01-15T14:22:01.000Z");
        assert_eq!(stamp(ms), "2024-01-15 14:22:01Z");
    }

    #[test]
    fn the_llm_view_leads_with_problems() {
        let text = to_llm(&sample());
        let problems = text.find("## Problems").expect("no problems section");
        let rest = text.find("## Everything else").expect("no rest section");
        assert!(
            rest > problems,
            "the INFO pattern was rendered above the errors"
        );
        assert!(text.starts_with("# 65 lines"));
        assert!(text.ends_with('\n'));
        assert!(!text.ends_with("\n\n"), "more than one trailing newline");
    }

    #[test]
    fn no_format_advertises_anything() {
        let a = sample();
        for text in [to_llm(&a), to_json(&a), to_human(&a, false, 80)] {
            let lower = text.to_lowercase();
            assert!(!lower.contains("powered by"));
            assert!(!lower.contains("learn more"));
            assert!(!lower.contains("http://") && !lower.contains("https://"));
        }
    }

    #[test]
    fn json_uses_the_ts_field_names() {
        let json = to_json(&sample());
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["lines"], 65);
        assert!(parsed["patternCount"].is_number());
        assert!(parsed["patterns"].is_array());
        assert!(parsed["truncated"].is_boolean());
        assert!(parsed["timeSpan"]["from"].is_number());
        assert!(parsed["bucketMs"].is_number());
        let p = &parsed["patterns"][0];
        assert!(p["template"]
            .as_str()
            .map(|s| !s.is_empty())
            .unwrap_or(false));
        assert!(p["vars"].is_array());
        assert!(p["firstSeen"].is_number());
    }

    #[test]
    fn json_omits_absent_optionals_rather_than_nulling_them() {
        let a = analyze(
            ["build started", "build finished"],
            AnalyzeOptions::default(),
        );
        let parsed: serde_json::Value = serde_json::from_str(&to_json(&a)).unwrap();
        assert!(parsed.get("timeSpan").is_none());
        assert!(parsed.get("bucketMs").is_none());
    }

    #[test]
    fn the_human_view_says_what_it_compressed() {
        let text = to_human(&sample(), false, 80);
        assert!(text.contains("65 lines → "));
        assert!(text.contains(" patterns"));
        assert!(!text.contains('\u{1b}'), "ANSI survived colour=false");
    }

    #[test]
    fn the_human_view_colours_when_asked() {
        let text = to_human(&sample(), true, 80);
        assert!(text.contains('\u{1b}'), "a terminal got no colour");
    }

    #[test]
    fn a_long_example_line_cannot_wreck_the_layout() {
        assert_eq!(truncate("short", 80), "short");
        assert_eq!(truncate("abcdefghij", 8), "abcdefg…");
        // Below the floor of 8 the string is left alone rather than reduced to
        // an ellipsis and one letter.
        assert_eq!(truncate("abcdefghij", 5), "abcdefghij");
    }
}
