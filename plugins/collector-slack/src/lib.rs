//! Invariant: THIS ROW HOLDS NO CREDENTIAL. Every Slack byte arrives through the mcp seam, whose
//! server row owns the token (a `${keychain:…}` reference, resolved at connect time by
//! `mcp-rmcp`); there is no key field to redact because there is no key. A missing server row or
//! an empty `queries` map disables the row's sources LOUDLY, a `disabled` entry every sweep, and
//! never fails the boot (§0.2).
//!
//! The sweep order is `collector-linear`'s and for the same reason: ref-guard, deliver, then
//! watermark. Each configured query is one SOURCE with its own watermark; the query text is what
//! makes the items directed at the viewer (`to:me`, a mention search), so the class stamped from
//! `wake_classes` is only as true as the query the deployment wrote — the same contract as
//! naming a repo on `collect.github`.

pub mod invariant;
pub mod parse;

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
pub const PLUGIN_NAME: &str = "collector-slack";

/// The row's config.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SlackCollectorConfig {
    pub cadence: Cadence,
    /// The name of the `mcp.rmcp` server row this collector sweeps through (e.g. `slack`).
    pub mcp_server: String,
    /// Source name → Slack search query (`to:me`, `is:dm`, …). EMPTY BY DEFAULT, and the row
    /// says so every sweep: the query is the scope, and a shipped default may not read anybody's
    /// workspace by omission.
    pub queries: BTreeMap<String, String>,
    pub deliver_to: Vec<String>,
    pub wake_classes: Vec<WakeClass>,
    pub state_db: PathBuf,
    pub batch: usize,
}

/// The live collector.
pub struct SlackCollector {
    cfg: Arc<SlackCollectorConfig>,
    ledger: LedgerHandle,
    agents: AgentsHandle,
    /// The router, when one is in the tree. `None` ⇒ the `deliver_to` fallback.
    mail: Option<MailHandle>,
    mcp: McpHandle,
    server: ServerName,
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

impl SlackCollector {
    /// Open the watermark store.
    pub fn open(
        cfg: Arc<SlackCollectorConfig>,
        ledger: LedgerHandle,
        agents: AgentsHandle,
        mcp: McpHandle,
    ) -> Result<SlackCollector, CollectError> {
        let state = WatermarkStore::open(&cfg.state_db)?;
        let server = ServerName::new(cfg.mcp_server.trim());
        Ok(SlackCollector {
            cfg,
            ledger,
            agents,
            mail: None,
            mcp,
            server,
            state,
            last: parking_lot::Mutex::new(empty_report(None)),
        })
    }

    /// The same, routing through `mail-router` instead of the `deliver_to` fallback.
    pub fn with_mail(mut self, mail: MailHandle) -> SlackCollector {
        self.mail = Some(mail);
        self
    }

