//! The Anthropic route — raw HTTP + SSE, no SDK (spec llm.md §3a).
//!
//! Prompt caching uses three breakpoints (longer TTLs must precede shorter
//! ones; the budget is four):
//!
//!   - the STABLE system block at a 1-hour TTL — that prefix is byte-identical
//!     across sessions, so it warms new sessions and survives a lunch break;
//!   - the VOLATILE system block, also 1h — caches across turns within a
//!     session without splintering the shared prefix;
//!   - the final block of the final message, at the default 5-minute sliding
//!     TTL — extends the cached conversation prefix each round.
//!
//! A thinking block replays verbatim, signature included — the API rejects a
//! tool_use whose preceding thinking was altered or dropped. Usage is
//! normalized input-inclusive-of-cache so the context meter shows the true
//! prompt size. No hidden retries: the retry policy is `with_retries` only.

use std::sync::{Arc, LazyLock};

use regex::Regex;
use serde_json::{json, Map, Value};
use tokio_util::sync::CancellationToken;

use crate::error::LlmError;
use crate::routing::{require_key, Provider, ProviderOpts};
use crate::sse::{aborted, fetch_cancellable, http_error, parse_tool_args, SseEvents};
use crate::types::Usage;
use crate::types::{
    Effort, LlmBlock, LlmClient, LlmContentBlock, LlmMessage, LlmParams, LlmResult, LlmRole, OnText,
};

/// The two system tiers as Anthropic system blocks, each with a 1-hour cache
/// breakpoint. Order is load-bearing: the API caches everything *before* a
/// breakpoint, so the volatile block must never precede the stable one — one
/// per-session byte early in the prefix defeats cross-session cache sharing
/// entirely.
pub fn anthropic_system_blocks(
    system: Option<&str>,
    system_volatile: Option<&str>,
) -> Option<Value> {
    let blocks: Vec<Value> = [system, system_volatile]
        .into_iter()
        .flatten()
        .filter(|t| !t.is_empty())
        .map(|text| {
            json!({
                "type": "text",
                "text": text,
                "cache_control": { "type": "ephemeral", "ttl": "1h" },
            })
        })
        .collect();
    if blocks.is_empty() {
        None
    } else {
        Some(Value::Array(blocks))
    }
}

/// Our normalized message → the Anthropic wire shape.
pub fn to_api_message(m: &LlmMessage) -> Value {
    let mut content: Vec<Value> = Vec::new();
    for b in &m.content {
        match b {
            LlmContentBlock::Text { text } => {
                content.push(json!({ "type": "text", "text": text }));
            }
            LlmContentBlock::Image {
                data, media_type, ..
            } => {
                // The name is dropped: the wire shape has no slot for it.
                content.push(json!({
                    "type": "image",
                    "source": { "type": "base64", "media_type": media_type, "data": data },
                }));
            }
            LlmContentBlock::Reasoning { text, meta } => {
                // A thinking block replays verbatim, signature included — the
                // API rejects a tool_use whose preceding thinking was altered
                // or dropped.
                let meta_type = meta
                    .as_ref()
                    .and_then(|m| m.get("type"))
                    .and_then(|t| t.as_str());
                if matches!(meta_type, Some("thinking") | Some("redacted_thinking")) {
                    content.push(meta.clone().unwrap());
                } else if !text.trim().is_empty() {
                    // Foreign reasoning degrades to prose; an empty text block
                    // is rejected.
                    content.push(json!({ "type": "text", "text": text }));
                }
            }
            LlmContentBlock::ToolUse { id, name, input } => {
                let input = if input.is_null() {
                    json!({})
                } else {
                    input.clone()
                };
                content.push(json!({ "type": "tool_use", "id": id, "name": name, "input": input }));
            }
            LlmContentBlock::ToolResult {
                tool_use_id,
                content: text,
                is_error,
            } => {
                content.push(json!({
                    "type": "tool_result",
                    "tool_use_id": tool_use_id,
                    "content": text,
                    "is_error": is_error,
                }));
            }
        }
    }
    json!({
        "role": match m.role { LlmRole::User => "user", LlmRole::Assistant => "assistant" },
        "content": content,
    })
}

