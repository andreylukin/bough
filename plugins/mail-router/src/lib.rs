//! Invariant: this crate CHOOSES RECIPIENTS and nothing else. It never re-implements delivery:
//! `route` calls [`bough_plugin_agents::Agent::deliver`] once per recipient, which appends
//! `mail/delivered` FIRST and then splices the message carrying that step's seq (P3-D15) — and
//! that is what makes per-agent consumption free rather than a second bookkeeping scheme.
//!
//! The fan-out rule (§3) is EVERY matching agent, not the best one; zero matches land in the
//! durable unsorted queue (P5-D4), which exists whether or not a leader does.

pub mod envelope;
pub mod error;
pub mod invariant;
pub mod link;
pub mod matching;
pub mod question;
pub mod unsorted;
pub mod vocabulary;

use std::collections::BTreeSet;
use std::sync::Arc;

use bough_kernel::{
    Context, EffectHandle, EmitEvent, Inject, InvariantSpec, Plugin, PluginError, ServiceKey,
    WaterfallEvent,
};
use bough_plugin_agents::{Delivery, InboxReceipt, MailClass};
use bough_plugin_ledger::{
    AgentName, Append, Cite, Class, Order, Ref, Step, StepId, StepQuery, StepType, TrajId,
};
use bough_plugin_rollups::Attribution;
use chrono::{DateTime, Utc};

pub use envelope::{Envelope, LinkReport, Question, RouteReport};
pub use error::MailError;
pub use link::{linked, unlinked};
pub use matching::{recipients, wake_classes_of, CLASS_NAMESPACE};
pub use question::{ask_ref, envelope_for, ASK_CLASS_REF};
pub use unsorted::{Adoption, NullSink, UnsortedSink};
pub use vocabulary::{AgentRouting, LeaderQuestion, MailAdopted, MailUnrouted, OWNER};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "mail-router";

/// The `mail` service key.
pub struct Mail;

impl ServiceKey for Mail {
    type Value = MailHandle;
    const NAME: &'static str = "mail";
}

/// The concrete handle newtype the key's value is (Decision D5).
#[derive(Clone)]
pub struct MailHandle(pub Arc<MailInner>);

/// The seam's live state: the bound ledger and agents seams, the config, and the sink slot.
pub struct MailInner {
    /// Where `mail/route` is dispatched and `mail/routed` emitted from. The plan's constructor
    /// omitted it; a waterfall has to be dispatched from somewhere (see the WP-1 report).
    ctx: Context,
    ledger: bough_plugin_ledger::LedgerHandle,
    agents: bough_plugin_agents::AgentsHandle,
    cfg: Arc<MailConfig>,
    /// P5-D4: leaderless by default. `None` — for which [`NullSink`] is the named stand-in — until
    /// a `leader` row installs its own.
    sink: parking_lot::Mutex<Option<Arc<dyn UnsortedSink>>>,
}

impl MailHandle {
    /// A seam with no sink mounted.
    pub fn new(
        ctx: Context,
        ledger: bough_plugin_ledger::LedgerHandle,
        agents: bough_plugin_agents::AgentsHandle,
        cfg: Arc<MailConfig>,
    ) -> MailHandle {
        MailHandle(Arc::new(MailInner {
            ctx,
            ledger,
            agents,
            cfg,
            sink: parking_lot::Mutex::new(None),
        }))
    }

    /// The trajectory zero-match mail lands on.
    pub fn unsorted_traj(&self) -> TrajId {
        TrajId::new(&self.0.cfg.unsorted_traj)
    }

    /// The mounted sink, if one names a real agent. [`NullSink`] reads as no sink at all.
    pub fn sink(&self) -> Option<Arc<dyn UnsortedSink>> {
        self.0
            .sink
            .lock()
            .clone()
            .filter(|s| !s.is_null() && !s.agent().as_str().is_empty())
    }

