//! Invariant: the API KEY NEVER APPEARS anywhere a human or a log can read it. `Debug`, the sweep
//! report, every error string and `--dump-config` render it as `<redacted>`; the row records only
//! that it resolved (P6-D7). A MISSING key disables the row's sources LOUDLY — a `disabled` entry
//! every sweep — and does not fail the boot: a machine without a Linear key must still boot.
//!
//! The sweep order is the same as `collector-github`'s and for the same reason: ref-guard,
//! deliver, then watermark.

pub mod graphql;
pub mod invariant;

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use bough_kernel::{ConfigError, Context, Inject, InvariantSpec, Plugin, PluginError};
use bough_plugin_agents::{Agent, Agents, AgentsHandle};
use bough_plugin_collect_core::{
    already_delivered, delivery_of, CollectError, Collected, SweepReport, WakeClass, Watermark,
    WatermarkStore,
};
use bough_plugin_ledger::{AgentName, Ledger, LedgerHandle, TrajId};
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
static ACTIVE_KEYS: parking_lot::Mutex<BTreeSet<String>> = parking_lot::Mutex::new(BTreeSet::new());

/// The secrets this tree's live Linear rows hold. Used by `invariant.rs` only.
pub fn active_keys() -> Vec<String> {
    ACTIVE_KEYS.lock().iter().cloned().collect()
}

/// The row's config.
#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LinearCollectorConfig {
    pub cadence: Cadence,
    /// The GraphQL endpoint. A config field, not a constant, because the test stub is a local URL.
    pub endpoint: String,
    /// `!!expr 'env("LINEAR_API_KEY")'`. NEVER logged, never in an error, never in `--dump-config`.
    pub api_key: String,
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
        if !cfg.api_key.trim().is_empty() {
            ACTIVE_KEYS.lock().insert(cfg.api_key.clone());
        }
        Ok(LinearCollector {
            cfg,
            http,
            ledger,
            agents,
            state,
            last: parking_lot::Mutex::new(empty_report(None)),
        })
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
        if self.cfg.api_key.trim().is_empty() {
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
        let targets = self.targets(&mut report).await?;
        if targets.is_empty() {
            *self.last.lock() = report.clone();
            return Ok(report);
        }

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
        let (mut items, cursor) = self.fetch(source, mark.cursor.as_deref()).await?;
        items.retain(|c| c.order > mark.last_row);
        items.sort_by_key(|c| c.order);
        items.truncate(self.cfg.batch);

        let mut delivered = 0usize;
        let mut skipped = 0usize;
        let mut high = mark.last_row;
        let mut last_at = mark.last_at;
        for item in &items {
            for target in targets {
                // THE REF GUARD, BEFORE the delivery.
                if already_delivered(&self.ledger, &target.traj, &item.r#ref).await? {
                    skipped += 1;
                    continue;
                }
                target.agent.deliver(delivery_of(item, PLUGIN_NAME)).await?;
                delivered += 1;
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
            "variables": { "first": self.cfg.batch, "after": after },
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
        let class = if self.cfg.wake_classes.contains(&kind) {
            bough_plugin_agents::MailClass::Wake
        } else {
            bough_plugin_agents::MailClass::Ordinary
        };
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
        Inject::required(["schedule", "agents", "ledger"])
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
        if cfg.deliver_to.is_empty() {
            return reject("deliver_to must name at least one agent".to_string());
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
        let collector = Arc::new(
            LinearCollector::open(cfg, LedgerHandle(ledger.0.clone()), (*agents).clone())
                .map_err(|e| PluginError::new(entry.clone(), e))?,
        );

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
