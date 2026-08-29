//! Invariant: the API KEY NEVER APPEARS anywhere a human or a log can read it. `Debug`, the sweep
//! report, every error string and `--dump-config` render it as `<redacted>`; the row records only
//! that it resolved (P6-D7). A MISSING key disables the row's sources LOUDLY — a `disabled` entry
//! every sweep — and does not fail the boot: a machine without a Linear key must still boot.
//!
//! The sweep order is the same as `collector-github`'s and for the same reason: ref-guard,
//! deliver, then watermark.

pub mod graphql;
pub mod invariant;
pub mod mcp_source;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use bough_kernel::{ConfigError, Context, Inject, InvariantSpec, Plugin, PluginError};
use bough_plugin_agents::{Agent, Agents, AgentsHandle};
use bough_plugin_collect_core::{
    already_delivered, delivery_of, envelope_of, CollectError, Collected, SweepReport, WakeClass,
    Watermark, WatermarkStore,
};
use bough_plugin_ledger::{AgentName, Ledger, LedgerHandle, TrajId};
use bough_plugin_mail_router::{Mail, MailHandle};
use bough_plugin_mcp::{Mcp, McpHandle, McpToolRef, ServerName};
use bough_plugin_schedule::{
    Cadence, Job, JobFire, JobName, JobOutcome, JobSpec, Schedule, ScheduleHandle,
};
use chrono::{DateTime, Utc};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "collector-linear";

/// What a redacted secret renders as, everywhere, in one place.
pub const REDACTED: &str = "<redacted>";

/// The keys live rows activated with. The invariant's `check` is a fn pointer that captures
/// nothing, so this is how it knows what to scan the ledger FOR — the value is never rendered,
/// logged or returned anywhere else.
static ACTIVE_KEYS: parking_lot::Mutex<Option<BTreeMap<String, usize>>> =
    parking_lot::Mutex::new(None);

/// The secrets this tree's live Linear rows hold. Used by `invariant.rs` only.
pub fn active_keys() -> Vec<String> {
    ACTIVE_KEYS
        .lock()
        .as_ref()
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default()
}

/// Take a reference on `key` for a row that is activating. REFCOUNTED, because two rows in one
/// process may legitimately hold the same key and the first to unload must not blind the other's
/// invariant.
pub fn hold_key(key: &str) {
    if key.trim().is_empty() {
        return;
    }
    *ACTIVE_KEYS
        .lock()
        .get_or_insert_with(BTreeMap::new)
        .entry(key.to_string())
        .or_insert(0) += 1;
}

/// Release it. The LAST holder's release takes the secret out of process memory, which is what
/// makes "unloading a row leaves no trace" true of the credential too.
pub fn release_key(key: &str) {
    if key.trim().is_empty() {
        return;
    }
    let mut slot = ACTIVE_KEYS.lock();
    let Some(map) = slot.as_mut() else { return };
    if let Some(n) = map.get_mut(key) {
        *n = n.saturating_sub(1);
        if *n == 0 {
            map.remove(key);
        }
    }
}

/// The row's config.
#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LinearCollectorConfig {
    pub cadence: Cadence,
    /// The GraphQL endpoint. A config field, not a constant, because the test stub is a local URL.
    pub endpoint: String,
    /// `!!expr 'env("LINEAR_API_KEY")'`. NEVER logged, never in an error, never in `--dump-config`.
    /// Unused (and may stay empty) when `mcp_server` is set.
    pub api_key: String,
    /// The name of an `mcp.rmcp` server row to sweep THROUGH instead of the GraphQL endpoint
    /// (e.g. `linear-server`, carrying Claude Code's OAuth grant as a `${keychain:…}` header).
    /// Empty = the GraphQL transport above. When set, `api_key` is not needed: the credential
    /// belongs to the server row and never enters this process's config at all.
    #[serde(default)]
    pub mcp_server: String,
    /// `"TEAM"`.
    pub teams: Vec<String>,
    pub projects: Vec<String>,
    pub deliver_to: Vec<String>,
    pub wake_classes: Vec<WakeClass>,
    pub state_db: PathBuf,
    pub batch: usize,
    pub timeout_ms: u64,
}

impl std::fmt::Debug for LinearCollectorConfig {
    /// The redaction, at the type, so no call site has to remember it (P6-D7).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LinearCollectorConfig")
            .field("cadence", &self.cadence)
            .field("endpoint", &self.endpoint)
            .field("api_key", &REDACTED)
            .field("mcp_server", &self.mcp_server)
            .field("teams", &self.teams)
            .field("projects", &self.projects)
            .field("deliver_to", &self.deliver_to)
            .field("wake_classes", &self.wake_classes)
            .field("state_db", &self.state_db)
            .field("batch", &self.batch)
            .field("timeout_ms", &self.timeout_ms)
            .finish()
    }
}

