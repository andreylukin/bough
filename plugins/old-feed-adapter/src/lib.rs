//! THIS CRATE IS SCHEDULED FOR DELETION IN PHASE 6, and `disabled: true` in the bundle patch is
//! its off switch (§14).
//!
//! Invariant: delivery is AT-LEAST-ONCE WITH A REF GUARD, so a restart duplicates nothing. Each
//! batch is filtered against the ledger's existing `mail/delivered` refs, then delivered through
//! `Agent::deliver` (which writes the cited step and the splice as a pair), then watermarked — so
//! a crash between the append and the watermark write cannot duplicate: the ref guard catches it
//! on restart (V7).
//!
//! And the rule that is easiest to get wrong: `command_history` / `command_tags` are COMPETENCE
//! MEMORY exposed through a priming query. They are never mail, never a step, and never a
//! projection section in this phase (§14, §17).

pub mod boughdb;
pub mod command;
pub mod invariant;
pub mod jungler;
pub mod state;

use std::collections::{BTreeSet, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use bough_kernel::{ConfigError, Context, Inject, InvariantSpec, Plugin, PluginError, ServiceKey};
use bough_plugin_agents::{Agents, AgentsHandle, Delivery, MailClass, Sender};
use bough_plugin_ledger::{
    AgentName, Cite, Ledger, LedgerHandle, NewRollup, Ref, RollupId, RollupKind, RollupQuery, Seq,
    StepQuery, StepType, TrajId,
};
use chrono::{DateTime, Utc};
use parking_lot::Mutex;

pub use jungler::{probe, FeedProbe};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "old-feed-adapter";

/// The `prompt_ver` every interim tier-1 block this row seals is stamped with, so Phase 4's real
/// summarizer can tell its own blocks from the bridge's at a glance.
pub const PROMPT_VER: &str = "old-feed/1";

/// The ref scheme a delivered jungler event cites. The ref guard and the invariant both key on it.
pub fn event_ref(id: i64) -> Ref {
    Ref::new(format!("jungler:event:{id}"))
}

/// The deterministic rollup id an interim tier-1 block gets. Determinism IS the seal-once guard:
/// a re-sweep of the same row computes the same id and finds it already sealed (§3).
pub fn rollup_id(source: &str, id: i64) -> RollupId {
    RollupId::new(format!("old-feed:{source}:{id}"))
}

/// The `old_feed` service key.
pub struct OldFeed;

impl ServiceKey for OldFeed {
    type Value = OldFeedHandle;
    const NAME: &'static str = "old_feed";
}

/// The row's config.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OldFeedConfig {
    /// `!!expr home_path(".jungler/jungler.db")`. MAY BE ABSENT (§14, AGENTS.md).
    pub jungler_db: PathBuf,
    /// `!!expr home_path(".bough/bough.db")`. Opened READ-ONLY, always.
    pub bough_db: PathBuf,
    /// The adapter's OWN watermark store, `!!expr bough_path("old-feed.db")` (P3-D13).
    pub state_db: PathBuf,
    pub poll_ms: u64,
    pub batch: usize,
    /// Which agent receives jungler mail until Phase 5's `mail-router` exists.
    pub deliver_to: String,
    pub priming_limit: usize,
    /// Seal `nodes.summary` / `lane_story` rows as interim tier-1 rollups.
    pub tier1: bool,
}

/// The concrete handle the key's value is.
#[derive(Clone)]
pub struct OldFeedHandle(pub Arc<OldFeedInner>);

/// The adapter's live state: the two source paths, the watermark store, the last sweep.
pub struct OldFeedInner {
    cfg: Arc<OldFeedConfig>,
    ledger: LedgerHandle,
    agents: AgentsHandle,
    /// The jungler sources this deployment may read; empty when the db is absent or unreadable.
    enabled: BTreeSet<String>,
    /// Every source that is off, and why. Rendered by `/oldfeed`, logged once at activation.
    disabled: Vec<(String, String)>,
    state: state::WatermarkStore,
    last: Mutex<FeedStatus>,
}

/// One synchronous read of the old db, handed to the async half. The connection never crosses an
/// await: `rusqlite::Connection` is `Send` but not `Sync`.
#[derive(Default)]
struct Batch {
    events: Vec<jungler::EventRow>,
    nodes: Vec<jungler::NodeRow>,
    story: Vec<jungler::StoryRow>,
}

