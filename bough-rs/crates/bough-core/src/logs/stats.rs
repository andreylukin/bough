//! Port of `src/logs/stats.ts` — stage three: turn a stream of clustered lines
//! into the numbers the output shows.
//!
//! STATISTICS ARE KEYED ON TEMPLATE POSITION, NOT ON TOKEN TEXT. A template
//! mutates as it generalizes, so anything keyed on what a token said at
//! insertion time would scatter one slot's values across several.
//!
//! TWO KINDS ARE DECIDED HERE RATHER THAN BY THE MASKER, because they are
//! properties of a distribution and not of any single value: `enum` (few
//! distinct values, repeatedly) and `id` (a different value nearly every time).
//!
//! THE TIME AXIS IS SHARED AND ONLY EVER COARSENS. Every pattern buckets against
//! one origin and one width, so bucket 3 covers the same minutes for all of
//! them — without that, comparing two patterns' shapes would be comparing
//! different clocks.

use std::collections::{BTreeMap, HashMap};

use super::drain::WILDCARD;
use super::sketch::{DDSketch, HyperLogLog, Reservoir, TopK};
use super::types::{NumericSummary, Severity, TopValue, VarKind, VarSummary, VarValue};

// ---------------------------------------------------------------------------
// Severity
// ---------------------------------------------------------------------------

/// The severity words, most severe first so the first hit wins.
///
/// Matched case-sensitively against upper-case forms and against the capitalized
/// form, but never against lower-case prose: a line saying `failed to warn the
/// operator` is not a warning.
const SEVERITY_WORDS: [(Severity, &[&str]); 5] = [
    (
        Severity::Fatal,
        &["FATAL", "PANIC", "CRITICAL", "CRIT", "EMERG", "ALERT"],
    ),
    (
        Severity::Error,
        &[
            "ERROR",
            "ERR",
            "SEVERE",
            "EXCEPTION",
            "FAIL",
            "FAILED",
            "FAILURE",
        ],
    ),
    (Severity::Warn, &["WARN", "WARNING"]),
    (Severity::Debug, &["DEBUG", "TRACE", "VERBOSE", "FINE"]),
    (Severity::Info, &["INFO", "NOTICE"]),
];

/// Read a severity off a line's own words, defaulting to `info`.
///
/// Only whole tokens count. Substring matching would classify `ERRORS_TOTAL=0` —
/// a metric name, quite possibly reporting zero errors — as an error line.
pub fn severity_of(line: &str) -> Severity {
    // Split on anything that is not a letter or underscore so `[ERROR]`,
    // `level=WARN` and `WARN:` all yield the bare word.
    let words: Vec<&str> = line
        .split(|c: char| !(c.is_ascii_alphabetic() || c == '_'))
        .collect();
    for (sev, forms) in SEVERITY_WORDS {
        for w in &words {
            if w.is_empty() {
                continue;
            }
            let upper = w.to_ascii_uppercase();
            // The word must have been shouted or capitalized in the original.
            let first = w.chars().next().expect("non-empty");
            if *w != upper.as_str() && !first.is_uppercase() {
                continue;
            }
            if forms.contains(&upper.as_str()) {
                return sev;
            }
        }
    }
    Severity::Info
}

// ---------------------------------------------------------------------------
// The shared time axis
// ---------------------------------------------------------------------------

/// Buckets held before the axis coarsens. Also the widest bar any formatter
/// draws.
const MAX_BUCKETS: i64 = 512;

/// One origin and one bucket width, shared by every pattern.
///
/// Indices may be negative: logs are not reliably ordered, and a line older than
/// the first one seen must extend the axis backwards rather than be clamped into
/// bucket zero — clamping would invent a spike at the start of every
/// out-of-order file.
#[derive(Debug, Clone)]
pub struct TimeAxis {
    pub origin: Option<i64>,
    pub bucket_ms: i64,
    /// Doublings so far. A pattern lagging by `n` generations rescales by `>> n`.
    pub generation: u32,
    lo: i64,
    hi: i64,
}

