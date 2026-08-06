//! Provider model discovery for the picker (port of the catalog half of
//! `src/llm/client.ts`; plan row 3.15).
//!
//! The failure policy is the whole design: discovery **never fails and never
//! caches**. No key, a bad key, a rate limit, a garbage body or an offline
//! machine all mean "no extra rows" — never an error, never a slow boot
//! (`server/models.rs` races this against a deadline) — and one provider
//! being down must not cost the others their rows. The caller owns any
//! caching; a module-level cache here would be exactly the global this file
//! is written to avoid.

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use crate::llm::routing::{
    api_key_env, Env, ModelRow, Provider, ProviderOpts, CLOUDFLARE_ACCOUNT_ENV,
};
use crate::llm::sse::{HttpRequest, Transport};

/// Every discovery call gives up after this long. A picker nicety must never
/// hold a boot.
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(10);

// Chat models only: completions/embeddings/audio/image ids would either 404
// on the Responses API or make no sense in a coding-agent picker. Dated
// snapshots are dropped — the alias id always exists and tracks the latest.
const OPENAI_EXCLUDE: [&str; 11] = [
    "audio",
    "realtime",
    "tts",
    "whisper",
    "embed",
    "dall",
    "image",
    "moderation",
    "transcribe",
    "search-preview",
    "instruct",
];
const OPENAI_CAP: usize = 25;

/// `/^(gpt-|o\d|chatgpt-)/`.
fn openai_included(id: &str) -> bool {
    if id.starts_with("gpt-") || id.starts_with("chatgpt-") {
        return true;
    }
    let mut chars = id.chars();
    chars.next() == Some('o') && chars.next().is_some_and(|c| c.is_ascii_digit())
}

/// `/-\d{4}-\d{2}-\d{2}$/` — a dated snapshot id.
fn openai_dated(id: &str) -> bool {
    let b = id.as_bytes();
    if b.len() < 11 {
        return false;
    }
    let tail = &b[b.len() - 11..];
    let digits = |r: &[u8]| r.iter().all(|c| c.is_ascii_digit());
    tail[0] == b'-'
        && digits(&tail[1..5])
        && tail[5] == b'-'
        && digits(&tail[6..8])
        && tail[8] == b'-'
        && digits(&tail[9..11])
}

/// Split an id into alternating non-digit / digit runs (TS `s.split(/(\d+)/)`
/// keeps the captured separators, and yields an empty leading piece when the
/// string starts with a digit).
fn split_digit_runs(s: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut in_digits = false;
    for c in s.chars() {
        let d = c.is_ascii_digit();
        if d != in_digits {
            out.push(std::mem::take(&mut cur));
            in_digits = d;
        }
        cur.push(c);
    }
    out.push(cur);
    out
}

/// Newest-first, comparing version numbers as NUMBERS.
///
/// A plain descending lexicographic sort reads `gpt-5.10` as older than
/// `gpt-5.6`, because "1" sorts before "6" one character at a time. That is
/// invisible until a family reaches a double-digit minor, and then it is the
/// newest model in the list that sinks below the cap and vanishes from the
/// picker — the exact failure this discovery code exists to prevent. Digit
/// runs are therefore compared numerically and everything else
/// lexicographically, which leaves ids without numbers ordered exactly as
/// before (a number against a word is still a string comparison, which is
/// what keeps `o3` above `gpt-5`).
fn by_newest(a: &str, b: &str) -> std::cmp::Ordering {
    let (as_, bs) = (split_digit_runs(a), split_digit_runs(b));
    for i in 0..as_.len().max(bs.len()) {
        let x = as_.get(i).map(String::as_str).unwrap_or("");
        let y = bs.get(i).map(String::as_str).unwrap_or("");
        if x == y {
            continue;
        }
        let numeric = !x.is_empty()
            && !y.is_empty()
            && x.bytes().all(|c| c.is_ascii_digit())
            && y.bytes().all(|c| c.is_ascii_digit());
        return if numeric {
            // Descending: the bigger number is newer.
            y.parse::<u128>()
                .unwrap_or(0)
                .cmp(&x.parse::<u128>().unwrap_or(0))
        } else {
            // Descending string order, which is what TS `y.localeCompare(x)`
            // gives for these ASCII ids.
            y.cmp(x)
        };
    }
    std::cmp::Ordering::Equal
}