    /// Fan out. Seeds a [`RouteDecision`] from the pure matcher, dispatches the `mail/route`
    /// waterfall (P5-D5), then delivers once per surviving recipient. Appends nothing when
    /// `matched` is empty except the unsorted step.
    pub async fn route(&self, env: Envelope) -> Result<RouteReport, MailError> {
        let rows = self.0.ledger.0.agents().await?;
        let seeded = matching::recipients(&env.refs, &rows);
        let env = Arc::new(env);

        // P5-D5: the matcher SEEDS the decision. A listener that skips `next()` short-circuits to
        // a decision that already has the true owners in it, never to an empty one.
        let decision = self
            .0
            .ctx
            .waterfall::<MailRoute>(RouteDecision {
                env: env.clone(),
                to: seeded,
            })
            .await;

        // A listener may add a recipient twice; delivering twice would be a double consumption.
        let mut to: Vec<AgentName> = Vec::new();
        for name in decision.to {
            if !to.contains(&name) {
                to.push(name);
            }
        }

        let report = if to.is_empty() {
            self.queue_unsorted(&env).await?
        } else {
            self.fan_out(&env, to).await?
        };
        self.0.ctx.emit::<MailRouted>(report.clone());
        Ok(report)
    }

    /// One `mail/delivered` step and one splice per recipient, through the agents seam.
    async fn fan_out(
        &self,
        env: &Arc<Envelope>,
        to: Vec<AgentName>,
    ) -> Result<RouteReport, MailError> {
        let mut delivered: Vec<(AgentName, InboxReceipt)> = Vec::new();
        let mut undeliverable: Vec<AgentName> = Vec::new();
        let mut deduped: Vec<AgentName> = Vec::new();
        let mut stranded: Option<StepId> = None;
        for name in &to {
            let Some(agent) = self.0.agents.by_name(name) else {
                // A matched lane with no live handle. Skipping it silently would DROP the event:
                // no ledger row would name it and nothing could recover it. So the item is written
                // to the unsorted trajectory exactly as a zero-match item is — the leader's queue
                // is §3's recovery surface — and the report says who it could not reach.
                if self.0.cfg.tolerate_absent_lane {
                    undeliverable.push(name.clone());
                    if stranded.is_none() {
                        stranded = Some(self.append_unrouted(env).await?);
                    }
                    continue;
                }
                return Err(MailError::NotLive(name.clone()));
            };
            // The AT-LEAST-ONCE guard, per (trajectory, ref), BEFORE the delivery — the same
            // ordering the collectors' own guard held before the router took the fan-out over.
            if let Some(r) = &env.dedupe_on {
                let traj = self.0.ledger.0.agent(name).await?.map(|row| row.traj);
                if let Some(traj) = traj {
                    if already_delivered(&self.0.ledger, &traj, r).await? {
                        deduped.push(name.clone());
                        continue;
                    }
                }
            }
            match agent.deliver(self.delivery(env, env.class)).await {
                Ok(receipt) => delivered.push((name.clone(), receipt)),
                Err(e) => {
                    return Err(MailError::PartialFanOut {
                        agent: name.clone(),
                        delivered: delivered.len(),
                        detail: e.to_string(),
                    })
                }
            }
        }
        Ok(RouteReport {
            matched: to,
            delivered,
            undeliverable,
            unsorted: stranded,
            adopted: false,
            deduped,
        })
    }

    /// The one durable write both the zero-match path and the absent-lane path share.
    async fn append_unrouted(&self, env: &Arc<Envelope>) -> Result<StepId, MailError> {
        let step = self
            .0
            .ledger
            .0
            .append(Append {
                traj: self.unsorted_traj(),
                wake: bough_plugin_agents::mail::outside_wake(),
                kind: StepType::new("mail/unrouted"),
                class: Class::Evidence,
                body: serde_json::to_value(MailUnrouted {
                    from: Ref::new(env.from.as_ref_str()),
                    subject: env.subject.clone(),
                    summary: env.summary.clone(),
                    refs: env.refs.iter().cloned().collect(),
                })
                .expect("MailUnrouted serializes"),
                cites: self.cites(env),
                at: env.at,
                id: None,
            })
            .await
            .map_err(|e| MailError::Unsorted(e.to_string()))?;
        Ok(step.id)
    }

    /// The zero-match path: one durable `mail/unrouted` step, plus — only when a sink is mounted —
    /// ONE ordinary-class delivery to the sink's agent. Ordinary, never wake: an unsorted item is
    /// the leader's inbox work, not a reason to interrupt it (§5).
    async fn queue_unsorted(&self, env: &Arc<Envelope>) -> Result<RouteReport, MailError> {
        let step = self.append_unrouted(env).await?;

        let mut delivered = Vec::new();
        let mut adopted = false;
        if let Some(sink) = self.sink() {
            let name = sink.agent();
            if let Some(agent) = self.0.agents.by_name(&name) {
                let receipt = agent
                    .deliver(self.delivery(env, MailClass::Ordinary))
                    .await
                    .map_err(|e| MailError::PartialFanOut {
                        agent: name.clone(),
                        delivered: 0,
                        detail: e.to_string(),
                    })?;
                delivered.push((name, receipt));
                adopted = true;
            }
        }
        Ok(RouteReport {
            matched: Vec::new(),
            delivered,
            undeliverable: Vec::new(),
            unsorted: Some(step),
            adopted,
            deduped: Vec::new(),
        })
    }