impl Default for TimeAxis {
    fn default() -> Self {
        Self {
            origin: None,
            bucket_ms: 1000,
            generation: 0,
            lo: 0,
            hi: 0,
        }
    }
}

impl TimeAxis {
    /// The bucket for a moment, coarsening the axis first if the span demands
    /// it. A loop rather than an `if` because a single line can arrive far
    /// outside the current range.
    pub fn index(&mut self, when: i64) -> i64 {
        let origin = *self.origin.get_or_insert(when);
        let mut idx = (when - origin).div_euclid(self.bucket_ms);
        while self.hi.max(idx) - self.lo.min(idx) >= MAX_BUCKETS {
            self.bucket_ms *= 2;
            self.generation += 1;
            // Arithmetic shift, not division: `>> 1` floors toward negative
            // infinity, the same direction `div_euclid` took.
            self.lo >>= 1;
            self.hi >>= 1;
            idx = (when - origin).div_euclid(self.bucket_ms);
        }
        if idx < self.lo {
            self.lo = idx;
        }
        if idx > self.hi {
            self.hi = idx;
        }
        idx
    }

    pub fn range(&self) -> (i64, i64) {
        (self.lo, self.hi)
    }
}

// ---------------------------------------------------------------------------
// Per-slot accumulation
// ---------------------------------------------------------------------------

/// Examples kept per pattern. Enough to see variety, few enough to stay
/// readable.
const EXAMPLES: usize = 3;

/// Everything learned about one variable position of one pattern.
#[derive(Debug)]
struct SlotAcc {
    top: TopK,
    unique: HyperLogLog,
    /// Built lazily: most slots never hold a number, and a sketch per slot is
    /// not free.
    numeric: Option<DDSketch>,
    count: u64,
    /// Kinds seen here, so a slot that holds two is described as the general one.
    kinds: HashMap<VarKind, u64>,
    /// Insertion order of `kinds`, so "most frequent, first-seen wins ties"
    /// matches the TS Map iteration.
    kind_order: Vec<VarKind>,
    unit: Option<String>,
}

impl SlotAcc {
    fn new() -> Self {
        Self {
            top: TopK::new(1024),
            unique: HyperLogLog::default(),
            numeric: None,
            count: 0,
            kinds: HashMap::new(),
            kind_order: Vec::new(),
            unit: None,
        }
    }

    fn add(&mut self, v: &VarValue) {
        self.count += 1;
        self.top.add(&v.raw);
        self.unique.add(&v.raw);
        match self.kinds.get_mut(&v.kind) {
            Some(n) => *n += 1,
            None => {
                self.kinds.insert(v.kind, 1);
                self.kind_order.push(v.kind);
            }
        }
        if let Some(num) = v.num {
            let sketch = self.numeric.get_or_insert_with(DDSketch::default);
            sketch.add(num);
            if v.kind == VarKind::Duration {
                self.unit = Some("ms".to_string());
            } else if v.kind == VarKind::Bytes {
                self.unit = Some("bytes".to_string());
            }
        }
    }
}

/// Everything learned about one cluster.
///
/// Slots are keyed by `(tokenIndex, ordinal)` rather than an array, because one
/// token can carry several values — `10.0.1.15:5432` is one token and two
/// variables — and because a position may go unfilled on lines where a wildcard
/// swallowed a differently-shaped token.
#[derive(Debug)]
pub struct PatternAcc {
    pub severity: Severity,
    pub first: Option<i64>,
    pub last: Option<i64>,
    pub examples: Reservoir<String>,
    buckets: HashMap<i64, u64>,
    bucket_gen: u32,
    slots: BTreeMap<(usize, usize), SlotAcc>,
    pub count: u64,
}

impl Default for PatternAcc {
    fn default() -> Self {
        Self {
            severity: Severity::Debug,
            first: None,
            last: None,
            examples: Reservoir::new(EXAMPLES, 0x5bf0_3635),
            buckets: HashMap::new(),
            bucket_gen: 0,
            slots: BTreeMap::new(),
            count: 0,
        }
    }
}

