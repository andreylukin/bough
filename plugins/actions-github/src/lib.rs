//! Invariant: this Provider writes to GitHub ONLY through a [`GhRunner`] (in production
//! `gh_cli::Gh`), and only after a pre-flight LOOKUP has proved the act is inside §7's boundary:
//!
//! - `push_to_pr` only onto a PR **Andrey authored** and that is **open** — never a teammate's
//!   branch. The author comparison is against `gh api user`'s login, cached per activation.
//! - `bot_thread_op` only on a thread whose opener classifies as [`Actor::Bot`]. **Uncertain is
//!   human**, and a human thread is never auto-resolved.
//!
//! And every artifact CARRIES THE MARKER derived from the idem key, so reconciliation is a lookup
//! and never a guess (§7): PR body last line, commit trailer, comment suffix.
//!
//! [`Actor::Bot`]: bough_plugin_gh_cli::Actor::Bot

pub mod ids;
pub mod invariant;
pub mod marker;
pub mod runner;

use std::sync::Arc;
use std::time::Duration;

use bough_kernel::{ConfigError, Context, Inject, InvariantSpec, Plugin, PluginError};
use bough_plugin_actions::{
    ActionArtifact, ActionError, ActionKind, ActionProvider, Actions, ActionsHandle, ExecuteRequest,
};
use bough_plugin_actions_reconcile::{ActionLookup, ArtifactLookup};
use bough_plugin_gh_cli::{Actor, Gh};

pub use ids::{CommentNodeId, ReviewCommentId, ReviewThreadNodeId};
pub use runner::{GhCli, GhRunner};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "actions-github";

/// The row's config.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GithubActionsConfig {
    /// `"gh"`. The tests put a recording shim here.
    pub gh_bin: String,
    /// The known-bot allowlist [`bough_plugin_gh_cli::classify`] consults.
    pub known_bots: Vec<String>,
    pub timeout_ms: u64,
}

/// `open_pr`'s payload.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OpenPrPayload {
    pub head: String,
    pub base: String,
    pub title: String,
    pub body: String,
}

/// `push_to_pr`'s payload. `commits` are the LOCAL commit shas, oldest first; the last one becomes
/// the PR head.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PushToPrPayload {
    pub branch: String,
    pub commits: Vec<String>,
}

/// `bot_thread_op`'s payload. The thread is named by its FIRST comment's REST database id — the
/// only id a reader of `gh api .../pulls/comments` has. The GraphQL ids the resolve and close
/// mutations need live in a different id space and are LOOKED UP from this one (see
/// [`crate::ids`]); they are never spelled by the caller.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BotThreadPayload {
    pub comment_id: ReviewCommentId,
    pub op: ThreadOp,
    pub body: Option<String>,
}

/// What may be done to a BOT review thread. There is no `create` and no human variant.
///
/// The three are three DIFFERENT acts against GitHub and each one is a different call:
///
/// - `Reply` leaves the comment and stops.
/// - `Resolve` leaves the comment, then `resolveReviewThread` on the THREAD's node id.
/// - `Close` leaves the comment, then `minimizeComment` (classifier `RESOLVED`) on the
///   COMMENT's node id — the thread is folded away rather than marked resolved.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ThreadOp {
    Reply,
    Resolve,
    Close,
}

impl ThreadOp {
    fn as_str(&self) -> &'static str {
        match self {
            ThreadOp::Reply => "reply",
            ThreadOp::Resolve => "resolve",
            ThreadOp::Close => "close",
        }
    }
}

/// The Provider.
pub struct GithubActions {
    cfg: Arc<GithubActionsConfig>,
    gh: Arc<dyn GhRunner>,
    /// `gh api user`'s login. Resolved ONCE and cached; the author comparison reads it.
    me: tokio::sync::Mutex<Option<String>>,
}

