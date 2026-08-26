//! No runtime invariant: `llm-retry` is a CONSUMER — one waterfall listener that either sets
//! `Recovery::Retry` or delegates. It owns no durable relation; the `agent-loop` invariant sees
//! the requests its retries produce, and its own behaviour is pinned by
//! `plugins/llm-retry/tests/retry.rs` (§0.2).