static EFFORT_MODELS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"claude-(fable|mythos|sonnet|opus)-5|opus-4-[89]").unwrap());

/// Thinking depth as request params: adaptive thinking with summarized
/// display, so the UI's reasoning folds carry text.
///
/// Guarded by model, because adaptive thinking exists only on the Claude 5
/// family and Opus 4.8+ — sending it to e.g. Haiku 4.5 is a hard 400, and a
/// per-session effort setting must not kill a turn just because the user
/// switched models. When the model is unknown the params ARE sent (the guard
/// is for known-incompatible models).
pub fn effort_params(effort: Option<Effort>, model: Option<&str>) -> Map<String, Value> {
    let supported = model.is_none_or(|m| EFFORT_MODELS.is_match(m));
    let mut out = Map::new();
    if let Some(effort) = effort {
        if supported {
            out.insert(
                "thinking".into(),
                json!({ "type": "adaptive", "display": "summarized" }),
            );
            out.insert(
                "output_config".into(),
                json!({ "effort": serde_json::to_value(effort).unwrap() }),
            );
        }
    }
    out
}

/// Build the full `/v1/messages` request body, three cache breakpoints placed.
fn request_body(params: &LlmParams) -> Value {
    let mut messages: Vec<Value> = params.messages.iter().map(to_api_message).collect();
    // Breakpoint 3: the final block of the final message, default 5-min TTL —
    // only when the last message's content is a non-empty array.
    if let Some(last) = messages.last_mut() {
        if let Some(content) = last.get_mut("content").and_then(|c| c.as_array_mut()) {
            if let Some(last_block) = content.last_mut() {
                if let Some(obj) = last_block.as_object_mut() {
                    obj.insert("cache_control".into(), json!({ "type": "ephemeral" }));
                }
            }
        }
    }
    let mut body = Map::new();
    body.insert("model".into(), json!(params.model));
    body.insert("max_tokens".into(), json!(params.max_tokens));
    body.insert("stream".into(), json!(true));
    if let Some(system) =
        anthropic_system_blocks(params.system.as_deref(), params.system_volatile.as_deref())
    {
        body.insert("system".into(), system);
    }
    body.insert("messages".into(), Value::Array(messages));
    body.insert(
        "tools".into(),
        Value::Array(
            params
                .tools
                .iter()
                .map(|t| {
                    json!({
                        "name": t.name,
                        "description": t.description,
                        "input_schema": t.input_schema,
                    })
                })
                .collect(),
        ),
    );
    if params.tool_choice_none {
        body.insert("tool_choice".into(), json!({ "type": "none" }));
    }
    for (k, v) in effort_params(params.effort, Some(&params.model)) {
        body.insert(k, v);
    }
    Value::Object(body)
}

/// One in-flight content block, assembled from the Messages SSE events.
enum Builder {
    Text(String),
    Thinking {
        thinking: String,
        signature: String,
    },
    /// The whole block, verbatim — nothing displayable, but it must be echoed
    /// back on the next round exactly as received.
    Redacted(Value),
    ToolUse {
        id: String,
        name: String,
        args: String,
    },
    /// Server tools etc. — we do not enable those features.
    Dropped,
}

struct AnthropicClient {
    opts: ProviderOpts,
    stall_ms: u64,
}