impl OldFeedHandle {
    /// Probe both old databases, open the adapter's own watermark store, and report what is
    /// usable. An absent or unreadable jungler db is NOT an error: the row activates either way
    /// (§14, V7). The one thing that CAN fail here is the adapter's own state db, which is this
    /// row's own file and therefore its own misconfiguration.
    pub fn open(
        cfg: Arc<OldFeedConfig>,
        ledger: LedgerHandle,
        agents: AgentsHandle,
    ) -> Result<OldFeedHandle, OldFeedError> {
        let probe = jungler::probe(&cfg.jungler_db);
        let (enabled, mut disabled) = jungler::sources(&probe, &cfg.jungler_db);
        if !cfg.bough_db.exists() {
            disabled.push((
                "bough.db".to_string(),
                format!("{} is absent", cfg.bough_db.display()),
            ));
        }
        let state = state::WatermarkStore::open(&cfg.state_db)?;
        Ok(OldFeedHandle(Arc::new(OldFeedInner {
            cfg,
            ledger,
            agents,
            enabled,
            disabled: disabled.clone(),
            state,
            last: Mutex::new(FeedStatus {
                sources: Vec::new(),
                disabled,
                last_sweep: None,
            }),
        })))
    }

    /// The ONE line activation logs when something is off. `None` when every source is live.
    pub fn disabled_line(&self) -> Option<String> {
        disabled_line(&self.0.disabled)
    }

    /// §14's cheap win: command memory for PRIMING. Never mail, never a step, never a
    /// projection section in this phase.
    pub async fn prime(&self, q: &PrimingQuery) -> Result<Vec<CommandMemory>, OldFeedError> {
        let limit = if q.limit == 0 {
            self.0.cfg.priming_limit
        } else {
            q.limit
        };
        let Some(conn) = boughdb::open(&self.0.cfg.bough_db)? else {
            return Ok(Vec::new());
        };
        if !boughdb::probe(&conn)?.0 {
            return Ok(Vec::new());
        }
        boughdb::prime(&conn, q, limit)
    }

    /// `note_sections` as CITED EVIDENCE: each carries `Cite { ref: "note:<note>#<ord>" }`.
    pub async fn notes(&self, q: &NoteQuery) -> Result<Vec<NoteEvidence>, OldFeedError> {
        let limit = if q.limit == 0 {
            self.0.cfg.priming_limit
        } else {
            q.limit
        };
        let Some(conn) = boughdb::open(&self.0.cfg.bough_db)? else {
            return Ok(Vec::new());
        };
        if !boughdb::probe(&conn)?.1 {
            return Ok(Vec::new());
        }
        boughdb::notes(&conn, q, limit)
    }

    /// What the last sweep did. The `/oldfeed` command renders it.
    pub fn status(&self) -> FeedStatus {
        self.0.last.lock().clone()
    }

    /// One sweep: events → mail, `nodes.summary` / `lane_story` → tier-1 rollups, watermarks
    /// advanced last. The poll loop calls it; the tests call it directly.
    pub async fn sweep(&self) -> Result<FeedStatus, OldFeedError> {
        self.sweep_at(Utc::now()).await
    }

    /// The sweep with its clock injected (AGENTS.md: `now` is passed in).
    pub async fn sweep_at(&self, now: DateTime<Utc>) -> Result<FeedStatus, OldFeedError> {
        let cfg = &self.0.cfg;
        let mut status = FeedStatus {
            sources: Vec::new(),
            disabled: self.0.disabled.clone(),
            last_sweep: Some(now),
        };
        if !self.0.enabled.is_empty() {
            let name = AgentName::new(&cfg.deliver_to);
            match self.0.agents.by_name(&name) {
                Some(agent) => {
                    // Every read of the old db happens HERE, in one synchronous block: a
                    // `rusqlite::Connection` is not `Sync`, so it may not be held across an await,
                    // and the batch is small and bounded by `batch` anyway.
                    let batch = self.read_batch()?;
                    let traj = self
                        .0
                        .ledger
                        .0
                        .agent(&name)
                        .await?
                        .map(|row| row.traj)
                        .ok_or_else(|| {
                            OldFeedError::Failed(format!("no ledger row for agent `{name}`"))
                        })?;

                    if self.0.enabled.contains(jungler::EVENTS) {
                        self.sweep_events(batch.events, &agent, now, &mut status)
                            .await?;
                    }
                    if cfg.tier1 {
                        if self.0.enabled.contains(jungler::NODES) {
                            self.sweep_nodes(batch.nodes, &traj, now, &mut status)
                                .await?;
                        }
                        if self.0.enabled.contains(jungler::LANE_STORY) {
                            self.sweep_story(batch.story, &traj, now, &mut status)
                                .await?;
                        }
                    }
                }
                None => status.disabled.push((
                    "deliver_to".to_string(),
                    format!("no live agent named `{}`", cfg.deliver_to),
                )),
            }
        }
        *self.0.last.lock() = status.clone();
        Ok(status)
    }

