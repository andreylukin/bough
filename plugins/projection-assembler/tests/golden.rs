//! Invariant: assembly is DETERMINISTIC and PROVIDER-INDEPENDENT (§5, §17 Phase 1). Every case
//! here is run against BOTH ledger providers and then against each other, byte for byte. The
//! goldens are plain `.txt` files compared with `assert_eq!` and rewritten by `UPDATE_GOLDEN=1`.
//!
//! Linking both providers is the one sanctioned exception to "a consumer never depends on a
//! provider" (P1-D1): comparing them IS the test.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use bough_kernel::{Context, KernelCore};
use bough_plugin_ledger::{
    AgentName, AgentRow, Append, Cite, Class, LedgerHandle, NewRollup, Ref, RollupId, RollupKind,
    Seq, StepId, StepType, TrajId, WakeId,
};
use bough_plugin_ledger_memory::store::MemoryStore;
use bough_plugin_ledger_sqlite::{store::SqliteStore, SqliteConfig};
use bough_plugin_projection::{
    AssembleRequest, DropPriority, Place, Position, ProjectionError, Projector, SectionBody,
    SectionId, SectionRender, SectionRequest, SectionScope, SectionSpec, Slot,
};
use bough_plugin_projection_assembler::{Assembler, AssemblerConfig};
use chrono::{DateTime, TimeZone, Utc};

// ---- the fixture ------------------------------------------------------------------------------

fn at() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).unwrap()
}

fn traj() -> TrajId {
    TrajId::new("t-sol")
}

fn agent() -> AgentName {
    AgentName::new("sol")
}

/// Which provider a case is running against. Both must produce the same bytes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Which {
    Sqlite,
    Memory,
}

struct Harness {
    ctx: Context,
    ledger: LedgerHandle,
    _dir: Option<tempfile::TempDir>,
}

impl Harness {
    fn open(which: Which) -> Harness {
        let ctx = Context::root(KernelCore::new());
        match which {
            Which::Memory => Harness {
                ledger: LedgerHandle(MemoryStore::new(ctx.clone())),
                ctx,
                _dir: None,
            },
            Which::Sqlite => {
                let dir = tempfile::tempdir().expect("a temp dir");
                let cfg = SqliteConfig {
                    path: dir.path().join("ledger.db"),
                    busy_timeout_ms: 5_000,
                };
                let store = SqliteStore::open(&cfg, ctx.clone()).expect("a fresh db opens");
                Harness {
                    ledger: LedgerHandle(store),
                    ctx,
                    _dir: Some(dir),
                }
            }
        }
    }

    async fn append(
        &self,
        id: &str,
        wake: &str,
        kind: &str,
        class: Class,
        body: serde_json::Value,
        cites: Vec<Cite>,
    ) -> StepId {
        self.ledger
            .0
            .append(Append {
                traj: traj(),
                wake: WakeId::new(wake),
                kind: StepType::new(kind),
                class,
                body,
                cites,
                at: at(),
                // Supplied so the goldens are byte-stable (P1-D6).
                id: Some(StepId::new(id)),
            })
            .await
            .unwrap_or_else(|e| panic!("append {id} ({kind}): {e}"))
            .id
    }

    async fn note(&self, id: &str, wake: &str, text: &str) -> StepId {
        self.append(
            id,
            wake,
            "step/start",
            Class::Thought,
            serde_json::json!({ "index": text.len() as u32 }),
            Vec::new(),
        )
        .await
    }

    async fn pin(&self, id: &str, title: &str, text: &str, supersedes: &[&str]) -> StepId {
        self.append(
            id,
            "w1",
            "pin/set",
            Class::Thought,
            serde_json::json!({
                "title": title, "text": text,
                "supersedes": supersedes.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            }),
            Vec::new(),
        )
        .await
    }

    async fn retire(&self, id: &str, retires: &[&str]) -> StepId {
        self.append(
            id,
            "w1",
            "pin/retire",
            Class::Thought,
            serde_json::json!({
                "retires": retires.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
                "reason": "withdrawn",
            }),
            Vec::new(),
        )
        .await
    }

