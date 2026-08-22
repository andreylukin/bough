//! OpenAI proper — the Responses API (port of the OpenAI half of
//! `src/llm/client.ts`; plan row 3.15).
//!
//! Chat/completions cannot combine function tools with reasoning on the
//! gpt-5/o* families, so OpenAI rides `/v1/responses`. Stateless
//! (`store: false`): each round replays the whole history as input items,
//! with reasoning items — their encrypted content requested via `include` —
//! echoed back **verbatim before their `function_call`**, because the API
//! rejects a function_call whose reasoning item is missing. Those items ride
//! `LlmBlock::meta` through the turn's in-memory round loop; across turns the
//! replay mapper drops them and old function_calls replay bare, which is
//! accepted, since the pairing rule binds items of the live chain.
//!
//! The two shapes a naive port gets wrong:
//!
//! - **meta-less reasoning is DROPPED, not sent bare** — a reasoning block
//!   with prose but no provider item is display text, and inventing an item
//!   for it 400s the request;
//! - **the final content comes whole from `response.completed`** — the
//!   `output_text.delta` events are display-only, so there is no per-item
//!   assembly to get wrong.

use std::sync::Arc;

use serde_json::{json, Map, Value};
use tokio_util::sync::CancellationToken;

use crate::error::LlmError;
use crate::routing::{joined_system, require_key, Provider, ProviderOpts};
use crate::sse::{aborted, fetch_cancellable, http_error, parse_tool_args, SseEvents};
use crate::types::Usage;
use crate::types::{
    Effort, LlmBlock, LlmClient, LlmContentBlock, LlmMessage, LlmParams, LlmResult, LlmRole,
    LlmToolDef, OnText,
};

/// `LlmMessage[]` → Responses `input` items, flattened in block order.
///
/// Reasoning items are emitted from `meta` **verbatim** and only when present:
/// a meta-less reasoning block is dropped rather than sent bare (test-pinned).
pub fn to_responses_input(messages: &[LlmMessage]) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    for m in messages {
        for b in &m.content {
            match b {
                LlmContentBlock::Text { text } => out.push(if m.role == LlmRole::User {
                    json!({ "role": "user", "content": [{ "type": "input_text", "text": text }] })
                } else {
                    json!({
                        "role": "assistant",
                        "content": [{ "type": "output_text", "text": text }],
                    })
                }),
                LlmContentBlock::Image {
                    data, media_type, ..
                } => out.push(json!({
                    "role": "user",
                    "content": [{
                        "type": "input_image",
                        "image_url": format!("data:{media_type};base64,{data}"),
                    }],
                })),
                LlmContentBlock::Reasoning { meta, .. } => {
                    // The raw reasoning item, replayed verbatim. TS `if (b.meta)`:
                    // a null meta is as absent as a missing one.
                    if let Some(meta) = meta.as_ref().filter(|m| !m.is_null()) {
                        out.push(meta.clone());
                    }
                }
                LlmContentBlock::ToolUse { id, name, input } => {
                    let input = if input.is_null() {
                        json!({})
                    } else {
                        input.clone()
                    };
                    out.push(json!({
                        "type": "function_call",
                        "call_id": id,
                        "name": name,
                        "arguments": input.to_string(),
                    }));
                }
                LlmContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    ..
                } => out.push(json!({
                    "type": "function_call_output",
                    "call_id": tool_use_id,
                    "output": content,
                })),
            }
        }
    }
    out
}

