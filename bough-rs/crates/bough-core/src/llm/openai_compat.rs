//! The chat-completions streaming family (port of the OpenRouter half of
//! `src/llm/client.ts`; plan row 2.16 pulled forward for the OpenRouter
//! daily-driver route).
//!
//! Four behaviors here each encode a shipped production bug:
//!
//! - the **repair pass** for orphan `tool_calls` — every assistant call id
//!   MUST be followed by a matching `{role:"tool", tool_call_id}` before the
//!   next non-tool message or the provider 400s the whole request (an
//!   interrupt can leave a call with no result);
//! - the **fragment accumulator by tool-call index** — `arguments` arrive as
//!   string fragments across chunks and concatenate per index;
//! - the **terminal error chunk** — an upstream failure arrives as an `error`
//!   chunk on an otherwise-200 stream; without the check the partial round
//!   passes as success;
//! - the **`[DONE]` truncation guard** — a stream that merely closes without
//!   `[DONE]`/finish_reason was cut mid-response, and returning the partial
//!   round would run half-assembled tool calls.
//!
//! Cloudflare Workers AI is a config of this family: the only thing that
//! differs is that the account id lives in the URL path, which is why the URL
//! is a function of the env resolved per `run()` (plan row 2.16).

use std::sync::Arc;

use serde_json::{json, Map, Value};
use tokio_util::sync::CancellationToken;

use crate::errors::BoughError;
use crate::llm::routing::{
    joined_system, require_key, Env, Provider, ProviderOpts, CLOUDFLARE_ACCOUNT_ENV,
};
use crate::llm::sse::{aborted, fetch_cancellable, http_error, parse_tool_args, SseEvents};
use crate::schema::parts::Usage;
use crate::types::{LlmBlock, LlmClient, LlmContentBlock, LlmMessage, LlmParams, LlmResult, LlmRole, OnText};

