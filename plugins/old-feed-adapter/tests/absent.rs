//! Invariant under test: an absent, unreadable or column-short old feed is a DISABLED SOURCE and
//! one logged line — never a panic and never a boot failure. `~/.jungler/jungler.db` does not
//! exist on this machine, so this is the case the row actually runs in today (§14, V7).

use crate::common;

use bough_plugin_old_feed_adapter::{probe, FeedProbe};
use common::{at, Fx, Which};

#[tokio::test]
async fn an_absent_jungler_db_activates_the_row_and_logs_one_line() {
    let fx = Fx::new(Which::Memory).await;
    // No `standard_jungler`: the file is simply not there.
    let _sol = fx.sol_agent().await;
    assert_eq!(probe(&fx.jungler_db), FeedProbe::Missing);

    // ACTIVATION: `open` is the only thing `apply` can fail on before providing the key.
    let feed = fx.feed(fx.cfg());
    let line = feed.disabled_line().expect("one line");
    assert!(!line.contains('\n'), "one line, not a paragraph: {line}");
    assert!(
        line.contains("jungler.events") && line.contains("absent"),
        "{line}"
    );

    let status = feed.sweep_at(at()).await.expect("a sweep is still fine");
    assert!(status.sources.is_empty(), "nothing to sweep");
    assert_eq!(status.disabled.len(), 4, "three jungler sources + bough.db");
    assert!(fx.all_steps().await.is_empty(), "and nothing was appended");
}

#[tokio::test]
async fn an_unreadable_jungler_db_activates_the_row_and_logs_one_line() {
    let fx = Fx::new(Which::Memory).await;
    std::fs::write(&fx.jungler_db, b"this is not a sqlite database").expect("a junk file");
    let _sol = fx.sol_agent().await;
    assert!(matches!(probe(&fx.jungler_db), FeedProbe::Unreadable(_)));

    let feed = fx.feed(fx.cfg());
    let line = feed.disabled_line().expect("one line");
    assert!(!line.contains('\n'), "one line, not a paragraph: {line}");
    assert!(line.contains("unreadable"), "{line}");

    let status = feed.sweep_at(at()).await.expect("a sweep is still fine");
    assert!(status.sources.is_empty());
    assert!(fx.all_steps().await.is_empty());
}

#[tokio::test]
async fn a_missing_required_column_disables_that_source_only() {
    let fx = Fx::new(Which::Memory).await;
    // `events` has no timestamp — a required column — while `nodes` is whole.
    let conn = rusqlite::Connection::open(&fx.jungler_db).expect("a fixture db");
    conn.execute_batch(
        "CREATE TABLE events (id INTEGER PRIMARY KEY, kind TEXT, subject TEXT, body TEXT);
         CREATE TABLE nodes (id INTEGER PRIMARY KEY, kind TEXT, title TEXT, summary TEXT,
                             updated_at INTEGER, lane TEXT);
         INSERT INTO events VALUES (1, 'pr', 'PR #4 opened', 'a body');
         INSERT INTO nodes VALUES (1, 'lane', 'the rebuild', 'under way', 1700000000000, 'rebuild');",
    )
    .expect("the fixture schema");
    drop(conn);
    let _sol = fx.sol_agent().await;

    let FeedProbe::Present {
        missing_columns, ..
    } = probe(&fx.jungler_db)
    else {
        panic!("the db is present");
    };
    assert_eq!(missing_columns, vec!["events.at".to_string()]);

    let feed = fx.feed(fx.cfg());
    let status = feed.sweep_at(at()).await.expect("a sweep");

    assert!(
        status
            .disabled
            .iter()
            .any(|(s, why)| s == "jungler.events" && why.contains("events.at")),
        "{:?}",
        status.disabled
    );
    assert!(
        status.sources.iter().any(|(s, _, _)| s == "jungler.nodes"),
        "the whole source still swept: {:?}",
        status.sources
    );
    assert!(
        fx.steps_of_kind("mail/delivered").await.is_empty(),
        "the short source delivered nothing"
    );
    assert_eq!(
        fx.ledger
            .0
            .rollups(&Default::default())
            .await
            .expect("a read")
            .len(),
        1,
        "and the whole one sealed its block"
    );
}
