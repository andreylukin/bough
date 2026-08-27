//! Invariant (P5-D4): the unsorted queue is a REAL trajectory and the leader is a SINK on it, not
//! its owner. A tree may boot with no leader — headless, and the moment before the `leader` row
//! activates — and mail must be neither dropped nor refused then. So the queue is durable and
//! leaderless, and a sink that arrives later adopts the backlog.

use bough_plugin_agents::InboxReceipt;
use bough_plugin_ledger::{AgentName, StepId};
use chrono::{DateTime, Utc};

/// Who receives unsorted mail as LIVE mail. An effect: the `leader` row installs it in its own
/// fiber, so moving the leader set moves the sink with it (the SWAP).
#[async_trait::async_trait]
pub trait UnsortedSink: Send + Sync + 'static {
    /// The agent unsorted mail is delivered to.
    fn agent(&self) -> AgentName;
    /// Whether this sink names nobody. Only [`NullSink`] says yes, and `route` asks BEFORE it
    /// delivers, which is why a leaderless tree queues instead of erroring.
    fn is_null(&self) -> bool {
        false
    }
}

/// The sink that is mounted when no leader is: it names nobody, and the queue simply keeps its
/// items until a real sink arrives.
pub struct NullSink;

#[async_trait::async_trait]
impl UnsortedSink for NullSink {
    /// The empty name, which matches no row in any query. `route` never reaches it: it checks
    /// [`UnsortedSink::is_null`] first.
    fn agent(&self) -> AgentName {
        AgentName::new("")
    }
    fn is_null(&self) -> bool {
        true
    }
}

/// What one adoption did.
#[derive(Clone, Debug, PartialEq)]
pub struct Adoption {
    pub unrouted: StepId,
    pub to: AgentName,
    pub receipt: InboxReceipt,
    pub at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::Arc;

    use bough_kernel::{Context, KernelCore};
    use bough_plugin_agents::{
        Agent, AgentCell, AgentDriver, AgentError, AgentFactory, AgentsHandle, CancelCause,
        CreateAgent, InboxReceipt, MailClass, Message, Sender, WakeCause, WakeKind, WakeRequest,
    };
    use bough_plugin_ledger::{AgentName, LedgerHandle, Order, Ref, StepQuery, StepType, TrajId};
    use bough_plugin_ledger_memory::store::MemoryStore;

    use crate::{Envelope, MailConfig, MailHandle};

    /// A driver that does nothing: these tests are about what the ROUTER wrote, not about wakes.
    struct Idle;

