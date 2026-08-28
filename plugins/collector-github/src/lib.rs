//! Invariant: a sweep is REF-GUARDED THEN WATERMARKED, in that order, exactly as
//! `old-feed-adapter` does it, so a restart re-sweep delivers nothing twice. Everything it
//! delivers is EVIDENCE and carries its `gh:` ref, so Phase 5's `mail-router` can route on refs
//! without this row changing.
//!
//! A `deliver_to` naming an agent that does not exist is reported EVERY sweep — a `disabled` entry
//! and a `tracing::warn!` — and never silently skipped (§0.2).
//!
//! A source that fails (an unreachable `gh`, an unparseable payload) fails THAT SOURCE ONLY: it
//! becomes a `disabled` entry on this sweep's report and every other source keeps sweeping.

pub mod invariant;
pub mod sweep;

use std::path::PathBuf;
use std::sync::Arc;

use bough_kernel::{ConfigError, Context, Inject, InvariantSpec, Plugin, PluginError};
use bough_plugin_agents::{Agent, Agents, AgentsHandle};
use bough_plugin_collect_core::{
    already_delivered, delivery_of, envelope_of, CollectError, Collected, SweepReport, WakeClass,
    Watermark, WatermarkStore,
};
use bough_plugin_gh_cli::Gh;
use bough_plugin_ledger::{AgentName, Ledger, LedgerHandle, TrajId};
use bough_plugin_mail_router::{Mail, MailHandle};
use bough_plugin_schedule::{
    Cadence, Job, JobFire, JobName, JobOutcome, JobSpec, Schedule, ScheduleHandle,
};
use chrono::{DateTime, Utc};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "collector-github";

/// The row's config.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GithubCollectorConfig {
    pub cadence: Cadence,
    /// `"gh"`. A config field, not a constant, because the tests put a recording shim here.
    pub gh_bin: String,
    /// `"owner/repo"`.
    pub repos: Vec<String>,
    /// Which sweeps run. Each is a SOURCE with its own watermark.
    pub prs: bool,
    pub review_requests: bool,
    pub mentions: bool,
    pub checks: bool,
    /// Agent names — the FALLBACK destination, for a tree with no `mail` seam. MERGE (track B →
    /// Phase 5): with `mail-router` in the tree the ROUTER chooses recipients from the refs this
    /// row cites, and `deliver_to` is not consulted at all. Empty is the shipped setting.
    pub deliver_to: Vec<String>,
    pub wake_classes: Vec<WakeClass>,
    /// `"dependabot[bot]"`, `"github-actions[bot]"`, … Feeds `gh_cli::classify`.
    pub known_bots: Vec<String>,
    pub state_db: PathBuf,
    pub batch: usize,
    pub timeout_ms: u64,
}

/// The live collector.
pub struct GithubCollector {
    cfg: Arc<GithubCollectorConfig>,
    gh: Gh,
    ledger: LedgerHandle,
    agents: AgentsHandle,
    /// The router, when one is in the tree. `None` ⇒ the `deliver_to` fallback.
    mail: Option<MailHandle>,
    state: WatermarkStore,
    last: parking_lot::Mutex<SweepReport>,
}

/// PURE: one source's parser. A named type so the source table below reads as a table.
type ParseFn = fn(&str, &serde_json::Value) -> Option<Collected>;

/// One delivery target: a live agent and the trajectory its mail lands on.
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

impl GithubCollector {
    /// Open the watermark store and build the `gh` invoker.
    pub fn open(
        cfg: Arc<GithubCollectorConfig>,
        ledger: LedgerHandle,
        agents: AgentsHandle,
    ) -> Result<GithubCollector, CollectError> {
        let state = WatermarkStore::open(&cfg.state_db)?;
        let gh = Gh::new(
            cfg.gh_bin.clone(),
            std::time::Duration::from_millis(cfg.timeout_ms),
        );
        Ok(GithubCollector {
            cfg,
            gh,
            ledger,
            agents,
            mail: None,
            state,
            last: parking_lot::Mutex::new(empty_report(None)),
        })
    }

    /// The same, routing through `mail-router` instead of the `deliver_to` fallback.
    pub fn with_mail(mut self, mail: MailHandle) -> GithubCollector {
        self.mail = Some(mail);
        self
    }

    /// The same, with the `gh` invoker's environment set (the tests' recording shim).
    pub fn with_gh_env(mut self, env: Vec<(String, String)>) -> GithubCollector {
        self.gh = self.gh.clone().with_env(env);
        self
    }