    /// The `Delivery` an envelope becomes, at `class`.
    fn delivery(&self, env: &Arc<Envelope>, class: MailClass) -> Delivery {
        Delivery {
            from: env.from.clone(),
            class,
            subject: env.subject.clone(),
            summary: env.summary.clone(),
            text: env.text.clone(),
            cites: self.cites(env),
            refs: env.refs.clone(),
            at: env.at,
        }
    }

    /// Evidence needs cites, and the ledger refuses it without them. An envelope that carried
    /// none still knows who sent it, and that is the honest citation.
    fn cites(&self, env: &Arc<Envelope>) -> Vec<Cite> {
        if env.cites.is_empty() {
            vec![Cite {
                r#ref: Ref::new(env.from.as_ref_str()),
                url: None,
            }]
        } else {
            env.cites.clone()
        }
    }

    /// Add routing refs to an agent's row and append `agent/routing`. No backfill, by
    /// construction: this method never queries for history.
    pub async fn link_ref(
        &self,
        agent: &AgentName,
        refs: BTreeSet<Ref>,
        at: DateTime<Utc>,
    ) -> Result<LinkReport, MailError> {
        let row = self
            .0
            .ledger
            .0
            .agent(agent)
            .await?
            .ok_or_else(|| MailError::NoSuchAgent(agent.clone()))?;
        let (after, added) = link::linked(&row, &refs);
        self.write_routing(row, after, added.clone(), BTreeSet::new(), at)
            .await
    }

    /// Remove routing refs from an agent's row and append `agent/routing`.
    pub async fn unlink_ref(
        &self,
        agent: &AgentName,
        refs: BTreeSet<Ref>,
        at: DateTime<Utc>,
    ) -> Result<LinkReport, MailError> {
        let row = self
            .0
            .ledger
            .0
            .agent(agent)
            .await?
            .ok_or_else(|| MailError::NoSuchAgent(agent.clone()))?;
        let (after, removed) = link::unlinked(&row, &refs);
        self.write_routing(row, after, BTreeSet::new(), removed.clone(), at)
            .await
    }

    /// The one write path both links share: rewrite the row, append the evidence, then report
    /// what the new refs REACH. `connected()` writes nothing (§3), so reading it here does not
    /// smuggle a backfill in through the back door.
    async fn write_routing(
        &self,
        row: bough_plugin_ledger::AgentRow,
        after: BTreeSet<Ref>,
        added: BTreeSet<Ref>,
        removed: BTreeSet<Ref>,
        at: DateTime<Utc>,
    ) -> Result<LinkReport, MailError> {
        let name = row.name.clone();
        let traj = row.traj.clone();
        self.0
            .ledger
            .0
            .put_agent(bough_plugin_ledger::AgentRow {
                routing_refs: after,
                ..row
            })
            .await?;
        self.0
            .ledger
            .0
            .append(Append {
                traj,
                wake: bough_plugin_agents::mail::outside_wake(),
                kind: StepType::new("agent/routing"),
                class: Class::Evidence,
                body: serde_json::to_value(AgentRouting {
                    agent: name.clone(),
                    added: added.iter().cloned().collect(),
                    removed: removed.iter().cloned().collect(),
                    wake_classes: None,
                    by: Attribution::System,
                })
                .expect("AgentRouting serializes"),
                cites: vec![Cite {
                    r#ref: Ref::new(format!("agent:{name}")),
                    url: None,
                }],
                at,
                id: None,
            })
            .await?;
        let connected = self.0.ledger.0.connected(&name).await?;
        Ok(LinkReport {
            agent: name,
            added,
            removed,
            // Never queried, never queued: §5's rule made a fact by construction.
            backfilled: 0,
            now_connected: connected.ref_matches,
        })
    }

