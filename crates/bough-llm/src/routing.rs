//! Model-string → provider routing (port of `src/llm/client.ts` routing).
//!
//! Routing is by model id and nothing else (spec §12):
//!
//!   - `openai:gpt-5`        → OpenAI proper, the Responses API
//!   - `cerebras:gpt-oss-120b` → Cerebras Inference, the chat-completions API
//!   - `@cf/vendor/model`    → Cloudflare Workers AI, the chat-completions API
//!   - `vendor/model`        → OpenRouter, the chat-completions API
//!   - `claude-opus-5`       → Anthropic
//! `provider_for` is pure, so the routing rule is unit-testable without a key.
//! Keys are read at `run()` time, never cached, so a key set through the
//! running server applies without a restart.

use std::sync::{Arc, LazyLock};

use serde::{Deserialize, Serialize};

use crate::error::LlmError;
use crate::types::LlmParams;

/// The five providers. Serializes to the TS union strings.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Anthropic,
    Openai,
    Openrouter,
    Cloudflare,
    Cerebras,
}

impl Provider {
    /// The lowercase provider label used in error text and the wire.
    pub fn as_str(&self) -> &'static str {
        match self {
            Provider::Anthropic => "anthropic",
            Provider::Openai => "openai",
            Provider::Openrouter => "openrouter",
            Provider::Cloudflare => "cloudflare",
            Provider::Cerebras => "cerebras",
        }
    }
}

impl std::fmt::Display for Provider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Route a model id to its provider: an `openai:model` id → OpenAI proper, a
/// `cerebras:model` id → Cerebras Inference, a `@cf/…` id → Cloudflare
/// Workers AI, any other `vendor/model` id → OpenRouter, everything else
/// (a bare `claude-…`) → Anthropic.
///
/// Workers AI ids are themselves `vendor/model` shaped (`@cf/meta/llama-…`),
/// so the `@cf/` test HAS to come before the slash test or every Cloudflare
/// model would be sent to OpenRouter — which would answer with a 400 naming a
/// model it never had. The `cerebras:` prefix is the same kind of claim as
/// `openai:`: Cerebras serves bare ids (`gpt-oss-120b`) that would otherwise
/// fall through to Anthropic.
pub fn provider_for(model: &str) -> Provider {
    if model.starts_with("openai:") {
        return Provider::Openai;
    }
    if model.starts_with("cerebras:") {
        return Provider::Cerebras;
    }
    if model.starts_with("@cf/") {
        return Provider::Cloudflare;
    }
    if model.contains('/') {
        Provider::Openrouter
    } else {
        Provider::Anthropic
    }
}

/// The env var carrying each provider's key. Read at `run()` time, never cached.
pub fn api_key_env(provider: Provider) -> &'static str {
    match provider {
        Provider::Anthropic => "ANTHROPIC_API_KEY",
        Provider::Openai => "OPENAI_API_KEY",
        Provider::Openrouter => "OPENROUTER_API_KEY",
        Provider::Cloudflare => "CLOUDFLARE_API_KEY",
        Provider::Cerebras => "CEREBRAS_API_KEY",
    }
}

/// Cloudflare is the one provider whose endpoint is account-scoped: the
/// account id is part of the URL, not a header, so a key alone cannot reach it.
pub const CLOUDFLARE_ACCOUNT_ENV: &str = "CLOUDFLARE_ACCOUNT_ID";

/// Reads one environment variable. Injected so tests never depend on the shell.
pub type Env = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// The production env reader.
pub fn process_env() -> Env {
    Arc::new(|key| std::env::var(key).ok())
}

/// The seams a provider client needs from the outside world. Both are injected.
#[derive(Clone, Default)]
pub struct ProviderOpts {
    /// Defaults to reading the process environment.
    pub env: Option<Env>,
    /// Defaults to the reqwest transport. Tests pass a canned SSE transport.
    pub transport: Option<Arc<dyn crate::sse::Transport>>,
}

impl ProviderOpts {
    pub fn env_or_default(&self) -> Env {
        self.env.clone().unwrap_or_else(process_env)
    }
    pub fn transport_or_default(&self) -> Arc<dyn crate::sse::Transport> {
        self.transport
            .clone()
            .unwrap_or_else(|| Arc::new(crate::sse::ReqwestTransport::new()))
    }
}