/// A Responses `output` array → our normalized blocks. `tools` is what lets a
/// truncated function call be told apart from a legitimately argument-less
/// one (`parse_tool_args`, spec §2).
pub fn from_responses_output(
    output: &[Value],
    tools: &[LlmToolDef],
) -> Result<Vec<LlmBlock>, LlmError> {
    let mut blocks = Vec::new();
    for item in output {
        match item["type"].as_str() {
            Some("message") => {
                let text: String = item["content"]
                    .as_array()
                    .map(|parts| {
                        parts
                            .iter()
                            .filter(|c| c["type"] == "output_text")
                            .map(|c| c["text"].as_str().unwrap_or(""))
                            .collect()
                    })
                    .unwrap_or_default();
                if !text.is_empty() {
                    blocks.push(LlmBlock::Text { text });
                }
            }
            Some("function_call") => {
                let name = item["name"].as_str().unwrap_or("").to_string();
                let input = parse_tool_args(
                    "openai",
                    item["arguments"].as_str(),
                    tools.iter().find(|t| t.name == name),
                    &name,
                )?;
                blocks.push(LlmBlock::ToolUse {
                    id: item["call_id"].as_str().unwrap_or("").to_string(),
                    name,
                    input,
                });
            }
            Some("reasoning") => {
                let text = item["summary"]
                    .as_array()
                    .map(|s| {
                        s.iter()
                            .map(|e| e["text"].as_str().unwrap_or(""))
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default();
                blocks.push(LlmBlock::Reasoning {
                    text,
                    meta: Some(item.clone()),
                });
            }
            // Anything else (server tools etc.) is dropped.
            _ => {}
        }
    }
    Ok(blocks)
}

/// The Responses API caps reasoning effort at `"high"`.
fn effort_str(effort: Effort) -> &'static str {
    match effort {
        Effort::Low => "low",
        Effort::Medium => "medium",
        Effort::High | Effort::Xhigh | Effort::Max => "high",
    }
}

struct OpenAIClient {
    opts: ProviderOpts,
    stall_ms: u64,
}

#[async_trait::async_trait]
impl LlmClient for OpenAIClient {
    async fn run(
        &self,
        params: LlmParams,
        on_text: OnText,
        cancel: CancellationToken,
    ) -> Result<LlmResult, LlmError> {
        let provider = "openai";
        let env = self.opts.env_or_default();
        let transport = self.opts.transport_or_default();
        let api_key = require_key(&env, Provider::Openai, &[])?;
        let base = env("OPENAI_API_BASE").unwrap_or_else(|| "https://api.openai.com".to_string());
        // The picker id carries the routing prefix; the wire wants the bare
        // model.
        let model = params
            .model
            .strip_prefix("openai:")
            .unwrap_or(&params.model)
            .to_string();

        let mut body = Map::new();
        body.insert("model".into(), json!(model));
        if let Some(instructions) = joined_system(&params) {
            body.insert("instructions".into(), json!(instructions));
        }
        body.insert("max_output_tokens".into(), json!(params.max_tokens));
        body.insert("stream".into(), json!(true));
        body.insert("store".into(), json!(false));
        body.insert("include".into(), json!(["reasoning.encrypted_content"]));
        if let Some(effort) = params.effort {
            body.insert("reasoning".into(), json!({ "effort": effort_str(effort) }));
        }
        body.insert(
            "input".into(),
            Value::Array(to_responses_input(&params.messages)),
        );
        body.insert(
            "tools".into(),
            Value::Array(
                params
                    .tools
                    .iter()
                    .map(|t| {
                        json!({
                            "type": "function",
                            "name": t.name,
                            "description": t.description,
                            "parameters": t.input_schema,
                        })
                    })
                    .collect(),
            ),
        );
        if params.tool_choice_none {
            // A bare string, not an object.
            body.insert("tool_choice".into(), json!("none"));
        }

        let req = crate::sse::HttpRequest {
            url: format!("{base}/v1/responses"),
            headers: vec![
                ("authorization".to_string(), format!("Bearer {api_key}")),
                ("content-type".to_string(), "application/json".to_string()),
            ],
            body: Some(Value::Object(body).to_string()),
        };
        let res = fetch_cancellable(transport.as_ref(), req, &cancel, provider).await?;
        if !res.ok() {
            return Err(http_error(provider, res).await);
        }

        // Deltas stream for the live feel; the final content comes whole from
        // the response.completed payload, so there is no per-item assembly to
        // get wrong.
        let mut events = SseEvents::with_stall(res.body, provider, self.stall_ms);
        let mut final_response: Option<Value> = None;
        loop {
            let data = tokio::select! {
                _ = cancel.cancelled() => return Err(aborted(provider)),
                next = events.next() => next?,
            };
            let Some(data) = data else { break };
            if data == "[DONE]" {
                continue;
            }
            // Unparseable data lines are silently skipped.
            let Ok(ev) = serde_json::from_str::<Value>(&data) else {
                continue;
            };
            match ev["type"].as_str() {
                Some("response.output_text.delta") => {
                    if let Some(delta) = ev["delta"].as_str().filter(|d| !d.is_empty()) {
                        on_text(delta);
                    }
                }
                Some("response.completed") | Some("response.incomplete") => {
                    if ev["response"].is_object() {
                        final_response = Some(ev["response"].clone());
                    }
                }
                Some("response.failed") | Some("error") if final_response.is_none() => {
                    // A mid-stream failure event is server-side (the request
                    // itself was accepted), so it classifies retryable; rate
                    // limits by their code.
                    let code = ev["response"]["error"]["code"].as_str().unwrap_or("");
                    let status = if code.contains("rate_limit") {
                        429
                    } else {
                        500
                    };
                    return Err(LlmError::with(format!("openai: {ev}"), status, None));
                }
                _ => {}
            }
        }
        // No terminal status at all → the stream was cut → a transport fault.
        let Some(final_response) = final_response else {
            return Err(LlmError::new(
                "openai: stream ended without response.completed",
            ));
        };

        let output = final_response["output"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let content = from_responses_output(&output, &params.tools)?;
        let stop_reason = if content
            .iter()
            .any(|b| matches!(b, LlmBlock::ToolUse { .. }))
        {
            "tool_use"
        } else if final_response["status"] == "incomplete"
            && final_response["incomplete_details"]["reason"] == "max_output_tokens"
        {
            "max_tokens"
        } else {
            "end_turn"
        };
        let usage = &final_response["usage"];
        Ok(LlmResult {
            content,
            stop_reason: stop_reason.to_string(),
            usage: Some(Usage {
                // The Responses `input_tokens` already includes the cached
                // read — unlike Anthropic's, it is not a remainder.
                input_tokens: usage["input_tokens"].as_i64().unwrap_or(0),
                output_tokens: usage["output_tokens"].as_i64().unwrap_or(0),
                reasoning_tokens: Some(
                    usage["output_tokens_details"]["reasoning_tokens"]
                        .as_i64()
                        .unwrap_or(0),
                ),
                cache_read_tokens: Some(
                    usage["input_tokens_details"]["cached_tokens"]
                        .as_i64()
                        .unwrap_or(0),
                ),
                cache_write_tokens: Some(0),
                cost_usd: None,
            }),
        })
    }
}

pub fn openai_client(opts: ProviderOpts) -> Arc<dyn LlmClient> {
    Arc::new(OpenAIClient {
        opts,
        stall_ms: crate::sse::STALL_TIMEOUT_MS,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::retry::is_retryable;
    use crate::test_support::{keyed_env, params_over, CannedTransport, TOOLS};
    use std::sync::Mutex;

    fn opts(transport: Arc<CannedTransport>) -> ProviderOpts {
        ProviderOpts {
            env: Some(keyed_env()),
            transport: Some(transport),
        }
    }

    #[test]
    fn to_responses_input_reasoning_items_ride_through_verbatim_before_their_call() {
        let reasoning = json!({ "type": "reasoning", "id": "rs_1", "encrypted_content": "enc" });
        let input = to_responses_input(&[
            LlmMessage {
                role: LlmRole::User,
                content: vec![LlmContentBlock::Text { text: "go".into() }],
            },
            LlmMessage {
                role: LlmRole::Assistant,
                content: vec![
                    LlmContentBlock::Reasoning {
                        text: "".into(),
                        meta: Some(reasoning.clone()),
                    },
                    LlmContentBlock::ToolUse {
                        id: "call_1".into(),
                        name: "run_steps".into(),
                        input: json!({ "code": "1" }),
                    },
                ],
            },
            LlmMessage {
                role: LlmRole::User,
                content: vec![LlmContentBlock::ToolResult {
                    tool_use_id: "call_1".into(),
                    content: "1".into(),
                    is_error: false,
                }],
            },
        ]);
        assert_eq!(
            input[0],
            json!({ "role": "user", "content": [{ "type": "input_text", "text": "go" }] })
        );
        // Verbatim, and BEFORE the call it belongs to — the API rejects a
        // function_call whose reasoning item is missing.
        assert_eq!(input[1], reasoning);
        assert_eq!(
            input[2],
            json!({
                "type": "function_call",
                "call_id": "call_1",
                "name": "run_steps",
                "arguments": "{\"code\":\"1\"}",
            })
        );
        assert_eq!(
            input[3],
            json!({ "type": "function_call_output", "call_id": "call_1", "output": "1" })
        );
    }

    #[test]
    fn to_responses_input_reasoning_with_no_meta_is_dropped_not_sent_bare() {
        let input = to_responses_input(&[LlmMessage {
            role: LlmRole::Assistant,
            content: vec![LlmContentBlock::Reasoning {
                text: "prose only".into(),
                meta: None,
            }],
        }]);
        assert!(input.is_empty(), "a bare reasoning item 400s the request");
    }

    #[test]
    fn from_responses_output_a_call_missing_required_arguments_is_a_truncation() {
        // The tool requires `code`, so an argument-less call was cut off
        // mid-stream. Inventing `{}` here would run the wrong program.
        let err = from_responses_output(
            &[json!({ "type": "function_call", "call_id": "c1", "name": "run_steps" })],
            &TOOLS,
        )
        .unwrap_err();
        assert!(err.to_string().contains("truncated mid-call"), "{err}");
    }

    #[test]
    fn from_responses_output_an_argument_less_call_is_fine_when_nothing_is_required() {
        let blocks = from_responses_output(
            &[json!({ "type": "function_call", "call_id": "c1", "name": "stop" })],
            &TOOLS,
        )
        .unwrap();
        assert_eq!(
            blocks,
            vec![LlmBlock::ToolUse {
                id: "c1".into(),
                name: "stop".into(),
                input: json!({})
            }]
        );
    }

    #[test]
    fn from_responses_output_malformed_argument_json_is_a_truncation_too() {
        let err = from_responses_output(
            &[json!({
                "type": "function_call", "call_id": "c1", "name": "run_steps",
                "arguments": "{\"code\":\"a",
            })],
            &TOOLS,
        )
        .unwrap_err();
        assert!(err.to_string().contains("malformed arguments"), "{err}");
    }

    #[tokio::test]
    async fn a_full_round_strips_the_prefix_and_normalizes_usage() {
        let transport = Arc::new(CannedTransport::sse(vec![vec![
            json!({ "type": "response.output_text.delta", "delta": "wor" }).to_string(),
            json!({ "type": "response.output_text.delta", "delta": "king" }).to_string(),
            json!({
                "type": "response.completed",
                "response": {
                    "status": "completed",
                    "output": [
                        { "type": "reasoning", "summary": [{ "text": "plan" }], "id": "rs_1" },
                        { "type": "message",
                          "content": [{ "type": "output_text", "text": "working" }] },
                        {
                            "type": "function_call", "call_id": "call_9", "name": "run_steps",
                            "arguments": "{\"code\":\"1\"}",
                        },
                    ],
                    "usage": {
                        "input_tokens": 100,
                        "output_tokens": 20,
                        "input_tokens_details": { "cached_tokens": 40 },
                        "output_tokens_details": { "reasoning_tokens": 7 },
                    },
                },
            })
            .to_string(),
            "[DONE]".to_string(),
        ]]));
        let deltas = Arc::new(Mutex::new(Vec::<String>::new()));
        let sink = deltas.clone();
        let result = openai_client(opts(transport.clone()))
            .run(
                params_over("openai:gpt-5", &TOOLS, |p| {
                    p.system = Some("S".into());
                    p.system_volatile = Some("V".into());
                    p.effort = Some(Effort::Max);
                }),
                Arc::new(move |d| sink.lock().unwrap().push(d.to_string())),
                CancellationToken::new(),
            )
            .await
            .unwrap();

        let body: Value = {
            let requests = transport.requests.lock().unwrap();
            assert_eq!(requests[0].url, "https://api.openai.com/v1/responses");
            serde_json::from_str(requests[0].body.as_ref().unwrap()).unwrap()
        };
        assert_eq!(
            body["model"], "gpt-5",
            "the openai: prefix is routing, not a model name"
        );
        assert_eq!(body["instructions"], "SV");
        assert_eq!(body["store"], false);
        assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
        // The Responses API caps reasoning effort at "high".
        assert_eq!(body["reasoning"], json!({ "effort": "high" }));
        assert!(body.get("tool_choice").is_none(), "omitted unless set");

        assert_eq!(
            *deltas.lock().unwrap(),
            vec!["wor".to_string(), "king".to_string()]
        );
        assert_eq!(result.stop_reason, "tool_use");
        assert_eq!(
            result.content[0],
            LlmBlock::Reasoning {
                text: "plan".into(),
                meta: Some(
                    json!({ "type": "reasoning", "summary": [{ "text": "plan" }], "id": "rs_1" })
                ),
            }
        );
        assert_eq!(
            result.content[1],
            LlmBlock::Text {
                text: "working".into()
            }
        );
        let usage = result.usage.unwrap();
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.output_tokens, 20);
        assert_eq!(usage.reasoning_tokens, Some(7));
        assert_eq!(usage.cache_read_tokens, Some(40));
        assert_eq!(usage.cache_write_tokens, Some(0));
    }

    #[tokio::test]
    async fn max_output_tokens_shows_up_as_max_tokens_not_as_a_finished_turn() {
        let transport = Arc::new(CannedTransport::sse(vec![vec![
            json!({
                "type": "response.incomplete",
                "response": {
                    "status": "incomplete",
                    "incomplete_details": { "reason": "max_output_tokens" },
                    "output": [{ "type": "message",
                                 "content": [{ "type": "output_text", "text": "half" }] }],
                },
            })
            .to_string(),
            "[DONE]".to_string(),
        ]]));
        let result = openai_client(opts(transport))
            .run(
                params_over("openai:gpt-5", &TOOLS, |_| {}),
                Arc::new(|_| {}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.stop_reason, "max_tokens");
    }

    #[tokio::test]
    async fn a_stream_that_ends_without_response_completed_is_a_transport_fault() {
        let transport = Arc::new(CannedTransport::sse(vec![vec![
            json!({ "type": "response.output_text.delta", "delta": "half a th" }).to_string(),
        ]]));
        let err = openai_client(opts(transport))
            .run(
                params_over("openai:gpt-5", &TOOLS, |_| {}),
                Arc::new(|_| {}),
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "openai: stream ended without response.completed"
        );
        assert!(is_retryable(&err), "a cut stream must be retryable");
    }

    #[tokio::test]
    async fn a_non_2xx_carries_its_status_and_retry_after_into_the_error() {
        let transport = Arc::new(CannedTransport::plain(vec![(
            429,
            vec![("retry-after".to_string(), "3".to_string())],
            "slow down".to_string(),
        )]));
        let err = openai_client(opts(transport))
            .run(
                params_over("openai:gpt-5", &TOOLS, |_| {}),
                Arc::new(|_| {}),
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert_eq!(err.status(), 429);
        assert_eq!(err.retry_after_ms, Some(3000));
        assert!(is_retryable(&err));
    }

    #[tokio::test]
    async fn a_mid_stream_failure_event_is_classified_not_swallowed() {
        let transport = Arc::new(CannedTransport::sse(vec![vec![json!({
            "type": "response.failed",
            "response": { "error": { "code": "rate_limit_exceeded", "message": "slow down" } },
        })
        .to_string()]]));
        let err = openai_client(opts(transport))
            .run(
                params_over("openai:gpt-5", &TOOLS, |_| {}),
                Arc::new(|_| {}),
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert_eq!(err.status(), 429, "a rate-limit code classifies as one");
        assert!(
            err.to_string().starts_with("openai: {"),
            "the whole event is the message"
        );
    }

    #[tokio::test]
    async fn a_missing_key_is_a_401_before_any_request() {
        let transport = Arc::new(CannedTransport::sse(vec![]));
        let err = openai_client(ProviderOpts {
            env: Some(Arc::new(|_| None)),
            transport: Some(transport.clone()),
        })
        .run(
            params_over("openai:gpt-5", &TOOLS, |_| {}),
            Arc::new(|_| {}),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(
            err.status(),
            401,
            "a missing key will still be missing in 15 seconds"
        );
        assert_eq!(
            err.to_string(),
            "openai: OPENAI_API_KEY is not set — put it in ~/.bough/env, then `bough restart`"
        );
        assert!(!is_retryable(&err));
        assert!(
            transport.requests.lock().unwrap().is_empty(),
            "no key, no request"
        );
    }

    #[tokio::test]
    async fn tool_choice_none_rides_as_a_bare_string() {
        let transport = Arc::new(CannedTransport::sse(vec![vec![json!({
            "type": "response.completed",
            "response": { "status": "completed", "output": [] },
        })
        .to_string()]]));
        openai_client(opts(transport.clone()))
            .run(
                params_over("openai:gpt-5", &TOOLS, |p| p.tool_choice_none = true),
                Arc::new(|_| {}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let requests = transport.requests.lock().unwrap();
        let body: Value = serde_json::from_str(requests[0].body.as_ref().unwrap()).unwrap();
        assert_eq!(
            body["tool_choice"],
            json!("none"),
            "a bare string, not an object"
        );
    }
}
