//! Invariant: the marker placement is PURE and per kind (§7's table), so the artifact a test reads
//! back from the `gh` shim and the artifact reconciliation searches for are the same string.
//!
//! | kind | artifact | placement |
//! |---|---|---|
//! | `open_pr` | the PR | last line of the body: `<!-- bough-action:<hex16> -->` |
//! | `push_to_pr` | the commit | trailer `Bough-Action: bough-action:<hex16>` |
//! | `bot_thread_op` | the comment | suffix `\n\n<!-- bough-action:<hex16> -->` |

/// PURE: the PR body with the marker as its last line. WP-3.
pub fn pr_body(body: &str, marker: &str) -> String {
    let _ = (body, marker);
    todo!("WP-3")
}

/// PURE: the commit message with the `Bough-Action:` trailer. WP-3.
pub fn commit_trailer(message: &str, marker: &str) -> String {
    let _ = (message, marker);
    todo!("WP-3")
}

/// PURE: a comment body with the HTML-comment marker suffix. WP-3.
pub fn comment_suffix(body: &str, marker: &str) -> String {
    let _ = (body, marker);
    todo!("WP-3")
}
