//! Port of `src/logs/correlation.ts` — stage five: which patterns move together.
//!
//! "The single most common question asked of a log is not 'what happened' but
//! 'what happened AT THE SAME TIME as this'."
//!
//! IT IS A LEAD, NOT A CAUSE, and the wording of every result reflects that.
//! Nothing here says "caused" — the detail line reports the observation and
//! leaves the inference to the reader.
//!
//! COSINE, NOT CORRELATION COEFFICIENT. Pearson's r subtracts the mean, which on
//! sparse log data makes co-absence the dominant signal and scores pairs of
//! unrelated rare patterns near 1. Cosine treats a zero as "nothing happened"
//! rather than "below average", which is what a zero means here.

use std::collections::HashMap;

use super::anomaly::to_fixed;
use super::types::{Correlation, CorrelationKind, Pattern, VarKind};

/// Cosine similarity below which a temporal pair is not worth mentioning.
const TEMPORAL_MIN: f64 = 0.8;

/// Overlap below which a shared-value pair is not worth mentioning.
const SHARED_MIN: f64 = 0.5;

/// Lines each side needs before its shape is trusted.
const MIN_COUNT: u64 = 10;

/// Active buckets each side needs, so a pair of one-bucket patterns cannot score
/// 1.0.
const MIN_ACTIVE: usize = 3;

/// Pairs reported. Beyond a handful this stops being a lead and becomes another
/// table.
const MAX_RESULTS: usize = 8;

