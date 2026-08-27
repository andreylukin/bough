//! V2's filter half (WP-2): the five dimensions compose, and narrowing one never widens the result.

use std::collections::BTreeSet;
use std::sync::Arc;

use bough_plugin_ledger::{Class, Ref};
use bough_plugin_tui_timeline::testing::{instant, row};
use bough_plugin_tui_timeline::{parse_filter, timeline, Filter, Row};
use chrono::{DateTime, Utc};

fn now() -> DateTime<Utc> {
    instant("13:00:00")
}

/// A fixture with something in every dimension: two agents, two kinds, two classes, two refs and
/// an hour of wall clock.
fn corpus() -> Vec<Row> {
    let mut rows = vec![
        row("sol", "t1", 1, "wake/start", "12:00:00"),
        row("sol", "t1", 2, "tool/call", "12:10:00"),
        row("sol", "t1", 3, "tool/call", "12:40:00"),
        row("terra", "t2", 1, "wake/start", "12:05:00"),
        row("terra", "t2", 2, "tool/call", "12:20:00"),
        row("terra", "t2", 3, "claim/made", "12:50:00"),
    ];
    for r in rows.iter_mut() {
        // Every `tool/call` carries the PR ref; the claim is the only piece of evidence.
        if r.step.kind.as_str() == "tool/call" {
            r.step.refs = Arc::new([Ref::new("pr/1204")].into_iter().collect());
        }
        if r.step.kind.as_str() == "claim/made" {
            r.step.class = Class::Evidence;
            r.step.refs = Arc::new([Ref::new("gh:o/r#12")].into_iter().collect());
        }
    }
    rows
}

fn ids(rows: &[Row]) -> Vec<String> {
    rows.iter().map(|r| r.step.id.to_string()).collect()
}

#[test]
fn agent_and_ref_and_type_and_time_compose() {
    let rows = corpus();
    let all = timeline(&rows, &Filter::default(), 100);
    assert_eq!(all.len(), 6, "the unfiltered timeline is the whole corpus");

    // One dimension at a time, each over the SAME corpus.
    let by_agent = timeline(&rows, &parse_filter("agent:sol", now()).unwrap(), 100);
    assert_eq!(ids(&by_agent), ["t1-1", "t1-2", "t1-3"]);

    let by_ref = timeline(&rows, &parse_filter("ref:pr/1204", now()).unwrap(), 100);
    assert_eq!(ids(&by_ref), ["t1-2", "t2-2", "t1-3"]);

    let by_kind = timeline(&rows, &parse_filter("type:tool/call", now()).unwrap(), 100);
    assert_eq!(ids(&by_kind), ["t1-2", "t2-2", "t1-3"]);

    let by_time = timeline(
        &rows,
        &parse_filter(
            "since:2026-08-27T12:15:00Z until:2026-08-27T12:45:00Z",
            now(),
        )
        .unwrap(),
        100,
    );
    assert_eq!(ids(&by_time), ["t2-2", "t1-3"]);

    let by_class = timeline(&rows, &parse_filter("class:evidence", now()).unwrap(), 100);
    assert_eq!(ids(&by_class), ["t2-3"]);

    // …and all of them at once is the INTERSECTION, not a re-query: the one `sol` tool call that
    // carries the PR ref inside the window.
    let composed = timeline(
        &rows,
        &parse_filter(
            "agent:sol ref:pr/1204 type:tool/call class:thought \
             since:2026-08-27T12:15:00Z until:2026-08-27T12:45:00Z",
            now(),
        )
        .unwrap(),
        100,
    );
    assert_eq!(ids(&composed), ["t1-3"]);

    // The composition really is the intersection of the five one-dimension answers.
    let mut expected: BTreeSet<String> = ids(&by_agent).into_iter().collect();
    for other in [ids(&by_ref), ids(&by_kind), ids(&by_time)] {
        let other: BTreeSet<String> = other.into_iter().collect();
        expected = expected.intersection(&other).cloned().collect();
    }
    assert_eq!(
        ids(&composed).into_iter().collect::<BTreeSet<_>>(),
        expected
    );
}

#[test]
fn narrowing_one_dimension_never_widens_the_result() {
    let rows = corpus();
    // Each step adds ONE conjunct to the one before it. Every result must be a subset of the
    // previous one — a filter that ever grew the row set would be a filter nobody could trust.
    let ladder = [
        "",
        "agent:sol agent:terra",
        "agent:sol agent:terra type:tool/call",
        "agent:sol agent:terra type:tool/call ref:pr/1204",
        "agent:sol agent:terra type:tool/call ref:pr/1204 since:2026-08-27T12:15:00Z",
        "agent:sol agent:terra type:tool/call ref:pr/1204 since:2026-08-27T12:15:00Z \
         until:2026-08-27T12:45:00Z",
    ];
    let mut previous: Option<BTreeSet<String>> = None;
    for q in ladder {
        let f = parse_filter(q, now()).unwrap_or_else(|e| panic!("{q:?}: {e}"));
        let got: BTreeSet<String> = ids(&timeline(&rows, &f, 100)).into_iter().collect();
        if let Some(prev) = &previous {
            assert!(
                got.is_subset(prev),
                "{q:?} widened the result: {got:?} is not a subset of {prev:?}"
            );
        }
        previous = Some(got);
    }
    assert_eq!(
        previous.expect("the ladder ran"),
        ["t1-3".to_string(), "t2-2".to_string()].into()
    );

    // Narrowing a dimension that is ALREADY narrow is still a narrowing: adding a second member
    // to a disjunction may widen it, but adding a whole dimension never can.
    let one = parse_filter("agent:sol", now()).unwrap();
    let two = parse_filter("agent:sol type:tool/call", now()).unwrap();
    let one: BTreeSet<String> = ids(&timeline(&rows, &one, 100)).into_iter().collect();
    let two: BTreeSet<String> = ids(&timeline(&rows, &two, 100)).into_iter().collect();
    assert!(two.is_subset(&one) && two.len() < one.len());
}
