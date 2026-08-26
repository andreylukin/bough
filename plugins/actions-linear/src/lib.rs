//! Invariant: `linear_write` can change a ticket's STATUS or leave a COMMENT, and nothing else.
//! EXACTLY ONE of [`LinearWritePayload`]'s two fields is `Some`; a payload naming a title, a team
//! or a new issue is refused. That refusal exists in ADDITION to the absent `create_ticket` kind,
//! so "ticket creation stays Andrey's" is enforced twice and by different mechanisms.
//!
//! The API key is redacted from every rendering, exactly as in `collector-linear` (P6-D7).

pub mod invariant;

use std::sync::Arc;
use std::time::Duration;

use bough_kernel::{ConfigError, Context, Inject, InvariantSpec, Plugin, PluginError};
use bough_plugin_actions::{
    ActionArtifact, ActionError, ActionKind, ActionProvider, Actions, ActionsHandle, ExecuteRequest,
};
use bough_plugin_actions_reconcile::{ActionLookup, ArtifactLookup};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "actions-linear";

/// The row's config.
#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LinearActionsConfig {
    pub endpoint: String,
    /// `!!expr 'env("LINEAR_API_KEY")'`. Redacted everywhere (P6-D7).
    pub api_key: String,
    pub timeout_ms: u64,
}

impl std::fmt::Debug for LinearActionsConfig {
    /// Every field, with `api_key` rendered as `<redacted>`: a `--dump-config` or a `PluginError`
    /// carrying the key would put it in a log file.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LinearActionsConfig")
            .field("endpoint", &self.endpoint)
            .field("api_key", &"<redacted>")
            .field("timeout_ms", &self.timeout_ms)
            .finish()
    }
}

/// `linear_write`'s payload. EXACTLY ONE of the two is `Some`.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LinearWritePayload {
    pub status: Option<String>,
    pub comment: Option<String>,
}

impl LinearWritePayload {
    /// PURE: refuse a payload that is neither or both. `deny_unknown_fields` already refuses a
    /// `title`/`team`; this is the "exactly one" half.
    pub fn check(&self) -> Result<(), LinearActionError> {
        match (&self.status, &self.comment) {
            (Some(_), Some(_)) => Err(LinearActionError::BadPayload {
                detail: "both were set: one act per action, so the journal names what happened"
                    .into(),
            }),
            (None, None) => Err(LinearActionError::BadPayload {
                detail: "neither was set: there is nothing to do".into(),
            }),
            (Some(s), None) if s.trim().is_empty() => Err(LinearActionError::BadPayload {
                detail: "`status` is blank".into(),
            }),
            (None, Some(c)) if c.trim().is_empty() => Err(LinearActionError::BadPayload {
                detail: "`comment` is blank".into(),
            }),
            _ => Ok(()),
        }
    }
}

/// What the Provider needs of Linear. A trait so the tests speak to a stub and this crate never
/// learns a second transport.
#[async_trait::async_trait]
pub trait LinearApi: Send + Sync + 'static {
    async fn graphql(
        &self,
        query: &str,
        variables: serde_json::Value,
    ) -> Result<serde_json::Value, LinearActionError>;
}

/// The production client: one POST to the configured endpoint, key in the `Authorization` header.
pub struct LinearHttp {
    endpoint: String,
    api_key: String,
    http: reqwest::Client,
}

#[async_trait::async_trait]
impl LinearApi for LinearHttp {
    async fn graphql(
        &self,
        query: &str,
        variables: serde_json::Value,
    ) -> Result<serde_json::Value, LinearActionError> {
        let res = self
            .http
            .post(&self.endpoint)
            .header("Authorization", &self.api_key)
            .json(&serde_json::json!({ "query": query, "variables": variables }))
            .send()
            .await
            .map_err(|e| LinearActionError::Transport(redact(&e.to_string(), &self.api_key)))?;
        let status = res.status();
        let body: serde_json::Value = res
            .json()
            .await
            .map_err(|e| LinearActionError::Transport(redact(&e.to_string(), &self.api_key)))?;
        if !status.is_success() {
            return Err(LinearActionError::Server(format!("HTTP {status}")));
        }
        if let Some(errors) = body.get("errors") {
            return Err(LinearActionError::Server(errors.to_string()));
        }
        Ok(body.get("data").cloned().unwrap_or(serde_json::Value::Null))
    }
}

/// PURE: never let the key reach a message.
fn redact(text: &str, key: &str) -> String {
    if key.is_empty() {
        return text.to_string();
    }
    text.replace(key, "<redacted>")
}

/// The Provider.
pub struct LinearActions {
    api: Arc<dyn LinearApi>,
}