/// Resolve the provider's API key from the env, first non-empty (trimmed) wins.
///
/// 401 so `is_retryable` says no: a missing key will still be missing in 15
/// seconds, and six backed-off attempts would only delay the message the user
/// needs to read.
pub fn require_key(
    env: &Env,
    provider: Provider,
    alternatives: &[&str],
) -> Result<String, LlmError> {
    let mut names: Vec<&str> = vec![api_key_env(provider)];
    names.extend_from_slice(alternatives);
    for name in &names {
        if let Some(value) = env(name) {
            let value = value.trim();
            if !value.is_empty() {
                return Ok(value.to_string());
            }
        }
    }
    // Names the file as well as the variable: a missing key is the first thing
    // a new install hits, and "which variable" is only half of what the person
    // reading it needs. The server reads this file at start, so `export` in a
    // shell it did not inherit is the other common way to be confused here.
    Err(LlmError::with(
        format!(
            "{provider}: {} is not set — put it in ~/.bough/env and start bough again",
            names.join(" / ")
        ),
        401,
        None,
    ))
}

/// Both system tiers joined, stable first, for the providers that take a
/// single system/instructions field and cache prefixes implicitly. `None`
/// when both are empty. No separator (`"A"+"B" = "AB"`, test-pinned).
pub fn joined_system(p: &LlmParams) -> Option<String> {
    let s = format!(
        "{}{}",
        p.system.as_deref().unwrap_or(""),
        p.system_volatile.as_deref().unwrap_or("")
    );
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

// ---- the model catalog ------------------------------------------------------
//
// Model ids live here for the same reason everything else does: an id IS a
// provider routing decision, so a picker entry written anywhere else would put
// a provider name outside `llm/`.

/// One picker row.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ModelRow {
    pub id: String,
    pub label: String,
    pub provider: Provider,
}

fn row(id: &str, label: &str, provider: Provider) -> ModelRow {
    ModelRow {
        id: id.to_string(),
        label: label.to_string(),
        provider,
    }
}