impl GithubActions {
    /// Build the Provider over the real `gh`.
    ///
    /// DEVIATION: `me` is resolved LAZILY on the first act that needs it rather than in `open`.
    /// `open` runs during boot, and a row that cannot reach the network must not make the tree
    /// fail to load when nothing has asked it to act yet.
    pub async fn open(cfg: Arc<GithubActionsConfig>) -> Result<Arc<GithubActions>, GhActionError> {
        let gh = Gh::new(cfg.gh_bin.clone(), Duration::from_millis(cfg.timeout_ms));
        Ok(GithubActions::with_runner(cfg, Arc::new(GhCli(gh))))
    }

    /// The same Provider over an injected transport. The tests' recording shim mounts here.
    pub fn with_runner(cfg: Arc<GithubActionsConfig>, gh: Arc<dyn GhRunner>) -> Arc<GithubActions> {
        Arc::new(GithubActions {
            cfg,
            gh,
            me: tokio::sync::Mutex::new(None),
        })
    }

    /// The authenticated login, resolved once.
    pub async fn me(&self) -> Result<String, GhActionError> {
        let mut slot = self.me.lock().await;
        if let Some(me) = slot.as_ref() {
            return Ok(me.clone());
        }
        let me = self.gh.whoami().await?;
        *slot = Some(me.clone());
        Ok(me)
    }

    /// PRE-FLIGHT: `gh pr view --json author,state,isDraft,headRefName`, compared to `me`.
    pub async fn check_push_target(&self, target: &str) -> Result<(), GhActionError> {
        self.pr_head_ref(target).await.map(|_| ())
    }

    /// The same pre-flight, keeping the head ref the push needs.
    async fn pr_head_ref(&self, target: &str) -> Result<String, GhActionError> {
        let (repo, number) = split_pr(target)?;
        let v = self
            .gh
            .json(&[
                "pr",
                "view",
                &number.to_string(),
                "--repo",
                &repo,
                "--json",
                "author,state,isDraft,headRefName",
            ])
            .await?;
        let author = v
            .pointer("/author/login")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let state = v
            .get("state")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let me = self.me().await?;
        if !author.eq_ignore_ascii_case(&me) {
            return Err(GhActionError::NotAuthored {
                target: target.to_string(),
                author,
                me,
            });
        }
        if !state.eq_ignore_ascii_case("open") {
            return Err(GhActionError::NotOpen {
                target: target.to_string(),
                state,
            });
        }
        v.get("headRefName")
            .and_then(|x| x.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| GhActionError::BadPayload {
                kind: "push_to_pr",
                detail: format!("`gh pr view` for {target} named no head ref"),
            })
    }

    /// PRE-FLIGHT: the thread's first comment's `user.type` / `user.login` through
    /// [`bough_plugin_gh_cli::classify`]. [`Actor::Human`] refuses — including the uncertain case.
    ///
    /// DEVIATION from the scaffold's signature: the repo is a parameter, because a review comment
    /// id is only addressable under its repo (`repos/{o}/{r}/pulls/comments/{id}`).
    ///
    /// [`Actor::Human`]: bough_plugin_gh_cli::Actor::Human
    pub async fn check_bot_thread(
        &self,
        repo: &str,
        comment: ReviewCommentId,
    ) -> Result<CommentNodeId, GhActionError> {
        let v = self
            .gh
            .json(&["api", &format!("repos/{repo}/pulls/comments/{comment}")])
            .await?;
        let login = v
            .pointer("/user/login")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let account_type = v
            .pointer("/user/type")
            .and_then(|x| x.as_str())
            .unwrap_or("");
        match bough_plugin_gh_cli::classify(account_type, &login, &self.cfg.known_bots) {
            Actor::Bot => {}
            Actor::Human => {
                return Err(GhActionError::NotABot {
                    thread: comment.to_string(),
                    reason: bough_plugin_gh_cli::classify_reason(
                        account_type,
                        &login,
                        &self.cfg.known_bots,
                    ),
                    login,
                });
            }
        }
        let node =
            v.get("node_id")
                .and_then(|x| x.as_str())
                .ok_or_else(|| GhActionError::BadPayload {
                    kind: "bot_thread_op",
                    detail: format!("review comment {comment} carries no `node_id`"),
                })?;
        Ok(CommentNodeId(node.to_string()))
    }

