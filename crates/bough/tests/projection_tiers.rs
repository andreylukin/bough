//! §17 Phase 4: the projection consumes REAL sealed tiers, through the launcher's own composition
//! path, and produces the same bytes on either ledger provider.
//!
//! The plugin-level golden (`projection-assembler/tests/goldens.rs`) drives the assembler directly.
//! This one boots a real tree — `bundles` → profile → the kernel's loader → the live `projection`
//! binding — once on `ledger-sqlite` and once on `ledger-memory`, seals the tier tree through the
//! live `ledger` binding, and compares the two texts against each other and against the golden.
//!
//! `$BOUGH_HOME` is process-global, so every test here holds `hello`'s process-wide test lock.

mod support;

use std::collections::BTreeSet;
use std::path::PathBuf;

use bough_plugin_hello::trace;
use bough_plugin_ledger::{
    AgentName, AgentRow, Append, Cite, Class, Ledger, LedgerHandle, NewRollup, Ref, RollupId,
    RollupKind, Seq, StepId, StepType, TrajId, WakeId,
};
use bough_plugin_projection::{AssembleRequest, Projection};
use bough_plugin_rollups::{Beneath, TierBlock, WindowRef};
use chrono::{DateTime, TimeZone, Utc};

/// The Phase-4 tree: a ledger and the assembler over it. `{LEDGER}` is the provider under test.
///
/// The three governance rows are deliberately absent: what is under test here is the PROJECTOR's
/// reading of sealed rows, and it must be the same whether or not a summarizer is loaded.
const TREE: &str = "\
- id: ledger
  plugin: {LEDGER}
  config: {LEDGER_CONFIG}
- id: projection
  plugin: projection-assembler
  config:
    budget_tokens: 160000
    headroom: 0.6
    tail_steps: 12
    tail_floor_steps: 3
    mail_newest_n: 5
    max_tiers: 3
    file_view_dir: !!expr 'bough_path(\"views\")'
";

fn tree(ledger: &str, config: &str) -> String {
    TREE.replace("{LEDGER}", ledger)
        .replace("{LEDGER_CONFIG}", config)
}

fn sqlite_tree() -> String {
    tree(
        "ledger-sqlite",
        "\n    path: !!expr 'bough_path(\"ledger.db\")'\n    busy_timeout_ms: 5000",
    )
}

fn memory_tree() -> String {
    tree("ledger-memory", "{}")
}

fn at() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).unwrap()
}

fn traj() -> TrajId {
    TrajId::new("t-sol")
}

/// The one fixture: an agent, a pin, twelve verbatim steps, mail, and a two-level tier tree.
/// Every id is supplied, so the bytes are stable (P1-D6).
async fn seed(ledger: &LedgerHandle) {
    ledger
        .0
        .put_agent(AgentRow {
            name: AgentName::new("sol"),
            traj: traj(),
            routing_refs: BTreeSet::from([Ref::new("gh:bough/rebuild#1")]),
            wake_classes: BTreeSet::from(["ordinary".to_string()]),
            model_override: None,
            tick_floor: None,
            digest_rollup: None,
        })
        .await
        .expect("agents is mutable config");

    append(
        ledger,
        "p1",
        "w1",
        "pin/set",
        Class::Thought,
        serde_json::json!({
            "title": "gates before commit",
            "text": "`make gates` must be green before every commit",
            "supersedes": [],
        }),
        Vec::new(),
    )
    .await;
    for n in 1..=12 {
        append(
            ledger,
            &format!("s{n}"),
            if n % 3 == 0 { "w2" } else { "w1" },
            "claim/proposed",
            Class::Thought,
            serde_json::json!({
                "claim": format!("s{n}"),
                "kind": "observation",
                "title": format!("verbatim step number {n}"),
                "body": format!("verbatim step number {n}"),
            }),
            Vec::new(),
        )
        .await;
    }
    append(
        ledger,
        "m1",
        "w1",
        "mail/delivered",
        Class::Evidence,
        serde_json::json!({
            "class": "ordinary", "from": "andrey",
            "subject": "look at the tiers band", "summary": "look at the tiers band",
            "refs": [],
        }),
        vec![Cite {
            r#ref: Ref::new("andrey"),
            url: None,
        }],
    )
    .await;

    tier(
        ledger,
        "r-t1a",
        1,
        1,
        6,
        "sol opened the trajectory and worked through the first six steps.",
        Beneath::Raw {
            steps: (1..=6).map(|n| StepId::new(format!("s{n}"))).collect(),
        },
    )
    .await;
    tier(
        ledger,
        "r-t1b",
        1,
        7,
        12,
        "sol finished the run and left the tree green.",
        Beneath::Raw {
            steps: (7..=12).map(|n| StepId::new(format!("s{n}"))).collect(),
        },
    )
    .await;
    tier(
        ledger,
        "r-t2",
        2,
        1,
        12,
        "one run: sol worked the trajectory end to end and kept the gates green.",
        Beneath::Blocks {
            rollups: vec![RollupId::new("r-t1a"), RollupId::new("r-t1b")],
        },
    )
    .await;
}