    /// Set an agent's wake classes and append `agent/routing` naming them (§5: a wake class is
    /// per-agent MUTABLE CONFIG, and the only thing that lets `MailClass::Wake` mail reactivate a
    /// dormant lane). Idempotent: setting the classes a row already has appends nothing.
    pub async fn set_wake_classes(
        &self,
        agent: &AgentName,
        classes: BTreeSet<String>,
        at: DateTime<Utc>,
    ) -> Result<BTreeSet<String>, MailError> {
        let row = self
            .0
            .ledger
            .0
            .agent(agent)
            .await?
            .ok_or_else(|| MailError::NoSuchAgent(agent.clone()))?;
        if row.wake_classes == classes {
            return Ok(classes);
        }
        let traj = row.traj.clone();
        let name = row.name.clone();
        self.0
            .ledger
            .0
            .put_agent(bough_plugin_ledger::AgentRow {
                wake_classes: classes.clone(),
                ..row
            })
            .await?;
        self.0
            .ledger
            .0
            .append(Append {
                traj,
                wake: bough_plugin_agents::mail::outside_wake(),
                kind: StepType::new("agent/routing"),
                class: Class::Evidence,
                body: serde_json::to_value(AgentRouting {
                    agent: name.clone(),
                    added: Vec::new(),
                    removed: Vec::new(),
                    wake_classes: Some(classes.iter().cloned().collect()),
                    by: Attribution::System,
                })
                .expect("AgentRouting serializes"),
                cites: vec![Cite {
                    r#ref: Ref::new(format!("agent:{name}")),
                    url: None,
                }],
                at,
                id: None,
            })
            .await?;
        Ok(classes)
    }

    /// §4's "ambiguous routing becomes a leader question, never a guess". Appends
    /// `leader/question` to the unsorted trajectory and routes it at [`MailClass::Wake`] with the
    /// `class:ask` ref, so it reactivates a dormant leader.
    pub async fn ask_leader(&self, q: Question) -> Result<StepId, MailError> {
        let step = self
            .0
            .ledger
            .0
            .append(Append {
                traj: self.unsorted_traj(),
                wake: bough_plugin_agents::mail::outside_wake(),
                kind: StepType::new("leader/question"),
                class: Class::Thought,
                body: serde_json::to_value(LeaderQuestion {
                    asked_by: q.asked_by.to_string(),
                    about: q.about.clone(),
                    options: q.options.clone(),
                })
                .expect("LeaderQuestion serializes"),
                cites: q.cites.clone(),
                at: q.at,
                id: None,
            })
            .await?;

        let mut env = question::envelope_for(&q);
        if env.cites.is_empty() {
            env.cites = vec![Cite {
                r#ref: Ref::new(format!("step:{}", step.id)),
                url: None,
            }];
        }
        self.route(env).await?;
        Ok(step.id)
    }

    /// The unsorted queue, oldest first: what the leader's `adopt_unsorted` reads.
    ///
    /// An item that a `mail/adopted` step already names is NOT in it. Without that clause the
    /// queue never drains: a leader that reads the oldest N on every wake reads the same N
    /// forever, re-adopts them, re-delivers the mail, and starves everything queued behind them.
    /// Adoption is the only thing that consumes an unsorted item, so it is the only thing that can
    /// define the queue.
    pub async fn unsorted(&self, limit: usize) -> Result<Vec<Step>, MailError> {
        let want = limit.min(self.0.cfg.unsorted_limit);
        let taken = self.adopted_ids().await?;
        let mut out: Vec<Step> = Vec::new();
        let mut after: Option<bough_plugin_ledger::Seq> = None;
        // Paged, because the ANSWER is `want` unadopted items and the page may be all adopted.
        loop {
            if out.len() >= want {
                break;
            }
            let page = self
                .0
                .ledger
                .0
                .steps(&StepQuery {
                    trajs: vec![self.unsorted_traj()],
                    kinds: vec![StepType::new("mail/unrouted")],
                    order: Order::SeqAsc,
                    after,
                    limit: Some(self.0.cfg.unsorted_limit.max(1)),
                    ..Default::default()
                })
                .await?;
            let Some(last) = page.last().map(|s| s.seq) else {
                break;
            };
            after = Some(last);
            for step in page {
                if taken.contains(&step.id) {
                    continue;
                }
                out.push(step);
                if out.len() >= want {
                    break;
                }
            }
        }
        Ok(out)
    }