    /// The GRAPHQL thread node id for a REST review-comment id. The two live in different id
    /// spaces (see [`crate::ids`]), so `resolveReviewThread` cannot be handed the number: this
    /// walks the PR's review threads and matches on the first comment's `databaseId`.
    pub async fn thread_node_id(
        &self,
        repo: &str,
        number: u64,
        comment: ReviewCommentId,
    ) -> Result<ReviewThreadNodeId, GhActionError> {
        let (owner, name) = repo
            .split_once('/')
            .ok_or_else(|| GhActionError::BadPayload {
                kind: "bot_thread_op",
                detail: format!("`{repo}` is not owner/repo"),
            })?;
        let v = self
            .gh
            .json(&[
                "api",
                "graphql",
                "-f",
                THREADS_QUERY,
                "-F",
                &format!("owner={owner}"),
                "-F",
                &format!("name={name}"),
                "-F",
                &format!("number={number}"),
            ])
            .await?;
        let nodes = v
            .pointer("/data/repository/pullRequest/reviewThreads/nodes")
            .and_then(|x| x.as_array())
            .cloned()
            .unwrap_or_default();
        for t in nodes {
            let hit = t
                .pointer("/comments/nodes")
                .and_then(|x| x.as_array())
                .map(|cs| {
                    cs.iter()
                        .any(|c| c.get("databaseId").and_then(|d| d.as_u64()) == Some(comment.0))
                })
                .unwrap_or(false);
            if hit {
                if let Some(id) = t.get("id").and_then(|x| x.as_str()) {
                    return Ok(ReviewThreadNodeId(id.to_string()));
                }
            }
        }
        Err(GhActionError::NoSuchThread {
            comment: comment.to_string(),
            target: format!("{repo}#{number}"),
        })
    }

    // ---- the three acts ------------------------------------------------------------------

    async fn open_pr(&self, req: &ExecuteRequest) -> Result<ActionArtifact, GhActionError> {
        let p: OpenPrPayload = payload(req, "open_pr")?;
        let body = marker::pr_body(&p.body, &req.marker);
        let out = self
            .gh
            .run(
                &[
                    "pr",
                    "create",
                    "--repo",
                    &req.canonical_target,
                    "--head",
                    &p.head,
                    "--base",
                    &p.base,
                    "--title",
                    &p.title,
                    "--body",
                    &body,
                ],
                None,
            )
            .await?;
        let locator = out
            .stdout
            .lines()
            .map(str::trim)
            .rfind(|l| !l.is_empty())
            .unwrap_or("")
            .to_string();
        Ok(ActionArtifact {
            locator,
            marker: req.marker.clone(),
            detail: serde_json::json!({ "head": p.head, "base": p.base }),
        })
    }

    /// The push is the PR head ref moved to the last local commit, through `gh api` (§13: no
    /// octocrab, and no second transport). The MARKER lives in that commit's trailer, so this
    /// path VERIFIES the trailer by reading the commit before it moves anything: a push whose
    /// artifact would not carry the marker is refused rather than made unreconcilable.
    async fn push_to_pr(&self, req: &ExecuteRequest) -> Result<ActionArtifact, GhActionError> {
        let p: PushToPrPayload = payload(req, "push_to_pr")?;
        let head_ref = self.pr_head_ref(&req.canonical_target).await?;
        let (repo, _n) = split_pr(&req.canonical_target)?;
        let sha = p
            .commits
            .last()
            .cloned()
            .ok_or_else(|| GhActionError::BadPayload {
                kind: "push_to_pr",
                detail: "no commits to push".into(),
            })?;
        let commit = self
            .gh
            .json(&["api", &format!("repos/{repo}/commits/{sha}")])
            .await?;
        let message = commit
            .pointer("/commit/message")
            .and_then(|x| x.as_str())
            .unwrap_or("");
        if !message.contains(&req.marker) {
            return Err(GhActionError::BadPayload {
                kind: "push_to_pr",
                detail: format!(
                    "commit {sha} carries no `{}: {}` trailer, so the push would leave no marker \
                     in the world",
                    marker::TRAILER_KEY,
                    req.marker
                ),
            });
        }
        self.gh
            .run(
                &[
                    "api",
                    "--method",
                    "PATCH",
                    &format!("repos/{repo}/git/refs/heads/{head_ref}"),
                    "-f",
                    &format!("sha={sha}"),
                ],
                None,
            )
            .await?;
        Ok(ActionArtifact {
            locator: sha,
            marker: req.marker.clone(),
            detail: serde_json::json!({ "branch": p.branch, "head_ref": head_ref }),
        })
    }