    async fn mail(&self, id: &str, class: &str, from: &str, subject: &str) -> StepId {
        self.append(
            id,
            "w1",
            "mail/delivered",
            Class::Evidence,
            serde_json::json!({
                "class": class, "from": from, "subject": subject,
                "summary": subject, "refs": [],
            }),
            vec![Cite {
                r#ref: Ref::new(from),
                url: None,
            }],
        )
        .await
    }

    async fn seal(&self, id: &str, kind: RollupKind, tier: u8, from: u64, to: u64, text: &str) {
        self.ledger
            .0
            .seal_rollup(NewRollup {
                id: Some(RollupId::new(id)),
                traj: traj(),
                kind,
                tier,
                from_seq: Seq(from),
                to_seq: Seq(to),
                src_trajs: vec![traj()],
                body: serde_json::Value::String(text.to_string()),
                notable_refs: BTreeSet::new(),
                prompt_ver: "p1".to_string(),
                sealed_at: at(),
            })
            .await
            .unwrap_or_else(|e| panic!("seal {id}: {e}"));
    }

    async fn put_agent(&self, digest: Option<&str>) {
        self.ledger
            .0
            .put_agent(AgentRow {
                name: agent(),
                traj: traj(),
                routing_refs: BTreeSet::from([Ref::new("gh:bough/rebuild#1")]),
                wake_classes: BTreeSet::from(["ordinary".to_string()]),
                model_override: None,
                tick_floor: None,
                digest_rollup: digest.map(RollupId::new),
            })
            .await
            .expect("agents is mutable config");
    }
}

fn cfg(budget: usize) -> AssemblerConfig {
    AssemblerConfig {
        budget_tokens: budget,
        headroom: 1.0,
        tail_steps: 12,
        tail_floor_steps: 3,
        mail_newest_n: 2,
        max_tiers: 3,
        file_view_dir: PathBuf::from("/unused-by-golden"),
    }
}

/// A contributed section with a constant body.
struct Fixed(&'static str, &'static str);

#[async_trait::async_trait]
impl SectionRender for Fixed {
    async fn render(&self, _r: &SectionRequest) -> Result<Option<SectionBody>, ProjectionError> {
        Ok(Some(SectionBody {
            title: self.0.to_string(),
            body: format!("{}\n", self.1),
            cites: Default::default(),
        }))
    }
}

fn spec(
    id: &str,
    scope: SectionScope,
    who: Option<&str>,
    title: &'static str,
    body: &'static str,
) -> SectionSpec {
    SectionSpec {
        id: SectionId::new(id),
        position: Position {
            slot: Slot::Identity,
            place: Place::After,
        },
        scope,
        agent: who.map(AgentName::new),
        priority: DropPriority::Never,
        render: Arc::new(Fixed(title, body)),
    }
}

// ---- the cases --------------------------------------------------------------------------------

/// Every golden case: a name, a seeding routine, a config, and the sections it contributes.
async fn run(case: &str, which: Which) -> String {
    let h = Harness::open(which);
    let (config, specs) = seed(case, &h).await;
    let assembler = Assembler::new(Arc::new(config), h.ledger.clone(), h.ctx.clone());
    for s in specs {
        // Leaked on purpose: the section must outlive the assembly, and the harness dies with it.
        std::mem::forget(assembler.section(s).expect("a fresh section registers"));
    }
    let out = assembler
        .assemble(&AssembleRequest {
            agent: agent(),
            wake: None,
            at: at(),
            budget: None,
        })
        .await
        .unwrap_or_else(|e| panic!("assemble {case}: {e}"));
    out.to_text()
}

