//! No runtime invariant: `llm-openrouter` is a MAPPER, `llm-openai`'s twin. It owns no stream and
//! no relation of its own — everything it produces leaves through the `llm` seam, whose invariant
//! (`every_stream_ends_with_exactly_one_terminal_chunk`) already polices its output. A second
//! check here would restate the seam's, from a worse vantage point (§0.2).

/// The specs this crate contributes: none, for the reason above.
pub fn specs() -> Vec<bough_kernel::InvariantSpec> {
    Vec::new()
}
