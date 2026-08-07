//! Streaming transport (port of `src/llm/stream.ts`), and the one place a
//! finished round becomes persisted parts.
//!
//! Three invariants live here, all learned the hard way:
//!
//! 1. **A stream that stops without its completion marker is a failure, not a
//!    short answer.** [`SseEvents`] guards every read with a stall timeout,
//!    and the callers treat "ended without a completion marker" as a
//!    retryable transport fault.
//! 2. **A tool call with missing arguments was truncated; it is not a call
//!    with no arguments.** [`parse_tool_args`] refuses to invent `{}` for a
//!    tool whose schema has required fields — the schema, not the emptiness,
//!    decides.
//! 3. **Reasoning is persisted WITH its provider payload.** [`blocks_to_parts`]
//!    keeps the opaque `meta`, stamped with the model that produced it.
//!
//! The parser stays hand-rolled (~40 lines): the `[DONE]`/stall/
//! trailing-fragment semantics are custom and test-pinned — do NOT substitute
//! `eventsource-stream`. Nothing here knows a provider by name — the provider
//! string is a label for error text, and `meta` is never opened, only carried.

use std::pin::Pin;
use std::time::Duration;

use futures::{Stream, StreamExt};
use serde_json::Value;

use crate::errors::BoughError;
use crate::schema::parts::Part;
use crate::types::{LlmBlock, LlmToolDef};

/// A stream that sends no bytes for this long is treated as dropped.
pub const STALL_TIMEOUT_MS: u64 = 60_000;

// ---- the transport seam -----------------------------------------------------

/// A response body as a stream of byte chunks. Errors mid-stream are transport
/// faults, already mapped to status-less (502 ⇒ retryable) `LlmError`s.
pub type ByteStream = Pin<Box<dyn Stream<Item = Result<Vec<u8>, BoughError>> + Send>>;

/// One HTTP request, as a provider client issues it.
pub struct HttpRequest {
    pub url: String,
    /// Lowercase header names.
    pub headers: Vec<(String, String)>,
    /// The JSON body. `None` = GET (discovery), `Some` = POST.
    pub body: Option<String>,
}

/// One HTTP response, streaming.
pub struct HttpResponse {
    pub status: u16,
    /// Lowercase header names.
    pub headers: Vec<(String, String)>,
    pub body: ByteStream,
}