/// Pure filter/mapper, so the selection rules are testable without a network.
pub fn filter_openai_models(ids: &[String]) -> Vec<ModelRow> {
    let mut kept: Vec<&String> = ids
        .iter()
        .filter(|id| {
            openai_included(id)
                && !OPENAI_EXCLUDE.iter().any(|bad| id.contains(bad))
                && !openai_dated(id)
        })
        .collect();
    kept.sort_by(|a, b| by_newest(a, b));
    kept.into_iter()
        .take(OPENAI_CAP)
        .map(|id| ModelRow {
            id: format!("openai:{id}"),
            label: format!("{id} (OpenAI)"),
            provider: Provider::Openai,
        })
        .collect()
}

/// Static table first, discovered entries after, deduped by id (static wins).
pub fn merge_models(static_models: &[ModelRow], dynamic: &[ModelRow]) -> Vec<ModelRow> {
    let seen: std::collections::HashSet<&str> =
        static_models.iter().map(|m| m.id.as_str()).collect();
    let mut out: Vec<ModelRow> = static_models.to_vec();
    out.extend(
        dynamic
            .iter()
            .filter(|m| !seen.contains(m.id.as_str()))
            .cloned(),
    );
    out
}

/// One GET, parsed as JSON, or an empty list.
///
/// Every provider's model endpoint answers a rows-in-a-key shape, so the
/// differences that remain are the URL, the auth header, and how a row
/// becomes a `ModelRow` — which is exactly what each caller passes in.
/// Sharing the failure policy matters more than sharing the shape.
async fn fetch_models(
    transport: &dyn Transport,
    url: String,
    headers: Vec<(String, String)>,
    map: impl Fn(&Value) -> Vec<ModelRow>,
) -> Vec<ModelRow> {
    let req = HttpRequest {
        url,
        headers,
        body: None,
    };
    let fetched = tokio::time::timeout(DISCOVERY_TIMEOUT, async {
        let res = transport.fetch(req).await.ok()?;
        if !res.ok() {
            return None;
        }
        serde_json::from_str::<Value>(&res.text().await).ok()
    })
    .await;
    match fetched {
        Ok(Some(body)) => map(&body),
        // A timeout, a dead socket, a non-2xx or a garbage body: no extra rows.
        _ => Vec::new(),
    }
}

