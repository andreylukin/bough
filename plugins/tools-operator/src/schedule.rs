//! Invariant: an intent fires EXACTLY ONCE. Every `schedule/fired` names a `schedule/intent` that
//! exists and was not already fired — including across a restart replay, which is why the intent
//! is a ledger step and not a timer in memory.
//!
//! This is §5's "own scheduled intents", which nothing exposes today. When Phase 7's
//! `ctx.schedule` lands, the due-watcher half is deleted and the tool registers a cron entry
//! instead; the handoff is written up in `docs/codemode-merge-notes.md`.

use std::collections::BTreeSet;
use std::sync::Arc;

use bough_plugin_agents::{AgentsHandle, MailClass, MessageId, Sender, Target};
use bough_plugin_ledger::{
    AgentName, Append, Cite, Class, LedgerHandle, Order, Ref, StepQuery, StepType,
};
use bough_plugin_tools::{FailureClass, Tool, ToolCall, ToolCx, ToolFailure, ToolOutcome};
use chrono::{DateTime, Utc};

use crate::clock::Clock;
use crate::OperatorConfig;

bough_util::brand_id!(
    /// One scheduled intent.
    pub struct ScheduleId;
);

/// The two step kinds, spelled once.
pub const INTENT: &str = "schedule/intent";
pub const FIRED: &str = "schedule/fired";

/// `schedule/intent` — Evidence (it cites the step that asked for it).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ScheduleIntentBody {
    pub id: ScheduleId,
    pub agent: AgentName,
    pub at: DateTime<Utc>,
    pub intent: String,
}

/// `schedule/fired` — Thought.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ScheduleFiredBody {
    pub id: ScheduleId,
    pub at: DateTime<Utc>,
    pub message: MessageId,
}

/// The two step types this row owns, for `declare_step_types`.
pub fn step_types() -> Vec<bough_plugin_ledger::StepTypeDef> {
    use bough_plugin_ledger::{ClassRule, StepTypeDef};
    const OWNER: &str = crate::PLUGIN_NAME;
    vec![
        StepTypeDef::of::<ScheduleIntentBody>(INTENT, OWNER).class_rule(ClassRule::Evidence),
        StepTypeDef::of::<ScheduleFiredBody>(FIRED, OWNER).class_rule(ClassRule::Thought),
    ]
}

fn err(kind: FailureClass, message: impl Into<String>) -> ToolFailure {
    ToolFailure {
        kind,
        message: message.into(),
    }
}

/// Parse the `at` argument: an absolute RFC 3339 instant, or a relative `+90s` / `+5m` / `+2h` /
/// `+1d` offset from `now`. Pure, with `now` passed in.
pub fn parse_at(raw: &str, now: DateTime<Utc>) -> Result<DateTime<Utc>, String> {
    let raw = raw.trim();
    if let Some(rest) = raw.strip_prefix('+') {
        let (digits, unit) = rest.split_at(
            rest.find(|c: char| !c.is_ascii_digit())
                .ok_or_else(|| format!("`{raw}` needs a unit: +30s, +5m, +2h, +1d"))?,
        );
        let n: i64 = digits
            .parse()
            .map_err(|_| format!("`{raw}` does not start with a number of units"))?;
        let d = match unit {
            "s" => chrono::Duration::seconds(n),
            "m" => chrono::Duration::minutes(n),
            "h" => chrono::Duration::hours(n),
            "d" => chrono::Duration::days(n),
            other => return Err(format!("unknown unit `{other}`; use s, m, h or d")),
        };
        return Ok(now + d);
    }
    DateTime::parse_from_rfc3339(raw)
        .map(|t| t.with_timezone(&Utc))
        .map_err(|e| format!("`{raw}` is not an RFC 3339 instant or a `+5m` offset: {e}"))
}

/// The horizon check, pure: §5 lets an agent schedule its OWN intents, not ones a successor will
/// have to honour a year from now.
pub fn check_horizon(at: DateTime<Utc>, now: DateTime<Utc>, max_days: u32) -> Result<(), String> {
    let horizon = now + chrono::Duration::days(max_days as i64);
    if at > horizon {
        return Err(format!(
            "{at} is beyond the {max_days}-day scheduling horizon (latest {horizon})"
        ));
    }
    Ok(())
}

/// `schedule` — takes `{at, intent}`.
pub struct Schedule {
    pub cfg: Arc<OperatorConfig>,
    pub clock: Arc<dyn Clock>,
    pub ledger: LedgerHandle,
    pub agents: Option<AgentsHandle>,
}