/// The live collector.
pub struct LinearCollector {
    cfg: Arc<LinearCollectorConfig>,
    http: reqwest::Client,
    ledger: LedgerHandle,
    agents: AgentsHandle,
    /// The router, when one is in the tree. `None` ⇒ the `deliver_to` fallback.
    mail: Option<MailHandle>,
    /// The mcp seam and the server row to sweep through, when `mcp_server` is set.
    /// `None` ⇒ the GraphQL transport.
    mcp: Option<(McpHandle, ServerName)>,
    state: WatermarkStore,
    last: parking_lot::Mutex<SweepReport>,
}

struct Target {
    agent: Agent,
    traj: TrajId,
}

fn empty_report(now: Option<DateTime<Utc>>) -> SweepReport {
    SweepReport {
        collector: PLUGIN_NAME,
        sources: Vec::new(),
        disabled: Vec::new(),
        last_sweep: now,
    }
}

impl LinearCollector {
    /// Open the watermark store and build the HTTP client.
    pub fn open(
        cfg: Arc<LinearCollectorConfig>,
        ledger: LedgerHandle,
        agents: AgentsHandle,
    ) -> Result<LinearCollector, CollectError> {
        let state = WatermarkStore::open(&cfg.state_db)?;
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(cfg.timeout_ms))
            .build()
            .map_err(|e| CollectError::Transport(e.to_string()))?;
        hold_key(&cfg.api_key);
        Ok(LinearCollector {
            cfg,
            http,
            ledger,
            agents,
            mail: None,
            mcp: None,
            state,
            last: parking_lot::Mutex::new(empty_report(None)),
        })
    }

    /// The same, routing through `mail-router` instead of the `deliver_to` fallback.
    pub fn with_mail(mut self, mail: MailHandle) -> LinearCollector {
        self.mail = Some(mail);
        self
    }

    /// The same, sweeping through the named `mcp.rmcp` server row instead of GraphQL.
    pub fn with_mcp(mut self, mcp: McpHandle, server: ServerName) -> LinearCollector {
        self.mcp = Some((mcp, server));
        self
    }

    /// PURE-ish: the one place a string that may have touched the secret is cleaned. Every error
    /// this row produces goes through it.
    fn redact(&self, text: String) -> String {
        if self.cfg.api_key.trim().is_empty() {
            return text;
        }
        text.replace(self.cfg.api_key.as_str(), REDACTED)
    }

    /// One sweep with its clock injected.
    pub async fn sweep_at(&self, now: DateTime<Utc>) -> Result<SweepReport, CollectError> {
        let mut report = empty_report(Some(now));
        if self.mcp.is_none() && self.cfg.api_key.trim().is_empty() {
            // LOUD, every sweep, and never a boot failure: a machine without a Linear key must
            // still boot (§0.2).
            tracing::warn!(
                target: "collector-linear",
                "no Linear API key resolved: every source is off"
            );
            report.disabled.push((
                "api_key".to_string(),
                "no Linear API key resolved; every source is off".to_string(),
            ));
            *self.last.lock() = report.clone();
            return Ok(report);
        }
        if self.cfg.teams.is_empty() && self.cfg.projects.is_empty() {
            // LOUD, every sweep, and never a boot failure — the same shape as the absent key. An
            // unscoped row would sweep the whole workspace and wake `deliver_to` for every
            // ticket in the org, which is not something a shipped default may do by omission.
            tracing::warn!(
                target: "collector-linear",
                "neither `teams` nor `projects` is set: this row collects nothing"
            );
            report.disabled.push((
                "scope".to_string(),
                "neither `teams` nor `projects` is set; this row collects nothing".to_string(),
            ));
            *self.last.lock() = report.clone();
            return Ok(report);
        }
        // MERGE (track B → Phase 5): with a router in the tree the destination is whatever the
        // refs match, decided per item at delivery time.
        let targets = match self.mail {
            Some(_) => Vec::new(),
            None => {
                if self.cfg.deliver_to.is_empty() {
                    // NEVER A SILENT SKIP (§0.2): neither a router nor a list, so this sweep has
                    // nowhere to put what it would collect. Loud, every sweep, and no HTTP call.
                    tracing::warn!(
                        target: "collector-linear",
                        "no `mail` seam and an empty `deliver_to`: this row has nowhere to deliver"
                    );
                    report.disabled.push((
                        "deliver_to".to_string(),
                        "no `mail` seam and an empty `deliver_to`: nowhere to deliver".to_string(),
                    ));
                }
                let targets = self.targets(&mut report).await?;
                if targets.is_empty() {
                    *self.last.lock() = report.clone();
                    return Ok(report);
                }
                targets
            }
        };

        for source in graphql::SOURCES {
            match self.sweep_source(source, &targets, now).await {
                Ok((delivered, skipped, mark)) => {
                    report
                        .sources
                        .push((source.to_string(), delivered, skipped, mark))
                }
                Err(e) => {
                    let why = self.redact(e.to_string());
                    tracing::warn!(
                        target: "collector-linear",
                        source = %source, error = %why,
                        "source failed this sweep; the others are unaffected"
                    );
                    report.disabled.push((source.to_string(), why));
                }
            }
        }
        *self.last.lock() = report.clone();
        Ok(report)
    }

    /// Every live `deliver_to` agent, with a `disabled` entry and a warning for each that is not.
    async fn targets(&self, report: &mut SweepReport) -> Result<Vec<Target>, CollectError> {
        let mut targets = Vec::new();
        for name in &self.cfg.deliver_to {
            let agent_name = AgentName::new(name);
            match self.agents.by_name(&agent_name) {
                Some(agent) => {
                    let traj = self
                        .ledger
                        .0
                        .agent(&agent_name)
                        .await?
                        .map(|row| row.traj)
                        .ok_or_else(|| CollectError::NoSuchAgent(name.clone()))?;
                    targets.push(Target { agent, traj });
                }
                None => {
                    tracing::warn!(
                        target: "collector-linear",
                        deliver_to = %name,
                        "no live agent by that name: nothing is being delivered to it"
                    );
                    report.disabled.push((
                        "deliver_to".to_string(),
                        format!("no live agent named `{name}`"),
                    ));
                }
            }
        }
        Ok(targets)
    }

    /// One source: read a bounded page from the watermark, ref-guard, deliver, THEN watermark.
    async fn sweep_source(
        &self,
        source: &str,
        targets: &[Target],
        now: DateTime<Utc>,
    ) -> Result<(usize, usize, i64), CollectError> {
        let mark = self.state.get(source)?;
        let (mut items, cursor) = match &self.mcp {
            Some(_) => (self.fetch_mcp(source, &mark).await?, None),
            None => self.fetch(source, mark.cursor.as_deref()).await?,
        };
        items.retain(|c| c.order > mark.last_row);
        items.sort_by_key(|c| c.order);
        items.truncate(self.cfg.batch);

        let mut delivered = 0usize;
        let mut skipped = 0usize;
        let mut high = mark.last_row;
        let mut last_at = mark.last_at;
        for item in &items {
            match &self.mail {
                // THE ROUTER DELIVERS: the collector appends cited mail and names the dedupe key.
                Some(mail) => {
                    let routed = mail
                        .route(envelope_of(item, PLUGIN_NAME))
                        .await
                        .map_err(|e| CollectError::Mail(self.redact(e.to_string())))?;
                    delivered += routed.delivered.len();
                    skipped += routed.deduped.len();
                }
                // The FALLBACK: no router in the tree, so `deliver_to` is the destination.
                None => {
                    for target in targets {
                        // THE REF GUARD, BEFORE the delivery.
                        if already_delivered(&self.ledger, &target.traj, &item.r#ref).await? {
                            skipped += 1;
                            continue;
                        }
                        target.agent.deliver(delivery_of(item, PLUGIN_NAME)).await?;
                        delivered += 1;
                    }
                }
            }
            high = high.max(item.order);
            last_at = Some(item.at);
        }
        // LAST, after the deliveries it covers.
        self.state.set(
            source,
            Watermark {
                last_row: high,
                last_at,
                cursor: cursor.or(mark.cursor),
            },
            now,
        )?;
        Ok((delivered, skipped, high))
    }

    /// One GraphQL POST, parsed. The key rides the `Authorization` header and NOTHING else.
    async fn fetch(
        &self,
        source: &str,
        after: Option<&str>,
    ) -> Result<(Vec<Collected>, Option<String>), CollectError> {
        let body = serde_json::json!({
            "query": graphql::query_for(source),
            "variables": {
                "first": self.cfg.batch,
                "after": after,
                // THE SCOPE. `teams` and `projects` are what a deployment sets to say which
                // slice of the workspace this row is for; before this they reached no query.
                "filter": graphql::filter_for(source, &self.cfg.teams, &self.cfg.projects),
            },
        });
        let response = self
            .http
            .post(&self.cfg.endpoint)
            .header("Authorization", self.cfg.api_key.as_str())
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| CollectError::Transport(self.redact(e.to_string())))?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|e| CollectError::Transport(self.redact(e.to_string())))?;
        if !status.is_success() {
            return Err(CollectError::Transport(
                self.redact(format!("the Linear endpoint answered {status}")),
            ));
        }
        let value: serde_json::Value = serde_json::from_str(&text).map_err(|e| {
            CollectError::Transport(self.redact(format!("unparseable payload: {e}")))
        })?;
        if let Some(errors) = value.get("errors") {
            return Err(CollectError::Transport(
                self.redact(format!("the Linear endpoint returned errors: {errors}")),
            ));
        }
        let (nodes, cursor) = graphql::page(source, &value).ok_or_else(|| {
            CollectError::Transport(format!("`{source}` payload has no `data.{source}.nodes`"))
        })?;
        let (kind, parse): (WakeClass, fn(&serde_json::Value) -> Option<Collected>) = match source {
            "issues" => (WakeClass::Assigned, graphql::issue_of),
            _ => (WakeClass::Mention, graphql::comment_of),
        };
        let class = self.class_for(kind);
        let items = nodes
            .iter()
            .filter_map(parse)
            .map(|mut c| {
                c.class = class;
                c
            })
            .collect();
        Ok((items, cursor))
    }

    /// The class a source's items carry, from the row's configured `wake_classes`.
    fn class_for(&self, kind: WakeClass) -> bough_plugin_agents::MailClass {
        if self.cfg.wake_classes.contains(&kind) {
            bough_plugin_agents::MailClass::Wake
        } else {
            bough_plugin_agents::MailClass::Ordinary
        }
    }

    /// One MCP call against the row's server, with `is_error` treated as the failure it is.
    async fn mcp_call(
        &self,
        tool: &str,
        args: serde_json::Value,
    ) -> Result<serde_json::Value, CollectError> {
        let (mcp, server) = self
            .mcp
            .as_ref()
            .expect("fetch_mcp is only reached with mcp");
        let result = mcp
            .call(
                &McpToolRef {
                    server: server.clone(),
                    tool: tool.to_string(),
                },
                args,
            )
            .await
            .map_err(|e| CollectError::Transport(self.redact(e.to_string())))?;
        if result.is_error {
            return Err(CollectError::Transport(
                self.redact(format!("`{tool}` answered an error: {}", result.content)),
            ));
        }
        mcp_source::payload(&result).map_err(CollectError::Transport)
    }

    /// The MCP transport's fetch: no cursor (the watermark is the `updatedAt` bound), the same
    /// [`Collected`] shape out. The `comments` source reads comments off the scope's updated
    /// assigned issues, because `list_comments` is per-issue (see `mcp_source`).
    async fn fetch_mcp(
        &self,
        source: &str,
        mark: &Watermark,
    ) -> Result<Vec<Collected>, CollectError> {
        let mut nodes = Vec::new();
        let mut seen_keys = Vec::new();
        for scope in mcp_source::scopes(&self.cfg.teams, &self.cfg.projects) {
            let value = self
                .mcp_call(
                    mcp_source::ISSUES_TOOL,
                    mcp_source::issues_args(&scope, mark.last_at, self.cfg.batch),
                )
                .await?;
            for node in mcp_source::nodes_of(&value, "issues").map_err(CollectError::Transport)? {
                // A team and a project can overlap; one issue is one item.
                let key = node.get("id").and_then(|v| v.as_str()).map(str::to_string);
                if let Some(key) = key {
                    if seen_keys.contains(&key) {
                        continue;
                    }
                    seen_keys.push(key);
                }
                nodes.push(node);
            }
        }
        if source == "issues" {
            let class = self.class_for(WakeClass::Assigned);
            return Ok(nodes
                .iter()
                .filter_map(mcp_source::mcp_issue_of)
                .map(|mut c| {
                    c.class = class;
                    c
                })
                .collect());
        }
        // `comments`: one bounded call per updated issue.
        nodes.truncate(self.cfg.batch);
        let class = self.class_for(WakeClass::Mention);
        let mut items = Vec::new();
        for node in &nodes {
            let Some(meta) = mcp_source::issue_meta(node) else {
                continue;
            };
            let value = self
                .mcp_call(
                    mcp_source::COMMENTS_TOOL,
                    mcp_source::comments_args(&meta.key, self.cfg.batch),
                )
                .await?;
            items.extend(
                mcp_source::nodes_of(&value, "comments")
                    .map_err(CollectError::Transport)?
                    .iter()
                    .filter_map(|n| mcp_source::mcp_comment_of(n, &meta))
                    .map(|mut c| {
                        c.class = class;
                        c
                    }),
            );
        }
        Ok(items)
    }

    /// What the last sweep did.
    pub fn status(&self) -> SweepReport {
        self.last.lock().clone()
    }
}

