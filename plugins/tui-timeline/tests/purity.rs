//! V2's purity half (WP-2): the same ledger yields the same timeline, twice.
//!
//! The rows come from a REAL ledger (`ledger-memory`, the behavioural twin of `ledger-sqlite`), so
//! what is being checked is the whole read-then-render path and not a hand-built `Vec<Row>`: two
//! reads of an unchanged ledger produce byte-identical rendered lines, in the same order, with the
//! same hit ids. A timeline that reordered itself between two frames would be a surface nobody
//! could use to say what happened when.

use bough_kernel::{Context, KernelCore};
use bough_plugin_ledger::{Append, Class, LedgerHandle, StepType, TrajId, WakeId};
use bough_plugin_ledger_memory::store::MemoryStore;
use bough_plugin_tui_timeline::testing::{config, instant};
use bough_plugin_tui_timeline::{hit_of, line, load_rows, parse_filter, timeline, Filter, Row};
use chrono::{DateTime, Utc};

fn ledger() -> LedgerHandle {
    LedgerHandle(MemoryStore::new(Context::root(KernelCore::new())))
}

async fn put(l: &LedgerHandle, traj: &str, kind: &str, at: DateTime<Utc>, index: u32) {
    l.0.append(Append {
        traj: TrajId::new(traj),
        wake: WakeId::new(format!("w-{traj}")),
        kind: StepType::new(kind),
        class: Class::Thought,
        body: match kind {
            "step/start" => serde_json::json!({ "index": index }),
            _ => serde_json::json!({ "urgency": "coalesced" }),
        },
        cites: vec![],
        at,
        id: None,
    })
    .await
    .expect("the step appends");
}

/// Two agents, interleaved in wall-clock time, written in an order that is NOT the timeline's.
async fn seeded() -> LedgerHandle {
    let l = ledger();
    l.0.put_agent(bough_plugin_ledger::AgentRow {
        name: bough_plugin_ledger::AgentName::new("sol"),
        traj: TrajId::new("t1"),
        routing_refs: Default::default(),
        wake_classes: Default::default(),
        model_override: None,
        tick_floor: None,
        digest_rollup: None,
    })
    .await
    .expect("sol");
    l.0.put_agent(bough_plugin_ledger::AgentRow {
        name: bough_plugin_ledger::AgentName::new("terra"),
        traj: TrajId::new("t2"),
        routing_refs: Default::default(),
        wake_classes: Default::default(),
        model_override: None,
        tick_floor: None,
        digest_rollup: None,
    })
    .await
    .expect("terra");

    put(&l, "t1", "wake/start", instant("12:00:00"), 0).await;
    put(&l, "t2", "wake/start", instant("12:00:05"), 0).await;
    put(&l, "t1", "step/start", instant("12:00:10"), 1).await;
    put(&l, "t2", "step/start", instant("12:00:02"), 1).await;
    put(&l, "t1", "step/start", instant("12:00:20"), 2).await;
    l
}

/// The screen: every visible row as the line it paints and the hit it records.
fn screen(rows: &[Row], f: &Filter) -> Vec<(String, String)> {
    timeline(rows, f, 100)
        .iter()
        .map(|r| (line(r, 120, "%H:%M:%S"), hit_of(r).as_str().to_string()))
        .collect()
}

#[tokio::test]
async fn the_same_ledger_yields_the_same_timeline_twice() {
    let l = seeded().await;
    let cfg = config();

    let a = load_rows(&l, &cfg, &Filter::default())
        .await
        .expect("the read");
    let b = load_rows(&l, &cfg, &Filter::default())
        .await
        .expect("the read again");
    assert_eq!(a, b, "an unchanged ledger reads the same twice");

    let first = screen(&a.rows, &Filter::default());
    let second = screen(&b.rows, &Filter::default());
    assert_eq!(first, second, "the same rows render the same screen");
    assert_eq!(first.len(), 5);
    // …and that screen is in wall-clock order, which is the whole claim of the surface.
    let times: Vec<&str> = first.iter().map(|(l, _)| &l[..8]).collect();
    assert_eq!(
        times,
        ["12:00:00", "12:00:02", "12:00:05", "12:00:10", "12:00:20"],
        "the write order was not this, so the order is the timeline's and not the ledger's"
    );

    // A filtered screen is stable too, and it is a subset of the unfiltered one.
    let f = parse_filter("agent:sol type:step/start", instant("13:00:00")).expect("well-formed");
    let filtered = screen(&a.rows, &f);
    assert_eq!(filtered, screen(&b.rows, &f));
    for row in &filtered {
        assert!(
            first.contains(row),
            "{row:?} is not on the unfiltered screen"
        );
    }
    assert_eq!(filtered.len(), 2);

    // The rows themselves are untouched by rendering: `load_rows` is the only impure call here,
    // and calling it once more after everything above still agrees.
    let c = load_rows(&l, &cfg, &Filter::default())
        .await
        .expect("thrice");
    assert_eq!(screen(&c.rows, &Filter::default()), first);
    assert!(
        !c.windowed,
        "the window was never full: {} rows",
        c.rows.len()
    );
}
