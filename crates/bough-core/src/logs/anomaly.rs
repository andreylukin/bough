//! Port of `src/logs/anomaly.ts` — stage four: point at the handful of things
//! in the output a reader should look at first.
//!
//! THE BAR FOR FIRING IS DELIBERATELY HIGH, and the reason is that this output
//! is mostly read by a language model, which will dutifully investigate whatever
//! it is told is anomalous. "A missed anomaly costs a reader one scan of a table
//! they already have. A phantom one costs them the investigation."
//!
//! NOTHING HERE IS STATISTICAL INFERENCE. Each detector is a plain, explicable
//! rule, and each one produces a sentence that says what it saw rather than what
//! it concluded.

use super::format::n as commas;
use super::types::{Anomaly, AnomalyKind, Pattern, Severity, VarKind};

/// Lines a pattern needs before any detector will describe its shape.
const MIN_SAMPLE: u64 = 20;

/// How far above its own median a bucket must be to count as a spike.
const SPIKE_FACTOR: u64 = 5;

/// Share of all lines below which a pattern is called rare.
const RARE_SHARE: f64 = 0.001;

/// Annotations rendered per pattern, past which they stop informing and start
/// burying.
const MAX_PER_PATTERN: usize = 4;

/// Everything worth saying about one pattern, ordered by how much it should
/// influence what a reader does next.
pub fn detect(p: &Pattern, total_lines: u64) -> Vec<Anomaly> {
    let mut found: Vec<Anomaly> = Vec::new();

    // --- Temporal shape ----------------------------------------------------
    //
    // Compared against the pattern's OWN median bucket rather than against a
    // global rate, because patterns differ in frequency by orders of magnitude.
    let active: Vec<u64> = p.buckets.iter().copied().filter(|n| *n > 0).collect();
    if active.len() >= 5 && p.count >= MIN_SAMPLE {
        let mut sorted = active.clone();
        sorted.sort_unstable();
        let median = sorted[sorted.len() / 2];
        let peak = p.buckets.iter().copied().max().unwrap_or(0);
        if median > 0 && peak >= median * SPIKE_FACTOR {
            found.push(Anomaly {
                kind: AnomalyKind::FrequencySpike,
                detail: format!(
                    "burst: peak bucket held {peak} lines against a median of {median}"
                ),
            });
        }
        // Concentration is the other shape worth naming, and it is not the same
        // as a spike: a pattern can put 90% of its lines in three adjacent
        // buckets without any single one clearing the spike factor.
        let mut top = p.buckets.clone();
        top.sort_unstable_by(|a, b| b.cmp(a));
        let concentrated: u64 = top.iter().take(3).sum();
        if active.len() >= 10 && concentrated as f64 / p.count as f64 >= 0.9 {
            found.push(Anomaly {
                kind: AnomalyKind::ErrorBurst,
                detail: format!(
                    "episodic: {}% of its lines fall in 3 of {} active buckets",
                    js_round(concentrated as f64 / p.count as f64 * 100.0),
                    active.len()
                ),
            });
        }
    }

    // --- Rarity ------------------------------------------------------------
    //
    // Rare is only interesting when it is also bad. A handful of DEBUG lines is
    // not news; a handful of FATAL lines is the most important thing in the file.
    let rare_cap = (5.0f64).max(total_lines as f64 * RARE_SHARE);
    if (p.count as f64) <= rare_cap && matches!(p.severity, Severity::Error | Severity::Fatal) {
        found.push(Anomaly {
            kind: AnomalyKind::Rare,
            detail: format!(
                "rare but severe: only {} {}, at {}",
                p.count,
                if p.count == 1 { "line" } else { "lines" },
                p.severity.as_str().to_uppercase()
            ),
        });
    }

    // --- Variable distributions -------------------------------------------
    for v in &p.vars {
        if v.count < MIN_SAMPLE {
            continue;
        }

        // A slot that never varies is not a variable. It usually means the
        // masker was too eager, and saying so is more useful than showing a p99
        // of a constant.
        if v.unique == 1 {
            if let Some(top) = v.top.as_ref().and_then(|t| t.first()) {
                found.push(Anomaly {
                    kind: AnomalyKind::SingleValue,
                    detail: format!("slot {} never varies — always {}", v.slot, top.value),
                });
                continue;
            }
        }

        // Every line brought a new value. Worth naming because it changes how
        // the slot should be read: as an identifier to join on, not as a
        // quantity to trend.
        if v.kind == VarKind::Id {
            found.push(Anomaly {
                kind: AnomalyKind::HighCardinality,
                detail: format!(
                    "slot {} is an identifier — ~{} distinct values in {} lines",
                    v.slot,
                    commas(v.unique),
                    commas(v.count)
                ),
            });
            continue;
        }

        let Some(nq) = v.numeric.as_ref() else {
            continue;
        };

        // Two clusters of magnitude, not one. This is the shape of a fast path
        // and a slow path sharing a code path, and a mean sitting between them
        // describes neither.
        if nq.p50 > 0.0 && nq.p99 >= nq.p50 * 10.0 && nq.p90 <= nq.p50 * 3.0 {
            found.push(Anomaly {
                kind: AnomalyKind::Bimodal,
                detail: format!(
                    "slot {} is bimodal — p50 {} and p90 {} sit together, p99 is {}",
                    v.slot,
                    fmt(nq.p50, nq.unit.as_deref()),
                    fmt(nq.p90, nq.unit.as_deref()),
                    fmt(nq.p99, nq.unit.as_deref())
                ),
            });
            continue;
        }

        // A long tail without the bimodal signature: still worth flagging,
        // because the worst case is what pages someone and the mean hides it.
        if nq.p50 > 0.0 && nq.max >= nq.p50 * 100.0 {
            found.push(Anomaly {
                kind: AnomalyKind::LongTail,
                detail: format!(
                    "slot {} has a long tail — worst {} against a median of {}",
                    v.slot,
                    fmt(nq.max, nq.unit.as_deref()),
                    fmt(nq.p50, nq.unit.as_deref())
                ),
            });
        }
    }

    // Capped, and the cap is a readability decision: a pattern with eight
    // constant slots produces eight `single-value` lines that say the same thing
    // eight times, burying the one detector that fired for an interesting
    // reason. `detect` emits in priority order, so truncating keeps the most
    // consequential.
    found.truncate(MAX_PER_PATTERN);
    found
}