    /// Reply, resolve and close are THREE acts and each one takes a different call. Every one of
    /// them leaves the comment first, because the comment is what carries the marker: an act with
    /// no trace in the world could not be reconciled after a crash.
    async fn bot_thread_op(&self, req: &ExecuteRequest) -> Result<ActionArtifact, GhActionError> {
        let p: BotThreadPayload = payload(req, "bot_thread_op")?;
        let (repo, number) = split_pr(&req.canonical_target)?;
        let comment_node = self.check_bot_thread(&repo, p.comment_id).await?;
        let body = marker::comment_suffix(
            p.body.as_deref().unwrap_or(match p.op {
                ThreadOp::Reply => "",
                ThreadOp::Resolve => "Resolved by bough.",
                ThreadOp::Close => "Closed by bough.",
            }),
            &req.marker,
        );
        let comment = self
            .gh
            .run(
                &[
                    "api",
                    "--method",
                    "POST",
                    &format!(
                        "repos/{repo}/pulls/{number}/comments/{}/replies",
                        p.comment_id
                    ),
                    "-f",
                    &format!("body={body}"),
                ],
                None,
            )
            .await?;
        let locator = serde_json::from_str::<serde_json::Value>(&comment.stdout)
            .ok()
            .and_then(|v| {
                v.get("html_url")
                    .or_else(|| v.get("url"))
                    .and_then(|x| x.as_str())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| format!("{repo}#{number}:{}", p.comment_id));
        let mut detail = serde_json::json!({
            "op": p.op.as_str(),
            "comment_id": p.comment_id,
        });
        match p.op {
            ThreadOp::Reply => {}
            ThreadOp::Resolve => {
                let thread = self.thread_node_id(&repo, number, p.comment_id).await?;
                self.gh
                    .run(
                        &[
                            "api",
                            "graphql",
                            "-f",
                            RESOLVE_MUTATION,
                            "-F",
                            &format!("threadId={thread}"),
                        ],
                        None,
                    )
                    .await?;
                detail["thread_node_id"] = serde_json::json!(thread.0);
            }
            ThreadOp::Close => {
                self.gh
                    .run(
                        &[
                            "api",
                            "graphql",
                            "-f",
                            MINIMIZE_MUTATION,
                            "-F",
                            &format!("subjectId={comment_node}"),
                        ],
                        None,
                    )
                    .await?;
                detail["comment_node_id"] = serde_json::json!(comment_node.0);
            }
        }
        Ok(ActionArtifact {
            locator,
            marker: req.marker.clone(),
            detail,
        })
    }
}

/// The review threads of one PR, with each thread's node id beside its comments' REST database
/// ids. This is the ONLY bridge between the two id spaces (see [`crate::ids`]).
const THREADS_QUERY: &str = "query=query($owner:String!,$name:String!,$number:Int!){repository(owner:$owner,name:$name){pullRequest(number:$number){reviewThreads(first:100){nodes{id isResolved comments(first:10){nodes{databaseId}}}}}}}";

/// `Resolve`: marks the THREAD resolved. Takes a [`ReviewThreadNodeId`].
const RESOLVE_MUTATION: &str =
    "query=mutation($threadId:ID!){resolveReviewThread(input:{threadId:$threadId}){thread{id isResolved}}}";

/// `Close`: folds the bot's comment away. Takes a [`CommentNodeId`] — a different id space and a
/// different mutation from [`RESOLVE_MUTATION`], which is what keeps `close` from being `resolve`.
const MINIMIZE_MUTATION: &str =
    "query=mutation($subjectId:ID!){minimizeComment(input:{subjectId:$subjectId,classifier:RESOLVED}){minimizedComment{isMinimized}}}";

/// The payload of one request, typed. `deny_unknown_fields` is what refuses a creation-shaped
/// field on a kind that has no business with one.
fn payload<T: serde::de::DeserializeOwned>(
    req: &ExecuteRequest,
    kind: &'static str,
) -> Result<T, GhActionError> {
    serde_json::from_value(req.request.payload.clone()).map_err(|e| GhActionError::BadPayload {
        kind,
        detail: e.to_string(),
    })
}

/// `owner/repo#12` → (`owner/repo`, 12). The canonical form is `plugins/actions`', so this only
/// takes it apart.
fn split_pr(canonical: &str) -> Result<(String, u64), GhActionError> {
    let (repo, n) = canonical
        .split_once('#')
        .ok_or_else(|| GhActionError::BadPayload {
            kind: "push_to_pr",
            detail: format!("`{canonical}` names no pull request"),
        })?;
    let n = n.parse::<u64>().map_err(|_| GhActionError::BadPayload {
        kind: "push_to_pr",
        detail: format!("`{canonical}` names no pull request number"),
    })?;
    Ok((repo.to_string(), n))
}

#[async_trait::async_trait]
impl ActionProvider for GithubActions {
    fn kinds(&self) -> Vec<ActionKind> {
        vec![
            ActionKind::OpenPr,
            ActionKind::PushToPr,
            ActionKind::BotThreadOp,
        ]
    }