impl PatternAcc {
    /// Fold one line in.
    ///
    /// `token_values` is aligned to the TEMPLATE's tokens, not the line's, and
    /// holds either the values masked out of that token or — where the template
    /// has generalized — the token's own reconstructed text as a single value.
    pub fn add(
        &mut self,
        raw: &str,
        when: Option<i64>,
        token_values: &[Vec<VarValue>],
        axis: &mut TimeAxis,
    ) {
        self.count += 1;
        // Severity is the worst seen, not the first or the last. A statement
        // whose level word generalized to a wildcard still emitted real errors.
        let sev = severity_of(raw);
        if sev.rank() > self.severity.rank() {
            self.severity = sev;
        }
        self.examples.add(raw.to_string());

        if let Some(when) = when {
            if self.first.map(|f| when < f).unwrap_or(true) {
                self.first = Some(when);
            }
            if self.last.map(|l| when > l).unwrap_or(true) {
                self.last = Some(when);
            }
            let idx = axis.index(when);
            self.rescale(axis.generation);
            *self.buckets.entry(idx).or_insert(0) += 1;
        }

        for (t, vals) in token_values.iter().enumerate() {
            for (o, v) in vals.iter().enumerate() {
                self.slots.entry((t, o)).or_insert_with(SlotAcc::new).add(v);
            }
        }
    }

    /// Fold this pattern's buckets down to the axis's current width.
    ///
    /// Done lazily on the next line rather than eagerly across every pattern
    /// when the axis doubles: coarsening touches every pattern's map, and doing
    /// it eagerly makes one unlucky line pay for all of them.
    pub fn rescale(&mut self, generation: u32) {
        while self.bucket_gen < generation {
            let mut folded: HashMap<i64, u64> = HashMap::new();
            for (idx, n) in self.buckets.drain() {
                *folded.entry(idx >> 1).or_insert(0) += n;
            }
            self.buckets = folded;
            self.bucket_gen += 1;
        }
    }

    /// Buckets as a dense array over the axis's range, for the formatters and
    /// detectors.
    pub fn bucket_array(&mut self, axis: &TimeAxis) -> Vec<u64> {
        self.rescale(axis.generation);
        let (lo, hi) = axis.range();
        if self.buckets.is_empty() {
            return Vec::new();
        }
        let mut out = vec![0u64; (hi - lo + 1).max(0) as usize];
        for (idx, n) in &self.buckets {
            let at = idx - lo;
            if at >= 0 && (at as usize) < out.len() {
                out[at as usize] += n;
            }
        }
        out
    }

    /// Every slot, described. Ordered by token position so output reads left to
    /// right (the `BTreeMap` key ordering is the TS's numeric sort on
    /// `"{token}.{ordinal}"`).
    pub fn summarize(&self) -> Vec<VarSummary> {
        self.slots
            .values()
            .enumerate()
            .map(|(i, slot)| describe_slot(slot, i))
            .collect()
    }
}

