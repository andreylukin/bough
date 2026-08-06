//! Port of `src/logs/analyze.ts` — the pipeline: lines in, an `Analysis` out.
//!
//! ```text
//! strip timestamp → mask values → tokenize → cluster → attribute → accumulate
//!                                                                      ↓
//!                      rank ← correlate ← detect anomalies ← summarize
//! ```
//!
//! ONE PASS OVER THE INPUT, and nothing that scales with line count is retained.
//! "A line is folded into its cluster's accumulators and dropped; what survives
//! is a fixed cost per pattern." Anything added here that pushes to a per-line
//! array silently converts the tool back into one that needs the whole file in
//! memory.
//!
//! RANKING IS THE LAST DECISION AND THE MOST IMPORTANT ONE. Sorting by count
//! alone puts a million INFO request lines above three FATAL ones, and the three
//! are the reason anyone opened the file. See `score`.

use std::collections::HashMap;

use super::anomaly::detect;
use super::correlation::correlate;
use super::drain::{Drain, DrainOptions};
use super::mask::mask;
use super::stats::{attribute, tokenize, PatternAcc, TimeAxis};
use super::timestamp::strip_timestamp;
use super::types::{Analysis, Pattern, TimeSpan};

#[derive(Debug, Clone)]
pub struct AnalyzeOptions {
    /// Patterns to render. The rest are counted and dropped.
    pub top: usize,
    /// Year for timestamp formats that omit one (syslog).
    pub ref_year: Option<i64>,
    /// Passed through to the clustering tree.
    pub drain: DrainOptions,
}

impl Default for AnalyzeOptions {
    fn default() -> Self {
        Self {
            top: 20,
            ref_year: None,
            drain: DrainOptions::default(),
        }
    }
}

/// The pipeline as something you push lines into.
///
/// EXPOSED AS A STRUCT SO NOTHING EVER HOLDS THE INPUT. The convenient shape is
/// a function over a slice, and it is a trap: a caller with a large source has
/// no choice but to collect it first, and collecting is precisely the thing the
/// bounded-memory design exists to avoid.
pub struct Analyzer {
    axis: TimeAxis,
    accs: HashMap<u32, PatternAcc>,
    drain: Drain,
    top: usize,
    ref_year: i64,
    total: u64,
    span_from: Option<i64>,
    span_to: Option<i64>,
}

impl Default for Analyzer {
    fn default() -> Self {
        Self::new(AnalyzeOptions::default())
    }
}

impl Analyzer {
    pub fn new(opts: AnalyzeOptions) -> Self {
        Self {
            axis: TimeAxis::default(),
            accs: HashMap::new(),
            drain: Drain::new(opts.drain),
            top: opts.top,
            // `stripTimestamp`'s own default, so an unset option and an absent
            // argument mean the same thing.
            ref_year: opts.ref_year.unwrap_or(1970),
            total: 0,
            span_from: None,
            span_to: None,
        }
    }

    /// Fold one raw line in. Nothing about it is retained beyond its statistics.
    pub fn push(&mut self, raw: &str) {
        // Blank lines carry no structure and would form one enormous empty
        // cluster.
        if raw.trim().is_empty() {
            return;
        }
        self.total += 1;

        let stamped = strip_timestamp(raw, self.ref_year);
        if let Some(when) = stamped.when {
            if self.span_from.map(|f| when < f).unwrap_or(true) {
                self.span_from = Some(when);
            }
            if self.span_to.map(|t| when > t).unwrap_or(true) {
                self.span_to = Some(when);
            }
        }

        let masked = mask(&stamped.rest);
        let toks = tokenize(&masked.logtype);
        let token_texts: Vec<String> = toks.iter().map(|t| t.text.clone()).collect();
        let cluster = self.drain.add(&token_texts);
        // A cluster the cap evicted must take its statistics with it, or this
        // map becomes the unbounded thing the cap exists to prevent.
        for id in self.drain.take_evictions() {
            self.accs.remove(&id);
        }

        let acc = self.accs.entry(cluster.id).or_default();
        // Attribution uses the template as it stands NOW. A token that
        // generalizes later means earlier lines were attributed under the more
        // specific reading — their values are still in the right slot, because
        // slots are keyed on position and the position did not move.
        let per_token = attribute(&toks, &masked.values, &cluster.tokens);
        acc.add(raw, stamped.when, &per_token, &mut self.axis);
    }