impl LinearActions {
    /// Build the Provider over the real endpoint.
    pub fn open(cfg: Arc<LinearActionsConfig>) -> Result<Arc<LinearActions>, LinearActionError> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_millis(cfg.timeout_ms))
            .build()
            .map_err(|e| LinearActionError::Transport(e.to_string()))?;
        Ok(LinearActions::with_api(Arc::new(LinearHttp {
            endpoint: cfg.endpoint.clone(),
            api_key: cfg.api_key.clone(),
            http,
        })))
    }

    /// The same Provider over an injected transport.
    pub fn with_api(api: Arc<dyn LinearApi>) -> Arc<LinearActions> {
        Arc::new(LinearActions { api })
    }

    async fn write(&self, req: &ExecuteRequest) -> Result<ActionArtifact, LinearActionError> {
        let p: LinearWritePayload = serde_json::from_value(req.request.payload.clone())
            .map_err(|e| creation_or_bad(&e.to_string()))?;
        p.check()?;
        let issue = self.issue(&req.canonical_target).await?;
        let id = issue
            .get("id")
            .and_then(|x| x.as_str())
            .ok_or_else(|| LinearActionError::Server(format!("no issue {}", req.canonical_target)))?
            .to_string();

        // The STATUS moves first and the comment carries the marker second, so a crash between
        // them leaves no marker and reconciliation surfaces a draft rather than claiming a done.
        let mut detail = serde_json::json!({ "issue": req.canonical_target });
        let body = match (&p.status, &p.comment) {
            (Some(status), _) => {
                let state_id = state_id(&issue, status)?;
                self.api
                    .graphql(
                        MUTATION_UPDATE,
                        serde_json::json!({ "id": id, "stateId": state_id }),
                    )
                    .await?;
                detail = serde_json::json!({ "issue": req.canonical_target, "status": status });
                format!("Status → {status}.")
            }
            (_, Some(comment)) => comment.clone(),
            _ => unreachable!("`check` proved exactly one is set"),
        };
        let body = format!("{}\n\n<!-- {} -->", body.trim_end(), req.marker);
        let out = self
            .api
            .graphql(
                MUTATION_COMMENT,
                serde_json::json!({ "issueId": id, "body": body }),
            )
            .await?;
        let locator = out
            .pointer("/commentCreate/comment/url")
            .or_else(|| out.pointer("/commentCreate/comment/id"))
            .and_then(|x| x.as_str())
            .unwrap_or(&req.canonical_target)
            .to_string();
        Ok(ActionArtifact {
            locator,
            marker: req.marker.clone(),
            detail,
        })
    }

    /// The issue and its team's states. One READ.
    async fn issue(&self, identifier: &str) -> Result<serde_json::Value, LinearActionError> {
        let out = self
            .api
            .graphql(QUERY_ISSUE, serde_json::json!({ "id": identifier }))
            .await?;
        out.pointer("/issue")
            .cloned()
            .filter(|v| !v.is_null())
            .ok_or_else(|| LinearActionError::Server(format!("no issue {identifier}")))
    }
}

/// PURE: the state id whose name matches, case-insensitively.
fn state_id(issue: &serde_json::Value, status: &str) -> Result<String, LinearActionError> {
    issue
        .pointer("/team/states/nodes")
        .and_then(|x| x.as_array())
        .and_then(|nodes| {
            nodes.iter().find(|n| {
                n.get("name")
                    .and_then(|x| x.as_str())
                    .is_some_and(|name| name.eq_ignore_ascii_case(status))
            })
        })
        .and_then(|n| n.get("id").and_then(|x| x.as_str()))
        .map(str::to_string)
        .ok_or_else(|| LinearActionError::Server(format!("the team has no state named `{status}`")))
}

/// A `deny_unknown_fields` rejection naming a creation-shaped field is the CREATION refusal, not a
/// generic bad payload: the message has to say why the harness will not do it.
fn creation_or_bad(detail: &str) -> LinearActionError {
    for field in ["title", "team", "teamId", "description", "issue"] {
        if detail.contains(&format!("`{field}`"))
            || detail.contains(&format!("unknown field `{field}`"))
        {
            return LinearActionError::Creation {
                field: match field {
                    "title" => "title",
                    "team" => "team",
                    "teamId" => "teamId",
                    "description" => "description",
                    _ => "issue",
                },
            };
        }
    }
    LinearActionError::BadPayload {
        detail: detail.to_string(),
    }
}

const QUERY_ISSUE: &str =
    "query($id:String!){issue(id:$id){id identifier team{states{nodes{id name}}}}}";
const MUTATION_UPDATE: &str =
    "mutation($id:String!,$stateId:String!){issueUpdate(id:$id,input:{stateId:$stateId}){success}}";