impl HttpResponse {
    /// Case-insensitive header lookup.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }
    /// Drain the whole body as text; read errors yield what arrived so far
    /// (mirrors `res.text().catch(() => "")`).
    pub async fn text(mut self) -> String {
        let mut bytes = Vec::new();
        while let Some(chunk) = self.body.next().await {
            match chunk {
                Ok(c) => bytes.extend_from_slice(&c),
                Err(_) => break,
            }
        }
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

/// The injected `fetch`. Production is [`ReqwestTransport`]; tests serve
/// canned SSE. Connect/timeout/decode failures map to status-less `LlmError`
/// (502 ⇒ retryable) at this edge.
#[async_trait::async_trait]
pub trait Transport: Send + Sync {
    async fn fetch(&self, req: HttpRequest) -> Result<HttpResponse, BoughError>;
}

/// The production transport over one shared `reqwest::Client`.
pub struct ReqwestTransport {
    client: reqwest::Client,
}

impl ReqwestTransport {
    pub fn new() -> Self {
        ReqwestTransport {
            client: reqwest::Client::new(),
        }
    }
}

impl Default for ReqwestTransport {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Transport for ReqwestTransport {
    async fn fetch(&self, req: HttpRequest) -> Result<HttpResponse, BoughError> {
        let mut builder = match &req.body {
            Some(body) => self.client.post(&req.url).body(body.clone()),
            None => self.client.get(&req.url),
        };
        for (k, v) in &req.headers {
            builder = builder.header(k, v);
        }
        // A transport-level failure carries no status: the 502 default is what
        // makes it retryable.
        let res = builder
            .send()
            .await
            .map_err(|e| BoughError::llm(e.to_string()))?;
        let status = res.status().as_u16();
        let headers = res
            .headers()
            .iter()
            .map(|(k, v)| {
                (
                    k.as_str().to_string(),
                    String::from_utf8_lossy(v.as_bytes()).into_owned(),
                )
            })
            .collect();
        let body = res.bytes_stream().map(|chunk| {
            chunk
                .map(|b| b.to_vec())
                .map_err(|e| BoughError::llm(e.to_string()))
        });
        Ok(HttpResponse {
            status,
            headers,
            body: Box::pin(body),
        })
    }
}

// ---- the SSE parser ---------------------------------------------------------

/// Yields each SSE `data:` payload from a response body — raw, INCLUDING the
/// `[DONE]` sentinel, because the callers treat that one differently.
///
/// Splits on `\n`, trims, and skips non-`data:` lines (SSE comments and
/// keepalives). A trailing un-newlined fragment at stream end is dropped: it
/// is by definition incomplete, and the caller's "did we see a completion
/// marker?" check is what catches the truncation.
///
/// Every chunk read is guarded by a stall timer; on stall the reader is
/// dropped and a status-less (502 ⇒ retryable) `LlmError` surfaces instead of
/// hanging the turn until the user interrupts.
pub struct SseEvents {
    body: Option<ByteStream>,
    provider: String,
    stall_ms: u64,
    buffer: Vec<u8>,
}

impl SseEvents {
    pub fn new(body: ByteStream, provider: &str) -> Self {
        Self::with_stall(body, provider, STALL_TIMEOUT_MS)
    }

    /// The knob the tests turn down so a stall assertion does not take a minute.
    pub fn with_stall(body: ByteStream, provider: &str, stall_ms: u64) -> Self {
        SseEvents {
            body: Some(body),
            provider: provider.to_string(),
            stall_ms,
            buffer: Vec::new(),
        }
    }

    /// The next `data:` payload, `None` at stream end.
    pub async fn next(&mut self) -> Result<Option<String>, BoughError> {
        loop {
            // Drain complete lines already buffered.
            while let Some(nl) = self.buffer.iter().position(|b| *b == b'\n') {
                let line_bytes: Vec<u8> = self.buffer.drain(..=nl).collect();
                let line = String::from_utf8_lossy(&line_bytes[..nl]);
                let line = line.trim();
                if let Some(rest) = line.strip_prefix("data:") {
                    return Ok(Some(rest.trim().to_string()));
                }
            }
            let Some(body) = self.body.as_mut() else {
                return Ok(None);
            };
            let read = tokio::time::timeout(Duration::from_millis(self.stall_ms), body.next());
            match read.await {
                Err(_) => {
                    // Cancel the reader (drop it) and surface the stall.
                    self.body = None;
                    let secs = (self.stall_ms as f64 / 1000.0).round() as u64;
                    return Err(BoughError::llm(format!(
                        "{}: stream stalled (no data for {secs}s)",
                        self.provider
                    )));
                }
                Ok(None) => {
                    // Stream end: a trailing un-newlined fragment is dropped.
                    self.body = None;
                    return Ok(None);
                }
                Ok(Some(Err(err))) => {
                    self.body = None;
                    return Err(err);
                }
                Ok(Some(Ok(chunk))) => self.buffer.extend_from_slice(&chunk),
            }
        }
    }
}

/// Map a non-2xx provider response to a classified `LlmError`. Retry-After is
/// parsed from the header as seconds; invalid/absent → no hint.
pub async fn http_error(provider: &str, res: HttpResponse) -> BoughError {
    let status = res.status;
    let retry_after_ms = res
        .header("retry-after")
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|secs| secs.is_finite() && *secs > 0.0)
        .map(|secs| (secs * 1000.0) as u64);
    let body = res.text().await;
    BoughError::llm_with(
        format!("{provider}: {status} {body}")
            .trim_end()
            .to_string(),
        status,
        retry_after_ms,
    )
}

/// The error a user abort surfaces as. 499 is not in the retryable status
/// set, so the retry ring never re-attempts an interrupt.
pub fn aborted(provider: &str) -> BoughError {
    BoughError::llm_with(format!("{provider}: aborted"), 499, None)
}

/// `transport.fetch` racing the turn's interrupt.
pub async fn fetch_cancellable(
    transport: &dyn Transport,
    req: HttpRequest,
    cancel: &tokio_util::sync::CancellationToken,
    provider: &str,
) -> Result<HttpResponse, BoughError> {
    tokio::select! {
        _ = cancel.cancelled() => Err(aborted(provider)),
        res = transport.fetch(req) => res,
    }
}

// ---- tool-argument truncation -----------------------------------------------

/// Decode a tool call's raw `arguments` JSON.
///
/// A round that streams a call's name but none of (or half of) its payload was
/// cut off mid-call — a transport fault, not a model mistake. Throwing a
/// status-less (and therefore retryable) `LlmError` puts it back through the
/// retry ring. `{}` is still correct for a tool with no required fields, so
/// `tool` — the declared schema — decides, not the emptiness.
pub fn parse_tool_args(
    provider: &str,
    raw: Option<&str>,
    tool: Option<&LlmToolDef>,
    name: &str,
) -> Result<Value, BoughError> {
    // TS `if (raw)`: an empty string is as absent as undefined.
    if let Some(raw) = raw.filter(|r| !r.is_empty()) {
        return serde_json::from_str(raw).map_err(|_| {
            BoughError::llm(format!(
                "{provider}: {name} call has malformed arguments (truncated mid-call)"
            ))
        });
    }
    let required = tool.and_then(|t| t.input_schema.get("required"));
    if let Some(Value::Array(required)) = required {
        if !required.is_empty() {
            return Err(BoughError::llm(format!(
                "{provider}: {name} call arrived with no arguments (truncated mid-call)"
            )));
        }
    }
    Ok(Value::Object(serde_json::Map::new()))
}

// ---- blocks → parts ---------------------------------------------------------

/// A finished round's blocks → the parts persisted on the supervisor message.
///
/// `model` stamps the reasoning parts, because a provider signature is only
/// valid for the model that produced it and replay is gated on that. A
/// reasoning block with NO displayable text is still persisted when it
/// carries `meta` — that is a redacted thinking block, and the provider's
/// rule is that a block comes back exactly as it was received or not at all.
pub fn blocks_to_parts(blocks: &[LlmBlock], model: Option<&str>) -> Vec<Part> {
    let mut parts = Vec::new();
    for b in blocks {
        match b {
            LlmBlock::Text { text } => {
                if !text.is_empty() {
                    parts.push(Part::Text { text: text.clone() });
                }
            }
            LlmBlock::Reasoning { text, meta } => {
                if !text.trim().is_empty() || meta.is_some() {
                    parts.push(Part::Reasoning {
                        text: text.clone(),
                        meta: meta.clone(),
                        model: model.map(String::from),
                    });
                }
            }
            LlmBlock::ToolUse { id, name, input } => {
                parts.push(Part::ToolCall {
                    id: id.clone(),
                    name: name.clone(),
                    input: input.clone(),
                });
            }
        }
    }
    parts
}

// ---- test helpers -----------------------------------------------------------

/// A body that emits the given chunks, byte-for-byte, in order. Used by the
/// canned transports in this module's and the clients' tests.
#[cfg(test)]
pub fn body_of(chunks: Vec<&str>) -> ByteStream {
    let owned: Vec<Result<Vec<u8>, BoughError>> = chunks
        .into_iter()
        .map(|c| Ok(c.as_bytes().to_vec()))
        .collect();
    Box::pin(futures::stream::iter(owned))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    async fn collect(mut events: SseEvents) -> Vec<String> {
        let mut out = Vec::new();
        while let Some(p) = events.next().await.unwrap() {
            out.push(p);
        }
        out
    }

    fn run_steps() -> LlmToolDef {
        LlmToolDef {
            name: "run_steps".into(),
            description: "Run one JavaScript program in the workspace.".into(),
            input_schema: json!({
                "type": "object",
                "properties": { "code": { "type": "string" } },
                "required": ["code"],
                "additionalProperties": false,
            }),
        }
    }

    fn stop() -> LlmToolDef {
        LlmToolDef {
            name: "stop".into(),
            description: "End the turn.".into(),
            input_schema: json!({ "type": "object", "properties": {}, "additionalProperties": false }),
        }
    }

    #[tokio::test]
    async fn sse_yields_data_payloads_and_passes_done_through_untouched() {
        let events = SseEvents::new(body_of(vec!["data: {\"a\":1}\n", "data: [DONE]\n"]), "test");
        assert_eq!(collect(events).await, vec!["{\"a\":1}", "[DONE]"]);
    }

    #[tokio::test]
    async fn sse_a_frame_split_across_chunk_boundaries_still_parses() {
        // The transport decides where the packets break; a payload cut
        // mid-JSON must be reassembled, not dropped or half-parsed.
        let events = SseEvents::new(
            body_of(vec!["data: {\"de", "lta\":\"hi\"}\n", "data: {\"x\":2}\n"]),
            "test",
        );
        assert_eq!(
            collect(events).await,
            vec!["{\"delta\":\"hi\"}", "{\"x\":2}"]
        );
    }

    #[tokio::test]
    async fn sse_comments_blank_lines_and_event_lines_are_skipped() {
        let events = SseEvents::new(
            body_of(vec![
                ": keepalive\n",
                "\n",
                "event: ping\n",
                "data: {\"real\":true}\n",
            ]),
            "test",
        );
        assert_eq!(collect(events).await, vec!["{\"real\":true}"]);
    }

    #[tokio::test]
    async fn sse_a_trailing_un_newlined_fragment_is_dropped_not_half_yielded() {
        // It is by definition incomplete. The caller's "did I see a completion
        // marker?" check is what turns this into a retryable transport fault.
        let events = SseEvents::new(
            body_of(vec!["data: {\"ok\":1}\n", "data: {\"cut\":"]),
            "test",
        );
        assert_eq!(collect(events).await, vec!["{\"ok\":1}"]);
    }

    #[tokio::test]
    async fn sse_a_stalled_stream_fails_instead_of_hanging_the_turn() {
        // One chunk, and then never again, and never closes.
        let first: Vec<Result<Vec<u8>, BoughError>> = vec![Ok(b"data: {\"a\":1}\n".to_vec())];
        let body: ByteStream =
            Box::pin(futures::stream::iter(first).chain(futures::stream::pending()));
        let mut events = SseEvents::with_stall(body, "openrouter", 10);
        assert_eq!(events.next().await.unwrap(), Some("{\"a\":1}".into()));
        let err = events.next().await.unwrap_err();
        assert!(err.to_string().contains("stream stalled"), "{err}");
        // No status set → defaults to 502 → the retry ring will try again.
        assert_eq!(err.status(), 502);
        assert!(crate::llm::retry::is_retryable(&err));
    }

    #[tokio::test]
    async fn http_error_status_body_text_and_retry_after_all_survive() {
        let res = HttpResponse {
            status: 429,
            headers: vec![("retry-after".into(), "7".into())],
            body: body_of(vec!["quota exhausted"]),
        };
        let err = http_error("openrouter", res).await;
        assert_eq!(err.status(), 429);
        match &err {
            BoughError::Llm { retry_after_ms, .. } => assert_eq!(*retry_after_ms, Some(7000)),
            _ => unreachable!(),
        }
        assert!(err.to_string().contains("openrouter: 429"));
        assert!(err.to_string().contains("quota exhausted"));
    }

    #[tokio::test]
    async fn http_error_no_retry_after_leaves_the_hint_absent() {
        let res = HttpResponse {
            status: 500,
            headers: vec![],
            body: body_of(vec!["nope"]),
        };
        let err = http_error("openai", res).await;
        match &err {
            BoughError::Llm { retry_after_ms, .. } => assert_eq!(*retry_after_ms, None),
            _ => unreachable!(),
        }
    }

    #[test]
    fn parse_tool_args_well_formed_arguments_decode() {
        let v = parse_tool_args(
            "openai",
            Some(r#"{"code":"console.log(1)"}"#),
            Some(&run_steps()),
            "run_steps",
        )
        .unwrap();
        assert_eq!(v, json!({ "code": "console.log(1)" }));
    }

    #[test]
    fn parse_tool_args_the_schema_decides_whether_emptiness_is_legitimate() {
        // `stop` requires nothing, so no arguments is a real call.
        assert_eq!(
            parse_tool_args("openai", None, Some(&stop()), "stop").unwrap(),
            json!({})
        );
        // `run_steps` requires `code`, so no arguments means the stream was cut.
        let err = parse_tool_args("openai", None, Some(&run_steps()), "run_steps").unwrap_err();
        assert!(
            err.to_string()
                .contains("no arguments (truncated mid-call)"),
            "{err}"
        );
        assert!(crate::llm::retry::is_retryable(&err));
    }

    #[test]
    fn parse_tool_args_unknown_tool_with_no_arguments_is_not_assumed_truncated() {
        // No schema to judge by — `{}` is the only defensible reading, and the
        // tool dispatcher will report the unknown name properly.
        assert_eq!(
            parse_tool_args("openrouter", None, None, "mystery").unwrap(),
            json!({})
        );
    }

    #[test]
    fn parse_tool_args_half_a_json_object_is_a_truncation_not_a_parse_bug() {
        let err = parse_tool_args(
            "openrouter",
            Some(r#"{"code":"a"#),
            Some(&run_steps()),
            "run_steps",
        )
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("malformed arguments (truncated mid-call)"),
            "{err}"
        );
        assert!(
            err.to_string().starts_with("openrouter:"),
            "the provider must be named"
        );
    }

    #[test]
    fn blocks_to_parts_text_reasoning_and_tool_calls_map_across_in_order() {
        let blocks = vec![
            LlmBlock::Reasoning {
                text: "weighing options".into(),
                meta: Some(json!({ "signature": "sig" })),
            },
            LlmBlock::Text {
                text: "here goes".into(),
            },
            LlmBlock::ToolUse {
                id: "t1".into(),
                name: "run_steps".into(),
                input: json!({ "code": "1" }),
            },
        ];
        assert_eq!(
            blocks_to_parts(&blocks, Some("some-model")),
            vec![
                Part::Reasoning {
                    text: "weighing options".into(),
                    meta: Some(json!({ "signature": "sig" })),
                    model: Some("some-model".into()),
                },
                Part::Text {
                    text: "here goes".into()
                },
                Part::ToolCall {
                    id: "t1".into(),
                    name: "run_steps".into(),
                    input: json!({ "code": "1" }),
                },
            ]
        );
    }

    #[test]
    fn blocks_to_parts_the_signature_is_persisted_stamped_with_its_model() {
        // It is what lets the next turn replay the block verbatim. Providers
        // reject a thinking block whose content was altered, so the payload
        // has to survive the round trip through the database intact.
        let parts = blocks_to_parts(
            &[LlmBlock::Reasoning {
                text: "hmm".into(),
                meta: Some(json!({ "type": "thinking", "signature": "secret" })),
            }],
            Some("claude-opus-5"),
        );
        assert_eq!(
            parts,
            vec![Part::Reasoning {
                text: "hmm".into(),
                meta: Some(json!({ "type": "thinking", "signature": "secret" })),
                model: Some("claude-opus-5".into()),
            }]
        );
    }

    #[test]
    fn blocks_to_parts_with_no_model_to_stamp_reasoning_stays_display_only() {
        // An unstamped part can never satisfy replay's model gate, which is
        // the conservative answer for a caller not building a live request.
        let parts = blocks_to_parts(
            &[LlmBlock::Reasoning {
                text: "hmm".into(),
                meta: Some(json!({ "type": "thinking", "signature": "s" })),
            }],
            None,
        );
        assert_eq!(
            parts,
            vec![Part::Reasoning {
                text: "hmm".into(),
                meta: Some(json!({ "type": "thinking", "signature": "s" })),
                model: None,
            }]
        );
    }

    #[test]
    fn blocks_to_parts_a_redacted_block_persists_unsigned_empty_reasoning_does_not() {
        // A redacted thinking block has nothing displayable but must still go
        // back whole, so it is kept for its payload alone. Reasoning with
        // neither text nor payload is worth nothing to anyone.
        let parts = blocks_to_parts(
            &[
                LlmBlock::Reasoning {
                    text: "".into(),
                    meta: Some(json!({ "type": "redacted_thinking" })),
                },
                LlmBlock::Reasoning {
                    text: "   \n ".into(),
                    meta: None,
                },
                LlmBlock::Text { text: "".into() },
            ],
            Some("m1"),
        );
        assert_eq!(
            parts,
            vec![Part::Reasoning {
                text: "".into(),
                meta: Some(json!({ "type": "redacted_thinking" })),
                model: Some("m1".into()),
            }]
        );
    }

    #[test]
    fn blocks_to_parts_a_tool_call_with_no_input_still_yields_a_part() {
        // `stop` takes no arguments; the call is the whole message.
        let parts = blocks_to_parts(
            &[LlmBlock::ToolUse {
                id: "t9".into(),
                name: "stop".into(),
                input: json!({}),
            }],
            None,
        );
        assert_eq!(
            parts,
            vec![Part::ToolCall {
                id: "t9".into(),
                name: "stop".into(),
                input: json!({})
            }]
        );
    }
}
