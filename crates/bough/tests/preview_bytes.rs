//! V1 (WP-7, §11 "Digging", §5): the preview's bytes ARE the request the loop sent. Boot with
//! `agent-loop-scripted`, `llm-replay` and `tui-probe`, run one wake, read the sent request and the
//! wake's `request/header`, take a `PreviewAt::Seq(header.as_of)` snapshot, and compare byte for
//! byte. The "if it woke now" half is pinned by the preface-delta test.

#[test]
#[ignore = "WP-7: fill in when tui-preview::snapshot lands"]
fn the_preview_bytes_equal_the_system_prefix_the_loop_sent() {}

#[test]
#[ignore = "WP-7"]
fn the_preview_digest_equals_the_request_headers_projection_digest() {}

#[test]
#[ignore = "WP-7"]
fn a_head_preview_and_the_next_wake_differ_only_by_that_wakes_preface_rows() {}