    /// One synchronous read of the old db: the next batch of each live source, from its watermark.
    fn read_batch(&self) -> Result<Batch, OldFeedError> {
        let cfg = &self.0.cfg;
        let conn = jungler::open(&cfg.jungler_db)?;
        let cols = jungler::column_map(&conn)?;
        let empty = BTreeSet::new();
        let mut batch = Batch::default();
        if self.0.enabled.contains(jungler::EVENTS) {
            let mark = self.0.state.get(jungler::EVENTS)?;
            batch.events = jungler::read_events(
                &conn,
                cols.get("events").unwrap_or(&empty),
                mark.last_row,
                cfg.batch,
            )?;
        }
        if cfg.tier1 && self.0.enabled.contains(jungler::NODES) {
            let mark = self.0.state.get(jungler::NODES)?;
            batch.nodes = jungler::read_nodes(
                &conn,
                cols.get("nodes").unwrap_or(&empty),
                mark.last_row,
                cfg.batch,
            )?;
        }
        if cfg.tier1 && self.0.enabled.contains(jungler::LANE_STORY) {
            let mark = self.0.state.get(jungler::LANE_STORY)?;
            batch.story = jungler::read_lane_story(
                &conn,
                cols.get("lane_story").unwrap_or(&empty),
                mark.last_row,
                cfg.batch,
            )?;
        }
        Ok(batch)
    }

    /// jungler `events` → cited mail. The ref guard runs BEFORE the delivery and the watermark
    /// AFTER it, which is the whole of the at-least-once argument.
    async fn sweep_events(
        &self,
        rows: Vec<jungler::EventRow>,
        agent: &bough_plugin_agents::Agent,
        now: DateTime<Utc>,
        status: &mut FeedStatus,
    ) -> Result<usize, OldFeedError> {
        let mut mark = self.0.state.get(jungler::EVENTS)?;
        if rows.is_empty() {
            status
                .sources
                .push((jungler::EVENTS.to_string(), 0, mark.last_row));
            return Ok(0);
        }

        // THE REF GUARD: whatever the ledger already delivered is dropped from this batch, so a
        // crash between the append and the watermark write cannot duplicate on restart.
        let refs: Vec<Ref> = rows.iter().map(|r| event_ref(r.id)).collect();
        let already = self
            .0
            .ledger
            .0
            .steps(&StepQuery {
                kinds: vec![StepType::new("mail/delivered")],
                refs: refs.clone(),
                ..Default::default()
            })
            .await?;
        let seen: HashSet<Ref> = already
            .iter()
            .flat_map(|s| s.refs.iter().cloned())
            .collect();

        let mut delivered = 0usize;
        for row in &rows {
            let r = event_ref(row.id);
            if !seen.contains(&r) {
                agent
                    .deliver(delivery_of(row, r, now))
                    .await
                    .map_err(|e| OldFeedError::Failed(e.to_string()))?;
                delivered += 1;
            }
            mark = state::Watermark {
                last_row: row.id,
                last_at: row.at,
            };
        }
        // LAST, after the deliveries it covers.
        self.0.state.set(jungler::EVENTS, mark, now)?;
        status
            .sources
            .push((jungler::EVENTS.to_string(), delivered, mark.last_row));
        Ok(delivered)
    }