    async fn execute(&self, req: &ExecuteRequest) -> Result<ActionArtifact, ActionError> {
        let kind = req.request.kind;
        let out = match kind {
            ActionKind::OpenPr => self.open_pr(req).await,
            ActionKind::PushToPr => self.push_to_pr(req).await,
            ActionKind::BotThreadOp => self.bot_thread_op(req).await,
            ActionKind::LinearWrite => Err(GhActionError::BadPayload {
                kind: "linear_write",
                detail: "GitHub does not do Linear writes".into(),
            }),
        };
        out.map_err(|e| ActionError::Provider {
            kind: kind.as_str(),
            source: anyhow::Error::new(e),
        })
    }
}

/// Reconciliation's half: is this action's marker in the world? A READ, always.
#[async_trait::async_trait]
impl ArtifactLookup for GithubActions {
    fn kinds(&self) -> Vec<ActionKind> {
        ActionProvider::kinds(self)
    }

    async fn find_marker(
        &self,
        kind: ActionKind,
        canonical_target: &str,
        marker: &str,
    ) -> Result<Option<ActionArtifact>, ActionError> {
        let found = self
            .find(kind, canonical_target, marker)
            .await
            .map_err(|e| ActionError::Provider {
                kind: kind.as_str(),
                source: anyhow::Error::new(e),
            })?;
        Ok(found)
    }
}

impl GithubActions {
    async fn find(
        &self,
        kind: ActionKind,
        canonical_target: &str,
        marker: &str,
    ) -> Result<Option<ActionArtifact>, GhActionError> {
        let (path, body_ptr, locator_key) = match kind {
            ActionKind::OpenPr => (
                format!("repos/{canonical_target}/pulls?state=all&per_page=100"),
                "/body",
                "html_url",
            ),
            ActionKind::PushToPr => {
                let (repo, n) = split_pr(canonical_target)?;
                (
                    format!("repos/{repo}/pulls/{n}/commits?per_page=100"),
                    "/commit/message",
                    "sha",
                )
            }
            ActionKind::BotThreadOp => {
                let (repo, n) = split_pr(canonical_target)?;
                (
                    format!("repos/{repo}/pulls/{n}/comments?per_page=100"),
                    "/body",
                    "html_url",
                )
            }
            ActionKind::LinearWrite => return Ok(None),
        };
        let v = self.gh.json(&["api", &path]).await?;
        let items = match v.as_array() {
            Some(a) => a.clone(),
            None => return Ok(None),
        };
        for item in items {
            let text = item
                .pointer(body_ptr)
                .and_then(|x| x.as_str())
                .unwrap_or("");
            if text.contains(marker) {
                let locator = item
                    .get(locator_key)
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string();
                return Ok(Some(ActionArtifact {
                    locator,
                    marker: marker.to_string(),
                    detail: serde_json::json!({ "found_by": "marker", "kind": kind.as_str() }),
                }));
            }
        }
        Ok(None)
    }
}

/// Pre-flight refusals, each a lookup against the world before anything is written.
#[derive(Debug, thiserror::Error)]
pub enum GhActionError {
    #[error("push_to_pr refused: {target} is authored by `{author}`, not `{me}` (§7: never teammates' branches)")]
    NotAuthored {
        target: String,
        author: String,
        me: String,
    },
    #[error("push_to_pr refused: {target} is {state}, not open")]
    NotOpen { target: String, state: String },
    #[error("bot_thread_op refused: {thread} was opened by `{login}` ({reason}); human threads are never auto-resolved")]
    NotABot {
        thread: String,
        login: String,
        reason: &'static str,
    },
    #[error("bot_thread_op refused: no review thread on {target} opens with comment {comment}")]
    NoSuchThread { comment: String, target: String },
    #[error("payload for `{kind}` is not what §7 sanctions: {detail}")]
    BadPayload { kind: &'static str, detail: String },
    #[error(transparent)]
    Gh(#[from] bough_plugin_gh_cli::GhError),
}

/// The row.
pub struct GithubActionsPlugin;

#[async_trait::async_trait]
impl Plugin for GithubActionsPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = GithubActionsConfig;

    fn inject() -> Inject {
        // `ledger`: the runtime invariant folds this row's own action rows, so the read has to be
        // declared (the actions seam already requires the ledger, so this never widens a tree).
        // `action_lookup` is OPTIONAL: the reconciler row may be absent from a tree, and the three
        // kinds still exist without it. Its presence is what makes reconciliation a lookup.
        Inject::required(["actions", "ledger"]).union(&Inject::optional(["action_lookup"]))
    }

    fn validate(cfg: &Self::Config) -> Result<(), ConfigError> {
        if cfg.gh_bin.trim().is_empty() {
            return Err(ConfigError::Rejected {
                detail: "gh_bin: name the `gh` binary (§13: `gh` is the GitHub transport)".into(),
            });
        }
        if cfg.timeout_ms == 0 {
            return Err(ConfigError::Rejected {
                detail: "timeout_ms: 0 would make every `gh` call fail immediately".into(),
            });
        }
        Ok(())
    }

    /// Register the Provider on `ctx.actions` as an effect, and its [`ArtifactLookup`] half on
    /// `actions-reconcile`'s registry when that row is in the tree.
    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let entry = ctx.entry_id().clone();
        let actions = ctx
            .get::<Actions>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;
        let provider = GithubActions::open(cfg)
            .await
            .map_err(|e| PluginError::new(entry.clone(), anyhow::Error::new(e)))?;
        ActionsHandle::provider(&actions, &ctx, provider.clone() as Arc<dyn ActionProvider>)
            .await?;
        if let Ok(reg) = ctx.get::<ActionLookup>() {
            reg.register(&ctx, provider as Arc<dyn ArtifactLookup>)
                .await?;
        } else {
            tracing::warn!(
                "actions-github: no `action_lookup` in the tree, so an interrupted act cannot be \
                 reconciled by lookup"
            );
        }
        Ok(())
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

bough_kernel::register_plugin!(GithubActionsPlugin);