#[async_trait::async_trait]
impl LlmClient for AnthropicClient {
    async fn run(
        &self,
        params: LlmParams,
        on_text: OnText,
        cancel: CancellationToken,
    ) -> Result<LlmResult, LlmError> {
        let env = self.opts.env_or_default();
        let transport = self.opts.transport_or_default();
        // Keys are read at run() time, not at construction, so a key set
        // through the running server applies without a restart.
        let api_key = require_key(&env, Provider::Anthropic, &["ANTHROPIC_AUTH_TOKEN"])?;
        let base = env("ANTHROPIC_API_BASE").unwrap_or_else(|| "https://api.anthropic.com".into());
        let req = crate::sse::HttpRequest {
            url: format!("{base}/v1/messages"),
            headers: vec![
                ("x-api-key".into(), api_key),
                ("anthropic-version".into(), "2023-06-01".into()),
                ("content-type".into(), "application/json".into()),
            ],
            body: Some(request_body(&params).to_string()),
        };
        let res = fetch_cancellable(transport.as_ref(), req, &cancel, "anthropic").await?;
        if !res.ok() {
            return Err(http_error("anthropic", res).await);
        }

        let mut events = SseEvents::with_stall(res.body, "anthropic", self.stall_ms);
        let mut builders: Vec<Builder> = Vec::new();
        let mut input_tokens: i64 = 0;
        let mut cache_read: i64 = 0;
        let mut cache_write: i64 = 0;
        let mut output_tokens: i64 = 0;
        let mut stop_reason: Option<String> = None;
        let mut done = false;

        loop {
            let data = tokio::select! {
                _ = cancel.cancelled() => return Err(aborted("anthropic")),
                next = events.next() => next?,
            };
            let Some(data) = data else { break };
            let Ok(ev) = serde_json::from_str::<Value>(&data) else {
                continue;
            };
            match ev.get("type").and_then(|t| t.as_str()) {
                Some("message_start") => {
                    let usage = &ev["message"]["usage"];
                    input_tokens = usage["input_tokens"].as_i64().unwrap_or(0);
                    cache_read = usage["cache_read_input_tokens"].as_i64().unwrap_or(0);
                    cache_write = usage["cache_creation_input_tokens"].as_i64().unwrap_or(0);
                    output_tokens = usage["output_tokens"].as_i64().unwrap_or(0);
                }
                Some("content_block_start") => {
                    let block = &ev["content_block"];
                    builders.push(match block.get("type").and_then(|t| t.as_str()) {
                        Some("text") => {
                            Builder::Text(block["text"].as_str().unwrap_or("").to_string())
                        }
                        Some("thinking") => Builder::Thinking {
                            thinking: block["thinking"].as_str().unwrap_or("").to_string(),
                            signature: block["signature"].as_str().unwrap_or("").to_string(),
                        },
                        // A redacted block arrives whole in the start event.
                        Some("redacted_thinking") => Builder::Redacted(block.clone()),
                        Some("tool_use") => Builder::ToolUse {
                            id: block["id"].as_str().unwrap_or("").to_string(),
                            name: block["name"].as_str().unwrap_or("").to_string(),
                            args: String::new(),
                        },
                        _ => Builder::Dropped,
                    });
                }
                Some("content_block_delta") => {
                    let delta = &ev["delta"];
                    let builder = builders.last_mut();
                    match (delta.get("type").and_then(|t| t.as_str()), builder) {
                        (Some("text_delta"), Some(Builder::Text(text))) => {
                            let piece = delta["text"].as_str().unwrap_or("");
                            text.push_str(piece);
                            on_text(piece);
                        }
                        (Some("thinking_delta"), Some(Builder::Thinking { thinking, .. })) => {
                            thinking.push_str(delta["thinking"].as_str().unwrap_or(""));
                        }
                        (Some("signature_delta"), Some(Builder::Thinking { signature, .. })) => {
                            signature.push_str(delta["signature"].as_str().unwrap_or(""));
                        }
                        (Some("input_json_delta"), Some(Builder::ToolUse { args, .. })) => {
                            args.push_str(delta["partial_json"].as_str().unwrap_or(""));
                        }
                        _ => {}
                    }
                }
                Some("message_delta") => {
                    if let Some(reason) = ev["delta"]["stop_reason"].as_str() {
                        stop_reason = Some(reason.to_string());
                    }
                    if let Some(out) = ev["usage"]["output_tokens"].as_i64() {
                        output_tokens = out;
                    }
                }
                Some("message_stop") => {
                    done = true;
                }
                Some("error") => {
                    // A mid-stream error event is server-side; status-less ⇒
                    // 502 ⇒ retryable.
                    let message = ev["error"]["message"].as_str().unwrap_or(&data).to_string();
                    return Err(LlmError::new(format!("anthropic: {message}")));
                }
                _ => {} // ping, content_block_stop, unknown
            }
        }
        // No terminal marker at all → the stream was cut → a transport fault.
        if !done {
            return Err(LlmError::new(
                "anthropic: stream ended without message_stop",
            ));
        }

        let mut content: Vec<LlmBlock> = Vec::new();
        for b in builders {
            match b {
                Builder::Text(text) => content.push(LlmBlock::Text { text }),
                Builder::Thinking {
                    thinking,
                    signature,
                } => {
                    // Keep the raw block (signature included) for verbatim
                    // in-turn replay.
                    let meta = json!({
                        "type": "thinking",
                        "thinking": thinking,
                        "signature": signature,
                    });
                    content.push(LlmBlock::Reasoning {
                        text: thinking,
                        meta: Some(meta),
                    });
                }
                Builder::Redacted(block) => {
                    // Nothing displayable, but the block must still be echoed
                    // on the next round.
                    content.push(LlmBlock::Reasoning {
                        text: String::new(),
                        meta: Some(block),
                    });
                }
                Builder::ToolUse { id, name, args } => {
                    let raw = if args.is_empty() {
                        None
                    } else {
                        Some(args.as_str())
                    };
                    let tool = params.tools.iter().find(|t| t.name == name);
                    let input = parse_tool_args("anthropic", raw, tool, &name)?;
                    content.push(LlmBlock::ToolUse { id, name, input });
                }
                Builder::Dropped => {}
            }
        }

        let usage = Usage {
            // `input_tokens` is the uncached remainder; add reads and writes
            // back so the context meter shows the true prompt size.
            input_tokens: input_tokens + cache_read + cache_write,
            output_tokens,
            reasoning_tokens: None,
            cache_read_tokens: Some(cache_read),
            cache_write_tokens: Some(cache_write),
            cost_usd: None,
        };
        Ok(LlmResult {
            content,
            stop_reason: stop_reason.unwrap_or_else(|| "end_turn".into()),
            usage: Some(usage),
        })
    }
}

