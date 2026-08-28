//! Invariant: one row is ONE line. Every rendered line is clipped to the pane's columns and never
//! wraps, because the pane maps painted line *i* to row *i* for the click map — a wrapped row
//! would put the hit rect on the wrong step, and a timeline whose clicks land elsewhere is worse
//! than no timeline.
//!
//! Pure: no clock (the `at` is the step's), no styling (the pane paints), no I/O.

use bough_plugin_ledger::StepId;
use bough_plugin_tui_shell::HitId;

use crate::Row;

/// The `HitId` convention for a timeline row.
pub const HIT_PREFIX: &str = "tl:";

/// PURE: the hit id a row records. `tl:<step id>`.
pub fn hit_of(row: &Row) -> HitId {
    HitId::new(format!("{HIT_PREFIX}{}", row.step.id))
}

/// The step a `HitId` names, when it is one of ours.
pub fn step_of_hit(hit: &HitId) -> Option<StepId> {
    hit.as_str().strip_prefix(HIT_PREFIX).map(StepId::new)
}

/// PURE: one rendered line, clipped to `cols`:
/// `12:04:31  sol   tool/call     bash(cargo test -p bough)      pr/1204`
pub fn line(row: &Row, cols: u16, time_format: &str) -> String {
    let mut text = format!(
        "{}  {:<8} {:<14} {}",
        row.step.at.format(time_format),
        clip(row.agent.as_str(), 8),
        clip(row.step.kind.as_str(), 14),
        summary(row)
    );
    if let Some(r) = row.step.refs.iter().next() {
        text.push_str(&format!("  {r}"));
    }
    // A body with a newline in it would paint a second line the click map does not know about.
    let text: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    clip(&text, cols as usize)
}

/// PURE: the one-line gist of a step's body. A `text` field if there is one, else compact JSON.
pub fn summary(row: &Row) -> String {
    match row.step.body.as_ref() {
        serde_json::Value::Object(map) => {
            for key in ["text", "summary", "name", "command"] {
                if let Some(serde_json::Value::String(s)) = map.get(key) {
                    return s.clone();
                }
            }
            row.step.body.to_string()
        }
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Clip to `n` CHARACTERS (never bytes: a clip inside a multi-byte char panics).
fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    s.chars().take(n).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::row;

    #[test]
    fn a_line_is_clipped_to_cols_and_never_wraps() {
        let mut r = row("sol", "t1", 1, "tool/call", "12:04:31");
        r.step.body = std::sync::Arc::new(serde_json::json!({
            "text": "bash(cargo test -p bough)\nand a second line nobody asked for"
        }));
        let full = line(&r, 200, "%H:%M:%S");
        assert!(full.starts_with("12:04:31  sol "), "{full}");
        assert!(full.contains("tool/call"), "{full}");
        assert!(!full.contains('\n'), "one row is one line: {full}");
        for cols in [1u16, 8, 20, 40] {
            let clipped = line(&r, cols, "%H:%M:%S");
            assert_eq!(
                clipped.chars().count(),
                cols as usize,
                "clipped to {cols}: {clipped:?}"
            );
            assert!(!clipped.contains('\n'));
        }
    }

    #[test]
    fn hit_of_is_the_step_id() {
        let r = row("sol", "t1", 1, "tool/call", "12:04:31");
        let hit = hit_of(&r);
        assert_eq!(hit.as_str(), format!("tl:{}", r.step.id));
        assert_eq!(step_of_hit(&hit), Some(r.step.id.clone()));
        // A hit that is not ours is not silently claimed.
        assert_eq!(step_of_hit(&HitId::new("hit:abc")), None);
    }
}