    /// `nodes.summary` → interim tier-1 blocks.
    async fn sweep_nodes(
        &self,
        rows: Vec<jungler::NodeRow>,
        traj: &TrajId,
        now: DateTime<Utc>,
        status: &mut FeedStatus,
    ) -> Result<(), OldFeedError> {
        let mut mark = self.0.state.get(jungler::NODES)?;
        if rows.is_empty() {
            status
                .sources
                .push((jungler::NODES.to_string(), 0, mark.last_row));
            return Ok(());
        }
        let sealed = self.sealed_ids(traj).await?;
        let mut n = 0usize;
        for row in &rows {
            let summary = row.summary.clone().unwrap_or_default();
            let id = rollup_id("node", row.id);
            if !summary.trim().is_empty() && !sealed.contains(&id) {
                let title = row.title.clone().unwrap_or_default();
                self.seal(
                    id,
                    traj,
                    Seq(row.id.max(0) as u64),
                    serde_json::json!({
                        "text": summary,
                        "title": title,
                        "source": format!("jungler:node:{}", row.id),
                        "lane": row.lane,
                    }),
                    now,
                )
                .await?;
                n += 1;
            }
            mark = state::Watermark {
                last_row: row.id,
                last_at: row.updated_at,
            };
        }
        self.0.state.set(jungler::NODES, mark, now)?;
        status
            .sources
            .push((jungler::NODES.to_string(), n, mark.last_row));
        Ok(())
    }

    /// `lane_story` sections → interim tier-1 blocks, sealed in `ord` order.
    async fn sweep_story(
        &self,
        mut rows: Vec<jungler::StoryRow>,
        traj: &TrajId,
        now: DateTime<Utc>,
        status: &mut FeedStatus,
    ) -> Result<(), OldFeedError> {
        let mut mark = self.0.state.get(jungler::LANE_STORY)?;
        if rows.is_empty() {
            status
                .sources
                .push((jungler::LANE_STORY.to_string(), 0, mark.last_row));
            return Ok(());
        }
        // The watermark is read by `id`; the STORY is told in `ord` order, so the batch is
        // resorted before sealing and the blocks land in the order the story is told.
        for row in &rows {
            mark = state::Watermark {
                last_row: row.id.max(mark.last_row),
                last_at: row.updated_at,
            };
        }
        rows.sort_by_key(|r| (r.ord, r.id));

        let sealed = self.sealed_ids(traj).await?;
        let mut n = 0usize;
        for row in &rows {
            let body = row.body.clone().unwrap_or_default();
            let id = rollup_id("story", row.id);
            if body.trim().is_empty() || sealed.contains(&id) {
                continue;
            }
            let heading = row.heading.clone().unwrap_or_default();
            self.seal(
                id,
                traj,
                Seq(row.ord.max(0) as u64),
                serde_json::json!({
                    "text": body,
                    "title": heading,
                    "source": format!("jungler:lane_story:{}", row.id),
                    "lane": row.lane,
                    "ord": row.ord,
                }),
                now,
            )
            .await?;
            n += 1;
        }
        self.0.state.set(jungler::LANE_STORY, mark, now)?;
        status
            .sources
            .push((jungler::LANE_STORY.to_string(), n, mark.last_row));
        Ok(())
    }

    /// Every tier rollup id already on this trajectory. Seal-once means a re-seal is a violation
    /// (§3), so the guard is a read, not a caught error.
    async fn sealed_ids(&self, traj: &TrajId) -> Result<HashSet<RollupId>, OldFeedError> {
        Ok(self
            .0
            .ledger
            .0
            .rollups(&RollupQuery {
                trajs: vec![traj.clone()],
                kind: Some(RollupKind::Tier),
                include_superseded: true,
                ..Default::default()
            })
            .await?
            .into_iter()
            .map(|r| r.id)
            .collect())
    }

    /// One interim tier-1 block.
    ///
    /// `notable_refs` is EMPTY on purpose: P1-D13 reads an empty set as "notable to everyone", and
    /// the point of §17's softening is that these blocks reach the agent's tiers band while it has
    /// no real rollups. A jungler-shaped ref here would filter them straight back out.
    async fn seal(
        &self,
        id: RollupId,
        traj: &TrajId,
        at_seq: Seq,
        body: serde_json::Value,
        now: DateTime<Utc>,
    ) -> Result<(), OldFeedError> {
        self.0
            .ledger
            .0
            .seal_rollup(NewRollup {
                id: Some(id),
                traj: traj.clone(),
                kind: RollupKind::Tier,
                tier: 1,
                from_seq: at_seq,
                to_seq: at_seq,
                src_trajs: vec![traj.clone()],
                body,
                notable_refs: BTreeSet::new(),
                prompt_ver: PROMPT_VER.to_string(),
                sealed_at: now,
            })
            .await?;
        Ok(())
    }
}

