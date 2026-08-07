//! Structural coordinates and the stored journal key (port of the position
//! half of `src/workflow/run.ts`).
//!
//! A call's coordinate is dot-joined slot indexes from the script's SHAPE —
//! `"0.1.1.0"` for pipeline 0, item 1, stage 1, first agent. `js/wf_worker.js`
//! computes it; this module only orders and compares.
//!
//! THE INVARIANT: **position comes from the script's structure, never from
//! arrival order**, and the stored key keeps both halves RECOVERABLE so a call
//! that MOVED and a call that was EDITED are different facts.

use std::cmp::Ordering;

/// A call's structural coordinate: dot-joined slot indexes, e.g. `"0.1.1.0"`.
pub type CallPos = String;

/// Component-wise NUMERIC comparison. `"0.10"` sorts after `"0.9"`, which
/// string comparison gets backwards — and a fan-out of ten items is not an
/// exotic case. A prefix sorts before what extends it (missing components read
/// as −1), so a bare call at `"2"` precedes the combinator subtree at
/// `"2.0.0"` that would only exist if they were the same slot.
///
/// `f64` on purpose: it reproduces the TS `Number(x[i])` semantics component
/// for component, including the NaN arm a non-numeric component lands in (JS
/// `NaN !== NaN` is true and `NaN < NaN` is false, so it sorts last).
pub fn compare_pos(a: &str, b: &str) -> Ordering {
    let x: Vec<&str> = a.split('.').collect();
    let y: Vec<&str> = b.split('.').collect();
    let n = x.len().max(y.len());
    for i in 0..n {
        let dx = component(&x, i);
        let dy = component(&y, i);
        if dx != dy {
            return if dx < dy {
                Ordering::Less
            } else {
                Ordering::Greater
            };
        }
    }
    Ordering::Equal
}

/// A missing component reads as −1 — that is what makes a prefix sort first.
fn component(parts: &[&str], i: usize) -> f64 {
    match parts.get(i) {
        // `Number("")` is 0 in JS and `Number("x")` is NaN. Mirror both.
        Some(s) if s.trim().is_empty() => 0.0,
        Some(s) => s.trim().parse::<f64>().unwrap_or(f64::NAN),
        None => -1.0,
    }
}

/// The stored journal key: the call's position and the hash of what it asks
/// for, joined by a character neither half can contain (positions are digits
/// and dots, the hash is hex).
///
/// Keeping the halves recoverable rather than hashing them together is the
/// whole point: it is what lets the divergence report distinguish a call that
/// was edited (same position, different hash) from one that moved (same hash,
/// different position). Hashing the pair would have made both read as "its key
/// changed".
pub fn journal_key(pos: &str, content_key: &str) -> String {
    format!("{pos}|{content_key}")
}

/// The two halves of a stored key. `pos` is `None` for a pre-coordinate row —
/// a journal written before coordinates existed has no position half.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitKey {
    pub pos: Option<CallPos>,
    pub content: String,
}

/// The inverse of [`journal_key`]. A key with no separator is pre-coordinate;
/// `pos` reads as `None`.
pub fn split_journal_key(key: &str) -> SplitKey {
    match key.find('|') {
        None => SplitKey {
            pos: None,
            content: key.to_string(),
        },
        Some(at) => SplitKey {
            pos: Some(key[..at].to_string()),
            content: key[at + 1..].to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The row-3.7 gate: comparePos is NUMERIC and component-wise.
    #[test]
    fn compare_pos_is_numeric_not_lexical() {
        // The case string comparison gets backwards.
        assert_eq!(compare_pos("0.9", "0.10"), Ordering::Less);
        assert_eq!(compare_pos("0.10", "0.9"), Ordering::Greater);
        assert_eq!(compare_pos("2", "10"), Ordering::Less);
        // A prefix sorts before what extends it.
        assert_eq!(compare_pos("2", "2.0.0"), Ordering::Less);
        assert_eq!(compare_pos("2.0.0", "2"), Ordering::Greater);
        // Identity.
        assert_eq!(compare_pos("0.1.1.0", "0.1.1.0"), Ordering::Equal);
        // Ordinary depth-first ordering.
        assert_eq!(compare_pos("0.0", "0.1"), Ordering::Less);
        assert_eq!(compare_pos("1.0", "0.99"), Ordering::Greater);
    }

    #[test]
    fn sorting_a_fan_out_of_ten_keeps_slot_order() {
        let mut v: Vec<String> = (0..12).map(|i| format!("0.{i}")).collect();
        v.reverse();
        v.sort_by(|a, b| compare_pos(a, b));
        assert_eq!(v[9], "0.9");
        assert_eq!(v[10], "0.10");
        assert_eq!(v[11], "0.11");
    }

    /// `|` cannot occur in either half, so the split is exact and the halves
    /// stay diagnosable.
    #[test]
    fn journal_key_round_trips_and_old_keys_have_no_position() {
        let k = journal_key("0.1.1.0", "deadbeefcafebabe");
        assert_eq!(k, "0.1.1.0|deadbeefcafebabe");
        assert_eq!(
            split_journal_key(&k),
            SplitKey {
                pos: Some("0.1.1.0".into()),
                content: "deadbeefcafebabe".into()
            }
        );
        // Pre-coordinate row: the whole key is content.
        assert_eq!(
            split_journal_key("deadbeefcafebabe"),
            SplitKey {
                pos: None,
                content: "deadbeefcafebabe".into()
            }
        );
    }
}