#[async_trait::async_trait]
impl Tool for Schedule {
    async fn call(&self, call: Arc<ToolCall>, _cx: ToolCx) -> Result<ToolOutcome, ToolFailure> {
        let now = self.clock.now();
        let raw = call
            .args
            .get("at")
            .and_then(|v| v.as_str())
            .ok_or_else(|| err(FailureClass::Error, "`at` is required and must be a string"))?;
        let intent = call
            .args
            .get("intent")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                err(
                    FailureClass::Error,
                    "`intent` is required and must be a string",
                )
            })?;
        let at = parse_at(raw, now).map_err(|m| err(FailureClass::Error, m))?;
        check_horizon(at, now, self.cfg.schedule_max_horizon_days)
            .map_err(|m| err(FailureClass::Denied, m))?;
        let Some(traj) =
            crate::inbox::own_traj(&self.ledger, self.agents.as_ref(), &call.agent).await?
        else {
            return Err(err(
                FailureClass::NotFound,
                format!("`{}` has no trajectory to schedule against", call.agent),
            ));
        };
        // The id is derived from the step that asked, so a replay of the same wake mints the same
        // id and the fired-set fold stays idempotent.
        let id = ScheduleId::new(format!("{}#{}", call.wake, call.step_index));
        let body = serde_json::to_value(ScheduleIntentBody {
            id: id.clone(),
            agent: call.agent.clone(),
            at,
            intent: intent.to_string(),
        })
        .expect("ScheduleIntentBody serializes");
        let step = self
            .ledger
            .0
            .append(Append {
                traj,
                wake: call.wake.clone(),
                kind: StepType::new(INTENT),
                class: Class::Evidence,
                body,
                cites: vec![Cite {
                    r#ref: Ref::new(format!("wake:{}", call.wake)),
                    url: None,
                }],
                at: now,
                id: None,
            })
            .await
            .map_err(|e| err(FailureClass::Error, e.to_string()))?;
        Ok(ToolOutcome {
            content: format!("scheduled `{id}` for {at}: {intent}"),
            value: Some(serde_json::json!({ "id": id, "at": at, "step": step.id.as_str() })),
            cites: vec![Cite {
                r#ref: Ref::new(format!("step:{}", step.id)),
                url: None,
            }],
            concludes_wake: false,
        })
    }
}

/// The due-watcher. One `tick` is the whole mechanism; `watch` is a loop around it.
pub struct Watcher {
    pub cfg: Arc<OperatorConfig>,
    pub clock: Arc<dyn Clock>,
    pub ledger: LedgerHandle,
    pub agents: AgentsHandle,
    /// The fiber the observations are attributed to, so unloading the row forgets them.
    pub fiber: bough_kernel::FiberUid,
}

/// One intent, as the fold reads it back off the ledger.
#[derive(Clone, Debug, PartialEq)]
pub struct Pending {
    pub body: ScheduleIntentBody,
    pub traj: bough_plugin_ledger::TrajId,
    pub step: bough_plugin_ledger::StepId,
}

/// The pure half of the watcher: which intents are due and not already fired.
///
/// Both halves come off the LEDGER, so a restart that replays the same rows computes the same
/// answer — which is what makes "fires exactly once" survive a restart.
pub fn due(intents: &[Pending], fired: &BTreeSet<ScheduleId>, now: DateTime<Utc>) -> Vec<Pending> {
    let mut seen = BTreeSet::new();
    intents
        .iter()
        .filter(|p| p.body.at <= now)
        .filter(|p| !fired.contains(&p.body.id))
        // One intent id can only be pending once even if two rows carry it.
        .filter(|p| seen.insert(p.body.id.clone()))
        .cloned()
        .collect()
}

impl Watcher {
    /// Read every `schedule/intent` in the store.
    pub async fn intents(&self) -> Result<Vec<Pending>, bough_plugin_ledger::LedgerError> {
        let steps = self
            .ledger
            .0
            .steps(&StepQuery {
                kinds: vec![StepType::new(INTENT)],
                order: Order::SeqAsc,
                ..Default::default()
            })
            .await?;
        Ok(steps
            .into_iter()
            .filter_map(|s| {
                serde_json::from_value::<ScheduleIntentBody>((*s.body).clone())
                    .ok()
                    .map(|body| Pending {
                        body,
                        traj: s.traj.clone(),
                        step: s.id.clone(),
                    })
            })
            .collect())
    }

    /// Every intent id that already fired.
    pub async fn fired(&self) -> Result<BTreeSet<ScheduleId>, bough_plugin_ledger::LedgerError> {
        let steps = self
            .ledger
            .0
            .steps(&StepQuery {
                kinds: vec![StepType::new(FIRED)],
                order: Order::SeqAsc,
                ..Default::default()
            })
            .await?;
        Ok(steps
            .into_iter()
            .filter_map(|s| {
                serde_json::from_value::<ScheduleFiredBody>((*s.body).clone())
                    .ok()
                    .map(|b| b.id)
            })
            .collect())
    }