/// PURE: the `Delivery` one jungler event becomes. Cited by construction — mail that cannot say
/// where it came from is not deliverable (§3).
pub fn delivery_of(row: &jungler::EventRow, r: Ref, fallback_at: DateTime<Utc>) -> Delivery {
    let kind = row.kind.clone().unwrap_or_else(|| "event".to_string());
    let subject = row
        .subject
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| format!("{kind} {}", row.id));
    let text = row.body.clone().unwrap_or_default();
    let summary = text.lines().next().unwrap_or("").trim().to_string();
    let mut refs: BTreeSet<Ref> = BTreeSet::from([r.clone()]);
    if let Some(extra) = row.r#ref.as_ref().filter(|s| !s.trim().is_empty()) {
        refs.insert(Ref::new(extra));
    }
    if let Some(lane) = row.lane.as_ref().filter(|s| !s.trim().is_empty()) {
        refs.insert(Ref::new(format!("lane:{lane}")));
    }
    Delivery {
        // Ordinary, always: §5 is explicit that pushes, CI and state changes never wake a dormant
        // agent, and everything the old daemon collected is one of those.
        class: MailClass::Ordinary,
        from: Sender::Collector("jungler".to_string()),
        subject,
        summary,
        text,
        cites: vec![Cite {
            r#ref: r,
            url: row.url.clone().filter(|u| !u.trim().is_empty()),
        }],
        refs,
        at: if row.at == 0 {
            fallback_at
        } else {
            jungler::ts_to_utc(row.at)
        },
    }
}

/// PURE: the ONE line activation logs when a source is off. `None` when everything is live.
pub fn disabled_line(disabled: &[(String, String)]) -> Option<String> {
    if disabled.is_empty() {
        return None;
    }
    let parts: Vec<String> = disabled
        .iter()
        .map(|(source, why)| format!("{source} ({why})"))
        .collect();
    Some(format!("old-feed: disabled — {}", parts.join("; ")))
}

/// The priming filter. Every field is optional; `limit` comes from the config.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PrimingQuery {
    pub repo: Option<String>,
    pub tags: Vec<String>,
    pub contains: Option<String>,
    pub limit: usize,
}

/// One remembered command. Competence memory, NEVER mail.
#[derive(Clone, Debug, PartialEq)]
pub struct CommandMemory {
    pub cmd: String,
    pub tags: Vec<String>,
    pub repo: String,
    pub at: DateTime<Utc>,
    pub exit_code: Option<i64>,
    pub output_head: String,
}

/// The notes filter.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NoteQuery {
    pub contains: Option<String>,
    pub limit: usize,
}

/// One note section, as cited evidence.
#[derive(Clone, Debug, PartialEq)]
pub struct NoteEvidence {
    pub note: i64,
    pub ord: i64,
    pub heading: String,
    pub body: String,
    pub author: String,
    pub cite: Cite,
}

/// What the last sweep did, per source.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FeedStatus {
    /// `('jungler.events', rows delivered, watermark)` triples.
    pub sources: Vec<(String, usize, i64)>,
    /// Sources that were disabled, and why (absent db, missing required column).
    pub disabled: Vec<(String, String)>,
    pub last_sweep: Option<DateTime<Utc>>,
}

/// Everything the adapter can go wrong as. An ABSENT or unreadable jungler db is NOT one of them.
#[derive(Debug, thiserror::Error)]
pub enum OldFeedError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("ledger: {0}")]
    Ledger(#[from] bough_plugin_ledger::LedgerError),
    #[error("{0}")]
    Failed(String),
}

/// The row.
pub struct OldFeedPlugin;