fn key(env: &Env, provider: Provider) -> Option<String> {
    let value = env(api_key_env(provider))?;
    let value = value.trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

fn base(env: &Env, var: &str, default: &str) -> String {
    env(var).unwrap_or_else(|| default.to_string())
}

fn parts(opts: &ProviderOpts) -> (Env, Arc<dyn Transport>) {
    (opts.env_or_default(), opts.transport_or_default())
}

/// Ask OpenAI what it offers, for the picker.
pub async fn discover_openai_models(opts: ProviderOpts) -> Vec<ModelRow> {
    let (env, transport) = parts(&opts);
    let Some(key) = key(&env, Provider::Openai) else {
        return Vec::new();
    };
    let base = base(&env, "OPENAI_API_BASE", "https://api.openai.com");
    fetch_models(
        transport.as_ref(),
        format!("{base}/v1/models"),
        vec![("authorization".into(), format!("Bearer {key}"))],
        |body| {
            let ids: Vec<String> = body["data"]
                .as_array()
                .map(|rows| {
                    rows.iter()
                        .filter_map(|m| m["id"].as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            filter_openai_models(&ids)
        },
    )
    .await
}

/// Ask Anthropic what it offers. Same failure policy as the OpenAI path.
///
/// `display_name` is used verbatim when present — the API already names its
/// models the way a human would ("Claude Opus 4.8"), so inventing a label
/// here would be a second naming scheme to keep in sync with theirs. Ids are
/// bare, which is what `provider_for` routes to Anthropic, so nothing is
/// prefixed.
pub async fn discover_anthropic_models(opts: ProviderOpts) -> Vec<ModelRow> {
    let (env, transport) = parts(&opts);
    let Some(key) = key(&env, Provider::Anthropic) else {
        return Vec::new();
    };
    let base = base(&env, "ANTHROPIC_API_BASE", "https://api.anthropic.com");
    fetch_models(
        transport.as_ref(),
        format!("{base}/v1/models?limit=1000"),
        vec![
            ("x-api-key".into(), key),
            ("anthropic-version".into(), "2023-06-01".into()),
        ],
        |body| {
            body["data"]
                .as_array()
                .map(|rows| {
                    rows.iter()
                        .filter_map(|m| {
                            let id = m["id"].as_str()?;
                            let label = m["display_name"].as_str().filter(|d| !d.is_empty());
                            Some(ModelRow {
                                id: id.to_string(),
                                label: label.unwrap_or(id).to_string(),
                                provider: Provider::Anthropic,
                            })
                        })
                        .collect()
                })
                .unwrap_or_default()
        },
    )
    .await
}

/// Ask OpenRouter what it offers.
///
/// The one provider whose catalog is PUBLIC — `/api/v1/models` answers
/// without a key. The key is still sent when there is one (it scopes the list
/// to what the account can actually reach), but its absence is not a reason
/// to skip the call: a user deciding whether to add an OpenRouter key is
/// better served by seeing what they would get.
pub async fn discover_openrouter_models(opts: ProviderOpts) -> Vec<ModelRow> {
    let (env, transport) = parts(&opts);
    let key = key(&env, Provider::Openrouter);
    let base = base(&env, "OPENROUTER_API_BASE", "https://openrouter.ai/api");
    let headers = match key {
        Some(key) => vec![("authorization".to_string(), format!("Bearer {key}"))],
        None => vec![],
    };
    fetch_models(
        transport.as_ref(),
        format!("{base}/v1/models"),
        headers,
        |body| {
            body["data"]
                .as_array()
                .map(|rows| {
                    rows.iter()
                        .filter_map(|m| {
                            let id = m["id"].as_str()?;
                            let label = m["name"].as_str().filter(|n| !n.is_empty());
                            Some(ModelRow {
                                id: id.to_string(),
                                label: label.unwrap_or(id).to_string(),
                                provider: Provider::Openrouter,
                            })
                        })
                        .collect()
                })
                .unwrap_or_default()
        },
    )
    .await
}

/// Ask Cloudflare what its account can run.
///
/// Two things make this one not reuse the shared body mapper: the catalog is
/// account-scoped (no account id, no list — same failure policy as no key: an
/// empty list, and no request at all), and Workers AI answers `{result: […]}`
/// rather than `{data: […]}`. The task filter is the point of the call — the
/// catalog is mostly embeddings, image and speech models.
pub async fn discover_cloudflare_models(opts: ProviderOpts) -> Vec<ModelRow> {
    let (env, transport) = parts(&opts);
    let key = key(&env, Provider::Cloudflare).or_else(|| {
        env("CLOUDFLARE_API_TOKEN")
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    });
    let account = env(CLOUDFLARE_ACCOUNT_ENV)
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());
    let (Some(key), Some(account)) = (key, account) else {
        return Vec::new();
    };
    let base = base(
        &env,
        "CLOUDFLARE_API_BASE",
        "https://api.cloudflare.com/client/v4",
    );
    fetch_models(
        transport.as_ref(),
        format!(
            "{base}/accounts/{account}/ai/models/search\
             ?task=Text+Generation&per_page=100&hide_experimental=true"
        ),
        vec![("authorization".into(), format!("Bearer {key}"))],
        |body| {
            body["result"]
                .as_array()
                .map(|rows| {
                    rows.iter()
                        .filter_map(|m| {
                            let name = m["name"].as_str()?;
                            let bare = name.strip_prefix("@cf/")?;
                            Some(ModelRow {
                                id: name.to_string(),
                                label: format!("{bare} (Workers AI)"),
                                provider: Provider::Cloudflare,
                            })
                        })
                        .collect()
                })
                .unwrap_or_default()
        },
    )
    .await
}

/// Every provider at once. **Concurrent and independently fallible**: one
/// provider being down, keyless or slow must not cost the others their rows.
/// Each discovery already answers `[]` rather than failing, so this is the
/// belt to that braces.
pub async fn discover_models(opts: ProviderOpts) -> Vec<ModelRow> {
    let (anthropic, openai, openrouter, cloudflare) = tokio::join!(
        discover_anthropic_models(opts.clone()),
        discover_openai_models(opts.clone()),
        discover_openrouter_models(opts.clone()),
        discover_cloudflare_models(opts),
    );
    let mut out = anthropic;
    out.extend(openai);
    out.extend(openrouter);
    out.extend(cloudflare);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::errors::BoughError;
    use crate::llm::routing::{provider_for, MODELS};
    use crate::llm::sse::HttpResponse;
    use serde_json::json;
    use std::sync::Mutex;

    /// A transport that answers every GET from one function of the URL, and
    /// records the URLs it was asked for.
    struct UrlTransport {
        seen: Mutex<Vec<String>>,
        answer: Box<dyn Fn(&str) -> Result<(u16, String), BoughError> + Send + Sync>,
    }

    impl UrlTransport {
        fn new(
            answer: impl Fn(&str) -> Result<(u16, String), BoughError> + Send + Sync + 'static,
        ) -> Arc<Self> {
            Arc::new(UrlTransport {
                seen: Mutex::new(Vec::new()),
                answer: Box::new(answer),
            })
        }
        fn ok(body: Value) -> Arc<Self> {
            Self::new(move |_| Ok((200, body.to_string())))
        }
        /// Fails the test if it is ever called.
        fn forbidden() -> Arc<Self> {
            Self::new(|url| panic!("must not be called: {url}"))
        }
    }

    #[async_trait::async_trait]
    impl Transport for UrlTransport {
        async fn fetch(&self, req: HttpRequest) -> Result<HttpResponse, BoughError> {
            assert!(req.body.is_none(), "discovery is a GET");
            self.seen.lock().unwrap().push(req.url.clone());
            let (status, body) = (self.answer)(&req.url)?;
            Ok(HttpResponse {
                status,
                headers: vec![],
                body: Box::pin(futures::stream::iter(vec![Ok(body.into_bytes())])),
            })
        }
    }

    fn opts(env: Env, transport: Arc<dyn Transport>) -> ProviderOpts {
        ProviderOpts {
            env: Some(env),
            transport: Some(transport),
        }
    }

    /// Keys only — a blanket stub would also answer `*_API_BASE` and rewrite
    /// every URL out from under the assertions.
    fn keys_only() -> Env {
        Arc::new(|k| k.ends_with("_API_KEY").then(|| "sk-test".to_string()))
    }

    fn no_env() -> Env {
        Arc::new(|_| None)
    }

    #[test]
    fn filter_openai_models_chat_ids_only_dated_snapshots_dropped_newest_first() {
        let ids: Vec<String> = [
            "gpt-5",
            "gpt-5-2026-01-01",
            "gpt-4o-audio-preview",
            "text-embedding-3-large",
            "o3",
            "dall-e-3",
            "chatgpt-4o-latest",
            "gpt-3.5-turbo-instruct",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let rows = filter_openai_models(&ids);
        assert_eq!(
            rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            vec!["openai:o3", "openai:gpt-5", "openai:chatgpt-4o-latest"]
        );
        for r in &rows {
            assert_eq!(provider_for(&r.id), Provider::Openai);
        }
    }

    #[test]
    fn filter_openai_models_version_numbers_sort_as_numbers_not_as_text() {
        // "gpt-5.10" is NEWER than "gpt-5.6". A plain string sort reads it as
        // older, so the newest model in the list is the one that falls past
        // the cap and never reaches the picker.
        let ids: Vec<String> = ["gpt-5.6", "gpt-5.10", "gpt-5.6-luna", "gpt-5.2"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            filter_openai_models(&ids)
                .iter()
                .map(|r| r.id.as_str())
                .collect::<Vec<_>>(),
            vec![
                "openai:gpt-5.10",
                "openai:gpt-5.6-luna",
                "openai:gpt-5.6",
                "openai:gpt-5.2"
            ]
        );
    }

    #[test]
    fn filter_openai_models_caps_the_list_at_25() {
        let ids: Vec<String> = (0..40).map(|i| format!("gpt-{i}")).collect();
        let rows = filter_openai_models(&ids);
        assert_eq!(rows.len(), OPENAI_CAP);
        assert_eq!(rows[0].id, "openai:gpt-39", "newest first, then the cap");
    }

    #[test]
    fn merge_models_the_static_table_wins_on_id_collisions() {
        let dynamic = vec![
            ModelRow {
                id: "openai:gpt-5".into(),
                label: "gpt-5 (OpenAI)".into(),
                provider: Provider::Openai,
            },
            ModelRow {
                id: "openai:o4".into(),
                label: "o4 (OpenAI)".into(),
                provider: Provider::Openai,
            },
        ];
        let merged = merge_models(&MODELS, &dynamic);
        assert_eq!(merged.iter().filter(|m| m.id == "openai:gpt-5").count(), 1);
        assert_eq!(merged.last().unwrap().id, "openai:o4");
        // The static entry's label survived the collision.
        let gpt5 = merged.iter().find(|m| m.id == "openai:gpt-5").unwrap();
        assert_eq!(gpt5.label, "GPT-5 (OpenAI)");
    }

    #[tokio::test]
    async fn discover_openai_models_no_key_and_a_failing_request_both_yield_an_empty_list() {
        assert!(
            discover_openai_models(opts(no_env(), UrlTransport::forbidden()))
                .await
                .is_empty()
        );

        let failing = UrlTransport::new(|_| Ok((401, "nope".into())));
        assert!(discover_openai_models(opts(keys_only(), failing))
            .await
            .is_empty());

        let dead = UrlTransport::new(|_| Err(BoughError::llm("offline")));
        assert!(discover_openai_models(opts(keys_only(), dead))
            .await
            .is_empty());
    }

    #[tokio::test]
    async fn discover_openai_models_a_good_response_maps_into_picker_rows() {
        let transport = UrlTransport::ok(
            json!({ "data": [{ "id": "gpt-5" }, { "id": 42 }, { "id": "whisper-1" }] }),
        );
        let rows = discover_openai_models(opts(keys_only(), transport.clone())).await;
        assert_eq!(
            rows,
            vec![ModelRow {
                id: "openai:gpt-5".into(),
                label: "gpt-5 (OpenAI)".into(),
                provider: Provider::Openai,
            }]
        );
        assert_eq!(
            transport.seen.lock().unwrap()[0],
            "https://api.openai.com/v1/models"
        );
    }

    #[tokio::test]
    async fn discover_anthropic_models_display_name_becomes_the_label_ids_stay_bare() {
        // Bare ids are what `provider_for` routes to Anthropic, so nothing is
        // prefixed.
        let transport = UrlTransport::ok(json!({
            "data": [
                { "id": "claude-opus-4-7", "display_name": "Claude Opus 4.7" },
                { "id": "claude-weird" },
            ],
        }));
        let env: Env = Arc::new(|k| (k == "ANTHROPIC_API_KEY").then(|| "sk-test".to_string()));
        let rows = discover_anthropic_models(opts(env, transport.clone())).await;
        assert_eq!(
            rows,
            vec![
                ModelRow {
                    id: "claude-opus-4-7".into(),
                    label: "Claude Opus 4.7".into(),
                    provider: Provider::Anthropic,
                },
                // No display_name: the id is a better label than an invented one.
                ModelRow {
                    id: "claude-weird".into(),
                    label: "claude-weird".into(),
                    provider: Provider::Anthropic,
                },
            ]
        );
        for r in &rows {
            assert_eq!(provider_for(&r.id), Provider::Anthropic);
        }
        assert_eq!(
            transport.seen.lock().unwrap()[0],
            "https://api.anthropic.com/v1/models?limit=1000"
        );
    }

    #[tokio::test]
    async fn discover_anthropic_models_no_key_means_no_rows_and_no_request() {
        let transport = UrlTransport::forbidden();
        assert!(discover_anthropic_models(opts(no_env(), transport.clone()))
            .await
            .is_empty());
        assert!(transport.seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn discover_openrouter_models_asks_without_a_key_its_catalog_is_public() {
        // The one provider whose list is worth showing to someone who has not
        // signed up.
        let transport = UrlTransport::ok(
            json!({ "data": [{ "id": "vendor/model", "name": "Vendor: Model" }] }),
        );
        let rows = discover_openrouter_models(opts(no_env(), transport.clone())).await;
        assert_eq!(
            rows,
            vec![ModelRow {
                id: "vendor/model".into(),
                label: "Vendor: Model".into(),
                provider: Provider::Openrouter,
            }]
        );
        assert_eq!(provider_for(&rows[0].id), Provider::Openrouter);
        assert_eq!(
            transport.seen.lock().unwrap()[0],
            "https://openrouter.ai/api/v1/models"
        );
    }

    #[tokio::test]
    async fn discover_cloudflare_models_text_generation_rows_only_account_scoped() {
        let transport = UrlTransport::ok(json!({
            "result": [
                { "name": "@cf/meta/llama-3.3-70b-instruct-fp8-fast" },
                { "name": "not-a-workers-ai-id" },
            ],
        }));
        let env: Env = Arc::new(|k| match k {
            "CLOUDFLARE_API_KEY" => Some("k".into()),
            "CLOUDFLARE_ACCOUNT_ID" => Some("acct-1".into()),
            _ => None,
        });
        let rows = discover_cloudflare_models(opts(env, transport.clone())).await;
        assert_eq!(
            rows,
            vec![ModelRow {
                id: "@cf/meta/llama-3.3-70b-instruct-fp8-fast".into(),
                label: "meta/llama-3.3-70b-instruct-fp8-fast (Workers AI)".into(),
                provider: Provider::Cloudflare,
            }]
        );
        let url = transport.seen.lock().unwrap()[0].clone();
        assert!(url.contains("/accounts/acct-1/ai/models/search"), "{url}");
        assert!(url.contains("task=Text+Generation"), "{url}");
        assert!(url.contains("per_page=100&hide_experimental=true"), "{url}");
        assert_eq!(provider_for(&rows[0].id), Provider::Cloudflare);

        // No account id → no list, and no call: the endpoint cannot even be
        // formed.
        let guard = UrlTransport::forbidden();
        let key_only: Env = Arc::new(|k| (k == "CLOUDFLARE_API_KEY").then(|| "k".to_string()));
        assert!(discover_cloudflare_models(opts(key_only, guard.clone()))
            .await
            .is_empty());
        assert!(guard.seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn discovery_never_fails_a_non_2xx_a_bad_body_and_a_dead_socket_all_yield_nothing() {
        let cases: Vec<Arc<UrlTransport>> = vec![
            UrlTransport::new(|_| Err(BoughError::llm("network"))),
            UrlTransport::new(|_| Ok((500, "nope".into()))),
            UrlTransport::new(|_| Ok((200, "<html>".into()))),
        ];
        for transport in cases {
            assert!(
                discover_anthropic_models(opts(keys_only(), transport.clone()))
                    .await
                    .is_empty()
            );
            assert!(
                discover_openrouter_models(opts(keys_only(), transport.clone()))
                    .await
                    .is_empty()
            );
            assert!(discover_openai_models(opts(keys_only(), transport.clone()))
                .await
                .is_empty());
        }
    }

    #[tokio::test]
    async fn discover_models_one_provider_failing_does_not_cost_the_others_their_rows() {
        // The reason this is a join of independently-fallible calls.
        let transport = UrlTransport::new(|url| {
            if url.contains("anthropic") {
                return Err(BoughError::llm("anthropic is down"));
            }
            if url.contains("openrouter") {
                return Ok((
                    200,
                    json!({ "data": [{ "id": "v/m", "name": "V M" }] }).to_string(),
                ));
            }
            Ok((200, json!({ "data": [{ "id": "gpt-5" }] }).to_string()))
        });
        let mut ids: Vec<String> = discover_models(opts(keys_only(), transport))
            .await
            .into_iter()
            .map(|r| r.id)
            .collect();
        ids.sort();
        assert_eq!(ids, vec!["openai:gpt-5".to_string(), "v/m".to_string()]);
    }
}