    /// Every unrouted step id a `mail/adopted` step already names.
    async fn adopted_ids(&self) -> Result<std::collections::BTreeSet<StepId>, MailError> {
        let mut taken = std::collections::BTreeSet::new();
        for step in self
            .0
            .ledger
            .0
            .steps(&StepQuery {
                trajs: vec![self.unsorted_traj()],
                kinds: vec![StepType::new("mail/adopted")],
                order: Order::SeqAsc,
                ..Default::default()
            })
            .await?
        {
            if let Ok(body) = serde_json::from_value::<MailAdopted>((*step.body).clone()) {
                taken.insert(body.unrouted);
            }
        }
        Ok(taken)
    }

    /// Mark unsorted items adopted by an agent: appends `mail/adopted` and re-routes them to it.
    ///
    /// `by` is WHO ADOPTED — the leader — and `to` is the lane the item is being routed to. They
    /// are almost never the same name, which is the whole point of the leader's curation being
    /// attributable (§2).
    ///
    /// Idempotent: an item a `mail/adopted` step already names is skipped, appending nothing and
    /// delivering nothing. Adoption is a delivery, and a delivery that can happen twice is a
    /// double consumption.
    pub async fn adopt(
        &self,
        to: &AgentName,
        steps: &[StepId],
        by: Attribution,
        at: DateTime<Utc>,
    ) -> Result<Vec<InboxReceipt>, MailError> {
        let agent = self
            .0
            .agents
            .by_name(to)
            .ok_or_else(|| MailError::NotLive(to.clone()))?;
        let taken = self.adopted_ids().await?;
        let mut out = Vec::with_capacity(steps.len());
        for id in steps {
            if taken.contains(id) {
                continue;
            }
            let step = self
                .0
                .ledger
                .0
                .step(id)
                .await?
                .ok_or_else(|| MailError::Unsorted(format!("no step `{id}` to adopt")))?;
            let item: MailUnrouted = serde_json::from_value((*step.body).clone()).map_err(|e| {
                MailError::Unsorted(format!("step `{id}` is not unrouted mail: {e}"))
            })?;
            let cite = Cite {
                r#ref: Ref::new(format!("step:{id}")),
                url: None,
            };
            self.0
                .ledger
                .0
                .append(Append {
                    traj: self.unsorted_traj(),
                    wake: bough_plugin_agents::mail::outside_wake(),
                    kind: StepType::new("mail/adopted"),
                    class: Class::Evidence,
                    body: serde_json::to_value(MailAdopted {
                        unrouted: id.clone(),
                        to: to.clone(),
                        by: by.clone(),
                    })
                    .expect("MailAdopted serializes"),
                    cites: vec![cite.clone()],
                    at,
                    id: None,
                })
                .await?;
            let receipt = agent
                .deliver(Delivery {
                    from: bough_plugin_agents::Sender::System("mail-router"),
                    class: MailClass::Ordinary,
                    subject: item.subject.clone(),
                    summary: item.summary.clone(),
                    text: item.summary.clone(),
                    cites: vec![cite],
                    refs: item.refs.iter().cloned().collect(),
                    at,
                })
                .await
                .map_err(|e| MailError::PartialFanOut {
                    agent: to.clone(),
                    delivered: out.len(),
                    detail: e.to_string(),
                })?;
            out.push(receipt);
        }
        Ok(out)
    }

    /// Install the unsorted sink. An EFFECT: the `leader` row installs it in its own fiber, so
    /// moving the leader set moves the sink with it, and unloading restores what was there before.
    pub async fn unsorted_sink(
        &self,
        ctx: &Context,
        sink: Arc<dyn UnsortedSink>,
    ) -> Result<EffectHandle, PluginError> {
        let inner = self.0.clone();
        let mine = sink;
        // The slot is replaced INSIDE the effect, after the effect exists. Replacing it first
        // would evict the previous sink even when `ctx.effect` fails, leaving a row that reported
        // failure quietly holding the routing of every unsorted item.
        ctx.effect(move |e| async move {
            let previous = { inner.sink.lock().replace(mine.clone()) };
            e.defer_sync(move || {
                let mut slot = inner.sink.lock();
                // Only give the slot back if it is still OURS: a later installer is not ours to
                // evict.
                if slot.as_ref().is_some_and(|held| Arc::ptr_eq(held, &mine)) {
                    *slot = previous;
                }
            });
            Ok(())
        })
        .await
    }
}