#[async_trait::async_trait]
impl Plugin for OldFeedPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = OldFeedConfig;

    fn inject() -> Inject {
        // `commands` is OPTIONAL: the bridge is a headless collector that happens to expose one
        // slash command, and it must sweep in a profile that mounts no surface at all.
        Inject::required(["agents", "ledger"]).union(&Inject::optional(["commands"]))
    }

    fn validate(cfg: &Self::Config) -> Result<(), ConfigError> {
        let reject = |detail: String| Err(ConfigError::Rejected { detail });
        if cfg.poll_ms == 0 {
            return reject("poll_ms must be > 0".to_string());
        }
        if cfg.batch == 0 {
            return reject("batch must be > 0".to_string());
        }
        if cfg.priming_limit == 0 {
            return reject("priming_limit must be > 0".to_string());
        }
        if cfg.deliver_to.trim().is_empty() {
            return reject("deliver_to must name an agent".to_string());
        }
        Ok(())
    }

    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let entry = ctx.entry_id().clone();
        let fail = |e: OldFeedError| PluginError::new(entry.clone(), e);

        let agents = ctx
            .get::<Agents>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;
        let ledger = ctx
            .get::<Ledger>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;

        let handle = OldFeedHandle::open(
            cfg.clone(),
            LedgerHandle(ledger.0.clone()),
            (*agents).clone(),
        )
        .map_err(fail)?;

        // §14, V7: ONE line, and the row activates anyway.
        if let Some(line) = handle.disabled_line() {
            tracing::info!(target: "old-feed", "{line}");
        }

        ctx.provide::<OldFeed>(handle.clone())
            .await
            .map_err(|e| PluginError::new(entry.clone(), e))?;

        command::register(&ctx, &handle).await?;

        // The sweep is an ORDINARY EFFECT, not a schedule: `ctx.schedule` arrives in Phase 6 and
        // this row retires there anyway. Disposing the row halts the loop at its checkpoint.
        let poll = std::time::Duration::from_millis(cfg.poll_ms);
        let feed = handle.clone();
        ctx.effect_spawn(move |ectx| async move {
            loop {
                if ectx.checkpoint().await.is_err() {
                    return Ok(());
                }
                if let Err(e) = feed.sweep().await {
                    // A sweep that fails is reported and retried at the next tick: the old feed
                    // is a bridge, and a bad read of it never takes the harness down.
                    tracing::warn!(target: "old-feed", error = %e, "sweep failed");
                }
                tokio::time::sleep(poll).await;
            }
        });
        Ok(())
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

bough_kernel::register_plugin!(OldFeedPlugin);

#[cfg(test)]
mod tests {
    use super::*;

    fn row() -> jungler::EventRow {
        jungler::EventRow {
            id: 12,
            at: 1_700_000_000_000,
            kind: Some("pr".to_string()),
            subject: Some("PR #4 opened".to_string()),
            body: Some("first line\nsecond line".to_string()),
            r#ref: Some("gh:bough/rebuild#4".to_string()),
            url: Some("https://example.invalid/4".to_string()),
            lane: Some("rebuild".to_string()),
        }
    }

    fn now() -> DateTime<Utc> {
        DateTime::from_timestamp(1_600_000_000, 0).expect("a fixed instant")
    }

    #[test]
    fn an_event_becomes_cited_ordinary_mail() {
        let d = delivery_of(&row(), event_ref(12), now());
        assert_eq!(d.class, MailClass::Ordinary);
        assert_eq!(d.cites.len(), 1);
        assert_eq!(d.cites[0].r#ref.as_str(), "jungler:event:12");
        assert_eq!(d.cites[0].url.as_deref(), Some("https://example.invalid/4"));
        assert_eq!(d.summary, "first line");
        assert!(d.refs.contains(&Ref::new("gh:bough/rebuild#4")));
        assert!(d.refs.contains(&Ref::new("lane:rebuild")));
    }

    #[test]
    fn a_subjectless_event_still_gets_a_subject() {
        let mut r = row();
        r.subject = None;
        assert_eq!(delivery_of(&r, event_ref(12), now()).subject, "pr 12");
    }

    #[test]
    fn a_timestampless_event_falls_back_to_the_injected_clock() {
        let mut r = row();
        r.at = 0;
        assert_eq!(delivery_of(&r, event_ref(12), now()).at, now());
    }

    #[test]
    fn every_source_live_logs_nothing() {
        assert_eq!(disabled_line(&[]), None);
    }

    #[test]
    fn the_disabled_report_is_exactly_one_line() {
        let line = disabled_line(&[
            ("jungler.events".to_string(), "absent".to_string()),
            ("jungler.nodes".to_string(), "absent".to_string()),
        ])
        .expect("a line");
        assert!(!line.contains('\n'), "one line, not a paragraph: {line}");
        assert!(line.contains("jungler.events") && line.contains("jungler.nodes"));
    }

    #[test]
    fn a_rollup_id_is_a_pure_function_of_the_row() {
        assert_eq!(rollup_id("node", 3).as_str(), "old-feed:node:3");
        assert_eq!(rollup_id("node", 3), rollup_id("node", 3));
    }
}
