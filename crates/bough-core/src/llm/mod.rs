//! The provider layer (port of `src/llm/`). No provider name appears outside
//! this module. The Anthropic client is hand-rolled reqwest + SSE (no SDK);
//! the SSE parser stays hand-rolled — the `[DONE]`/stall/trailing-fragment
//! semantics are custom and test-pinned. Provider-specific handling must not
//! leak past `types::LlmClient`.
//!
//! The invariant: **the turn runner must not know which provider it is
//! talking to.** Three wire protocols, three message encodings, three usage
//! shapes, three ways of admitting that a stream died — all of it collapses
//! to one `run()`.

pub mod anthropic;
pub mod discovery;
pub mod openai;
pub mod openai_compat;
pub mod pricing;
pub mod retry;
pub mod routing;
pub mod sse;
pub mod trace;

use std::sync::Arc;

use crate::errors::BoughError;
use crate::types::{LlmClient, LlmContentBlock, LlmMessage, LlmParams, LlmRole};

pub use retry::RetryOpts;
pub use routing::{provider_for, Provider, ProviderOpts};
pub use trace::TraceLabel;

/// What `client_for` composes with.
#[derive(Clone, Default)]
pub struct ClientOpts {
    pub provider: ProviderOpts,
    pub retry: RetryOpts,
    /// Record raw provider I/O for this turn. `None` = no tracing and no
    /// wrapper.
    pub trace: Option<TraceLabel>,
}

/// The bare provider client for a model id, without retries or pricing.
pub fn provider_client(model: &str, opts: ProviderOpts) -> Arc<dyn LlmClient> {
    match provider_for(model) {
        Provider::Openai => openai::openai_client(opts),
        Provider::Openrouter => openai_compat::openrouter_client(opts),
        Provider::Cloudflare => openai_compat::cloudflare_client(opts),
        Provider::Anthropic => anthropic::anthropic_client(opts),
    }
}

/// **The only entry point the rest of the tree uses.** Routes a model id to
/// its provider, prices the round from the vendored catalog, and wraps the
/// whole thing in transient-failure retries. `retry.on_retry` observes
/// re-attempts — the turn runner uses it to reset the streaming buffer and
/// emit `message.retry`.
///
/// Composition order is load-bearing: tracing sits INSIDE the retries so a
/// recorded trace shows each attempt, and outside pricing so a recorded
/// round already carries its cost.
pub fn client_for(model: &str, opts: ClientOpts) -> Arc<dyn LlmClient> {
    retry::with_retries(
        trace::with_trace(
            pricing::with_pricing(provider_client(model, opts.provider)),
            opts.trace,
        ),
        opts.retry,
    )
}

/// Options for [`complete_text`].
pub struct CompleteTextOpts {
    pub model: String,
    pub system: String,
    pub max_tokens: i64,
    pub prompt: String,
}