/// The scheduled body of this row: ONE job, whose every fire is one sweep.
struct SweepJob {
    collector: Arc<LinearCollector>,
}

#[async_trait::async_trait]
impl Job for SweepJob {
    async fn run(&self, fire: JobFire) -> JobOutcome {
        match self.collector.sweep_at(fire.at).await {
            Ok(report) => {
                let delivered: usize = report.sources.iter().map(|(_, d, _, _)| d).sum();
                if report.sources.is_empty() && !report.disabled.is_empty() {
                    return JobOutcome::Pending {
                        reason: report
                            .disabled
                            .iter()
                            .map(|(s, why)| format!("{s}: {why}"))
                            .collect::<Vec<_>>()
                            .join("; "),
                    };
                }
                JobOutcome::Ran {
                    detail: format!(
                        "{delivered} delivered from {} sources",
                        report.sources.len()
                    ),
                }
            }
            Err(e) => JobOutcome::Failed {
                error: self.collector.redact(e.to_string()),
            },
        }
    }
}

/// The row.
pub struct LinearCollectorPlugin;

#[async_trait::async_trait]
impl Plugin for LinearCollectorPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = LinearCollectorConfig;

    fn inject() -> Inject {
        // `mail` is OPTIONAL and DECLARED: with a router in the tree it delivers, and without
        // one the row falls back to its own `deliver_to` list (§0.3). `mcp` is optional the same
        // way, but a row that NAMES an `mcp_server` and finds no seam fails activation loudly.
        Inject::required(["schedule", "agents", "ledger"]).union(&Inject::optional(["mail", "mcp"]))
    }

    fn validate(cfg: &Self::Config) -> Result<(), ConfigError> {
        let reject = |detail: String| Err(ConfigError::Rejected { detail });
        cfg.cadence.check().map_err(|e| ConfigError::Rejected {
            detail: e.to_string(),
        })?;
        if reqwest::Url::parse(&cfg.endpoint).is_err() {
            return reject(format!("`{}` is not a URL", cfg.endpoint));
        }
        if cfg.batch == 0 {
            return reject("batch must be > 0".to_string());
        }
        if cfg.timeout_ms == 0 {
            return reject("timeout_ms must be > 0".to_string());
        }
        // An ABSENT key is not a config error: the row activates and reports every sweep.
        Ok(())
    }

    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let entry = ctx.entry_id().clone();
        let agents = ctx
            .get::<Agents>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;
        let ledger = ctx
            .get::<Ledger>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;
        let schedule: ScheduleHandle = (*ctx
            .get::<Schedule>()
            .map_err(|e| PluginError::new(entry.clone(), e))?)
        .clone();

        let cadence = cfg.cadence.clone();
        let key = cfg.api_key.clone();
        let mut collector =
            LinearCollector::open(cfg, LedgerHandle(ledger.0.clone()), (*agents).clone())
                .map_err(|e| PluginError::new(entry.clone(), e))?;
        // MERGE (track B -> Phase 5): reported by the SWEEP, not here. See `collector-github`.
        if let Ok(mail) = ctx.get::<Mail>() {
            collector = collector.with_mail((*mail).clone());
        }
        if !collector.cfg.mcp_server.trim().is_empty() {
            // A row that names an MCP server in a tree with no mcp seam is MISCONFIGURED, and
            // misconfiguration fails loud (§0.2): this is an activation failure, not a warning.
            let mcp = ctx
                .get::<Mcp>()
                .map_err(|e| PluginError::new(entry.clone(), e))?;
            let server = ServerName::new(collector.cfg.mcp_server.trim());
            collector = collector.with_mcp((*mcp).clone(), server);
        }
        let collector = Arc::new(collector);
        // The key `open` took a reference on comes back out with the row. Without this, disabling
        // or reloading the row left the secret in process memory and left the crate's own
        // invariant scanning the ledger for a key whose row has gone.
        ctx.effect(move |e| async move {
            e.defer(move || {
                let key = key.clone();
                async move {
                    crate::release_key(&key);
                }
            });
            Ok(())
        })
        .await?;

        schedule
            .0
            .register(
                &ctx,
                JobSpec {
                    name: JobName::new(PLUGIN_NAME),
                    cadence,
                    catch_up: true,
                    job: Arc::new(SweepJob { collector }),
                },
            )
            .await?;
        Ok(())
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

bough_kernel::register_plugin!(LinearCollectorPlugin);
