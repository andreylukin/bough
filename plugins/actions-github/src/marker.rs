//! Invariant: the marker placement is PURE and per kind (§7's table), so the artifact a test reads
//! back from the `gh` shim and the artifact reconciliation searches for are the same string.
//!
//! | kind | artifact | placement |
//! |---|---|---|
//! | `open_pr` | the PR | last line of the body: `<!-- bough-action:<hex16> -->` |
//! | `push_to_pr` | the commit | trailer `Bough-Action: bough-action:<hex16>` |
//! | `bot_thread_op` | the comment | suffix `\n\n<!-- bough-action:<hex16> -->` |

/// The HTML-comment form the two markdown artifacts carry.
pub fn html_comment(marker: &str) -> String {
    format!("<!-- {marker} -->")
}

/// PURE: the PR body with the marker as its last line.
pub fn pr_body(body: &str, marker: &str) -> String {
    let body = body.trim_end();
    if body.is_empty() {
        return html_comment(marker);
    }
    format!("{body}\n\n{}", html_comment(marker))
}

/// PURE: the commit message with the `Bough-Action:` trailer.
///
/// A trailer is its own paragraph at the end of the message, so a message that already ends in a
/// trailer block keeps one blank line before this one and no more.
pub fn commit_trailer(message: &str, marker: &str) -> String {
    let message = message.trim_end();
    if message.is_empty() {
        return format!("{TRAILER_KEY}: {marker}");
    }
    format!("{message}\n\n{TRAILER_KEY}: {marker}")
}

/// The trailer key. A protocol constant: it is written into commits that outlive any config.
pub const TRAILER_KEY: &str = "Bough-Action";

/// PURE: a comment body with the HTML-comment marker suffix.
pub fn comment_suffix(body: &str, marker: &str) -> String {
    let body = body.trim_end();
    if body.is_empty() {
        return html_comment(marker);
    }
    format!("{body}\n\n{}", html_comment(marker))
}

#[cfg(test)]
mod tests {
    use super::*;

    const M: &str = "bough-action:0123456789abcdef";

    #[test]
    fn the_pr_body_ends_with_the_marker_comment() {
        let b = pr_body("does the thing\n", M);
        assert_eq!(b.lines().last().unwrap(), format!("<!-- {M} -->"));
        assert!(b.starts_with("does the thing"));
    }

    #[test]
    fn the_commit_message_ends_with_the_trailer() {
        let m = commit_trailer("fix it", M);
        assert_eq!(m.lines().last().unwrap(), format!("Bough-Action: {M}"));
    }

    #[test]
    fn an_empty_body_is_the_marker_alone_rather_than_blank_lines() {
        assert_eq!(pr_body("", M), format!("<!-- {M} -->"));
        assert_eq!(comment_suffix("  ", M), format!("<!-- {M} -->"));
        assert_eq!(commit_trailer("", M), format!("Bough-Action: {M}"));
    }

    #[test]
    fn a_comment_keeps_its_body_and_gains_the_suffix() {
        let c = comment_suffix("thanks, fixed", M);
        assert!(c.starts_with("thanks, fixed"));
        assert!(c.ends_with(&format!("<!-- {M} -->")));
    }
}