const MUTATION_COMMENT: &str =
    "mutation($issueId:String!,$body:String!){commentCreate(input:{issueId:$issueId,body:$body}){comment{id url}}}";
const QUERY_COMMENTS: &str = "query($id:String!){issue(id:$id){comments{nodes{id url body}}}}";

#[async_trait::async_trait]
impl ActionProvider for LinearActions {
    fn kinds(&self) -> Vec<ActionKind> {
        vec![ActionKind::LinearWrite]
    }

    async fn execute(&self, req: &ExecuteRequest) -> Result<ActionArtifact, ActionError> {
        self.write(req).await.map_err(|e| ActionError::Provider {
            kind: ActionKind::LinearWrite.as_str(),
            source: anyhow::Error::new(e),
        })
    }
}

/// Reconciliation's half: is the marker on one of the issue's comments? A READ, always.
#[async_trait::async_trait]
impl ArtifactLookup for LinearActions {
    fn kinds(&self) -> Vec<ActionKind> {
        ActionProvider::kinds(self)
    }

    async fn find_marker(
        &self,
        kind: ActionKind,
        canonical_target: &str,
        marker: &str,
    ) -> Result<Option<ActionArtifact>, ActionError> {
        if kind != ActionKind::LinearWrite {
            return Ok(None);
        }
        let out = self
            .api
            .graphql(
                QUERY_COMMENTS,
                serde_json::json!({ "id": canonical_target }),
            )
            .await
            .map_err(|e| ActionError::Provider {
                kind: kind.as_str(),
                source: anyhow::Error::new(e),
            })?;
        let nodes = out
            .pointer("/issue/comments/nodes")
            .and_then(|x| x.as_array())
            .cloned()
            .unwrap_or_default();
        for n in nodes {
            if n.get("body")
                .and_then(|x| x.as_str())
                .is_some_and(|b| b.contains(marker))
            {
                return Ok(Some(ActionArtifact {
                    locator: n
                        .get("url")
                        .or_else(|| n.get("id"))
                        .and_then(|x| x.as_str())
                        .unwrap_or(canonical_target)
                        .to_string(),
                    marker: marker.to_string(),
                    detail: serde_json::json!({ "found_by": "marker" }),
                }));
            }
        }
        Ok(None)
    }
}

/// What this Provider refuses.
///
/// `plugins/actions` is off-limits in this track and [`ActionError`] has no `BadPayload` variant,
/// so a refusal surfaces as `ActionError::Provider { kind, source }` wrapping one of these. Merge
/// note: `ActionError::BadPayload`.
#[derive(Debug, thiserror::Error)]
pub enum LinearActionError {
    #[error("linear_write refused: exactly one of `status` or `comment` must be set ({detail})")]
    BadPayload { detail: String },
    #[error("linear_write refused: creating tickets is Andrey's, not the harness's (`{field}`)")]
    Creation { field: &'static str },
    #[error("transport: {0}")]
    Transport(String),
    #[error("linear: {0}")]
    Server(String),
}

/// The row.
pub struct LinearActionsPlugin;

#[async_trait::async_trait]
impl Plugin for LinearActionsPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = LinearActionsConfig;

    fn inject() -> Inject {
        // `ledger`: the runtime invariant folds this row's own action rows, so the read has to be
        // declared (the actions seam already requires the ledger, so this never widens a tree).
        Inject::required(["actions", "ledger"]).union(&Inject::optional(["action_lookup"]))
    }

    fn validate(cfg: &Self::Config) -> Result<(), ConfigError> {
        if !cfg.endpoint.starts_with("http://") && !cfg.endpoint.starts_with("https://") {
            return Err(ConfigError::Rejected {
                detail: format!("endpoint: `{}` is not an http(s) url", cfg.endpoint),
            });
        }
        if cfg.timeout_ms == 0 {
            return Err(ConfigError::Rejected {
                detail: "timeout_ms: 0 would make every Linear call fail immediately".into(),
            });
        }
        Ok(())
    }

    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let entry = ctx.entry_id().clone();
        let actions = ctx
            .get::<Actions>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;
        let provider = LinearActions::open(cfg)
            .map_err(|e| PluginError::new(entry.clone(), anyhow::Error::new(e)))?;
        ActionsHandle::provider(&actions, &ctx, provider.clone() as Arc<dyn ActionProvider>)
            .await?;
        if let Ok(reg) = ctx.get::<ActionLookup>() {
            reg.register(&ctx, provider as Arc<dyn ArtifactLookup>)
                .await?;
        } else {
            tracing::warn!(
                "actions-linear: no `action_lookup` in the tree, so an interrupted write cannot be \
                 reconciled by lookup"
            );
        }
        Ok(())
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

bough_kernel::register_plugin!(LinearActionsPlugin);
