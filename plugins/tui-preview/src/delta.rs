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
/// WP-1.
pub fn added_lines(before: &str, after: &str) -> Vec<String> {
    let _ = (before, after);
    todo!("WP-1: the suffix `after` gained over `before`, oldest first")
}

/// PURE: whether every added line is a tail line for one of [`WAKE_PREFACE_KINDS`].
///
/// WP-1.
pub fn only_preface(added: &[String]) -> bool {
    let _ = added;
    todo!("WP-1: every added line is a tail row for a wake-preface kind")
}
