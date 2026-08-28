//! MERGE (track B → Phase 5): the ROUTER delivers.
//!
//! Track B built the collectors before `mail-router` existed, so each row named its own
//! `deliver_to` list and fanned out to it — with the note that "every delivery carries the refs
//! (`gh:o/r#12`) that `mail-router` will later route on". This is that later.
//!
//! What the collector does now is APPEND CITED MAIL and name a dedupe key; who receives it is the
//! router's decision, made per item on the refs. `deliver_to` survives as the fallback for a tree
//! with no `mail` seam, and `sweep.rs` is the suite that still exercises it.

mod common;

use std::collections::BTreeSet;

use bough_plugin_ledger::{AgentName, Ref};
use common::{at, Fx};

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

const PRS: &str = r#"[
  {"number":12,"title":"a PR","url":"https://example.invalid/12","updatedAt":"2026-08-01T00:00:00Z",
   "author":{"login":"andrey"},"state":"OPEN","isDraft":false}
]"#;

fn refs(items: &[&str]) -> BTreeSet<Ref> {
    items.iter().map(Ref::new).collect()
}

/// One repo, one PR, one source. `review_requests` off so the fixture set is one call.
fn one_pr(fx: &Fx) -> bough_plugin_collector_github::GithubCollectorConfig {
    fx.gh_fixture(&PR_ARGS, "json", PRS);
    let mut cfg = fx.cfg();
    cfg.review_requests = false;
    // THE POINT: with a router in the tree this list is not consulted at all.
    cfg.deliver_to = Vec::new();
    cfg
}

#[tokio::test]
async fn with_a_router_the_lane_that_matches_the_ref_gets_the_mail_and_deliver_to_is_not_read() {
    let fx = Fx::new().await;
    let cfg = one_pr(&fx);
    let _sol = fx.agent("sol").await;
    let _terra = fx.agent("terra").await;
    let mail = fx.mail();
    // `terra` watches the REPOSITORY; `sol` watches nothing. The scope ref is what a lane is
    // actually linked to — nobody links a lane to a pull request that does not exist yet — and
    // every collected item carries it beside its own `gh:o/r#12`.
    mail.link_ref(&AgentName::new("terra"), refs(&["repo:o/r"]), at())
        .await
        .expect("the link lands");

    let report = fx
        .collector_routing(cfg, mail)
        .sweep_at(at())
        .await
        .expect("a sweep");

    assert!(report.disabled.is_empty(), "{:?}", report.disabled);
    let delivered: usize = report.sources.iter().map(|(_, d, _, _)| d).sum();
    assert_eq!(delivered, 1, "{:?}", report.sources);

    assert_eq!(
        fx.delivered("terra").await.len(),
        1,
        "the lane whose routing ref matched got it"
    );
    assert!(
        fx.delivered("sol").await.is_empty(),
        "and the row's own `deliver_to` was never consulted"
    );
}

/// The at-least-once guard moved WITH the fan-out. A restart whose watermark write was lost
/// re-offers the same item; the router sees the lane already carries it and skips.
#[tokio::test]
async fn a_re_offered_item_is_deduped_by_the_router_and_reported_as_skipped() {
    let fx = Fx::new().await;
    let cfg = one_pr(&fx);
    let _terra = fx.agent("terra").await;
    let mail = fx.mail();
    mail.link_ref(&AgentName::new("terra"), refs(&["repo:o/r"]), at())
        .await
        .expect("the link lands");

    fx.collector_routing(cfg.clone(), mail.clone())
        .sweep_at(at())
        .await
        .expect("the first sweep");
    assert_eq!(fx.delivered("terra").await.len(), 1);

    // A LOST WATERMARK: a fresh state db over the same ledger is what a restart before the
    // watermark write looks like from the row's side.
    let mut again = cfg.clone();
    again.state_db = fx.dir.path().join("second.db");
    let report = fx
        .collector_routing(again, mail)
        .sweep_at(at())
        .await
        .expect("the second sweep");

    assert_eq!(
        fx.delivered("terra").await.len(),
        1,
        "the same item is not delivered twice"
    );
    let skipped: usize = report.sources.iter().map(|(_, _, s, _)| s).sum();
    assert_eq!(skipped, 1, "and the sweep SAYS it skipped it: {report:?}");
}

/// No router in the tree: the row falls back to its own `deliver_to`, which is what every case in
/// `sweep.rs` exercises. Stated here as one bullet so the fallback is a claim and not a leftover.
#[tokio::test]
async fn with_no_router_the_deliver_to_list_is_the_destination() {
    let fx = Fx::new().await;
    let mut cfg = one_pr(&fx);
    cfg.deliver_to = vec!["sol".to_string()];
    let _sol = fx.agent("sol").await;

    let report = fx.collector(cfg).sweep_at(at()).await.expect("a sweep");

    assert!(report.disabled.is_empty(), "{:?}", report.disabled);
    assert_eq!(fx.delivered("sol").await.len(), 1);
}