/// Find related pairs among the patterns given.
///
/// Quadratic in the pattern count, which is fine and bounded by design: this
/// runs over the patterns being RENDERED — a few dozen after `--top` — not over
/// the thousands that may exist.
pub fn correlate(patterns: &[Pattern]) -> Vec<Correlation> {
    let mut out: Vec<Correlation> = Vec::new();
    for i in 0..patterns.len() {
        for j in (i + 1)..patterns.len() {
            let a = &patterns[i];
            let b = &patterns[j];
            if let Some(c) = temporal_pair(a, b) {
                out.push(c);
            }
            if let Some(c) = shared_value_pair(a, b) {
                out.push(c);
            }
        }
    }
    // A stable sort, matching `Array.prototype.sort` in every modern engine, so
    // equal-strength pairs keep discovery order.
    out.sort_by(|x, y| {
        y.strength
            .partial_cmp(&x.strength)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out.truncate(MAX_RESULTS);
    out
}

fn active_count(buckets: &[u64]) -> usize {
    buckets.iter().filter(|b| **b > 0).count()
}

/// Do these two rise and fall together?
fn temporal_pair(a: &Pattern, b: &Pattern) -> Option<Correlation> {
    if a.count < MIN_COUNT || b.count < MIN_COUNT {
        return None;
    }
    let n = a.buckets.len().min(b.buckets.len());
    if n == 0 {
        return None;
    }
    // Both sides must be spread over time. Two patterns that each occupy a
    // single bucket score a perfect 1.0 while sharing nothing but a minute.
    if active_count(&a.buckets) < MIN_ACTIVE || active_count(&b.buckets) < MIN_ACTIVE {
        return None;
    }

    let mut dot = 0.0f64;
    let mut na = 0.0f64;
    let mut nb = 0.0f64;
    for i in 0..n {
        let x = a.buckets[i] as f64;
        let y = b.buckets[i] as f64;
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        return None;
    }
    let strength = dot / (na.sqrt() * nb.sqrt());
    if strength < TEMPORAL_MIN {
        return None;
    }
    Some(Correlation {
        a: a.id,
        b: b.id,
        kind: CorrelationKind::Temporal,
        strength,
        detail: format!(
            "#{} and #{} rise and fall together ({}% aligned over time)",
            a.id,
            b.id,
            to_fixed(strength * 100.0, 0)
        ),
    })
}

/// Do these two talk about the same things?
///
/// Only slots with a usable ranking participate — `top` is null for identifiers
/// and for saturated slots, and both are exactly the cases where an overlap
/// would be meaningless.
fn shared_value_pair(a: &Pattern, b: &Pattern) -> Option<Correlation> {
    if a.count < MIN_COUNT || b.count < MIN_COUNT {
        return None;
    }
    let mut best: Option<(f64, String, usize, usize)> = None;

    for va in &a.vars {
        let Some(top_a) = va.top.as_ref().filter(|t| !t.is_empty()) else {
            continue;
        };
        if va.unique > 50 {
            continue;
        }
        for vb in &b.vars {
            let Some(top_b) = vb.top.as_ref().filter(|t| !t.is_empty()) else {
                continue;
            };
            if vb.unique > 50 {
                continue;
            }
            // Kinds must agree, or a status code and a retry count "share" the
            // value 3.
            if va.kind != vb.kind {
                continue;
            }
            // Bare integers are excluded outright: small integers collide
            // constantly across unrelated slots and would dominate every result.
            if va.kind == VarKind::Int || va.kind == VarKind::Float {
                continue;
            }

            let set_b: HashMap<&str, f64> =
                top_b.iter().map(|e| (e.value.as_str(), e.share)).collect();
            for ea in top_a {
                let Some(share_b) = set_b.get(ea.value.as_str()) else {
                    continue;
                };
                // Strength is the weaker of the two shares: a value that is 90%
                // of one slot and 2% of the other is not a shared story.
                let overlap = ea.share.min(*share_b);
                if best.as_ref().map(|b| overlap > b.0).unwrap_or(true) {
                    best = Some((overlap, ea.value.clone(), va.slot, vb.slot));
                }
            }
        }
    }

    let (overlap, value, sa, sb) = best?;
    if overlap < SHARED_MIN {
        return None;
    }
    Some(Correlation {
        a: a.id,
        b: b.id,
        kind: CorrelationKind::SharedValue,
        strength: overlap,
        detail: format!(
            "#{} slot {} and #{} slot {} both centre on {} ({}% of each)",
            a.id,
            sa,
            b.id,
            sb,
            value,
            to_fixed(overlap * 100.0, 0)
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logs::types::{Pattern, Severity, TopValue, VarSummary};

    fn pattern(id: u32, count: u64, buckets: Vec<u64>, vars: Vec<VarSummary>) -> Pattern {
        Pattern {
            id,
            template: format!("t{id}"),
            count,
            share: 0.1,
            severity: Severity::Info,
            first_seen: None,
            last_seen: None,
            vars,
            examples: vec![],
            buckets,
            anomalies: vec![],
        }
    }

    fn enum_var(slot: usize, value: &str, share: f64) -> VarSummary {
        VarSummary {
            slot,
            kind: VarKind::Ipv4,
            count: 100,
            unique: 3,
            top: Some(vec![TopValue {
                value: value.to_string(),
                count: (share * 100.0) as u64,
                share,
            }]),
            numeric: None,
        }
    }

    #[test]
    fn two_patterns_that_rise_together_are_reported() {
        let a = pattern(1, 100, vec![5, 10, 20, 10, 5], vec![]);
        let b = pattern(2, 100, vec![4, 9, 18, 11, 4], vec![]);
        let out = correlate(&[a, b]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].kind, CorrelationKind::Temporal);
        assert_eq!(
            out[0].detail,
            "#1 and #2 rise and fall together (100% aligned over time)"
        );
    }

    #[test]
    fn one_bucket_patterns_do_not_score_a_perfect_alignment() {
        // Two patterns occupying a single bucket share nothing but a minute.
        let a = pattern(1, 100, vec![0, 100, 0, 0, 0], vec![]);
        let b = pattern(2, 100, vec![0, 100, 0, 0, 0], vec![]);
        assert!(correlate(&[a, b]).is_empty());
    }

    #[test]
    fn a_thin_pattern_is_not_correlated_at_all() {
        let a = pattern(1, 5, vec![1, 1, 1, 1, 1], vec![]);
        let b = pattern(2, 100, vec![5, 10, 20, 10, 5], vec![]);
        assert!(correlate(&[a, b]).is_empty());
    }

    #[test]
    fn a_shared_value_links_patterns_that_never_move_together() {
        let a = pattern(1, 100, vec![], vec![enum_var(0, "10.0.1.15", 0.9)]);
        let b = pattern(2, 100, vec![], vec![enum_var(1, "10.0.1.15", 0.7)]);
        let out = correlate(&[a, b]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].kind, CorrelationKind::SharedValue);
        // Strength is the WEAKER of the two shares.
        assert!((out[0].strength - 0.7).abs() < 1e-9);
        assert_eq!(
            out[0].detail,
            "#1 slot 0 and #2 slot 1 both centre on 10.0.1.15 (70% of each)"
        );
    }

    #[test]
    fn a_lopsided_overlap_is_a_coincidence_not_a_story() {
        let a = pattern(1, 100, vec![], vec![enum_var(0, "10.0.1.15", 0.9)]);
        let b = pattern(2, 100, vec![], vec![enum_var(1, "10.0.1.15", 0.02)]);
        assert!(correlate(&[a, b]).is_empty());
    }

    #[test]
    fn bare_integers_never_correlate() {
        let mut va = enum_var(0, "3", 0.9);
        va.kind = VarKind::Int;
        let mut vb = enum_var(1, "3", 0.9);
        vb.kind = VarKind::Int;
        let a = pattern(1, 100, vec![], vec![va]);
        let b = pattern(2, 100, vec![], vec![vb]);
        assert!(correlate(&[a, b]).is_empty());
    }

    #[test]
    fn mismatched_kinds_never_correlate() {
        let va = enum_var(0, "3", 0.9);
        let mut vb = enum_var(1, "3", 0.9);
        vb.kind = VarKind::Hex;
        let a = pattern(1, 100, vec![], vec![va]);
        let b = pattern(2, 100, vec![], vec![vb]);
        assert!(correlate(&[a, b]).is_empty());
    }

    #[test]
    fn at_most_eight_pairs_are_reported() {
        let patterns: Vec<Pattern> = (1..=8)
            .map(|i| pattern(i, 100, vec![5, 10, 20, 10, 5], vec![]))
            .collect();
        assert_eq!(correlate(&patterns).len(), MAX_RESULTS);
    }
}