async fn seed(case: &str, h: &Harness) -> (AssemblerConfig, Vec<SectionSpec>) {
    match case {
        "fixed_section_order" => {
            h.put_agent(Some("r-digest")).await;
            h.seal(
                "r-digest",
                RollupKind::Digest,
                0,
                1,
                4,
                "sol keeps the tree green.",
            )
            .await;
            h.pin("p1", "gates before commit", "make gates must be green", &[])
                .await;
            h.note("s1", "w1", "one").await;
            h.note("s2", "w2", "two").await;
            h.note("s3", "w1", "three").await;
            h.mail("m1", "ordinary", "andrey", "look at WP-5").await;
            h.seal("r-t1", RollupKind::Tier, 1, 1, 2, "fine tier over 1..2")
                .await;
            h.seal("r-t2", RollupKind::Tier, 2, 1, 4, "coarse tier over 1..4")
                .await;
            (cfg(100_000), vec![])
        }
        "degradation_order" => {
            h.put_agent(None).await;
            h.pin(
                "p1",
                "a standing rule",
                "the rule, at length, in the pin's own words",
                &[],
            )
            .await;
            for n in 1..=10 {
                h.note(
                    &format!("s{n}"),
                    if n % 2 == 0 { "w2" } else { "w1" },
                    "step",
                )
                .await;
            }
            h.seal(
                "r-t1",
                RollupKind::Tier,
                1,
                1,
                5,
                "the fine tier goes first",
            )
            .await;
            h.seal(
                "r-t3",
                RollupKind::Tier,
                3,
                1,
                10,
                "the coarse tier survives it",
            )
            .await;
            (cfg(120), vec![])
        }
        "pins_collapse" => {
            h.put_agent(None).await;
            for n in 1..=4 {
                h.pin(
                    &format!("p{n}"),
                    &format!("rule {n}"),
                    "a body long enough that collapsing it actually saves tokens, repeatedly said",
                    &[],
                )
                .await;
            }
            (cfg(60), vec![])
        }
        "mail_headers_collapse" => {
            h.put_agent(None).await;
            for n in 1..=6 {
                let class = if n % 3 == 0 { "wake" } else { "ordinary" };
                h.mail(
                    &format!("m{n}"),
                    class,
                    "andrey",
                    &format!("subject number {n}, stated at some length"),
                )
                .await;
            }
            (cfg(40), vec![])
        }
        "agent_section_shadows_global" => {
            h.put_agent(None).await;
            h.note("s1", "w1", "one").await;
            (
                cfg(100_000),
                vec![
                    spec(
                        "about",
                        SectionScope::Global,
                        None,
                        "About",
                        "the global about-line",
                    ),
                    spec(
                        "about",
                        SectionScope::Agent,
                        Some("sol"),
                        "About",
                        "sol's own about-line",
                    ),
                ],
            )
        }
        "pins_superseded" => {
            // §3: a pin rides every projection verbatim regardless of age, and the projector
            // honors the supersession/retirement markers rather than the raw pin/set rows.
            h.put_agent(None).await;
            h.pin(
                "p-keep",
                "gates",
                "run `make gates` before every commit",
                &[],
            )
            .await;
            h.pin("p-old", "budget", "assembly under 50ms", &[]).await;
            h.pin("p-gone", "temporary", "until friday", &[]).await;
            // Enough steps that `p-old` is far outside the verbatim tail window (tail_steps = 12).
            for n in 1..=40 {
                h.note(&format!("s{n}"), "w1", "step").await;
            }
            // Re-accepting the requirement supersedes its old pin.
            h.pin("p-new", "budget", "assembly under 40ms", &["p-old"])
                .await;
            h.retire("r-gone", &["p-gone"]).await;
            (cfg(100_000), vec![])
        }
        "zero_rollups" => {
            // Phase 4 produces tiers and digests. With none, the bands render NOTHING — not an
            // empty header — and assembly still succeeds.
            h.put_agent(None).await;
            h.pin("p1", "the one pin", "still standing", &[]).await;
            h.note("s1", "w1", "one").await;
            (cfg(100_000), vec![])
        }
        other => panic!("unknown golden case `{other}`"),
    }
}

const CASES: &[&str] = &[
    "fixed_section_order",
    "degradation_order",
    "pins_collapse",
    "mail_headers_collapse",
    "agent_section_shadows_global",
    "zero_rollups",
    "pins_superseded",
];

// ---- the golden mechanism ---------------------------------------------------------------------

fn golden_path(case: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(format!("{case}.txt"))
}

/// Compare against the golden, or rewrite it under `UPDATE_GOLDEN=1`.
fn assert_golden(case: &str, got: &str) {
    let path = golden_path(case);
    // `Some("1")`, like every other env gate in the phase: `UPDATE_GOLDEN=0` or a stray empty
    // export must not silently regenerate every golden and report green.
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
    assert_eq!(got, want, "golden `{case}` drifted");
}

