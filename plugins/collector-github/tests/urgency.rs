//! Invariant under test (V8): a collected event reaches every configured agent's inbox and wakes
//! it PER ITS CLASS — a wake-class item (a review request) starts a wake at the moment of
//! delivery, an ordinary item (a PR update) starts none and waits in the `next-wake` queue until
//! a drain wake claims it, and one drain claims ALL the ordinary mail that accumulated.
//!
//! The driver here is not a mock of the answer: it implements the documented seam contract
//! (`notify` reacts to `receipt.wake`, `wake_now(Drain)` claims `Ordinary` off `next-wake`) and
//! the assertions read what the REAL sweep + REAL `Agent::deliver` did to a REAL inbox.

mod common;

use std::sync::Arc;

use bough_plugin_agents::{
    Agent, AgentCell, AgentDriver, AgentError, AgentFactory, Attach, CancelCause, ClaimSelector,
    InboxReceipt, MailClass, Message, Target, WakeCause, WakeKind, WakeRequest,
};
use bough_plugin_ledger::WakeId;
use common::{at, Fx};
use parking_lot::Mutex;

const PR_ARGS: [&str; 8] = [
    "pr",
    "list",
    "--repo",
    "o/r",
    "--json",
    "number,title,url,updatedAt,author,state,isDraft",
    "--limit",
    "50",
];

fn review_args() -> Vec<String> {
    vec![
        "api".to_string(),
        "search/issues".to_string(),
        "-f".to_string(),
        "q=is:open is:pr review-requested:@me repo:o/r".to_string(),
    ]
}

/// TWO ordinary items, so "one drain claims all of it" is a real claim.
const PRS: &str = r#"[
  {"number":12,"title":"a PR","url":"https://example.invalid/12","updatedAt":"2026-08-01T00:00:00Z",
   "author":{"login":"andrey"},"state":"OPEN","isDraft":false},
  {"number":13,"title":"another PR","url":"https://example.invalid/13","updatedAt":"2026-08-01T00:30:00Z",
   "author":{"login":"andrey"},"state":"OPEN","isDraft":false}
]"#;

const REVIEWS: &str = r#"{"items":[
  {"number":4,"title":"please review","updated_at":"2026-08-01T01:00:00Z",
   "html_url":"https://example.invalid/4","user":{"login":"teammate"},"body":"a look?"}
]}"#;

const NO_REVIEWS: &str = r#"{"items":[]}"#;
const NO_PRS: &str = "[]";

fn fixtures(fx: &Fx, prs: &str, reviews: &str) {
    fx.gh_fixture(&PR_ARGS, "json", prs);
    let args = review_args();
    let argv: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    fx.gh_fixture(&argv, "json", reviews);
}

// ---- a driver that honours the wake contract instead of answering for it ----------------------

#[derive(Default)]
struct Factory {
    drivers: Mutex<Vec<Arc<UrgencyDriver>>>,
}

#[async_trait::async_trait]
impl AgentFactory for Factory {
    fn driver(&self) -> &'static str {
        "urgency-test-driver"
    }
    async fn attach(
        &self,
        cell: AgentCell,
        _mode: Attach,
    ) -> Result<Arc<dyn AgentDriver>, AgentError> {
        let d = Arc::new(UrgencyDriver {
            cell,
            wakes: Mutex::new(Vec::new()),
        });
        self.drivers.lock().push(d.clone());
        Ok(d as Arc<dyn AgentDriver>)
    }
}

/// One wake this driver ran: its kind and the subjects it claimed.
#[derive(Clone, Debug, PartialEq)]
struct Wake {
    kind: WakeKind,
    claimed: Vec<String>,
}

struct UrgencyDriver {
    cell: AgentCell,
    wakes: Mutex<Vec<Wake>>,
}

impl UrgencyDriver {
    async fn run(&self, kind: WakeKind, sel: ClaimSelector) -> usize {
        let wake = WakeId::new(format!("wake:{}", uuid::Uuid::now_v7()));
        self.cell.wake_started();
        let claimed = self.cell.claim(sel, wake, at()).await.expect("a claim");
        let subjects: Vec<String> = claimed.iter().map(|c| c.message.subject.clone()).collect();
        let n = subjects.len();
        self.wakes.lock().push(Wake {
            kind,
            claimed: subjects,
        });
        n
    }
}

#[async_trait::async_trait]
impl AgentDriver for UrgencyDriver {
    fn driver(&self) -> &'static str {
        "urgency-test-driver"
    }
    /// The seam's contract: a receipt asking for a wake IS the wake. Anything else queues.
    async fn notify(&self, receipt: &InboxReceipt, msg: &Message) {
        if !receipt.wake {
            return;
        }
        self.run(
            WakeKind::Answer,
            ClaimSelector {
                target: receipt.target,
                only: Some(vec![msg.id.clone()]),
                classes: None,
                exclude_andrey: false,
                limit: None,
            },
        )
        .await;
    }
    async fn cancel(&self, _cause: CancelCause, _keep_inbox: bool) {}
    async fn stop(&self) {}
    /// A drain claims ORDINARY `next-wake` mail — all of it, in one wake.
    async fn wake_now(&self, kind: WakeKind, _cause: WakeCause) -> WakeRequest {
        let n = self
            .run(
                kind,
                ClaimSelector {
                    target: Target::NextWake,
                    only: None,
                    classes: Some(vec![MailClass::Ordinary]),
                    exclude_andrey: false,
                    limit: None,
                },
            )
            .await;
        if n == 0 {
            WakeRequest::Nothing
        } else {
            WakeRequest::Started(WakeId::new("wake:drain"))
        }
    }
}