/// Flatten our multi-block messages into chat-completions messages, splitting
/// tool_results out into their own `tool` messages.
///
/// The repair pass at the end is not optional. Every assistant `tool_calls`
/// id MUST be followed by a matching `{role:"tool", tool_call_id}` before the
/// next non-tool message, or the provider rejects the whole request with a
/// 400 — including the case where an interrupt left the transcript with a
/// call and no result. A synthesized `(interrupted)` result keeps the request
/// well-formed no matter what history assembly handed us.
pub fn to_openai_messages(system: Option<&str>, messages: &[LlmMessage]) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    if let Some(system) = system.filter(|s| !s.is_empty()) {
        out.push(json!({ "role": "system", "content": system }));
    }
    for m in messages {
        if m.role == LlmRole::Assistant {
            let text: String = m
                .content
                .iter()
                .filter_map(|b| match b {
                    LlmContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect();
            let tool_calls: Vec<Value> = m
                .content
                .iter()
                .filter_map(|b| match b {
                    LlmContentBlock::ToolUse { id, name, input } => {
                        let input = if input.is_null() { json!({}) } else { input.clone() };
                        Some(json!({
                            "id": id,
                            "type": "function",
                            "function": { "name": name, "arguments": input.to_string() },
                        }))
                    }
                    _ => None,
                })
                .collect();
            let mut msg = Map::new();
            msg.insert("role".into(), json!("assistant"));
            msg.insert("content".into(), if text.is_empty() { Value::Null } else { json!(text) });
            if !tool_calls.is_empty() {
                msg.insert("tool_calls".into(), Value::Array(tool_calls));
            }
            out.push(Value::Object(msg));
        } else {
            // A user turn: text/image blocks become one user message; each
            // tool_result becomes its own tool message. With no images the
            // content stays a plain string (wire shape unchanged); with
            // images it becomes the multimodal parts array, which a
            // non-vision model rejects — surfaced as-is.
            let texts: Vec<&str> = m
                .content
                .iter()
                .filter_map(|b| match b {
                    LlmContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect();
            let images: Vec<&LlmContentBlock> = m
                .content
                .iter()
                .filter(|b| matches!(b, LlmContentBlock::Image { .. }))
                .collect();
            if !texts.is_empty() || !images.is_empty() {
                let joined = texts.join("\n");
                let content = if images.is_empty() {
                    json!(joined)
                } else {
                    let mut parts: Vec<Value> = Vec::new();
                    if !joined.is_empty() {
                        parts.push(json!({ "type": "text", "text": joined }));
                    }
                    for b in &images {
                        if let LlmContentBlock::Image { data, media_type, .. } = b {
                            parts.push(json!({
                                "type": "image_url",
                                "image_url": { "url": format!("data:{media_type};base64,{data}") },
                            }));
                        }
                    }
                    Value::Array(parts)
                };
                out.push(json!({ "role": "user", "content": content }));
            }
            for b in &m.content {
                if let LlmContentBlock::ToolResult { tool_use_id, content, .. } = b {
                    out.push(json!({
                        "role": "tool",
                        "tool_call_id": tool_use_id,
                        "content": content,
                    }));
                }
            }
        }
    }
    // The repair pass.
    let mut repaired: Vec<Value> = Vec::new();
    for i in 0..out.len() {
        let msg = out[i].clone();
        let calls: Vec<String> = if msg["role"] == "assistant" {
            msg["tool_calls"]
                .as_array()
                .map(|calls| {
                    calls
                        .iter()
                        .filter_map(|c| c["id"].as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        repaired.push(msg);
        if calls.is_empty() {
            continue;
        }
        let mut provided = std::collections::HashSet::new();
        for t in out.iter().skip(i + 1) {
            if t["role"] != "tool" {
                break;
            }
            if let Some(id) = t["tool_call_id"].as_str() {
                provided.insert(id.to_string());
            }
        }
        for id in calls {
            if !provided.contains(&id) {
                repaired.push(json!({
                    "role": "tool",
                    "tool_call_id": id,
                    "content": "(interrupted)",
                }));
            }
        }
    }
    repaired
}

/// One streamed tool call, accumulated by `index` across chunks: `id` and
/// `name` land once, `arguments` fragments concatenate.
#[derive(Default, Clone)]
struct ToolCallAcc {
    id: Option<String>,
    name: Option<String>,
    arguments: Option<String>,
}

/// The URL is a function of the env resolved per `run()` (Cloudflare's
/// account id is part of the path) — a value set through the running server
/// must apply without a restart.
type UrlFn = Arc<dyn Fn(&crate::llm::routing::Env) -> Result<String, BoughError> + Send + Sync>;

struct OpenAICompatClient {
    opts: ProviderOpts,
    provider: Provider,
    url: UrlFn,
    extra_headers: Vec<(String, String)>,
    key_alternatives: Vec<&'static str>,
    stall_ms: u64,
}

#[async_trait::async_trait]
impl LlmClient for OpenAICompatClient {
    async fn run(
        &self,
        params: LlmParams,
        on_text: OnText,
        cancel: CancellationToken,
    ) -> Result<LlmResult, BoughError> {
        let provider = self.provider.as_str();
        let env = self.opts.env_or_default();
        let transport = self.opts.transport_or_default();
        let api_key = require_key(&env, self.provider, &self.key_alternatives)?;
        let url = (self.url)(&env)?;

        let mut body = Map::new();
        body.insert("model".into(), json!(params.model));
        body.insert("max_tokens".into(), json!(params.max_tokens));
        body.insert("stream".into(), json!(true));
        body.insert("stream_options".into(), json!({ "include_usage": true }));
        body.insert(
            "messages".into(),
            Value::Array(to_openai_messages(joined_system(&params).as_deref(), &params.messages)),
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
                            "function": {
                                "name": t.name,
                                "description": t.description,
                                "parameters": t.input_schema,
                            },
                        })
                    })
                    .collect(),
            ),
        );
        if params.tool_choice_none {
            body.insert("tool_choice".into(), json!("none"));
        }

        let mut headers = vec![
            ("authorization".to_string(), format!("Bearer {api_key}")),
            ("content-type".to_string(), "application/json".to_string()),
        ];
        headers.extend(self.extra_headers.iter().cloned());
        let req = crate::llm::sse::HttpRequest {
            url,
            headers,
            body: Some(Value::Object(body).to_string()),
        };
        let res = fetch_cancellable(transport.as_ref(), req, &cancel, provider).await?;
        if !res.ok() {
            return Err(http_error(provider, res).await);
        }

        let mut events = SseEvents::with_stall(res.body, provider, self.stall_ms);
        let mut text = String::new();
        let mut tool_calls: std::collections::BTreeMap<i64, ToolCallAcc> =
            std::collections::BTreeMap::new();
        let mut finish_reason = "stop".to_string();
        let mut usage: Option<Usage> = None;
        // Whether the stream reached a proper end ([DONE] or a finish_reason).
        // A stream that merely closes was cut mid-response, and returning the
        // partial round as success would run half-assembled tool calls.
        let mut ended = false;

        loop {
            let data = tokio::select! {
                _ = cancel.cancelled() => return Err(aborted(provider)),
                next = events.next() => next?,
            };
            let Some(data) = data else { break };
            if data == "[DONE]" {
                ended = true;
                continue;
            }
            let Ok(chunk) = serde_json::from_str::<Value>(&data) else { continue };
            // An upstream provider failure arrives as a terminal `error` chunk
            // on an otherwise-200 stream; without this the partial round
            // passes as success.
            if let Some(error) = chunk.get("error").filter(|e| !e.is_null()) {
                let message = error["message"]
                    .as_str()
                    .map(String::from)
                    .unwrap_or_else(|| error.to_string());
                let status =
                    error["code"].as_i64().and_then(|c| u16::try_from(c).ok()).unwrap_or(502);
                return Err(BoughError::llm_with(format!("{provider}: {message}"), status, None));
            }
            if let Some(u) = chunk.get("usage").filter(|u| u.is_object()) {
                usage = Some(Usage {
                    input_tokens: u["prompt_tokens"].as_i64().unwrap_or(0),
                    output_tokens: u["completion_tokens"].as_i64().unwrap_or(0),
                    reasoning_tokens: Some(
                        u["completion_tokens_details"]["reasoning_tokens"].as_i64().unwrap_or(0),
                    ),
                    // The upstream provider's cache hits, relayed in the
                    // OpenAI shape.
                    cache_read_tokens: Some(
                        u["prompt_tokens_details"]["cached_tokens"].as_i64().unwrap_or(0),
                    ),
                    cache_write_tokens: Some(0),
                    cost_usd: None,
                });
            }
            let Some(choice) = chunk["choices"].get(0) else { continue };
            if let Some(reason) = choice["finish_reason"].as_str() {
                finish_reason = reason.to_string();
                ended = true;
            }
            let delta = &choice["delta"];
            if let Some(content) = delta["content"].as_str().filter(|c| !c.is_empty()) {
                text.push_str(content);
                on_text(content);
            }
            if let Some(calls) = delta["tool_calls"].as_array() {
                for tc in calls {
                    let Some(index) = tc["index"].as_i64() else { continue };
                    let cur = tool_calls.entry(index).or_default();
                    if let Some(id) = tc["id"].as_str() {
                        cur.id = Some(id.to_string());
                    }
                    if let Some(name) = tc["function"]["name"].as_str() {
                        cur.name = Some(name.to_string());
                    }
                    if let Some(fragment) = tc["function"]["arguments"].as_str() {
                        if !fragment.is_empty() {
                            let args = cur.arguments.get_or_insert_with(String::new);
                            args.push_str(fragment);
                        }
                    }
                }
            }
        }
        if !ended {
            return Err(BoughError::llm(format!("{provider}: stream truncated before completion")));
        }

        let mut content: Vec<LlmBlock> = Vec::new();
        if !text.is_empty() {
            content.push(LlmBlock::Text { text });
        }
        // BTreeMap iterates in index order — the TS sort by index.
        for tc in tool_calls.into_values() {
            let name = tc.name.unwrap_or_default();
            let input = parse_tool_args(
                provider,
                tc.arguments.as_deref(),
                params.tools.iter().find(|t| t.name == name),
                &name,
            )?;
            content.push(LlmBlock::ToolUse {
                id: tc.id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
                name,
                input,
            });
        }
        // Normalize the finish_reason vocabulary to ours.
        let stop_reason = match finish_reason.as_str() {
            "tool_calls" => "tool_use".to_string(),
            "length" => "max_tokens".to_string(),
            other => other.to_string(),
        };
        Ok(LlmResult { content, stop_reason, usage })
    }
}

/// The OpenRouter route: the chat-completions family at its public endpoint.
pub fn openrouter_client(opts: ProviderOpts) -> Arc<dyn LlmClient> {
    openrouter_client_with_stall(opts, crate::llm::sse::STALL_TIMEOUT_MS)
}

pub(crate) fn openrouter_client_with_stall(opts: ProviderOpts, stall_ms: u64) -> Arc<dyn LlmClient> {
    Arc::new(OpenAICompatClient {
        opts,
        provider: Provider::Openrouter,
        url: Arc::new(|_| Ok("https://openrouter.ai/api/v1/chat/completions".to_string())),
        extra_headers: vec![("x-title".to_string(), "bough".to_string())],
        key_alternatives: vec![],
        stall_ms,
    })
}

/// The account-scoped Workers AI base, overridable for a gateway or a test
/// server. 401 for the same reason a missing key is: a missing account id
/// will still be missing in 15 seconds, so six backed-off attempts would only
/// delay the message that fixes it.
fn cloudflare_base(env: &Env) -> Result<String, BoughError> {
    let account = env(CLOUDFLARE_ACCOUNT_ENV)
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());
    let Some(account) = account else {
        return Err(BoughError::llm_with(
            format!("cloudflare: {CLOUDFLARE_ACCOUNT_ENV} is not set"),
            401,
            None,
        ));
    };
    let base = env("CLOUDFLARE_API_BASE")
        .unwrap_or_else(|| "https://api.cloudflare.com/client/v4".to_string());
    Ok(format!("{base}/accounts/{account}/ai"))
}

/// Workers AI over its OpenAI-compatible endpoint.
///
/// Cloudflare serves `/ai/v1/chat/completions` in the chat-completions shape,
/// so it reuses the OpenRouter family wholesale; the only thing that differs
/// is that the account id lives in the path, which is why the URL is a
/// function of the env — a value set through the running server applies
/// without a restart.
pub fn cloudflare_client(opts: ProviderOpts) -> Arc<dyn LlmClient> {
    Arc::new(OpenAICompatClient {
        opts,
        provider: Provider::Cloudflare,
        url: Arc::new(|env| cloudflare_base(env).map(|base| format!("{base}/v1/chat/completions"))),
        extra_headers: vec![],
        // Cloudflare's own docs and dashboard call it a token, so accept that
        // spelling.
        key_alternatives: vec!["CLOUDFLARE_API_TOKEN"],
        stall_ms: crate::llm::sse::STALL_TIMEOUT_MS,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::test_support::{keyed_env, params_over, CannedTransport, TOOLS};
    use std::sync::Mutex;

    fn msg(role: LlmRole, content: Vec<LlmContentBlock>) -> LlmMessage {
        LlmMessage { role, content }
    }

    #[test]
    fn to_openai_messages_an_orphaned_tool_call_is_repaired_not_left_to_400() {
        let msgs = to_openai_messages(
            None,
            &[
                msg(
                    LlmRole::Assistant,
                    vec![
                        LlmContentBlock::Text { text: "running".into() },
                        LlmContentBlock::ToolUse {
                            id: "c1".into(),
                            name: "run_steps".into(),
                            input: json!({ "code": "1" }),
                        },
                    ],
                ),
                msg(LlmRole::User, vec![LlmContentBlock::Text { text: "actually, stop".into() }]),
            ],
        );
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[0]["role"], "assistant");
        assert_eq!(
            msgs[1],
            json!({ "role": "tool", "tool_call_id": "c1", "content": "(interrupted)" })
        );
        assert_eq!(msgs[2]["role"], "user");
    }

    #[test]
    fn to_openai_messages_a_satisfied_tool_call_is_left_exactly_as_it_was() {
        let msgs = to_openai_messages(
            Some("SYS"),
            &[
                msg(
                    LlmRole::Assistant,
                    vec![LlmContentBlock::ToolUse {
                        id: "c1".into(),
                        name: "stop".into(),
                        input: json!({}),
                    }],
                ),
                msg(
                    LlmRole::User,
                    vec![LlmContentBlock::ToolResult {
                        tool_use_id: "c1".into(),
                        content: "done".into(),
                        is_error: false,
                    }],
                ),
            ],
        );
        assert_eq!(msgs[0], json!({ "role": "system", "content": "SYS" }));
        assert_eq!(msgs.len(), 3, "no synthesized result should have been added");
        assert_eq!(msgs[2], json!({ "role": "tool", "tool_call_id": "c1", "content": "done" }));
    }

    #[test]
    fn to_openai_messages_images_become_multimodal_parts_text_alone_stays_a_string() {
        let plain = to_openai_messages(
            None,
            &[msg(LlmRole::User, vec![LlmContentBlock::Text { text: "hi".into() }])],
        );
        assert_eq!(plain[0]["content"], json!("hi"));

        let with_image = to_openai_messages(
            None,
            &[msg(
                LlmRole::User,
                vec![
                    LlmContentBlock::Text { text: "look".into() },
                    LlmContentBlock::Image {
                        data: "AAAA".into(),
                        media_type: "image/png".into(),
                        name: "s.png".into(),
                    },
                ],
            )],
        );
        assert_eq!(
            with_image[0]["content"],
            json!([
                { "type": "text", "text": "look" },
                { "type": "image_url", "image_url": { "url": "data:image/png;base64,AAAA" } },
            ])
        );
    }

    fn chunk(delta: Value, finish: Option<&str>) -> String {
        let mut choice = Map::new();
        choice.insert("delta".into(), delta);
        if let Some(f) = finish {
            choice.insert("finish_reason".into(), json!(f));
        }
        json!({ "choices": [choice] }).to_string()
    }

    #[tokio::test]
    async fn a_full_round_assembles_streamed_tool_call_fragments_in_order() {
        let transport = Arc::new(CannedTransport::sse(vec![vec![
            chunk(json!({ "content": "one moment" }), None),
            chunk(
                json!({ "tool_calls": [{ "index": 0, "id": "c1", "function": { "name": "run_steps" } }] }),
                None,
            ),
            chunk(json!({ "tool_calls": [{ "index": 0, "function": { "arguments": "{\"co" } }] }), None),
            chunk(
                json!({ "tool_calls": [{ "index": 0, "function": { "arguments": "de\":\"1\"}" } }] }),
                None,
            ),
            chunk(json!({}), Some("tool_calls")),
            json!({
                "choices": [],
                "usage": {
                    "prompt_tokens": 200,
                    "completion_tokens": 30,
                    "prompt_tokens_details": { "cached_tokens": 50 },
                    "completion_tokens_details": { "reasoning_tokens": 3 },
                },
            })
            .to_string(),
            "[DONE]".to_string(),
        ]]));
        let client = openrouter_client(ProviderOpts {
            env: Some(keyed_env()),
            transport: Some(transport.clone()),
        });
        let deltas: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let deltas2 = deltas.clone();
        let result = client
            .run(
                params_over("google/gemini-2.5-pro", &TOOLS, |_| {}),
                Arc::new(move |d| deltas2.lock().unwrap().push(d.to_string())),
                CancellationToken::new(),
            )
            .await
            .unwrap();

        let requests = transport.requests.lock().unwrap();
        assert_eq!(requests[0].url, "https://openrouter.ai/api/v1/chat/completions");
        let body: Value = serde_json::from_str(requests[0].body.as_ref().unwrap()).unwrap();
        assert_eq!(body["model"], "google/gemini-2.5-pro");
        assert_eq!(body["stream_options"], json!({ "include_usage": true }));
        assert!(requests[0].headers.iter().any(|(k, v)| k == "x-title" && v == "bough"));
        assert!(requests[0]
            .headers
            .iter()
            .any(|(k, v)| k == "authorization" && v == "Bearer test-key"));

        assert_eq!(*deltas.lock().unwrap(), vec!["one moment"]);
        assert_eq!(result.stop_reason, "tool_use");
        assert_eq!(
            result.content,
            vec![
                LlmBlock::Text { text: "one moment".into() },
                LlmBlock::ToolUse {
                    id: "c1".into(),
                    name: "run_steps".into(),
                    input: json!({ "code": "1" }),
                },
            ]
        );
        assert_eq!(
            result.usage,
            Some(Usage {
                input_tokens: 200,
                output_tokens: 30,
                reasoning_tokens: Some(3),
                cache_read_tokens: Some(50),
                cache_write_tokens: Some(0),
                cost_usd: None,
            })
        );
    }

    #[tokio::test]
    async fn a_stream_that_closes_without_a_finish_reason_is_a_transport_fault() {
        let transport = Arc::new(CannedTransport::sse(vec![vec![chunk(
            json!({ "content": "partial" }),
            None,
        )]]));
        let client =
            openrouter_client(ProviderOpts { env: Some(keyed_env()), transport: Some(transport) });
        let err = client
            .run(
                params_over("z-ai/glm-5.2", &TOOLS, |_| {}),
                Arc::new(|_| {}),
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("truncated before completion"), "{err}");
        assert!(crate::llm::retry::is_retryable(&err));
    }

    #[tokio::test]
    async fn a_terminal_error_chunk_on_a_200_stream_is_not_passed_off_as_success() {
        let transport = Arc::new(CannedTransport::sse(vec![vec![
            chunk(json!({ "content": "start" }), None),
            json!({ "error": { "message": "upstream is down", "code": 502 } }).to_string(),
        ]]));
        let client =
            openrouter_client(ProviderOpts { env: Some(keyed_env()), transport: Some(transport) });
        let err = client
            .run(
                params_over("z-ai/glm-5.2", &TOOLS, |_| {}),
                Arc::new(|_| {}),
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert_eq!(err.status(), 502);
        assert!(err.to_string().contains("upstream is down"), "{err}");
    }

    #[tokio::test]
    async fn a_truncated_tool_call_is_retried_rather_than_run_with_empty_args() {
        let transport = Arc::new(CannedTransport::sse(vec![vec![
            chunk(
                json!({ "tool_calls": [{ "index": 0, "id": "c1", "function": { "name": "run_steps" } }] }),
                Some("tool_calls"),
            ),
            "[DONE]".to_string(),
        ]]));
        let client =
            openrouter_client(ProviderOpts { env: Some(keyed_env()), transport: Some(transport) });
        let err = client
            .run(
                params_over("z-ai/glm-5.2", &TOOLS, |_| {}),
                Arc::new(|_| {}),
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("no arguments (truncated mid-call)"), "{err}");
        assert!(crate::llm::retry::is_retryable(&err));
    }

    #[tokio::test]
    async fn finish_reason_length_normalizes_to_max_tokens() {
        let transport = Arc::new(CannedTransport::sse(vec![vec![
            chunk(json!({ "content": "cut" }), Some("length")),
            "[DONE]".to_string(),
        ]]));
        let client =
            openrouter_client(ProviderOpts { env: Some(keyed_env()), transport: Some(transport) });
        let result = client
            .run(
                params_over("z-ai/glm-5.2", &TOOLS, |_| {}),
                Arc::new(|_| {}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.stop_reason, "max_tokens");
    }

    #[tokio::test]
    async fn an_id_less_streamed_tool_call_gets_a_generated_id() {
        let transport = Arc::new(CannedTransport::sse(vec![vec![
            chunk(
                json!({ "tool_calls": [{ "index": 0, "function": { "name": "stop", "arguments": "{}" } }] }),
                Some("tool_calls"),
            ),
            "[DONE]".to_string(),
        ]]));
        let client =
            openrouter_client(ProviderOpts { env: Some(keyed_env()), transport: Some(transport) });
        let result = client
            .run(
                params_over("z-ai/glm-5.2", &TOOLS, |_| {}),
                Arc::new(|_| {}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let LlmBlock::ToolUse { id, name, input } = &result.content[0] else { panic!() };
        assert!(!id.is_empty(), "the stream never sent an id — one is generated");
        assert_eq!(name, "stop");
        assert_eq!(*input, json!({}));
    }

    /// Key and account id, the pair Cloudflare needs; no `_API_BASE` so the
    /// URL is real.
    fn cf_env() -> crate::llm::routing::Env {
        Arc::new(|k| match k {
            "CLOUDFLARE_API_KEY" => Some("cf-key".to_string()),
            "CLOUDFLARE_ACCOUNT_ID" => Some("acct-1".to_string()),
            _ => None,
        })
    }

    #[tokio::test]
    async fn cloudflare_the_account_id_lands_in_the_url_and_the_round_decodes() {
        let transport = Arc::new(CannedTransport::sse(vec![vec![
            chunk(json!({ "content": "hi" }), None),
            chunk(json!({}), Some("stop")),
            "[DONE]".to_string(),
        ]]));
        let client = cloudflare_client(ProviderOpts {
            env: Some(cf_env()),
            transport: Some(transport.clone()),
        });
        let result = client
            .run(
                params_over("@cf/zai-org/glm-5.2", &TOOLS, |_| {}),
                Arc::new(|_| {}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let requests = transport.requests.lock().unwrap();
        assert_eq!(
            requests[0].url,
            "https://api.cloudflare.com/client/v4/accounts/acct-1/ai/v1/chat/completions"
        );
        assert!(requests[0]
            .headers
            .iter()
            .any(|(k, v)| k == "authorization" && v == "Bearer cf-key"));
        let body: Value = serde_json::from_str(requests[0].body.as_ref().unwrap()).unwrap();
        assert_eq!(body["model"], "@cf/zai-org/glm-5.2", "full id incl. @cf/");
        assert_eq!(result.content, vec![LlmBlock::Text { text: "hi".into() }]);
    }

    #[tokio::test]
    async fn cloudflare_the_endpoint_comes_from_the_env_read_per_run() {
        // A key or a base set through the running server must apply without a
        // restart, so both are read at run() time — not when the client was
        // constructed.
        let account = Arc::new(Mutex::new("first".to_string()));
        let account2 = account.clone();
        let env: crate::llm::routing::Env = Arc::new(move |k| match k {
            "CLOUDFLARE_API_TOKEN" => Some("tok".to_string()),
            "CLOUDFLARE_ACCOUNT_ID" => Some(account2.lock().unwrap().clone()),
            "CLOUDFLARE_API_BASE" => Some("http://127.0.0.1:9/v4".to_string()),
            _ => None,
        });
        let done = || vec![chunk(json!({}), Some("stop")), "[DONE]".to_string()];
        let transport = Arc::new(CannedTransport::sse(vec![done(), done()]));
        let client = cloudflare_client(ProviderOpts {
            env: Some(env),
            transport: Some(transport.clone()),
        });
        let p = || params_over("@cf/openai/gpt-oss-120b", &TOOLS, |_| {});
        client.run(p(), Arc::new(|_| {}), CancellationToken::new()).await.unwrap();
        *account.lock().unwrap() = "second".to_string();
        client.run(p(), Arc::new(|_| {}), CancellationToken::new()).await.unwrap();
        let requests = transport.requests.lock().unwrap();
        assert_eq!(requests[0].url, "http://127.0.0.1:9/v4/accounts/first/ai/v1/chat/completions");
        assert_eq!(requests[1].url, "http://127.0.0.1:9/v4/accounts/second/ai/v1/chat/completions");
        // CLOUDFLARE_API_TOKEN is accepted as the key — it is Cloudflare's
        // own spelling.
        assert!(requests[0].headers.iter().any(|(k, v)| k == "authorization" && v == "Bearer tok"));
    }

    #[tokio::test]
    async fn cloudflare_a_key_with_no_account_id_fails_fast_naming_the_missing_var() {
        // The transport has NOTHING queued — a fetch would fail loudly, so a
        // passing test proves the endpoint was never formed.
        let transport = Arc::new(CannedTransport::sse(vec![]));
        let env: crate::llm::routing::Env =
            Arc::new(|k| (k == "CLOUDFLARE_API_KEY").then(|| "cf-key".to_string()));
        let client = cloudflare_client(ProviderOpts { env: Some(env), transport: Some(transport.clone()) });
        let err = client
            .run(
                params_over("@cf/zai-org/glm-5.2", &TOOLS, |_| {}),
                Arc::new(|_| {}),
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert_eq!(err.status(), 401, "a missing account id must not be retried");
        assert!(err.to_string().contains("CLOUDFLARE_ACCOUNT_ID"), "{err}");
        assert!(!crate::llm::retry::is_retryable(&err));
        assert!(transport.requests.lock().unwrap().is_empty(), "must not be called");
    }
}