    /// Materialize, rank, detect and correlate.
    pub fn finish(mut self) -> Analysis {
        let mut all: Vec<Pattern> = Vec::new();
        for cluster in self.drain.clusters() {
            let Some(acc) = self.accs.get_mut(&cluster.id) else {
                continue;
            };
            let buckets = acc.bucket_array(&self.axis);
            let mut pattern = Pattern {
                id: cluster.id,
                template: cluster.tokens.join(" "),
                count: acc.count,
                share: if self.total == 0 {
                    0.0
                } else {
                    acc.count as f64 / self.total as f64
                },
                severity: acc.severity,
                first_seen: acc.first,
                last_seen: acc.last,
                vars: acc.summarize(),
                examples: acc.examples.sample(),
                buckets,
                anomalies: vec![],
            };
            pattern.anomalies = detect(&pattern, self.total);
            all.push(pattern);
        }

        // A stable sort, as `Array.prototype.sort` is: ties keep drain order.
        all.sort_by(|a, b| {
            score(b)
                .partial_cmp(&score(a))
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b.count.cmp(&a.count))
        });
        let pattern_count = all.len();
        all.truncate(self.top);
        // IDs are reassigned to render order so the correlation lines ("#1 and
        // #3") name things the reader can actually find. Cluster ids are
        // allocation order, an implementation detail nobody should be shown.
        for (i, p) in all.iter_mut().enumerate() {
            p.id = i as u32 + 1;
        }
        let correlations = correlate(&all);

        let (time_span, bucket_ms) = match (self.span_from, self.span_to) {
            (Some(from), Some(to)) => (Some(TimeSpan { from, to }), Some(self.axis.bucket_ms)),
            _ => (None, None),
        };

        Analysis {
            lines: self.total,
            pattern_count,
            patterns: all,
            correlations,
            time_span,
            bucket_ms,
            truncated: self.drain.truncated(),
        }
    }
}

/// Analyze lines already in hand. For a very large source, use `Analyzer`.
pub fn analyze<I, S>(lines: I, opts: AnalyzeOptions) -> Analysis
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut a = Analyzer::new(opts);
    for line in lines {
        a.push(line.as_ref());
    }
    a.finish()
}