// ---- events ------------------------------------------------------------------------------

/// The value the `mail/route` waterfall carries.
#[derive(Clone)]
pub struct RouteDecision {
    pub env: Arc<Envelope>,
    pub to: Vec<AgentName>,
}

/// `mail/route` — WATERFALL over the routing DECISION: the extension point §0.2 names for the
/// mail domain. A later row (a ward, a collector policy) may add or remove recipients.
///
/// P5-D5: the crate's own matcher is NOT a listener. It SEEDS the decision before dispatch, so a
/// policy listener that deliberately skips `next()` short-circuits to a decision that already
/// exists rather than to an empty one.
pub struct MailRoute;
impl WaterfallEvent for MailRoute {
    const NAME: &'static str = "mail/route";
    type Value = RouteDecision;
}

/// `mail/routed` — EMIT, post-delivery. The TUI's toast and the invariant read it.
pub struct MailRouted;
impl EmitEvent for MailRouted {
    const NAME: &'static str = "mail/routed";
    type Payload = RouteReport;
}

// ---- the row -----------------------------------------------------------------------------

/// The row's config.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MailConfig {
    /// The durable trajectory zero-match mail lands on (P5-D4).
    pub unsorted_traj: String,
    /// How many unsorted items one `unsorted()` read returns at most.
    pub unsorted_limit: usize,
    /// What to do when a MATCHED lane has no live handle at all — a row that exists in the
    /// registry while its agent is not up. `true` keeps the fan-out going past it and records the
    /// undelivered item on the unsorted trajectory so it stays recoverable (§3); `false` makes it
    /// a loud [`MailError::NotLive`].
    ///
    /// It was called `deliver_to_dormant` and that name was wrong twice over: dormancy never
    /// removes an agent's handle (it defers WAKES), so this flag has nothing to do with dormancy,
    /// and `true` never meant "deliver" — it meant "do not fail".
    pub tolerate_absent_lane: bool,
}

/// The `mail` row.
pub struct MailRouterPlugin;

#[async_trait::async_trait]
impl Plugin for MailRouterPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = MailConfig;

    fn inject() -> Inject {
        Inject::required(["ledger", "agents"])
    }

    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let entry = ctx.entry_id().clone();
        let ledger = ctx
            .get::<bough_plugin_ledger::Ledger>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;
        let ledger = (*ledger).clone();
        let agents = ctx
            .get::<bough_plugin_agents::Agents>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;
        let agents = (*agents).clone();

        ledger
            .declare_step_types(&ctx, vocabulary::step_types())
            .await?;

        ctx.provide::<Mail>(MailHandle::new(ctx.clone(), ledger, agents, cfg))
            .await
            .map_err(|e| PluginError::new(entry.clone(), e))?;
        Ok(())
    }

    fn invariants() -> Vec<InvariantSpec> {
        vec![
            invariant::unrouted_matched_nobody(),
            invariant::one_delivery_per_recipient(),
        ]
    }
}

bough_kernel::register_plugin!(MailRouterPlugin);

/// Has this trajectory already been delivered mail CITING `r`?
///
/// The at-least-once guard of [`Envelope::dedupe_on`], and the one implementation of it: a
/// producer that re-offers a world item (a collector whose watermark write was lost) must not
/// deliver it twice, and the router is the only place that knows who the recipients are.
///
/// Per (trajectory, ref), never global: two lanes configured for one repository each get their
/// own copy, and deduping globally would silently starve the second.
///
/// The step query is an ANY-match over the DERIVED refs, which is the indexed way to find the
/// candidates — but a `gh:o/r#12` that a check-run mail carries in `refs` for routing is not a
/// delivery OF the pull request. The delivered fact is what the step CITES, so the candidates are
/// narrowed to the steps that cite `r`.
pub async fn already_delivered(
    ledger: &bough_plugin_ledger::LedgerHandle,
    traj: &TrajId,
    r: &Ref,
) -> Result<bool, MailError> {
    let hits = ledger
        .0
        .steps(&StepQuery {
            trajs: vec![traj.clone()],
            kinds: vec![StepType::new("mail/delivered")],
            refs: vec![r.clone()],
            order: Order::SeqAsc,
            limit: Some(1),
            ..Default::default()
        })
        .await?;
    Ok(hits.iter().any(|s| s.cites.iter().any(|c| &c.r#ref == r)))
}