macro_rules! golden_case {
    ($sqlite:ident, $memory:ident, $case:literal) => {
        #[tokio::test]
        async fn $sqlite() {
            assert_golden($case, &run($case, Which::Sqlite).await);
        }
        #[tokio::test]
        async fn $memory() {
            assert_golden($case, &run($case, Which::Memory).await);
        }
    };
}

golden_case!(
    fixed_section_order_on_sqlite,
    fixed_section_order_on_memory,
    "fixed_section_order"
);
golden_case!(
    degradation_order_on_sqlite,
    degradation_order_on_memory,
    "degradation_order"
);
golden_case!(
    pins_collapse_flags_degraded_on_sqlite,
    pins_collapse_flags_degraded_on_memory,
    "pins_collapse"
);
golden_case!(
    mail_headers_collapse_on_sqlite,
    mail_headers_collapse_on_memory,
    "mail_headers_collapse"
);
golden_case!(
    agent_section_shadows_global_on_sqlite,
    agent_section_shadows_global_on_memory,
    "agent_section_shadows_global"
);
golden_case!(
    pins_superseded_on_sqlite,
    pins_superseded_on_memory,
    "pins_superseded"
);
golden_case!(
    zero_rollups_assembles_on_sqlite,
    zero_rollups_assembles_on_memory,
    "zero_rollups"
);

#[tokio::test]
async fn every_golden_is_byte_identical_between_providers() {
    for case in CASES {
        let sqlite = run(case, Which::Sqlite).await;
        let memory = run(case, Which::Memory).await;
        assert_eq!(
            sqlite, memory,
            "`{case}` differs between ledger-sqlite and ledger-memory"
        );
    }
}

// ---- the claims the goldens are ABOUT ----------------------------------------------------------
//
// A golden proves the bytes did not move; these assert what the bytes MEAN, so a careless
// `UPDATE_GOLDEN=1` cannot quietly rewrite the rule along with the text.

#[tokio::test]
async fn the_six_bands_appear_in_slot_order() {
    let text = run("fixed_section_order", Which::Memory).await;
    let order: Vec<usize> = [
        "## Identity",
        "## Pins",
        "## Digest",
        "## Tier 2 summary",
        "## Tier 1 summary",
        "## Recent steps",
        "## Unconsumed mail",
    ]
    .iter()
    .map(|h| {
        text.find(h)
            .unwrap_or_else(|| panic!("`{h}` is missing from:\n{text}"))
    })
    .collect();
    let mut sorted = order.clone();
    sorted.sort();
    assert_eq!(order, sorted, "the section order is not §5's fixed order");
}

#[tokio::test]
async fn a_superseded_pin_leaves_the_assembled_projection() {
    let text = run("pins_superseded", Which::Memory).await;
    assert!(
        text.contains("assembly under 40ms"),
        "the superseding pin stands:\n{text}"
    );
    assert!(
        !text.contains("assembly under 50ms"),
        "the superseded pin is retired from the projection (\u{a7}3):\n{text}"
    );
    assert!(
        !text.contains("until friday"),
        "a retired pin leaves the projection:\n{text}"
    );
    assert!(!text.starts_with("> DEGRADED:"), "no budget pressure here");
}

#[tokio::test]
async fn a_pin_far_older_than_the_tail_still_rides_verbatim() {
    let text = run("pins_superseded", Which::Memory).await;
    let pins = text.find("## Pins").expect("a pins band");
    let tail = text.find("## Recent steps").expect("a tail band");
    let body = &text[pins..tail];
    // `p-keep` is seq 1 of 45; the verbatim tail window is the last 12 steps. Age is never a
    // criterion for a pin (\u{a7}3), so it renders in full anyway.
    assert!(
        body.contains("run `make gates` before every commit") && body.contains("gates"),
        "the oldest pin renders verbatim with its title:\n{body}"
    );
    assert!(
        !text[tail..].contains("make gates"),
        "it is in the pins band, not merely surviving in the tail:\n{text}"
    );
}

#[tokio::test]
async fn a_collapsed_pin_set_says_so_in_context() {
    let text = run("pins_collapse", Which::Memory).await;
    assert!(
        text.starts_with("> DEGRADED:") && text.contains("pins"),
        "pin degradation is never silent (§5):\n{text}"
    );
}