/// Decide what a slot is and render its distribution.
///
/// The ordering of the tests matters. `id` is checked before `enum` because a
/// slot with two lines and two distinct values satisfies both readings, and
/// calling it an enum of two would be a claim about a distribution that has not
/// been observed yet.
fn describe_slot(slot: &SlotAcc, index: usize) -> VarSummary {
    // Clamped to the number of values actually seen. HyperLogLog is unbiased,
    // which means it overshoots about half the time, and `unique=1,936 of 1,925
    // lines` is arithmetically impossible on its face.
    let unique = slot.unique.count().min(slot.count);
    // The masker's most frequent verdict, as the starting point.
    let mut kind = VarKind::String;
    let mut best: i64 = -1;
    for k in &slot.kind_order {
        let n = slot.kinds[k] as i64;
        if n > best {
            kind = *k;
            best = n;
        }
    }

    let ratio = if slot.count == 0 {
        0.0
    } else {
        unique as f64 / slot.count as f64
    };
    let top_three = slot.top.top(3);
    let top_share = if slot.count == 0 {
        0.0
    } else {
        top_three.iter().map(|e| e.1).sum::<u64>() as f64 / slot.count as f64
    };

    // An identifier: nearly every line brought a new value. The threshold is
    // high and the sample requirement real.
    let looks_like_id = slot.count >= 10 && ratio > 0.9;
    // An enumeration: few values, seen repeatedly, dominating the slot. All
    // three conditions are needed.
    let looks_like_enum = slot.count >= 10 && unique <= 20 && top_share >= 0.8;

    if looks_like_id
        && matches!(
            kind,
            VarKind::Int | VarKind::Hex | VarKind::String | VarKind::Uuid
        )
    {
        kind = VarKind::Id;
    } else if looks_like_enum && matches!(kind, VarKind::Int | VarKind::String | VarKind::Float) {
        kind = VarKind::Enum;
    }

    // Suppress the ranking when it cannot be trusted: past the tracking cap the
    // counts favour whatever arrived first, and for an identifier the "top
    // values" are three arbitrary IDs that a reader would mistake for hot spots.
    let rankable = !slot.top.saturated() && kind != VarKind::Id;
    let top = if rankable {
        Some(
            top_three
                .iter()
                .map(|(value, count)| TopValue {
                    value: value.clone(),
                    count: *count,
                    share: *count as f64 / slot.count as f64,
                })
                .collect(),
        )
    } else {
        None
    };

    // Quantiles are reported only where they mean something, and there are three
    // ways for them not to: an IDENTIFIER's p99 is noise dressed as a statistic;
    // an ENUM is categorical; a CONSTANT has no distribution at all.
    let meaningful = kind != VarKind::Id && kind != VarKind::Enum && unique > 1;
    let numeric = if meaningful {
        slot.numeric
            .as_ref()
            .and_then(|s| s.summary())
            .map(|q| NumericSummary {
                min: q.min,
                max: q.max,
                mean: q.mean,
                p50: q.p50,
                p90: q.p90,
                p99: q.p99,
                unit: slot.unit.clone(),
            })
    } else {
        None
    };

    VarSummary {
        slot: index,
        kind,
        count: slot.count,
        unique,
        top,
        numeric,
    }
}

// ---------------------------------------------------------------------------
// Attributing a line's values to template positions
// ---------------------------------------------------------------------------

/// A whitespace-delimited token of a logtype, with where it started (in CHARS,
/// the same unit `mask` records `at` in).
#[derive(Debug, Clone, PartialEq)]
pub struct Tok {
    pub text: String,
    pub start: usize,
}

/// Split a logtype into tokens, keeping each one's offset so values can be
/// attributed.
pub fn tokenize(logtype: &str) -> Vec<Tok> {
    let mut out = Vec::new();
    let chars: Vec<char> = logtype.chars().collect();
    let mut i = 0usize;
    while i < chars.len() {
        if chars[i].is_whitespace() {
            i += 1;
            continue;
        }
        let start = i;
        while i < chars.len() && !chars[i].is_whitespace() {
            i += 1;
        }
        out.push(Tok {
            text: chars[start..i].iter().collect(),
            start,
        });
    }
    out
}