/// The fixture with THIS driver in the factory slot.
async fn fx_with_driver() -> (Fx, Arc<Factory>) {
    let fx = Fx::new_without_factory().await;
    let factory = Arc::new(Factory::default());
    std::mem::forget(
        fx.agents
            .set_factory(&fx.ctx, factory.clone() as Arc<dyn AgentFactory>)
            .await
            .expect("the slot is free"),
    );
    (fx, factory)
}

fn driver_for(factory: &Factory, agent: &Agent) -> Arc<UrgencyDriver> {
    let _ = agent;
    factory
        .drivers
        .lock()
        .last()
        .cloned()
        .expect("the agent attached a driver")
}

#[tokio::test]
async fn a_review_request_is_wake_class_and_wakes_the_agent_now() {
    let (fx, factory) = fx_with_driver().await;
    fixtures(&fx, NO_PRS, REVIEWS);
    let sol = fx.agent("sol").await;
    let driver = driver_for(&factory, &sol);

    fx.collector(fx.cfg())
        .sweep_at(at())
        .await
        .expect("a sweep");

    // The wake happened DURING the delivery, without anything asking for a drain.
    let wakes = driver.wakes.lock().clone();
    assert_eq!(wakes.len(), 1, "{wakes:?}");
    assert_eq!(wakes[0].kind, WakeKind::Answer);
    assert_eq!(
        wakes[0].claimed,
        vec!["review requested: o/r#4 please review".to_string()]
    );
    // It reached the ledger as wake-class mail, and the inbox is empty because the wake took it.
    let steps = fx.delivered("sol").await;
    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0].body["class"].as_str(), Some("wake"));
    assert_eq!(sol.inbox().len(), 0);
    assert!(!sol.has_pending_wake(), "the wake picked the mail up");
}

#[tokio::test]
async fn an_ordinary_push_queues_and_schedules_a_drain_instead() {
    let (fx, factory) = fx_with_driver().await;
    fixtures(&fx, PRS, NO_REVIEWS);
    let sol = fx.agent("sol").await;
    let driver = driver_for(&factory, &sol);

    fx.collector(fx.cfg())
        .sweep_at(at())
        .await
        .expect("a sweep");

    // Delivered, cited, and NOT woken: two ordinary items sitting on the next-wake queue.
    let steps = fx.delivered("sol").await;
    assert_eq!(steps.len(), 2, "{steps:?}");
    for step in &steps {
        assert_eq!(step.body["class"].as_str(), Some("ordinary"));
    }
    assert!(
        driver.wakes.lock().is_empty(),
        "ordinary mail must not start a wake: {:?}",
        driver.wakes.lock()
    );
    assert!(!sol.has_pending_wake());
    assert_eq!(sol.inbox().pending(Target::NextWake).len(), 2);

    // The coalesced drain: ONE wake claims BOTH accumulated items.
    let started = sol
        .request_wake(WakeKind::Drain, WakeCause::Schedule("drain"))
        .await;
    assert!(matches!(started, WakeRequest::Started(_)), "{started:?}");
    let wakes = driver.wakes.lock().clone();
    assert_eq!(wakes.len(), 1, "one drain, not one per item: {wakes:?}");
    assert_eq!(wakes[0].kind, WakeKind::Drain);
    let mut claimed = wakes[0].claimed.clone();
    claimed.sort();
    assert_eq!(
        claimed,
        vec!["o/r#12 a PR".to_string(), "o/r#13 another PR".to_string()]
    );
    assert_eq!(sol.inbox().len(), 0);

    // And the drain is durable: two `inbox/spliced` claims are on the ledger.
    assert_eq!(fx.claims("sol").await.len(), 2);
}

#[tokio::test]
async fn mail_reaches_every_agent_in_deliver_to() {
    let (fx, factory) = fx_with_driver().await;
    fixtures(&fx, PRS, REVIEWS);
    let _sol = fx.agent("sol").await;
    let terra = fx.agent("terra").await;
    let terra_driver = driver_for(&factory, &terra);
    let mut cfg = fx.cfg();
    cfg.deliver_to = vec!["sol".to_string(), "terra".to_string()];

    fx.collector(cfg).sweep_at(at()).await.expect("a sweep");

    for who in ["sol", "terra"] {
        let steps = fx.delivered(who).await;
        let subjects: Vec<&str> = steps
            .iter()
            .map(|s| s.body["subject"].as_str().expect("a subject"))
            .collect();
        assert_eq!(subjects.len(), 3, "{who}: {subjects:?}");
        assert!(
            subjects.contains(&"review requested: o/r#4 please review"),
            "{who}: {subjects:?}"
        );
        assert!(subjects.contains(&"o/r#12 a PR"), "{who}: {subjects:?}");
    }
    // Each agent got its OWN wake for the wake-class item — delivery is per agent, not shared.
    let terra_wakes = terra_driver.wakes.lock().clone();
    assert_eq!(terra_wakes.len(), 1, "{terra_wakes:?}");
    assert_eq!(
        terra_wakes[0].claimed,
        vec!["review requested: o/r#4 please review".to_string()]
    );
    assert_eq!(terra.inbox().pending(Target::NextWake).len(), 2);
}