/// `Math.round` — half up.
fn js_round(v: f64) -> i64 {
    (v + 0.5).floor() as i64
}

/// `toFixed(digits)`: JS rounds half away from zero on the decimal value, where
/// Rust's `{:.n}` rounds half to even. The difference shows up on exactly the
/// round numbers a reader notices.
pub(crate) fn to_fixed(v: f64, digits: usize) -> String {
    let factor = 10f64.powi(digits as i32);
    let scaled = v * factor;
    let rounded = if scaled < 0.0 {
        -((-scaled) + 0.5).floor()
    } else {
        (scaled + 0.5).floor()
    };
    format!("{:.*}", digits, rounded / factor)
}

/// A magnitude with its unit, rounded to something a person would say out loud.
pub fn fmt(value: f64, unit: Option<&str>) -> String {
    if unit == Some("ms") {
        if value >= 60000.0 {
            return format!("{}min", to_fixed(value / 60000.0, 1));
        }
        if value >= 1000.0 {
            return format!("{}s", to_fixed(value / 1000.0, 2));
        }
        if value >= 1.0 {
            return format!("{}ms", num_str(round(value)));
        }
        return format!("{}µs", num_str(round(value * 1000.0)));
    }
    if unit == Some("bytes") {
        let units = ["B", "KB", "MB", "GB", "TB"];
        let mut v = value;
        let mut i = 0usize;
        while v >= 1024.0 && i < units.len() - 1 {
            v /= 1024.0;
            i += 1;
        }
        return format!("{}{}", num_str(round(v)), units[i]);
    }
    num_str(round(value))
}

/// Three significant-ish figures, without trailing zeros.
fn round(v: f64) -> f64 {
    if !v.is_finite() {
        return v;
    }
    let abs = v.abs();
    if abs >= 100.0 {
        return js_round(v) as f64;
    }
    if abs >= 10.0 {
        return js_round(v * 10.0) as f64 / 10.0;
    }
    js_round(v * 100.0) as f64 / 100.0
}