/// Line up one line's masked values with a template's token positions.
///
/// Two cases per position. The template kept the token: whatever the masker
/// pulled out of it belongs to that slot, in order. The template generalized the
/// token to `<*>`: then the interesting value is the token itself, reconstructed
/// with its placeholders filled back in, so the slot reports `db-primary` and
/// `10.0.1.15` rather than the useless literal string `<ipv4>`.
pub fn attribute(tokens: &[Tok], values: &[VarValue], template: &[String]) -> Vec<Vec<VarValue>> {
    let mut per_token: Vec<Vec<VarValue>> = tokens.iter().map(|_| Vec::new()).collect();
    if tokens.is_empty() {
        return Vec::new();
    }
    for v in values {
        // The last token starting at or before the value's offset is the one
        // containing it.
        let mut t = tokens.len() - 1;
        while t > 0 && tokens[t].start > v.at {
            t -= 1;
        }
        per_token[t].push(v.clone());
    }

    let mut out: Vec<Vec<VarValue>> = Vec::with_capacity(tokens.len());
    for (i, tok) in tokens.iter().enumerate() {
        let vals = std::mem::take(&mut per_token[i]);
        if template.get(i).map(String::as_str) != Some(WILDCARD) {
            out.push(vals);
            continue;
        }
        // Reconstruct: walk the token, substituting each placeholder's raw text
        // back in.
        let tok_chars: Vec<char> = tok.text.chars().collect();
        let mut text = String::new();
        let mut cursor = tok.start;
        for v in &vals {
            let from = cursor.saturating_sub(tok.start).min(tok_chars.len());
            let to = v.at.saturating_sub(tok.start).min(tok_chars.len());
            if to > from {
                text.extend(&tok_chars[from..to]);
            }
            text.push_str(&v.raw);
            cursor = v.at + v.kind.as_str().chars().count() + 2;
        }
        let from = cursor.saturating_sub(tok.start).min(tok_chars.len());
        text.extend(&tok_chars[from..]);
        let sole_kind = if vals.len() == 1 && vals[0].raw == text {
            vals[0].kind
        } else {
            VarKind::String
        };
        let num = if vals.len() == 1 { vals[0].num } else { None };
        out.push(vec![VarValue {
            kind: sole_kind,
            raw: text,
            num,
            at: tok.start,
        }]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logs::mask::mask;

    #[test]
    fn severity_reads_shouted_words_only() {
        assert_eq!(severity_of("failed to warn the operator"), Severity::Info);
        assert_eq!(severity_of("[ERROR] boom"), Severity::Error);
        assert_eq!(severity_of("level=WARN slow"), Severity::Warn);
        assert_eq!(severity_of("Fatal: the end"), Severity::Fatal);
        assert_eq!(severity_of("nothing to say"), Severity::Info);
    }

    #[test]
    fn severity_matches_whole_tokens_only() {
        // `ERRORS_TOTAL=0` is a metric name, quite possibly reporting zero
        // errors. `_` is a word character for the splitter, so the token is
        // `ERRORS_TOTAL` and matches nothing.
        assert_eq!(severity_of("ERRORS_TOTAL=0"), Severity::Info);
    }

    #[test]
    fn severity_takes_the_most_severe_word() {
        assert_eq!(severity_of("INFO ERROR both here"), Severity::Error);
    }

    #[test]
    fn the_time_axis_coarsens_rather_than_growing() {
        let mut axis = TimeAxis::default();
        axis.index(0);
        axis.index(1_000_000); // 1000 buckets at 1s — must double past 512
        assert!(axis.bucket_ms > 1000);
        let (lo, hi) = axis.range();
        assert!(hi - lo < MAX_BUCKETS);
    }

    #[test]
    fn the_time_axis_extends_backwards_for_out_of_order_lines() {
        let mut axis = TimeAxis::default();
        axis.index(10_000);
        let earlier = axis.index(0);
        assert!(earlier < 0, "an older line was clamped into bucket zero");
    }

    #[test]
    fn tokenize_records_character_offsets() {
        let toks = tokenize("from <ipv4> in <duration>");
        assert_eq!(toks.len(), 4);
        assert_eq!(toks[1].text, "<ipv4>");
        assert_eq!(toks[1].start, 5);
        assert_eq!(toks[3].start, 15);
    }

    #[test]
    fn attribute_assigns_two_values_of_one_token_to_two_slots() {
        let m = mask("connect 10.0.1.15:5432 failed");
        let toks = tokenize(&m.logtype);
        let template: Vec<String> = toks.iter().map(|t| t.text.clone()).collect();
        let per = attribute(&toks, &m.values, &template);
        assert_eq!(per[1].len(), 2);
        assert_eq!(per[1][0].kind, VarKind::Ipv4);
        assert_eq!(per[1][1].kind, VarKind::Int);
    }

    #[test]
    fn attribute_reconstructs_a_generalized_token() {
        // The point: a wildcard slot must report `db-primary`/`10.0.1.15`, not
        // the useless literal string `<ipv4>`.
        let m = mask("connect to 10.0.1.15 failed");
        let toks = tokenize(&m.logtype);
        let template: Vec<String> = toks
            .iter()
            .enumerate()
            .map(|(i, t)| {
                if i == 2 {
                    WILDCARD.to_string()
                } else {
                    t.text.clone()
                }
            })
            .collect();
        let per = attribute(&toks, &m.values, &template);
        assert_eq!(per[2].len(), 1);
        assert_eq!(per[2][0].raw, "10.0.1.15");
        assert_eq!(per[2][0].kind, VarKind::Ipv4);
    }

    #[test]
    fn attribute_reconstructs_a_mixed_token_as_a_string() {
        let m = mask("peer 10.0.1.15:5432 gone");
        let toks = tokenize(&m.logtype);
        let template: Vec<String> = toks
            .iter()
            .enumerate()
            .map(|(i, t)| {
                if i == 1 {
                    WILDCARD.to_string()
                } else {
                    t.text.clone()
                }
            })
            .collect();
        let per = attribute(&toks, &m.values, &template);
        assert_eq!(per[1][0].raw, "10.0.1.15:5432");
        assert_eq!(per[1][0].kind, VarKind::String);
    }

    #[test]
    fn a_slot_that_never_repeats_is_an_identifier() {
        let mut acc = PatternAcc::default();
        let mut axis = TimeAxis::default();
        for i in 0..40 {
            let line = format!("request {}", 100000 + i);
            let m = mask(&line);
            let toks = tokenize(&m.logtype);
            let template: Vec<String> = toks.iter().map(|t| t.text.clone()).collect();
            acc.add(
                &line,
                None,
                &attribute(&toks, &m.values, &template),
                &mut axis,
            );
        }
        let vars = acc.summarize();
        assert_eq!(vars[0].kind, VarKind::Id);
        // The ranking is suppressed for an identifier and no quantiles are
        // reported over request ids.
        assert!(vars[0].top.is_none());
        assert!(vars[0].numeric.is_none());
    }

    #[test]
    fn a_slot_with_three_repeated_values_is_an_enum() {
        let mut acc = PatternAcc::default();
        let mut axis = TimeAxis::default();
        for i in 0..40 {
            let code = [200, 200, 200, 404, 500][i % 5];
            let line = format!("status={code}");
            let m = mask(&line);
            let toks = tokenize(&m.logtype);
            let template: Vec<String> = toks.iter().map(|t| t.text.clone()).collect();
            acc.add(
                &line,
                None,
                &attribute(&toks, &m.values, &template),
                &mut axis,
            );
        }
        let vars = acc.summarize();
        assert_eq!(vars[0].kind, VarKind::Enum);
        // "A status code's median is 200 and that is not a fact about latency."
        assert!(vars[0].numeric.is_none());
        assert_eq!(vars[0].top.as_ref().unwrap()[0].value, "200");
    }

    #[test]
    fn unique_never_exceeds_the_count() {
        let mut acc = PatternAcc::default();
        let mut axis = TimeAxis::default();
        for i in 0..200 {
            let line = format!("took {}ms", i % 7);
            let m = mask(&line);
            let toks = tokenize(&m.logtype);
            let template: Vec<String> = toks.iter().map(|t| t.text.clone()).collect();
            acc.add(
                &line,
                None,
                &attribute(&toks, &m.values, &template),
                &mut axis,
            );
        }
        let vars = acc.summarize();
        assert!(vars[0].unique <= vars[0].count);
    }

    #[test]
    fn buckets_fold_pairwise_when_the_axis_coarsens() {
        let mut acc = PatternAcc::default();
        let mut axis = TimeAxis::default();
        for i in 0..600 {
            acc.add("x", Some(i * 1000), &[], &mut axis);
        }
        let buckets = acc.bucket_array(&axis);
        // Nothing may be lost by coarsening: two adjacent buckets summed is
        // exactly the count of the wider bucket they tile.
        assert_eq!(buckets.iter().sum::<u64>(), 600);
        assert!(buckets.len() as i64 <= MAX_BUCKETS);
    }
}