    /// One sweep with its clock injected (AGENTS.md: `now` is passed in).
    pub async fn sweep_at(&self, now: DateTime<Utc>) -> Result<SweepReport, CollectError> {
        let mut report = empty_report(Some(now));
        if self.cfg.repos.is_empty() {
            // LOUD, every sweep, and never a boot failure — the same shape as an absent
            // `deliver_to` agent. A row that fires every five minutes and reports
            // `0 delivered from 0 sources` reads as working; it is not.
            tracing::warn!(
                target: "collector-github",
                "`repos` is empty: this row collects nothing"
            );
            report.disabled.push((
                "repos".to_string(),
                "`repos` is empty; this row collects nothing".to_string(),
            ));
            *self.last.lock() = report.clone();
            return Ok(report);
        }
        // MERGE (track B → Phase 5): with a router in the tree the destination is whatever the
        // refs match, decided per item at delivery time — so there is nothing to resolve here and
        // an empty `deliver_to` is the shipped setting rather than a misconfiguration.
        let targets = match self.mail {
            Some(_) => Vec::new(),
            None => {
                if self.cfg.deliver_to.is_empty() {
                    // NEVER A SILENT SKIP (§0.2): neither a router nor a list, so this sweep has
                    // nowhere to put what it would collect. Loud, every sweep, and no `gh` call.
                    tracing::warn!(
                        target: "collector-github",
                        "no `mail` seam and an empty `deliver_to`: this row has nowhere to deliver"
                    );
                    report.disabled.push((
                        "deliver_to".to_string(),
                        "no `mail` seam and an empty `deliver_to`: nowhere to deliver".to_string(),
                    ));
                }
                let targets = self.targets(&mut report).await?;
                if targets.is_empty() {
                    // Nothing to deliver to, so nothing is fetched either: a sweep with no
                    // destination must not spend a `gh` call. The `disabled` entries say so,
                    // every sweep.
                    *self.last.lock() = report.clone();
                    return Ok(report);
                }
                targets
            }
        };

        for repo in &self.cfg.repos {
            for source in sweep::SOURCES {
                if !self.enabled(source) {
                    continue;
                }
                let key = format!("{source}:{repo}");
                match self.sweep_source(source, repo, &targets, now).await {
                    Ok((delivered, skipped, mark)) => {
                        report.sources.push((key, delivered, skipped, mark))
                    }
                    Err(e) => {
                        // ONE source's failure is not the sweep's failure (§0.2: reported, never
                        // silent).
                        tracing::warn!(
                            target: "collector-github",
                            source = %key, error = %e,
                            "source failed this sweep; the others are unaffected"
                        );
                        report.disabled.push((key, e.to_string()));
                    }
                }
            }
        }
        *self.last.lock() = report.clone();
        Ok(report)
    }

