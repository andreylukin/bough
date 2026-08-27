//! Invariant (D-C1): the Head mode STATES its delta instead of claiming an exactness it does not
//! have. A wake appends `wake/start`, any `mail/delivered` and `step/start` before it assembles,
//! so what a real next wake adds over a Head preview must be exactly those preface rows.

/// The step kinds a wake appends BEFORE it assembles, in order (§5's wake flow steps 3–5).
/// The one place the preview's stated caveat is spelled.
pub const WAKE_PREFACE_KINDS: [&str; 3] = ["wake/start", "mail/delivered", "step/start"];

/// PURE: the lines a later assembly added over an earlier one, oldest first.
///
/// Used by the header (`+3 preface rows at wake`) and by V1's second test. A projection that
/// SHRANK added nothing: the result is empty, never a negative delta dressed up as an addition.
///
/// The rule is one rule and not two: take the common line prefix of the two texts, and report
/// whatever `after` has past it. A pure suffix gain reports the suffix; a shrink reports nothing.
pub fn added_lines(before: &str, after: &str) -> Vec<String> {
    let b: Vec<&str> = before.lines().collect();
    let a: Vec<&str> = after.lines().collect();
    let common = a.iter().zip(b.iter()).take_while(|(x, y)| x == y).count();
    if a.len() <= common {
        return Vec::new();
    }
    a[common..].iter().map(|l| (*l).to_string()).collect()
}

/// The step kind a verbatim tail line names, if the line is one.
///
/// The tail's line shape is `- #<seq> <kind> [<class>] <json>` (the assembler's `step_line`).
/// A line that is not a tail line at all has no kind, and is not a preface row.
pub fn tail_kind(line: &str) -> Option<&str> {
    let rest = line.trim_start().strip_prefix("- #")?;
    let mut words = rest.split_whitespace();
    let _seq = words.next()?;
    words.next()
}

/// PURE: whether every added line is a tail line for one of [`WAKE_PREFACE_KINDS`].
///
/// Blank lines and a `### wake <id>` block header are structure, not content: a wake that opens a
/// new block adds them for free, and counting them as content would make the honest case fail.
pub fn only_preface(added: &[String]) -> bool {
    added.iter().all(|line| {
        let t = line.trim();
        if t.is_empty() || t.starts_with("### wake ") {
            return true;
        }
        match tail_kind(t) {
            Some(kind) => WAKE_PREFACE_KINDS.contains(&kind),
            None => false,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tail(seq: u64, kind: &str) -> String {
        format!("- #{seq} {kind} [thought] {{}}")
    }

    #[test]
    fn added_lines_are_the_suffix_the_later_text_gained() {
        let before = "## band\n\n- #1 tool/result [evidence] {}\n";
        let after = "## band\n\n- #1 tool/result [evidence] {}\n- #2 wake/start [thought] {}\n";
        assert_eq!(
            added_lines(before, after),
            vec!["- #2 wake/start [thought] {}".to_string()]
        );
    }

    #[test]
    fn only_preface_accepts_the_three_wake_preface_kinds() {
        let added: Vec<String> = vec![
            "### wake w9".to_string(),
            tail(11, "wake/start"),
            tail(12, "mail/delivered"),
            tail(13, "step/start"),
            String::new(),
        ];
        assert!(only_preface(&added), "{added:?}");
    }

    #[test]
    fn only_preface_rejects_a_tool_result_line() {
        let added = vec![tail(11, "wake/start"), tail(12, "tool/result")];
        assert!(
            !only_preface(&added),
            "a tool result is content the preview did not show, not a preface row"
        );
    }

    #[test]
    fn a_shrinking_projection_reports_no_added_lines() {
        let before = "a\nb\nc\n";
        let after = "a\nb\n";
        assert!(added_lines(before, after).is_empty());
    }
}