/// One-shot text completion: no tools, no event consumer. Used by the cheap
/// tier and by the history operations that need a summary. Returns the
/// concatenated text blocks untrimmed; callers trim if they care.
pub async fn complete_text(
    llm: &Arc<dyn LlmClient>,
    opts: CompleteTextOpts,
) -> Result<String, BoughError> {
    let params = LlmParams {
        model: opts.model,
        system: Some(opts.system),
        system_volatile: None,
        max_tokens: opts.max_tokens,
        messages: vec![LlmMessage {
            role: LlmRole::User,
            content: vec![LlmContentBlock::Text { text: opts.prompt }],
        }],
        tools: vec![],
        tool_choice_none: false,
        effort: None,
    };
    let result = llm
        .run(
            params,
            Arc::new(|_| {}),
            tokio_util::sync::CancellationToken::new(),
        )
        .await?;
    Ok(result
        .content
        .iter()
        .filter_map(|b| match b {
            crate::types::LlmBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect())
}

// ---- shared test plumbing ---------------------------------------------------

/// A scripted `LlmClient` and canned transports: the shape every upstream
/// test uses — the turn runner, the subagent launcher, the history
/// operations — so if this fake is sufficient, provider knowledge really has
/// stayed inside `llm/`.
#[cfg(test)]
pub(crate) mod test_support {
    use std::collections::VecDeque;
    use std::sync::{Arc, LazyLock, Mutex};

    use serde_json::json;
    use tokio_util::sync::CancellationToken;

    use crate::errors::BoughError;
    use crate::llm::sse::{body_of, HttpRequest, HttpResponse, Transport};
    use crate::types::{
        LlmBlock, LlmClient, LlmContentBlock, LlmMessage, LlmParams, LlmResult, LlmRole,
        LlmToolDef, OnText,
    };

    pub static TOOLS: LazyLock<Vec<LlmToolDef>> = LazyLock::new(|| {
        vec![
            LlmToolDef {
                name: "run_steps".into(),
                description: "Run one JavaScript program in the workspace.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": { "code": { "type": "string" }, "done": { "type": "boolean" } },
                    "required": ["code"],
                    "additionalProperties": false,
                }),
            },
            LlmToolDef {
                name: "stop".into(),
                description: "End the turn.".into(),
                input_schema: json!({
                    "type": "object", "properties": {}, "additionalProperties": false }),
            },
        ]
    });

    pub fn params_for_model(model: &str, tools: &[LlmToolDef]) -> LlmParams {
        LlmParams {
            model: model.to_string(),
            system: None,
            system_volatile: None,
            max_tokens: 1024,
            messages: vec![LlmMessage {
                role: LlmRole::User,
                content: vec![LlmContentBlock::Text {
                    text: "hello".into(),
                }],
            }],
            tools: tools.to_vec(),
            tool_choice_none: false,
            effort: None,
        }
    }

    pub fn params(tools: &[LlmToolDef]) -> LlmParams {
        params_for_model("claude-opus-5", tools)
    }

    pub fn params_over(
        model: &str,
        tools: &[LlmToolDef],
        f: impl FnOnce(&mut LlmParams),
    ) -> LlmParams {
        let mut p = params_for_model(model, tools);
        f(&mut p);
        p
    }

    /// Answers every `*_API_KEY` with "test-key" — and nothing else, so
    /// `*_API_BASE` stays at its real default.
    pub fn keyed_env() -> crate::llm::routing::Env {
        Arc::new(|k| k.ends_with("_API_KEY").then(|| "test-key".to_string()))
    }

    /// Hand it the rounds it should return, in order; it records what it was
    /// asked.
    pub struct FakeClient {
        script: Mutex<VecDeque<Result<LlmResult, BoughError>>>,
        calls: Arc<Mutex<Vec<LlmParams>>>,
    }

    #[async_trait::async_trait]
    impl LlmClient for FakeClient {
        async fn run(
            &self,
            params: LlmParams,
            on_text: OnText,
            cancel: CancellationToken,
        ) -> Result<LlmResult, BoughError> {
            self.calls.lock().unwrap().push(params);
            if cancel.is_cancelled() {
                return Err(crate::llm::sse::aborted("fake"));
            }
            let next = self.script.lock().unwrap().pop_front();
            match next {
                None => Err(BoughError::llm("fake: script exhausted")),
                Some(Err(err)) => Err(err),
                Some(Ok(result)) => {
                    for b in &result.content {
                        if let LlmBlock::Text { text } = b {
                            on_text(text);
                        }
                    }
                    Ok(result)
                }
            }
        }
    }

    #[allow(clippy::type_complexity)]
    pub fn fake_client(
        script: Vec<Result<LlmResult, BoughError>>,
    ) -> (Arc<dyn LlmClient>, Arc<Mutex<Vec<LlmParams>>>) {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let client = Arc::new(FakeClient {
            script: Mutex::new(script.into()),
            calls: calls.clone(),
        });
        (client, calls)
    }

    pub struct RecordedRequest {
        pub url: String,
        pub headers: Vec<(String, String)>,
        pub body: Option<String>,
    }

    struct CannedResponse {
        status: u16,
        headers: Vec<(String, String)>,
        chunks: Vec<String>,
    }

    /// A `Transport` that answers from a queue and records every request.
    pub struct CannedTransport {
        responses: Mutex<VecDeque<CannedResponse>>,
        pub requests: Mutex<Vec<RecordedRequest>>,
    }

    impl CannedTransport {
        /// Each inner vec of `data:` payloads becomes one 200 SSE response;
        /// each framed line is delivered as its own chunk.
        pub fn sse(payload_lists: Vec<Vec<String>>) -> Self {
            let responses = payload_lists
                .into_iter()
                .map(|payloads| CannedResponse {
                    status: 200,
                    headers: vec![("content-type".into(), "text/event-stream".into())],
                    chunks: payloads
                        .into_iter()
                        .map(|p| format!("data: {p}\n"))
                        .collect(),
                })
                .collect();
            CannedTransport {
                responses: Mutex::new(responses),
                requests: Mutex::new(Vec::new()),
            }
        }

        /// Plain (non-SSE) responses: `(status, headers, body)`.
        pub fn plain(list: Vec<(u16, Vec<(String, String)>, String)>) -> Self {
            let responses = list
                .into_iter()
                .map(|(status, headers, body)| CannedResponse {
                    status,
                    headers,
                    chunks: vec![body],
                })
                .collect();
            CannedTransport {
                responses: Mutex::new(responses),
                requests: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait::async_trait]
    impl Transport for CannedTransport {
        async fn fetch(&self, req: HttpRequest) -> Result<HttpResponse, BoughError> {
            self.requests.lock().unwrap().push(RecordedRequest {
                url: req.url,
                headers: req.headers,
                body: req.body,
            });
            let next = self.responses.lock().unwrap().pop_front();
            let Some(next) = next else {
                return Err(BoughError::llm("CannedTransport: no response queued"));
            };
            let chunks: Vec<&str> = next.chunks.iter().map(String::as_str).collect();
            Ok(HttpResponse {
                status: next.status,
                headers: next.headers,
                body: body_of(chunks),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{fake_client, params, params_for_model, TOOLS};
    use super::*;
    use crate::types::{LlmBlock, LlmResult};
    use serde_json::json;
    use std::sync::Mutex;
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn a_fake_satisfies_llm_client_streamed_deltas_blocks_and_stop_reason() {
        let (client, calls) = fake_client(vec![Ok(LlmResult {
            content: vec![
                LlmBlock::Reasoning {
                    text: "thinking about it".into(),
                    meta: None,
                },
                LlmBlock::Text {
                    text: "on it".into(),
                },
                LlmBlock::ToolUse {
                    id: "t1".into(),
                    name: "run_steps".into(),
                    input: json!({ "code": "1" }),
                },
            ],
            stop_reason: "tool_use".into(),
            usage: Some(crate::schema::parts::Usage {
                input_tokens: 10,
                output_tokens: 5,
                reasoning_tokens: None,
                cache_read_tokens: None,
                cache_write_tokens: None,
                cost_usd: None,
            }),
        })]);
        let deltas: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let deltas2 = deltas.clone();
        let result = client
            .run(
                params(&TOOLS),
                Arc::new(move |d| deltas2.lock().unwrap().push(d.to_string())),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(*deltas.lock().unwrap(), vec!["on it"]);
        assert_eq!(result.stop_reason, "tool_use");
        assert_eq!(result.content.len(), 3);
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].model, "claude-opus-5");
    }

    #[tokio::test]
    async fn complete_text_drives_the_interface_with_no_tools_and_no_consumer() {
        let (client, calls) = fake_client(vec![Ok(LlmResult {
            content: vec![
                LlmBlock::Text { text: "a ".into() },
                LlmBlock::Reasoning {
                    text: "hm".into(),
                    meta: None,
                },
                LlmBlock::Text {
                    text: "title".into(),
                },
            ],
            stop_reason: "end_turn".into(),
            usage: None,
        })]);
        let text = complete_text(
            &client,
            CompleteTextOpts {
                model: "claude-haiku-4-5".into(),
                system: "name it".into(),
                max_tokens: 32,
                prompt: "the transcript".into(),
            },
        )
        .await
        .unwrap();
        assert_eq!(text, "a title");
        let calls = calls.lock().unwrap();
        assert!(calls[0].tools.is_empty());
        assert_eq!(calls[0].messages.len(), 1);
    }

    #[tokio::test]
    async fn client_for_routes_without_a_key_and_only_fails_when_asked_to_run() {
        // Construction must not read a key or touch the network — the server
        // builds a client per model id long before anyone runs a round. The
        // ported routes fail 401 naming the env var(s); the stubbed OpenAI
        // route fails 401 "provider not configured". Neither is retried.
        let cases: &[(&str, &str)] = &[
            ("claude-opus-5", "ANTHROPIC_API_KEY"),
            ("openai/gpt-5", "OPENROUTER_API_KEY"),
            ("openai:gpt-5", "OPENAI_API_KEY"),
            (
                "@cf/zai-org/glm-5.2",
                "CLOUDFLARE_API_KEY / CLOUDFLARE_API_TOKEN",
            ),
        ];
        for (model, needle) in cases {
            let client = client_for(
                model,
                ClientOpts {
                    provider: ProviderOpts {
                        env: Some(Arc::new(|_| None)),
                        transport: None,
                    },
                    retry: RetryOpts {
                        max_attempts: Some(2),
                        base_delay_ms: Some(0),
                        on_retry: None,
                    },
                    trace: None,
                },
            );
            let err = client
                .run(
                    params_for_model(model, &TOOLS),
                    Arc::new(|_| {}),
                    CancellationToken::new(),
                )
                .await
                .unwrap_err();
            assert_eq!(
                err.status(),
                401,
                "{model}: a missing key must not be retried"
            );
            assert!(err.to_string().contains(needle), "{model}: got {err}");
        }
    }

    #[tokio::test]
    async fn client_for_composes_pricing_under_the_retries() {
        // A transient failure then a success: the surviving round carries a
        // catalog cost, proving with_pricing sits inside with_retries.
        let (inner, _calls) = fake_client(vec![
            Err(crate::errors::BoughError::llm_with(
                "openrouter: 500 upstream",
                500,
                None,
            )),
            Ok(LlmResult {
                content: vec![LlmBlock::Text { text: "ok".into() }],
                stop_reason: "end_turn".into(),
                usage: Some(crate::schema::parts::Usage {
                    input_tokens: 1_000_000,
                    output_tokens: 0,
                    reasoning_tokens: None,
                    cache_read_tokens: Some(0),
                    cache_write_tokens: Some(0),
                    cost_usd: None,
                }),
            }),
        ]);
        let composed = retry::with_retries(
            trace::with_trace(pricing::with_pricing(inner), None),
            RetryOpts {
                max_attempts: Some(3),
                base_delay_ms: Some(0),
                on_retry: None,
            },
        );
        let result = composed
            .run(params(&TOOLS), Arc::new(|_| {}), CancellationToken::new())
            .await
            .unwrap();
        let cost = result.usage.unwrap().cost_usd;
        assert!(matches!(cost, Some(c) if c > 0.0), "{cost:?}");
    }
}
