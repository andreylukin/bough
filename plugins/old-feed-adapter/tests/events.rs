//! Invariant under test: jungler events become CITED MAIL exactly once, whatever the restart or
//! the crash. The watermark advances after the delivery it covers; the ref guard is what makes a
//! rolled-back watermark harmless (§2.6, V7).

use crate::common;

use bough_plugin_ledger::Class;
use bough_plugin_old_feed_adapter::state::{Watermark, WatermarkStore};
use common::{at, Fx, Which};

#[tokio::test]
async fn events_become_cited_mail_on_the_configured_agent() {
    let fx = Fx::new(Which::Memory).await;
    common::standard_jungler(&fx.jungler_db);
    let _sol = fx.sol_agent().await;

    let feed = fx.feed(fx.cfg());
    let status = feed.sweep_at(at()).await.expect("a sweep");
    assert_eq!(
        status
            .sources
            .iter()
            .find(|(s, _, _)| s == "jungler.events")
            .map(|(_, n, _)| *n),
        Some(3)
    );

    let mail = fx.steps_of_kind("mail/delivered").await;
    assert_eq!(mail.len(), 3, "one step per event");
    for (i, step) in mail.iter().enumerate() {
        assert_eq!(step.class, Class::Evidence, "delivered mail is evidence");
        let want = format!("jungler:event:{}", i + 1);
        assert!(
            step.cites.iter().any(|c| c.r#ref.as_str() == want),
            "step {} cites {want}: {:?}",
            step.id,
            step.cites
        );
    }
    // The url of the first event rides its cite, so the mail is dereferenceable.
    assert_eq!(mail[0].cites[0].url.as_deref(), Some("https://x/4"));

    // And it is really in the inbox, not only in the chain.
    let queued = fx
        .ledger
        .0
        .unconsumed_mail(&common::traj())
        .await
        .expect("a read");
    assert_eq!(queued.len(), 3, "every delivered event is queued mail");
}

#[tokio::test]
async fn the_watermark_advances_past_the_last_delivered_row() {
    let fx = Fx::new(Which::Memory).await;
    common::standard_jungler(&fx.jungler_db);
    let _sol = fx.sol_agent().await;

    let feed = fx.feed(fx.cfg());
    feed.sweep_at(at()).await.expect("a sweep");

    let store = WatermarkStore::open(&fx.state_db).expect("the adapter's own db");
    assert_eq!(
        store.get("jungler.events").expect("a read").last_row,
        3,
        "the watermark is the last row the sweep covered"
    );
}

#[tokio::test]
async fn a_restart_delivers_nothing_twice() {
    let fx = Fx::new(Which::Memory).await;
    common::standard_jungler(&fx.jungler_db);
    let _sol = fx.sol_agent().await;

    fx.feed(fx.cfg()).sweep_at(at()).await.expect("a sweep");
    // A RESTART: a second handle over the same state db, exactly as a reboot builds one.
    let status = fx.feed(fx.cfg()).sweep_at(at()).await.expect("a re-sweep");

    assert_eq!(
        status
            .sources
            .iter()
            .find(|(s, _, _)| s == "jungler.events")
            .map(|(_, n, _)| *n),
        Some(0),
        "the watermark already covers every row"
    );
    assert_eq!(fx.steps_of_kind("mail/delivered").await.len(), 3);
}

#[tokio::test]
async fn a_crash_between_the_append_and_the_watermark_still_delivers_once() {
    let fx = Fx::new(Which::Memory).await;
    common::standard_jungler(&fx.jungler_db);
    let _sol = fx.sol_agent().await;

    fx.feed(fx.cfg()).sweep_at(at()).await.expect("a sweep");

    // THE CRASH: the steps landed, the watermark write did not. Rolled back by hand.
    let store = WatermarkStore::open(&fx.state_db).expect("the adapter's own db");
    store
        .set("jungler.events", Watermark::default(), at())
        .expect("a rollback");
    drop(store);

    let status = fx.feed(fx.cfg()).sweep_at(at()).await.expect("a re-sweep");
    assert_eq!(
        status
            .sources
            .iter()
            .find(|(s, _, _)| s == "jungler.events")
            .map(|(_, n, _)| *n),
        Some(0),
        "the ref guard drops every row the ledger already carries"
    );
    assert_eq!(
        fx.steps_of_kind("mail/delivered").await.len(),
        3,
        "still exactly one step per event"
    );
    assert!(
        bough_plugin_old_feed_adapter::invariant::check_steps(&fx.all_steps().await).is_ok(),
        "no duplicate `jungler:event:` ref across `mail/delivered` steps"
    );
}
