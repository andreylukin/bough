//! Invariant: assembly is DETERMINISTIC (§5). Seven steps in order — connected, the six bands,
//! the contributed sections, `order()`, the `projection/assemble` waterfall, the degradation
//! ladder, finalize — with the waterfall BETWEEN rendering and degradation so a listener may add a
//! section and still be budgeted. Nothing in the request path reads a clock, the filesystem, or a
//! model; `at` comes from the request.

use bough_plugin_projection::{AssembleRequest, Assembled, ProjectionError};

use crate::Assembler;

/// The seven steps, in order.
pub async fn assemble(a: &Assembler, req: &AssembleRequest) -> Result<Assembled, ProjectionError> {
    todo!("WP-5: assemble::assemble")
}