/// The curated picker entries. Frontier and cheap tiers are both chosen here
/// (spec §12). Every entry's id must route to its declared provider — pinned
/// by test.
pub static MODELS: LazyLock<Vec<ModelRow>> = LazyLock::new(|| {
    use Provider::*;
    vec![
        row("claude-opus-4-8", "Opus 4.8", Anthropic),
        row("claude-opus-5", "Opus 5", Anthropic),
        row("claude-fable-5", "Fable 5", Anthropic),
        row("claude-sonnet-5", "Sonnet 5", Anthropic),
        row("claude-haiku-4-5", "Haiku 4.5", Anthropic),
        row("openai:gpt-5", "GPT-5 (OpenAI)", Openai),
        row("openai:gpt-5-mini", "GPT-5 mini (OpenAI)", Openai),
        row("openai/gpt-5", "GPT-5 (OpenRouter)", Openrouter),
        row(
            "openai/gpt-oss-120b",
            "GPT-OSS 120B (OpenRouter)",
            Openrouter,
        ),
        row(
            "google/gemini-2.5-pro",
            "Gemini 2.5 Pro (OpenRouter)",
            Openrouter,
        ),
        row("z-ai/glm-5.2", "GLM 5.2 (OpenRouter)", Openrouter),
        row(
            "deepseek/deepseek-v4-flash",
            "DeepSeek V4 Flash (OpenRouter)",
            Openrouter,
        ),
        row("moonshotai/kimi-k3", "Kimi K3 (OpenRouter)", Openrouter),
        row("@cf/zai-org/glm-5.2", "GLM 5.2 (Workers AI)", Cloudflare),
        row(
            "@cf/openai/gpt-oss-120b",
            "GPT-OSS 120B (Workers AI)",
            Cloudflare,
        ),
        row(
            "@cf/moonshotai/kimi-k2.7-code",
            "Kimi K2.7 Code (Workers AI)",
            Cloudflare,
        ),
        row(
            "@cf/meta/llama-3.3-70b-instruct-fp8-fast",
            "Llama 3.3 70B (Workers AI)",
            Cloudflare,
        ),
        row("cerebras:gpt-oss-120b", "GPT-OSS 120B (Cerebras)", Cerebras),
        row("cerebras:zai-glm-4.7", "GLM 4.7 (Cerebras)", Cerebras),
        row("cerebras:gemma-4-31b", "Gemma 4 31B (Cerebras)", Cerebras),
    ]
});

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pricing::catalog_keys;

    #[test]
    fn provider_for_openai_prefix_vendor_model_bare_id() {
        let table: &[(&str, Provider)] = &[
            ("claude-opus-5", Provider::Anthropic),
            ("claude-haiku-4-5", Provider::Anthropic),
            ("openai:gpt-5", Provider::Openai),
            ("openai:gpt-5-mini", Provider::Openai),
            // The prefix wins over the slash: "openai:" is OpenAI proper even
            // though the bare id could look routable.
            ("openai:ft/custom-model", Provider::Openai),
            ("openai/gpt-5", Provider::Openrouter),
            ("google/gemini-2.5-pro", Provider::Openrouter),
            ("moonshotai/kimi-k3", Provider::Openrouter),
            // `@cf/` wins over the slash — a Workers AI id is vendor/model
            // shaped too, and sending it to OpenRouter would 400 on a model
            // that provider never had.
            ("@cf/zai-org/glm-5.2", Provider::Cloudflare),
            (
                "@cf/meta/llama-3.3-70b-instruct-fp8-fast",
                Provider::Cloudflare,
            ),
            ("cerebras:gpt-oss-120b", Provider::Cerebras),
            ("cerebras:zai-glm-4.7", Provider::Cerebras),
            // prefix wins over slash, same as openai:
            ("cerebras:org/custom", Provider::Cerebras),
        ];
        for (model, provider) in table {
            assert_eq!(provider_for(model), *provider, "{model}");
        }
    }

    #[test]
    fn every_catalog_entry_routes_to_the_provider_it_claims() {
        for m in MODELS.iter() {
            assert_eq!(provider_for(&m.id), m.provider, "{}", m.id);
        }
    }

    #[test]
    fn pricing_keys_and_client_routing_cannot_drift_apart() {
        // pricing.rs derives its catalog key from the model id independently.
        // If the two rules diverge, an entire provider silently stops being
        // priced and every cost quietly becomes None — so pin them together.
        for m in MODELS.iter() {
            let expected_prefix = match provider_for(&m.id) {
                Provider::Anthropic => "anthropic/",
                Provider::Openai => "openai/",
                Provider::Openrouter => "openrouter/",
                Provider::Cloudflare => "cloudflare-workers-ai/",
                Provider::Cerebras => "cerebras/",
            };
            let keys = catalog_keys(&m.id);
            assert!(
                keys[0].starts_with(expected_prefix),
                "{}: catalog key {} does not match provider {}",
                m.id,
                keys[0],
                provider_for(&m.id)
            );
        }
    }

    #[test]
    fn require_key_names_the_env_vars_and_is_401() {
        let env: Env = Arc::new(|_| None);
        let err = require_key(&env, Provider::Anthropic, &["ANTHROPIC_AUTH_TOKEN"]).unwrap_err();
        assert_eq!(err.status(), 401, "a missing key must not be retried");
        assert_eq!(
            err.to_string(),
            "anthropic: ANTHROPIC_API_KEY / ANTHROPIC_AUTH_TOKEN is not set \
             — put it in ~/.bough/env and start bough again"
        );
        // Blank values are not keys; the first non-empty (trimmed) wins.
        let env: Env = Arc::new(|k| match k {
            "ANTHROPIC_API_KEY" => Some("   ".into()),
            "ANTHROPIC_AUTH_TOKEN" => Some(" tok ".into()),
            _ => None,
        });
        assert_eq!(
            require_key(&env, Provider::Anthropic, &["ANTHROPIC_AUTH_TOKEN"]).unwrap(),
            "tok"
        );
    }

    #[test]
    fn joined_system_stable_first_none_when_both_empty() {
        let p = |s: Option<&str>, v: Option<&str>| LlmParams {
            model: "m".into(),
            system: s.map(String::from),
            system_volatile: v.map(String::from),
            max_tokens: 1,
            messages: vec![],
            tools: vec![],
            tool_choice_none: false,
            effort: None,
        };
        assert_eq!(joined_system(&p(Some("A"), Some("B"))), Some("AB".into()));
        assert_eq!(joined_system(&p(None, None)), None);
        assert_eq!(joined_system(&p(Some(""), None)), None);
        assert_eq!(
            joined_system(&p(None, Some("only volatile"))),
            Some("only volatile".into())
        );
    }
}