    /// Fire everything due. Returns what fired, so a test never has to sleep.
    ///
    /// An intent whose agent is not live is left PENDING rather than fired-and-dropped: a wake
    /// nobody could receive is the one thing worse than a late one.
    pub async fn tick(&self) -> Result<Vec<ScheduleId>, bough_plugin_ledger::LedgerError> {
        let now = self.clock.now();
        let intents = self.intents().await?;
        let already = self.fired().await?;
        // Whatever the ledger already knows is what the invariant window sees, so a restart that
        // replays these rows re-records them rather than reporting a double fire.
        crate::invariant::note_intents(
            self.fiber,
            intents.iter().map(|p| p.body.id.to_string()).collect(),
        );
        let mut out = Vec::new();
        for p in due(&intents, &already, now) {
            let Some(agent) = self.agents.by_name(&p.body.agent) else {
                continue;
            };
            let message = MessageId::new(format!("schedule:{}", p.body.id));
            let msg = bough_plugin_agents::Message {
                id: message.clone(),
                from: Sender::System("schedule"),
                class: MailClass::Wake,
                text: p.body.intent.clone(),
                subject: format!("scheduled intent {}", p.body.id),
                cites: vec![Cite {
                    r#ref: Ref::new(format!("step:{}", p.step)),
                    url: None,
                }],
                refs: BTreeSet::new(),
                mail_seq: None,
                at: now,
            };
            if agent.send(msg, Target::NextWake, true).await.is_err() {
                // A disposed agent between the lookup and the send: leave it pending.
                continue;
            }
            let body = serde_json::to_value(ScheduleFiredBody {
                id: p.body.id.clone(),
                at: now,
                message,
            })
            .expect("ScheduleFiredBody serializes");
            self.ledger
                .0
                .append(Append {
                    traj: p.traj.clone(),
                    wake: bough_plugin_agents::mail::outside_wake(),
                    kind: StepType::new(FIRED),
                    class: Class::Thought,
                    body,
                    cites: vec![Cite {
                        r#ref: Ref::new(format!("step:{}", p.step)),
                        url: None,
                    }],
                    at: now,
                    id: None,
                })
                .await?;
            crate::invariant::note_fire(self.fiber, p.body.id.to_string());
            out.push(p.body.id);
        }
        Ok(out)
    }
}

/// The due-watcher loop: at the due time it delivers a `Wake` message to the creator's next wake
/// and appends `schedule/fired`.
///
/// It is the body of an `effect_spawn`, so the row's disposal halts it at the next checkpoint.
pub async fn watch(
    ectx: bough_kernel::EffectCtx,
    watcher: Watcher,
) -> Result<(), bough_kernel::PluginError> {
    // The wait is SLICED. `EffectHandle::dispose` awaits the spawned task before it unwinds, so a
    // body that slept the whole tick between checkpoints would hold up teardown for a whole
    // `schedule_tick_ms` — which, at the bundle's five seconds, is long enough to make a SIGINT
    // look like a hang.
    const SLICE_MS: u64 = 100;
    let tick = watcher.cfg.schedule_tick_ms;
    let mut waited = 0u64;
    loop {
        tokio::time::sleep(std::time::Duration::from_millis(SLICE_MS.min(tick))).await;
        if ectx.checkpoint().await.is_err() {
            return Ok(());
        }
        waited += SLICE_MS.min(tick);
        if waited < tick {
            continue;
        }
        waited = 0;
        if let Err(e) = watcher.tick().await {
            tracing::warn!(error = %e, "schedule tick failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-08-27T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn relative_and_absolute_instants_both_parse() {
        assert_eq!(
            parse_at("+5m", now()).unwrap(),
            now() + chrono::Duration::minutes(5)
        );
        assert_eq!(
            parse_at("+2h", now()).unwrap(),
            now() + chrono::Duration::hours(2)
        );
        assert_eq!(
            parse_at("2026-08-28T00:00:00Z", now()).unwrap(),
            DateTime::parse_from_rfc3339("2026-08-28T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc)
        );
        assert!(parse_at("tomorrow", now()).is_err());
        assert!(parse_at("+5x", now()).is_err());
    }

    #[test]
    fn the_horizon_is_a_bound_on_how_far_ahead_an_intent_may_sit() {
        assert!(check_horizon(now() + chrono::Duration::days(3), now(), 30).is_ok());
        let e = check_horizon(now() + chrono::Duration::days(31), now(), 30)
            .expect_err("beyond the horizon is refused");
        assert!(e.contains("30-day"), "{e}");
    }
}