/// `String(number)` — JS prints an integral float without a `.0` tail.
pub(crate) fn num_str(v: f64) -> String {
    if v.is_finite() && v.fract() == 0.0 && v.abs() < 1e21 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logs::types::{NumericSummary, TopValue, VarSummary};

    fn pattern(
        count: u64,
        severity: Severity,
        buckets: Vec<u64>,
        vars: Vec<VarSummary>,
    ) -> Pattern {
        Pattern {
            id: 1,
            template: "x".to_string(),
            count,
            share: 0.5,
            severity,
            first_seen: None,
            last_seen: None,
            vars,
            examples: vec![],
            buckets,
            anomalies: vec![],
        }
    }

    fn numeric_var(slot: usize, p50: f64, p90: f64, p99: f64, max: f64) -> VarSummary {
        VarSummary {
            slot,
            kind: VarKind::Int,
            count: 100,
            unique: 50,
            top: Some(vec![]),
            numeric: Some(NumericSummary {
                min: 1.0,
                max,
                mean: p50,
                p50,
                p90,
                p99,
                unit: Some("ms".to_string()),
            }),
        }
    }

    #[test]
    fn a_spike_is_measured_against_the_patterns_own_median() {
        let p = pattern(100, Severity::Info, vec![1, 1, 1, 1, 1, 50], vec![]);
        let found = detect(&p, 1000);
        assert!(found.iter().any(|a| a.kind == AnomalyKind::FrequencySpike));
        assert!(found[0].detail.contains("peak bucket held 50 lines"));
    }

    #[test]
    fn a_flat_pattern_is_not_a_spike() {
        let p = pattern(100, Severity::Info, vec![20, 20, 20, 20, 20], vec![]);
        assert!(detect(&p, 1000).is_empty());
    }

    #[test]
    fn a_small_sample_says_nothing_about_shape() {
        // MIN_SAMPLE: with 10 lines the shape is noise.
        let p = pattern(10, Severity::Info, vec![1, 1, 1, 1, 6], vec![]);
        assert!(detect(&p, 1000).is_empty());
    }

    #[test]
    fn rare_fires_only_when_it_is_also_severe() {
        let bad = pattern(3, Severity::Fatal, vec![], vec![]);
        let found = detect(&bad, 100_000);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].kind, AnomalyKind::Rare);
        assert!(found[0].detail.contains("at FATAL"));

        let dull = pattern(3, Severity::Debug, vec![], vec![]);
        assert!(detect(&dull, 100_000).is_empty());
    }

    #[test]
    fn a_single_line_is_singular_in_the_prose() {
        let p = pattern(1, Severity::Error, vec![], vec![]);
        assert!(detect(&p, 100_000)[0].detail.contains("only 1 line,"));
    }

    #[test]
    fn a_constant_slot_is_named_rather_than_quantified() {
        let v = VarSummary {
            slot: 0,
            kind: VarKind::Int,
            count: 100,
            unique: 1,
            top: Some(vec![TopValue {
                value: "5432".to_string(),
                count: 100,
                share: 1.0,
            }]),
            numeric: None,
        };
        let p = pattern(100, Severity::Info, vec![], vec![v]);
        let found = detect(&p, 1000);
        assert_eq!(found[0].kind, AnomalyKind::SingleValue);
        assert_eq!(found[0].detail, "slot 0 never varies — always 5432");
    }

    #[test]
    fn bimodal_beats_long_tail_when_both_would_fire() {
        let p = pattern(
            100,
            Severity::Info,
            vec![],
            vec![numeric_var(0, 10.0, 20.0, 500.0, 5000.0)],
        );
        let found = detect(&p, 1000);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].kind, AnomalyKind::Bimodal);
    }

    #[test]
    fn a_long_tail_without_the_bimodal_signature_still_fires() {
        let p = pattern(
            100,
            Severity::Info,
            vec![],
            vec![numeric_var(0, 10.0, 400.0, 900.0, 5000.0)],
        );
        let found = detect(&p, 1000);
        assert_eq!(found[0].kind, AnomalyKind::LongTail);
    }

    #[test]
    fn at_most_four_annotations_per_pattern() {
        let vars: Vec<VarSummary> = (0..8)
            .map(|i| VarSummary {
                slot: i,
                kind: VarKind::Int,
                count: 100,
                unique: 1,
                top: Some(vec![TopValue {
                    value: "1".to_string(),
                    count: 100,
                    share: 1.0,
                }]),
                numeric: None,
            })
            .collect();
        let p = pattern(100, Severity::Info, vec![], vars);
        assert_eq!(detect(&p, 1000).len(), MAX_PER_PATTERN);
    }

    #[test]
    fn fmt_speaks_the_units_out_loud() {
        assert_eq!(fmt(0.5, Some("ms")), "500µs");
        assert_eq!(fmt(45.0, Some("ms")), "45ms");
        assert_eq!(fmt(1500.0, Some("ms")), "1.50s");
        assert_eq!(fmt(120_000.0, Some("ms")), "2.0min");
        assert_eq!(fmt(1024.0, Some("bytes")), "1KB");
        assert_eq!(fmt(1536.0, Some("bytes")), "1.5KB");
        assert_eq!(fmt(5.0, None), "5");
        assert_eq!(fmt(1234.5, None), "1235");
    }
}