    #[async_trait::async_trait]
    impl AgentDriver for Idle {
        fn driver(&self) -> &'static str {
            "idle"
        }
        async fn wake_now(&self, _k: WakeKind, _c: WakeCause) -> WakeRequest {
            WakeRequest::Nothing
        }
        async fn notify(&self, _r: &InboxReceipt, _m: &Message) {}
        async fn cancel(&self, _c: CancelCause, _keep: bool) {}
        async fn stop(&self) {}
    }

    struct IdleFactory;

    #[async_trait::async_trait]
    impl AgentFactory for IdleFactory {
        fn driver(&self) -> &'static str {
            "idle"
        }
        async fn attach(
            &self,
            _cell: AgentCell,
            _mode: bough_plugin_agents::Attach,
        ) -> Result<Arc<dyn AgentDriver>, AgentError> {
            Ok(Arc::new(Idle) as Arc<dyn AgentDriver>)
        }
    }

    pub(super) struct Fx {
        pub ledger: LedgerHandle,
        pub agents: AgentsHandle,
        pub mail: MailHandle,
    }

    pub(super) async fn fx() -> Fx {
        let ctx = Context::root(KernelCore::new());
        let ledger = LedgerHandle(MemoryStore::new(ctx.clone()) as Arc<_>);
        for def in crate::vocabulary::step_types() {
            ledger.0.register_step_type(def).expect("a fresh map");
        }
        let agents = AgentsHandle::new(ctx.clone(), ledger.clone());
        agents
            .set_factory(&ctx, Arc::new(IdleFactory) as Arc<dyn AgentFactory>)
            .await
            .expect("the slot is free");
        let mail = MailHandle::new(
            ctx.clone(),
            ledger.clone(),
            agents.clone(),
            Arc::new(MailConfig {
                unsorted_traj: "unsorted".into(),
                unsorted_limit: 200,
                deliver_to_dormant: true,
            }),
        );
        let _ = ctx;
        Fx {
            ledger,
            agents,
            mail,
        }
    }

    pub(super) fn now() -> chrono::DateTime<chrono::Utc> {
        chrono::Utc::now()
    }

    pub(super) fn envelope(refs: &[&str]) -> Envelope {
        Envelope {
            from: Sender::Collector("github".into()),
            class: MailClass::Wake,
            subject: "CI is red on main".into(),
            summary: "the delegate test failed again".into(),
            text: "the full body".into(),
            cites: Vec::new(),
            refs: refs.iter().map(Ref::new).collect(),
            at: now(),
        }
    }

    pub(super) async fn live(fx: &Fx, name: &str) -> Agent {
        let (agent, disposer) = fx
            .agents
            .create(CreateAgent::resident(
                AgentName::new(name),
                TrajId::new(format!("t-{name}")),
                now(),
            ))
            .await
            .expect("the transaction commits");
        // The disposer is deliberately leaked: these tests end with the process, and dropping it
        // would tear the agent down under the router.
        std::mem::forget(disposer);
        agent
    }

    #[tokio::test]
    async fn a_zero_match_envelope_becomes_one_unrouted_step() {
        let fx = fx().await;
        // A lane exists, and it routes on something else. Nothing may be dropped.
        live(&fx, "docs").await;
        fx.mail
            .link_ref(
                &AgentName::new("docs"),
                BTreeSet::from([Ref::new("repo:wiki")]),
                now(),
            )
            .await
            .expect("a link");

        let report = fx
            .mail
            .route(envelope(&["repo:bough"]))
            .await
            .expect("a route");
        assert!(report.matched.is_empty());
        assert!(report.delivered.is_empty());
        assert!(report.unsorted.is_some());
        assert!(!report.adopted, "no sink is mounted");

        let queued = fx
            .ledger
            .0
            .steps(&StepQuery {
                trajs: vec![TrajId::new("unsorted")],
                kinds: vec![StepType::new("mail/unrouted")],
                order: Order::SeqAsc,
                ..Default::default()
            })
            .await
            .expect("a read");
        assert_eq!(queued.len(), 1, "exactly one unrouted step, never two");
        assert_eq!(queued[0].id, report.unsorted.unwrap());
    }

    #[tokio::test]
    async fn adoption_names_the_unrouted_step_it_consumes() {
        let fx = fx().await;
        live(&fx, "ci").await;
        let report = fx
            .mail
            .route(envelope(&["repo:bough"]))
            .await
            .expect("a route");
        let unrouted = report.unsorted.expect("it went unsorted");

        fx.mail
            .adopt(
                &AgentName::new("ci"),
                std::slice::from_ref(&unrouted),
                now(),
            )
            .await
            .expect("an adoption");

        let adopted = fx
            .ledger
            .0
            .steps(&StepQuery {
                trajs: vec![TrajId::new("unsorted")],
                kinds: vec![StepType::new("mail/adopted")],
                ..Default::default()
            })
            .await
            .expect("a read");
        assert_eq!(adopted.len(), 1);
        let body: crate::MailAdopted =
            serde_json::from_value((*adopted[0].body).clone()).expect("a mail/adopted body");
        // The adoption NAMES its item: without it, "who took this" is unanswerable after the fact.
        assert_eq!(body.unrouted, unrouted);
        assert_eq!(body.to, AgentName::new("ci"));
    }
}