async fn append(
    ledger: &LedgerHandle,
    id: &str,
    wake: &str,
    kind: &str,
    class: Class,
    body: serde_json::Value,
    cites: Vec<Cite>,
) {
    ledger
        .0
        .append(Append {
            traj: traj(),
            wake: WakeId::new(wake),
            kind: StepType::new(kind),
            class,
            body,
            cites,
            at: at(),
            id: Some(StepId::new(id)),
        })
        .await
        .unwrap_or_else(|e| panic!("append {id} ({kind}): {e}"));
}

/// Seal one tier block with the vocabulary `bough-plugin-rollups` owns — the shape a real
/// `rollups-summarizer` pass writes.
async fn tier(
    ledger: &LedgerHandle,
    id: &str,
    tier: u8,
    from: u64,
    to: u64,
    text: &str,
    beneath: Beneath,
) {
    let block = TierBlock {
        text: text.to_string(),
        themes: Vec::new(),
        beneath,
        evidence: (from..=to).map(|n| StepId::new(format!("s{n}"))).collect(),
        windows: vec![WindowRef {
            from_seq: Seq(from),
            to_seq: Seq(to),
            cut: bough_plugin_rollups::Cut::Gap,
        }],
        tier,
        prompt_ver: "r4.1".to_string(),
    };
    ledger
        .0
        .seal_rollup(NewRollup {
            id: Some(RollupId::new(id)),
            traj: traj(),
            kind: RollupKind::Tier,
            tier,
            from_seq: Seq(from),
            to_seq: Seq(to),
            src_trajs: vec![traj()],
            body: serde_json::to_value(&block).expect("a tier block serializes"),
            notable_refs: BTreeSet::new(),
            prompt_ver: "r4.1".to_string(),
            sealed_at: at(),
        })
        .await
        .unwrap_or_else(|e| panic!("seal {id}: {e}"));
}

/// Boot the tree, seed it, and assemble through the LIVE `projection` binding.
async fn assembled(tree_yaml: &str) -> String {
    let (kernel, dir) = support::boot_with(tree_yaml).await;
    let ledger = kernel
        .root()
        .peek_live::<Ledger>()
        .expect("ledger is bound")
        .as_ref()
        .clone();
    seed(&LedgerHandle(ledger.0.clone())).await;

    let projection = kernel
        .root()
        .peek_live::<Projection>()
        .expect("projection is bound")
        .as_ref()
        .clone();
    let text = projection
        .0
        .assemble(&AssembleRequest {
            as_of: None,
            agent: AgentName::new("sol"),
            wake: None,
            at: at(),
            budget: None,
        })
        .await
        .expect("the projection assembles")
        .to_text();
    kernel.shutdown().await;
    drop(dir);
    text
}

fn golden_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/projection_tiers.txt")
}

fn assert_golden(got: &str) {
    let path = golden_path();
    if std::env::var("UPDATE_GOLDEN").ok().as_deref() == Some("1") {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, got.as_bytes()).unwrap();
        return;
    }
    let want = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "golden {} is missing ({e}); rerun with UPDATE_GOLDEN=1 to write it",
            path.display()
        )
    });
    assert_eq!(got, want, "the booted-tree tier golden drifted");
}

#[tokio::test]
async fn the_golden_matches_on_both_ledger_providers() {
    let _guard = trace::test_lock();
    let sqlite = assembled(&sqlite_tree()).await;
    let memory = assembled(&memory_tree()).await;
    assert_eq!(
        sqlite, memory,
        "the projection over real sealed tiers differs between ledger-sqlite and ledger-memory"
    );
    // The claim the golden is ABOUT, so `UPDATE_GOLDEN=1` cannot quietly rewrite the rule.
    assert!(
        sqlite
            .find("## Tier 2 summary")
            .expect("a coarse tier band")
            < sqlite.find("## Tier 1 summary").expect("a fine tier band"),
        "the tiers band is not coarse to fine:\n{sqlite}"
    );
    assert!(
        sqlite.contains("`make gates` must be green before every commit"),
        "the pin still rides the projection verbatim over a sealed range:\n{sqlite}"
    );
    assert_golden(&sqlite);
}
