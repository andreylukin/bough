//! Invariant: copying NEVER fails the caller (P3-D7). OSC52 is the copy path because it works over
//! SSH and inside the PTY the gate is measured in; `arboard` is best effort, and its failure is a
//! `notify` line rather than an error return.

use crate::TuiConfig;

/// What actually happened to the copy.
#[derive(Clone, Debug, PartialEq)]
pub enum CopyOutcome {
    Osc52AndLocal,
    Osc52Only,
    LocalOnly,
    /// Nothing was copied, and this is why. Rendered as a notice.
    Nothing(String),
}

/// OSC52 first (crossterm's `clipboard::CopyToClipboard`, feature `osc52`), then `arboard` when
/// `clipboard: true`. An `arboard` failure is a `notify` line, never an error: a PTY has no
/// display server and must still copy (P3-D7).
pub async fn copy(_text: &str, _cfg: &TuiConfig, _out: &mut impl std::io::Write) -> CopyOutcome {
    todo!("WP-2")
}