    fn enabled(&self, source: &str) -> bool {
        match source {
            "prs" => self.cfg.prs,
            "review_requests" => self.cfg.review_requests,
            "mentions" => self.cfg.mentions,
            "checks" => self.cfg.checks,
            _ => false,
        }
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
                    // NEVER SILENTLY SKIP A MISSING REFERENT (§0.2).
                    tracing::warn!(
                        target: "collector-github",
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

    /// One (source, repo): read a bounded batch from the watermark, ref-guard each item, deliver,
    /// THEN write the watermark. Returns `(delivered, skipped_as_duplicate, watermark)`.
    async fn sweep_source(
        &self,
        source: &str,
        repo: &str,
        targets: &[Target],
        now: DateTime<Utc>,
    ) -> Result<(usize, usize, i64), CollectError> {
        let key = format!("{source}:{repo}");
        let mark = self.state.get(&key)?;
        let mut items = self.fetch(source, repo).await?;
        items.retain(|c| c.order > mark.last_row);
        items.sort_by_key(|c| c.order);
        items.truncate(self.cfg.batch);

        let mut delivered = 0usize;
        let mut skipped = 0usize;
        let mut high = mark.last_row;
        let mut last_at = mark.last_at;
        for item in &items {
            match &self.mail {
                // THE ROUTER DELIVERS. The collector appends cited mail and names the dedupe key;
                // who receives it is the router's decision, made per item on the refs.
                Some(mail) => {
                    let report = mail
                        .route(envelope_of(item, PLUGIN_NAME))
                        .await
                        .map_err(|e| CollectError::Mail(e.to_string()))?;
                    delivered += report.delivered.len();
                    skipped += report.deduped.len();
                }
                // The FALLBACK: no router in the tree, so the row's own `deliver_to` list is the
                // destination and the guard is applied here.
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
            &key,
            Watermark {
                last_row: high,
                last_at,
                cursor: None,
            },
            now,
        )?;
        Ok((delivered, skipped, high))
    }

    /// The one place a source's `gh` call and its parser meet.
    async fn fetch(&self, source: &str, repo: &str) -> Result<Vec<Collected>, CollectError> {
        let transport = |e: bough_plugin_gh_cli::GhError| CollectError::Transport(e.to_string());
        let items = match source {
            "prs" => {
                let rows = self
                    .gh
                    .pr_list(repo, &sweep::PR_FIELDS, self.cfg.batch)
                    .await
                    .map_err(transport)?;
                rows.iter().filter_map(|r| sweep::pr_of(repo, r)).collect()
            }
            "checks" => {
                let rows = self
                    .gh
                    .pr_list(repo, &sweep::CHECK_FIELDS, self.cfg.batch)
                    .await
                    .map_err(transport)?;
                rows.iter()
                    .filter_map(|r| sweep::check_of(repo, r))
                    .collect()
            }
            "review_requests" | "mentions" => {
                let q = sweep::search_query(source, repo);
                let value = self
                    .gh
                    .api("search/issues", &[("q", q.as_str())])
                    .await
                    .map_err(transport)?;
                let rows = value
                    .get("items")
                    .and_then(|i| i.as_array())
                    .ok_or_else(|| {
                        CollectError::Transport(format!(
                            "`gh api search/issues` for {repo} has no `items` array"
                        ))
                    })?;
                let (kind, parse): (WakeClass, ParseFn) = if source == "review_requests" {
                    (WakeClass::ReviewRequest, sweep::review_request_of)
                } else {
                    (WakeClass::Mention, sweep::mention_of)
                };
                rows.iter()
                    .filter_map(|r| {
                        let author = sweep::author_of(r, &self.cfg.known_bots);
                        parse(repo, r).map(|mut c| {
                            c.class = sweep::class_of(kind, &self.cfg.wake_classes, author);
                            c
                        })
                    })
                    .collect()
            }
            other => {
                return Err(CollectError::Transport(format!("unknown source `{other}`")));
            }
        };
        Ok(items)
    }

    /// What the last sweep did.
    pub fn status(&self) -> SweepReport {
        self.last.lock().clone()
    }
}

/// The scheduled body of this row: ONE job, whose every fire is one sweep.
struct SweepJob {
    collector: Arc<GithubCollector>,
}

#[async_trait::async_trait]
impl Job for SweepJob {
    async fn run(&self, fire: JobFire) -> JobOutcome {
        match self.collector.sweep_at(fire.at).await {
            Ok(report) => {
                let delivered: usize = report.sources.iter().map(|(_, d, _, _)| d).sum();
                if report.sources.is_empty() && !report.disabled.is_empty() {
                    // Nothing could be swept, and it is not this row's failure: say so and try
                    // again next cadence (P6-D2).
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
pub struct GithubCollectorPlugin;

#[async_trait::async_trait]
impl Plugin for GithubCollectorPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = GithubCollectorConfig;

    fn inject() -> Inject {
        // `mail` is OPTIONAL and DECLARED: with a router in the tree it delivers, and without one
        // the row falls back to its own `deliver_to` list (§0.3 — an undeclared read is a
        // capability escape, so the optional read is declared even though it may be absent).
        Inject::required(["schedule", "agents", "ledger"]).union(&Inject::optional(["mail"]))
    }

    fn validate(cfg: &Self::Config) -> Result<(), ConfigError> {
        let reject = |detail: String| Err(ConfigError::Rejected { detail });
        cfg.cadence.check().map_err(|e| ConfigError::Rejected {
            detail: e.to_string(),
        })?;
        if cfg.batch == 0 {
            return reject("batch must be > 0".to_string());
        }
        if cfg.timeout_ms == 0 {
            return reject("timeout_ms must be > 0".to_string());
        }
        if cfg.gh_bin.trim().is_empty() {
            return reject("gh_bin must name the `gh` binary".to_string());
        }
        for repo in &cfg.repos {
            if repo.split('/').count() != 2 || repo.split('/').any(|p| p.trim().is_empty()) {
                return reject(format!("`{repo}` is not an `owner/repo`"));
            }
        }
        Ok(())
    }

    /// Build the handle and register ONE `JobSpec { name: "collector-github", catch_up: true }`
    /// on `ctx.schedule` as an effect. Disabling the row unloads the fiber, which unwinds the
    /// registration, which removes the job — the SWAP bullet, with no code of its own.
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
        let mut collector =
            GithubCollector::open(cfg, LedgerHandle(ledger.0.clone()), (*agents).clone())
                .map_err(|e| PluginError::new(entry.clone(), e))?;
        // MERGE (track B -> Phase 5): "neither a router nor a list" is reported by the SWEEP, not
        // here. `mail` is optional and the committed view is captured at apply time, so a row that
        // applies in the window before `mail` is bound sees none — and then reloads and sees it.
        // Warning here fired on almost every boot of the shipped tree and was wrong every time.
        // The sweep reads the FINAL view, reports every five minutes, and spends no `gh` call.
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

bough_kernel::register_plugin!(GithubCollectorPlugin);