    /// One sweep with its clock injected.
    pub async fn sweep_at(&self, now: DateTime<Utc>) -> Result<SweepReport, CollectError> {
        let mut report = empty_report(Some(now));
        if self.cfg.queries.is_empty() {
            // LOUD, every sweep, and never a boot failure, the `collect.github` `repos: []`
            // shape: an unscoped row may not read the workspace by omission.
            tracing::warn!(
                target: "collector-slack",
                "`queries` is empty: this row collects nothing"
            );
            report.disabled.push((
                "queries".to_string(),
                "`queries` is empty; this row collects nothing".to_string(),
            ));
            *self.last.lock() = report.clone();
            return Ok(report);
        }
        let targets = match self.mail {
            Some(_) => Vec::new(),
            None => {
                if self.cfg.deliver_to.is_empty() {
                    // NEVER A SILENT SKIP (§0.2): neither a router nor a list, so this sweep has
                    // nowhere to put what it would collect.
                    tracing::warn!(
                        target: "collector-slack",
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

        for (source, query) in &self.cfg.queries {
            match self.sweep_source(source, query, &targets, now).await {
                Ok((delivered, skipped, mark)) => {
                    report
                        .sources
                        .push((source.to_string(), delivered, skipped, mark))
                }
                Err(e) => {
                    tracing::warn!(
                        target: "collector-slack",
                        source = %source, error = %e,
                        "source failed this sweep; the others are unaffected"
                    );
                    report.disabled.push((source.to_string(), e.to_string()));
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
                        target: "collector-slack",
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
        query: &str,
        targets: &[Target],
        now: DateTime<Utc>,
    ) -> Result<(usize, usize, i64), CollectError> {
        let mark = self.state.get(source)?;
        let mut items = self.fetch(query, &mark).await?;
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
                        .map_err(|e| CollectError::Mail(e.to_string()))?;
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
                // No cursor: the watermark (in the `after` argument) is the pagination.
                cursor: None,
            },
            now,
        )?;
        Ok((delivered, skipped, high))
    }

    /// One search through the mcp seam, parsed. The watermark rides the `after` argument in
    /// epoch seconds (`order` is that ts in microseconds).
    async fn fetch(&self, query: &str, mark: &Watermark) -> Result<Vec<Collected>, CollectError> {
        let after_secs = (mark.last_row > 0).then(|| mark.last_row.div_euclid(1_000_000));
        let result = self
            .mcp
            .call(
                &McpToolRef {
                    server: self.server.clone(),
                    tool: parse::SEARCH_TOOL.to_string(),
                },
                parse::search_args(query, after_secs, self.cfg.batch),
            )
            .await
            .map_err(|e| CollectError::Transport(e.to_string()))?;
        if result.is_error {
            return Err(CollectError::Transport(format!(
                "`{}` answered an error: {}",
                parse::SEARCH_TOOL,
                result.content
            )));
        }
        let class = if self.cfg.wake_classes.contains(&WakeClass::Mention) {
            bough_plugin_agents::MailClass::Wake
        } else {
            bough_plugin_agents::MailClass::Ordinary
        };
        Ok(parse::messages_of(&parse::results_text(&result))
            .map_err(CollectError::Transport)?
            .into_iter()
            .map(|mut c| {
                c.class = class;
                c
            })
            .collect())
    }

    /// What the last sweep did.
    pub fn status(&self) -> SweepReport {
        self.last.lock().clone()
    }
}

/// The scheduled body of this row: ONE job, whose every fire is one sweep.
struct SweepJob {
    collector: Arc<SlackCollector>,
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
                error: e.to_string(),
            },
        }
    }
}

/// The row.
pub struct SlackCollectorPlugin;

#[async_trait::async_trait]
impl Plugin for SlackCollectorPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = SlackCollectorConfig;

    fn inject() -> Inject {
        // `mcp` is REQUIRED: this collector has no other wire. `mail` is optional the way it is
        // on the other two collectors (§0.3).
        Inject::required(["schedule", "agents", "ledger", "mcp"]).union(&Inject::optional(["mail"]))
    }

    fn validate(cfg: &Self::Config) -> Result<(), ConfigError> {
        let reject = |detail: String| Err(ConfigError::Rejected { detail });
        cfg.cadence.check().map_err(|e| ConfigError::Rejected {
            detail: e.to_string(),
        })?;
        if cfg.mcp_server.trim().is_empty() {
            return reject("`mcp_server` must name an mcp.rmcp server row".to_string());
        }
        if cfg.batch == 0 {
            return reject("batch must be > 0".to_string());
        }
        if let Some(name) = cfg
            .queries
            .iter()
            .find(|(_, q)| q.trim().is_empty())
            .map(|(n, _)| n)
        {
            return reject(format!("query `{name}` is empty"));
        }
        // An EMPTY `queries` map is not a config error: the row activates and reports every
        // sweep, the `collect.github` `repos: []` shape.
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
        let mcp = ctx
            .get::<Mcp>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;

        let cadence = cfg.cadence.clone();
        let mut collector = SlackCollector::open(
            cfg,
            LedgerHandle(ledger.0.clone()),
            (*agents).clone(),
            (*mcp).clone(),
        )
        .map_err(|e| PluginError::new(entry.clone(), e))?;
        if let Ok(mail) = ctx.get::<Mail>() {
            collector = collector.with_mail((*mail).clone());
        }
        let collector = Arc::new(collector);

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

bough_kernel::register_plugin!(SlackCollectorPlugin);