#[tokio::test]
async fn a_collapsed_mail_header_says_so_in_context() {
    let text = run("mail_headers_collapse", Which::Memory).await;
    assert!(
        text.starts_with("> DEGRADED:") && text.contains("mail"),
        "mail degradation is never silent (§5):\n{text}"
    );
    assert!(
        text.contains("unconsumed"),
        "per-class counts survive:\n{text}"
    );
}

#[tokio::test]
async fn zero_rollups_renders_no_digest_and_no_tier_header() {
    let text = run("zero_rollups", Which::Memory).await;
    assert!(
        !text.contains("## Digest"),
        "an empty band renders NO header:\n{text}"
    );
    assert!(
        !text.contains("## Tier"),
        "an empty band renders NO header:\n{text}"
    );
    assert!(
        text.contains("## Pins"),
        "the bands that do have input still render"
    );
}

#[tokio::test]
async fn the_agent_scoped_about_line_is_the_one_that_renders() {
    let text = run("agent_section_shadows_global", Which::Memory).await;
    assert!(text.contains("sol's own about-line"), "{text}");
    assert!(!text.contains("the global about-line"), "{text}");
}

/// The `degradation_order` golden is a single over-budget snapshot; it shows the END state, not the
/// ORDER. This walks the same real fixture (real ledger, real assembler) down a budget ramp and
/// asserts what §5 requires: fine tiers go before the coarse tier, and the verbatim tail is only
/// shortened after the fine tier is already gone — never the other way round.
#[tokio::test]
async fn degradation_walks_the_ladder_in_order_on_both_providers() {
    for which in [Which::Sqlite, Which::Memory] {
        let mut seen: Vec<(usize, bool, bool, usize)> = Vec::new();
        for budget in [100_000usize, 260, 240, 220, 200, 180, 160, 140, 120] {
            let text = run_with_budget("degradation_order", which, budget).await;
            let fine = text.contains("the fine tier goes first");
            let coarse = text.contains("the coarse tier survives it");
            let tail = text.matches("step/start").count();
            seen.push((budget, fine, coarse, tail));
        }
        let full = seen[0];
        assert!(
            full.1 && full.2 && full.3 > 3,
            "the unconstrained projection must carry both tiers and a full tail: {seen:?}"
        );
        for w in seen.windows(2) {
            let (_, fine_a, coarse_a, tail_a) = w[0];
            let (b, fine_b, coarse_b, tail_b) = w[1];
            assert!(
                fine_a || !fine_b,
                "a dropped fine tier came back at budget {b}: {seen:?}"
            );
            assert!(
                coarse_a || !coarse_b,
                "a dropped coarse tier came back at budget {b}: {seen:?}"
            );
            assert!(
                tail_b <= tail_a,
                "the tail grew as the budget shrank at {b}: {seen:?}"
            );
            assert!(
                !(tail_b < tail_a && fine_b),
                "the tail was cut at budget {b} while the fine tier was still present: {seen:?}"
            );
            assert!(
                !(!coarse_b && coarse_a && tail_b > 3),
                "§5 keeps the coarse tier until the tail is at its floor; it went at budget {b}: {seen:?}"
            );
            assert!(
                !(!coarse_b && coarse_a && fine_b),
                "the coarse tier was dropped at budget {b} before the fine one: {seen:?}"
            );
        }
        let last = seen.last().copied().unwrap();
        assert!(
            !last.1 && last.3 >= 3,
            "at the tightest budget the fine tier is gone and the tail holds its floor: {seen:?}"
        );
    }
}

/// `run`, with the case's configured budget overridden.
async fn run_with_budget(case: &str, which: Which, budget: usize) -> String {
    let h = Harness::open(which);
    let (mut config, specs) = seed(case, &h).await;
    config.budget_tokens = budget;
    let assembler = Assembler::new(Arc::new(config), h.ledger.clone(), h.ctx.clone());
    for s in specs {
        std::mem::forget(assembler.section(s).expect("a fresh section registers"));
    }
    assembler
        .assemble(&AssembleRequest {
            agent: agent(),
            wake: None,
            at: at(),
            budget: None,
        })
        .await
        .unwrap_or_else(|e| panic!("assemble {case} @ {budget}: {e}"))
        .to_text()
}