/// How interesting a pattern is, as one number.
///
/// Severity dominates by construction — the gap between severity tiers is larger
/// than the entire range volume can contribute — because "show me the errors" is
/// what running this means nine times out of ten. Within a tier, volume orders
/// things, on a log scale. An anomaly is worth a fixed nudge.
fn score(p: &Pattern) -> f64 {
    let severity = p.severity.rank() as f64 * 100.0;
    let volume = ((p.count + 1) as f64).log10() * 5.0;
    let flagged = if p.anomalies.is_empty() { 0.0 } else { 10.0 };
    severity + volume + flagged
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logs::timestamp::date_utc_ms;
    use crate::logs::types::Severity;

    /// The `sampleLog()` of `src/cli/patterns.test.ts`: two statements, one of
    /// them failing.
    pub(crate) fn sample_log() -> Vec<String> {
        let mut lines = Vec::new();
        let base = date_utc_ms(2024, 0, 15, 14, 0, 0, 0);
        for i in 0..60i64 {
            let t = iso(base + i * 1000);
            lines.push(format!(
                "{t} INFO Request from 10.0.1.{} completed in {}ms status=200",
                i % 4,
                20 + (i % 30)
            ));
        }
        for i in 0..5i64 {
            let t = iso(base + i * 1000);
            lines.push(format!(
                "{t} ERROR Timeout connecting to 10.0.9.{i} after {}ms",
                5000 + i
            ));
        }
        lines
    }

    fn iso(ms: i64) -> String {
        crate::logs::format::iso_stamp(ms)
    }

    #[test]
    fn sixty_five_lines_of_two_statements_do_not_become_sixty_five_patterns() {
        let a = analyze(sample_log(), AnalyzeOptions::default());
        assert_eq!(a.lines, 65);
        assert!(
            a.pattern_count <= 4,
            "65 lines produced {} patterns",
            a.pattern_count
        );
        let totals: u64 = a.patterns.iter().map(|p| p.count).sum();
        assert_eq!(totals, 65, "counts do not add up to the lines read");
    }

    #[test]
    fn blank_lines_are_skipped_rather_than_clustered() {
        let a = analyze(["INFO a", "", "   ", "INFO b"], AnalyzeOptions::default());
        assert_eq!(a.lines, 2);
    }

    #[test]
    fn a_log_with_no_timestamps_analyzes_without_a_span() {
        let a = analyze(
            [
                "make: entering dir /a/b",
                "make: entering dir /a/c",
                "cc -o x x.c",
            ],
            AnalyzeOptions::default(),
        );
        assert!(a.time_span.is_none());
        assert!(a.bucket_ms.is_none());
        assert_eq!(a.lines, 3);
    }

    #[test]
    fn errors_outrank_volume_however_large_the_volume() {
        let a = analyze(sample_log(), AnalyzeOptions::default());
        assert_eq!(
            a.patterns[0].severity,
            Severity::Error,
            "the 92%-of-traffic INFO pattern outranked the failures"
        );
    }

    #[test]
    fn top_truncates_the_rendering_but_not_the_count() {
        let a = analyze(
            sample_log(),
            AnalyzeOptions {
                top: 1,
                ..Default::default()
            },
        );
        assert_eq!(a.patterns.len(), 1);
        assert!(
            a.pattern_count > 1,
            "patternCount was truncated with the rendering"
        );
    }

    #[test]
    fn ids_are_renumbered_to_render_order() {
        let a = analyze(sample_log(), AnalyzeOptions::default());
        let ids: Vec<u32> = a.patterns.iter().map(|p| p.id).collect();
        assert_eq!(ids, (1..=ids.len() as u32).collect::<Vec<_>>());
    }

    #[test]
    fn the_same_input_twice_produces_an_identical_analysis() {
        // Two runs over one file must be diffable: seeded reservoir, stable
        // TopK tiebreak, stable sorts.
        let one = analyze(sample_log(), AnalyzeOptions::default());
        let two = analyze(sample_log(), AnalyzeOptions::default());
        assert_eq!(one, two);
    }

    #[test]
    fn an_empty_input_is_an_empty_analysis_not_a_failure() {
        let a = analyze(Vec::<String>::new(), AnalyzeOptions::default());
        assert_eq!(a.lines, 0);
        assert_eq!(a.pattern_count, 0);
        assert!(!a.truncated);
    }

    #[test]
    fn the_cluster_cap_is_reported_in_the_analysis() {
        let lines: Vec<String> = (0..50)
            .map(|i| format!("alpha{i} bravo{i} charlie{i} delta{i}"))
            .collect();
        let a = analyze(
            lines,
            AnalyzeOptions {
                drain: DrainOptions {
                    max_clusters: 3,
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        assert!(
            a.truncated,
            "eviction happened but the header would not say so"
        );
    }

    #[test]
    fn timestamps_bound_the_span() {
        let a = analyze(sample_log(), AnalyzeOptions::default());
        let span = a.time_span.expect("timestamped log has a span");
        assert_eq!(span.from, date_utc_ms(2024, 0, 15, 14, 0, 0, 0));
        assert_eq!(span.to, date_utc_ms(2024, 0, 15, 14, 0, 59, 0));
        assert!(a.bucket_ms.is_some());
    }
}