/// The bare Anthropic client, without retries or pricing.
pub fn anthropic_client(opts: ProviderOpts) -> Arc<dyn LlmClient> {
    Arc::new(AnthropicClient {
        opts,
        stall_ms: crate::sse::STALL_TIMEOUT_MS,
    })
}

/// The stall knob, turned down so an assertion does not take a minute. The
/// stall guard itself is pinned at the sse layer (`llm::sse`, porting
/// src/llm/stream.test.ts:72), which is the only place TS tests it.
#[cfg(test)]
#[allow(dead_code)]
pub(crate) fn anthropic_client_with_stall(opts: ProviderOpts, stall_ms: u64) -> Arc<dyn LlmClient> {
    Arc::new(AnthropicClient { opts, stall_ms })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{keyed_env, params_over, CannedTransport, TOOLS};
    use std::sync::Mutex;

    #[test]
    fn system_blocks_stable_first_a_1h_breakpoint_on_each() {
        let blocks = anthropic_system_blocks(Some("STABLE"), Some("VOLATILE")).unwrap();
        let blocks = blocks.as_array().unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0]["text"], "STABLE");
        assert_eq!(blocks[1]["text"], "VOLATILE");
        for b in blocks {
            assert_eq!(
                b["cache_control"],
                json!({ "type": "ephemeral", "ttl": "1h" })
            );
        }
    }

    #[test]
    fn system_blocks_none_when_there_is_no_system_text() {
        assert_eq!(anthropic_system_blocks(None, None), None);
        assert_eq!(anthropic_system_blocks(Some(""), None), None);
        let one = anthropic_system_blocks(None, Some("only volatile")).unwrap();
        assert_eq!(one.as_array().unwrap().len(), 1);
    }

    #[test]
    fn to_api_message_a_thinking_block_replays_verbatim_signature_included() {
        let raw = json!({ "type": "thinking", "thinking": "step one", "signature": "sig-abc" });
        let msg = to_api_message(&LlmMessage {
            role: LlmRole::Assistant,
            content: vec![
                LlmContentBlock::Reasoning {
                    text: "step one".into(),
                    meta: Some(raw.clone()),
                },
                LlmContentBlock::ToolUse {
                    id: "t1".into(),
                    name: "run_steps".into(),
                    input: json!({ "code": "x" }),
                },
            ],
        });
        let content = msg["content"].as_array().unwrap();
        assert_eq!(content[0], raw);
        assert_eq!(content[1]["type"], "tool_use");
    }

    #[test]
    fn to_api_message_foreign_reasoning_degrades_to_prose_empty_reasoning_vanishes() {
        let with_text = to_api_message(&LlmMessage {
            role: LlmRole::Assistant,
            content: vec![LlmContentBlock::Reasoning {
                text: "a summary".into(),
                meta: Some(json!({ "type": "reasoning" })),
            }],
        });
        assert_eq!(
            with_text["content"],
            json!([{ "type": "text", "text": "a summary" }])
        );

        // A summary-less item would become an empty text block, which the API
        // rejects.
        let empty = to_api_message(&LlmMessage {
            role: LlmRole::Assistant,
            content: vec![LlmContentBlock::Reasoning {
                text: "   ".into(),
                meta: Some(json!({ "type": "reasoning" })),
            }],
        });
        assert_eq!(empty["content"], json!([]));
    }

    #[test]
    fn to_api_message_tool_results_and_images_take_their_native_shapes() {
        let msg = to_api_message(&LlmMessage {
            role: LlmRole::User,
            content: vec![
                LlmContentBlock::ToolResult {
                    tool_use_id: "t1".into(),
                    content: "out".into(),
                    is_error: false,
                },
                LlmContentBlock::Image {
                    data: "AAAA".into(),
                    media_type: "image/png".into(),
                    name: "shot.png".into(),
                },
            ],
        });
        let content = msg["content"].as_array().unwrap();
        assert_eq!(
            content[0],
            json!({ "type": "tool_result", "tool_use_id": "t1", "content": "out", "is_error": false })
        );
        assert_eq!(
            content[1],
            json!({
                "type": "image",
                "source": { "type": "base64", "media_type": "image/png", "data": "AAAA" },
            })
        );
    }

    #[test]
    fn effort_params_only_sent_to_models_that_accept_adaptive_thinking() {
        let sent = effort_params(Some(Effort::High), Some("claude-opus-5"));
        assert_eq!(
            sent["thinking"],
            json!({ "type": "adaptive", "display": "summarized" })
        );
        assert_eq!(sent["output_config"], json!({ "effort": "high" }));
        assert!(effort_params(Some(Effort::Low), Some("claude-opus-4-8")).contains_key("thinking"));
        // Haiku 4.5 hard-400s on the param: an effort setting must not kill
        // the turn.
        assert!(effort_params(Some(Effort::High), Some("claude-haiku-4-5")).is_empty());
        // No effort at all leaves the request shape untouched.
        assert!(effort_params(None, Some("claude-opus-5")).is_empty());
        // Unknown model: the params ARE sent (the guard is for known-
        // incompatible models).
        assert!(effort_params(Some(Effort::High), None).contains_key("thinking"));
    }

    // ---- canned-SSE round trips ---------------------------------------------

    fn anthropic_sse() -> Vec<String> {
        [
            json!({ "type": "message_start", "message": { "usage": {
                "input_tokens": 100, "output_tokens": 1,
                "cache_read_input_tokens": 40, "cache_creation_input_tokens": 10 } } }),
            json!({ "type": "content_block_start", "index": 0,
                "content_block": { "type": "thinking", "thinking": "", "signature": "" } }),
            json!({ "type": "content_block_delta", "index": 0,
                "delta": { "type": "thinking_delta", "thinking": "step one" } }),
            json!({ "type": "content_block_delta", "index": 0,
                "delta": { "type": "signature_delta", "signature": "sig-abc" } }),
            json!({ "type": "content_block_stop", "index": 0 }),
            json!({ "type": "content_block_start", "index": 1,
                "content_block": { "type": "text", "text": "" } }),
            json!({ "type": "content_block_delta", "index": 1,
                "delta": { "type": "text_delta", "text": "wor" } }),
            json!({ "type": "content_block_delta", "index": 1,
                "delta": { "type": "text_delta", "text": "king" } }),
            json!({ "type": "content_block_stop", "index": 1 }),
            json!({ "type": "content_block_start", "index": 2,
                "content_block": { "type": "tool_use", "id": "t1", "name": "run_steps", "input": {} } }),
            json!({ "type": "content_block_delta", "index": 2,
                "delta": { "type": "input_json_delta", "partial_json": "{\"co" } }),
            json!({ "type": "content_block_delta", "index": 2,
                "delta": { "type": "input_json_delta", "partial_json": "de\":\"1\"}" } }),
            json!({ "type": "content_block_stop", "index": 2 }),
            json!({ "type": "message_delta", "delta": { "stop_reason": "tool_use" },
                "usage": { "output_tokens": 20 } }),
            json!({ "type": "message_stop" }),
        ]
        .iter()
        .map(|v| v.to_string())
        .collect()
    }

    #[tokio::test]
    async fn a_full_round_three_cache_breakpoints_in_order_and_normalized_usage() {
        let transport = Arc::new(CannedTransport::sse(vec![anthropic_sse()]));
        let client = anthropic_client(ProviderOpts {
            env: Some(keyed_env()),
            transport: Some(transport.clone()),
        });
        let deltas: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let deltas2 = deltas.clone();
        let params = params_over("claude-opus-5", &TOOLS, |p| {
            p.system = Some("STABLE".into());
            p.system_volatile = Some("VOLATILE".into());
            p.effort = Some(Effort::High);
        });
        let result = client
            .run(
                params,
                Arc::new(move |d| deltas2.lock().unwrap().push(d.to_string())),
                CancellationToken::new(),
            )
            .await
            .unwrap();

        // The request: three breakpoints, in order.
        let requests = transport.requests.lock().unwrap();
        assert_eq!(requests[0].url, "https://api.anthropic.com/v1/messages");
        assert!(requests[0]
            .headers
            .iter()
            .any(|(k, v)| k == "x-api-key" && v == "test-key"));
        assert!(requests[0]
            .headers
            .iter()
            .any(|(k, v)| k == "anthropic-version" && v == "2023-06-01"));
        let body: Value = serde_json::from_str(requests[0].body.as_ref().unwrap()).unwrap();
        let system = body["system"].as_array().unwrap();
        // (1) stable @1h, (2) volatile @1h — stable MUST precede volatile.
        assert_eq!(system[0]["text"], "STABLE");
        assert_eq!(
            system[0]["cache_control"],
            json!({ "type": "ephemeral", "ttl": "1h" })
        );
        assert_eq!(system[1]["text"], "VOLATILE");
        assert_eq!(
            system[1]["cache_control"],
            json!({ "type": "ephemeral", "ttl": "1h" })
        );
        // (3) the last content block of the last message, default 5-min TTL.
        let messages = body["messages"].as_array().unwrap();
        let last_content = messages.last().unwrap()["content"].as_array().unwrap();
        assert_eq!(
            last_content.last().unwrap()["cache_control"],
            json!({ "type": "ephemeral" })
        );
        // Effort params ride along for a supported model.
        assert_eq!(
            body["thinking"],
            json!({ "type": "adaptive", "display": "summarized" })
        );
        assert_eq!(body["output_config"], json!({ "effort": "high" }));
        assert_eq!(body["stream"], json!(true));

        // The round: deltas streamed, blocks assembled, usage normalized.
        assert_eq!(*deltas.lock().unwrap(), vec!["wor", "king"]);
        assert_eq!(result.stop_reason, "tool_use");
        assert_eq!(
            result.content[0],
            LlmBlock::Reasoning {
                text: "step one".into(),
                meta: Some(json!({
                    "type": "thinking", "thinking": "step one", "signature": "sig-abc" })),
            }
        );
        assert_eq!(
            result.content[1],
            LlmBlock::Text {
                text: "working".into()
            }
        );
        assert_eq!(
            result.content[2],
            LlmBlock::ToolUse {
                id: "t1".into(),
                name: "run_steps".into(),
                input: json!({ "code": "1" }),
            }
        );
        // input_tokens is the uncached remainder — reads and writes are added
        // back so the context meter shows the true prompt size.
        let usage = result.usage.unwrap();
        assert_eq!(usage.input_tokens, 150);
        assert_eq!(usage.output_tokens, 20);
        assert_eq!(usage.cache_read_tokens, Some(40));
        assert_eq!(usage.cache_write_tokens, Some(10));
    }

    #[tokio::test]
    async fn the_thinking_meta_replayed_on_the_next_round_is_byte_identical() {
        // Round-trip: what the client assembled goes back out verbatim.
        let transport = Arc::new(CannedTransport::sse(vec![anthropic_sse()]));
        let client = anthropic_client(ProviderOpts {
            env: Some(keyed_env()),
            transport: Some(transport.clone()),
        });
        let result = client
            .run(
                params_over("claude-opus-5", &TOOLS, |_| {}),
                Arc::new(|_| {}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let LlmBlock::Reasoning { text, meta } = &result.content[0] else {
            panic!()
        };
        let msg = to_api_message(&LlmMessage {
            role: LlmRole::Assistant,
            content: vec![LlmContentBlock::Reasoning {
                text: text.clone(),
                meta: meta.clone(),
            }],
        });
        assert_eq!(msg["content"][0], *meta.as_ref().unwrap());
    }

    #[tokio::test]
    async fn no_breakpoint_is_stamped_on_an_empty_last_message() {
        let transport = Arc::new(CannedTransport::sse(vec![anthropic_sse()]));
        let client = anthropic_client(ProviderOpts {
            env: Some(keyed_env()),
            transport: Some(transport.clone()),
        });
        let params = params_over("claude-opus-5", &TOOLS, |p| {
            p.messages = vec![LlmMessage {
                role: LlmRole::User,
                content: vec![],
            }];
        });
        client
            .run(params, Arc::new(|_| {}), CancellationToken::new())
            .await
            .unwrap();
        let requests = transport.requests.lock().unwrap();
        let body: Value = serde_json::from_str(requests[0].body.as_ref().unwrap()).unwrap();
        assert_eq!(body["messages"][0]["content"], json!([]));
    }

    #[tokio::test]
    async fn a_stream_that_ends_without_message_stop_is_a_transport_fault() {
        let cut: Vec<String> = anthropic_sse().into_iter().take(8).collect();
        let transport = Arc::new(CannedTransport::sse(vec![cut]));
        let client = anthropic_client(ProviderOpts {
            env: Some(keyed_env()),
            transport: Some(transport),
        });
        let err = client
            .run(
                params_over("claude-opus-5", &TOOLS, |_| {}),
                Arc::new(|_| {}),
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("stream ended without message_stop"),
            "{err}"
        );
        assert!(
            crate::retry::is_retryable(&err),
            "a cut stream must be retryable"
        );
    }

    #[tokio::test]
    async fn a_non_2xx_carries_its_status_and_retry_after_into_the_error() {
        let transport = Arc::new(CannedTransport::plain(vec![(
            429,
            vec![("retry-after".to_string(), "3".to_string())],
            "slow down".to_string(),
        )]));
        let client = anthropic_client(ProviderOpts {
            env: Some(keyed_env()),
            transport: Some(transport),
        });
        let err = client
            .run(
                params_over("claude-opus-5", &TOOLS, |_| {}),
                Arc::new(|_| {}),
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert_eq!(err.status(), 429);
        assert_eq!(err.retry_after_ms, Some(3000));
        assert!(crate::retry::is_retryable(&err));
    }

    #[tokio::test]
    async fn a_missing_key_is_a_401_naming_the_env_vars_before_any_fetch() {
        let transport = Arc::new(CannedTransport::sse(vec![]));
        let client = anthropic_client(ProviderOpts {
            env: Some(Arc::new(|_| None)),
            transport: Some(transport.clone()),
        });
        let err = client
            .run(
                params_over("claude-opus-5", &TOOLS, |_| {}),
                Arc::new(|_| {}),
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert_eq!(err.status(), 401);
        assert!(err.to_string().contains("ANTHROPIC_API_KEY"), "{err}");
        assert_eq!(
            transport.requests.lock().unwrap().len(),
            0,
            "no fetch without a key"
        );
    }
}
