//! Invariant: a syntax error the MODEL can act on beats an engine's bare message. This is main's
//! `preflight.rs` scanner (`git show main:crates/bough-core/src/harness/preflight.rs`), which
//! finds the unterminated string/template a bare "unexpected end of input" hides.

use bough_plugin_js::JsError;

/// What the scanner found, if anything.
#[derive(Clone, Debug, PartialEq)]
pub enum Finding {
    UnterminatedString { line: u32, col: u32, quote: char },
    UnterminatedTemplate { line: u32, col: u32 },
    UnterminatedComment { line: u32, col: u32 },
}

/// Scan `src` for the lexical mistakes whose engine message is useless to a model.
/// `None` ⇒ hand the source to the engine's parser.
///
/// WP-1 owns the body (a verbatim port of main's scanner).
pub fn scan(_src: &str) -> Option<Finding> {
    todo!("WP-1: port main's unterminated-string/template/comment scanner")
}

/// Render a finding as the model-facing [`JsError::Syntax`] main produced.
///
/// WP-1 owns the body.
pub fn diagnose(_f: &Finding) -> JsError {
    todo!("WP-1: port main's model-facing syntax messages verbatim")
}
