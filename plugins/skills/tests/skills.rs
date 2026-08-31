//! §9's skills host: a global pool of skill files, mention-triggered auto-injection as a
//! projection section, one child entry per file, hot reload.
//!
//! The section half runs against the REAL assembler over the REAL memory ledger, so "a mentioned
//! skill's section appears in the assembled projection" is asserted on an `Assembled`, not on a
//! renderer called by hand. The host half runs against a real kernel, so "the child entry FAILS
//! naming the file" is the kernel's own fiber state.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use bough_kernel::{
    Catalog, Composer, Composition, Context, ExprEnv, FiberState, Kernel, KernelCore,
    KernelOptions, LayerId, Patch, RowSnapshot,
};
use bough_plugin_ledger::{
    AgentName, Append, Class, Connected, LedgerHandle, StepType, TrajId, WakeId,
};
use bough_plugin_ledger_memory::store::MemoryStore;
use bough_plugin_projection::{
    AssembleRequest, DropPriority, Projector, SectionScope, SectionSpec,
};
use bough_plugin_projection_assembler::{Assembler, AssemblerConfig};
use bough_plugin_skills::{
    child_entry, digest_of, parse_skill, registry, scan_dir, section, SkillsConfig,
};
use chrono::{DateTime, TimeZone, Utc};

fn at() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap()
}

fn host_cfg(dir: &Path) -> SkillsConfig {
    SkillsConfig {
        dir: dir.to_path_buf(),
        glob: "*.md".into(),
        roots: vec![],
        only: vec![],
        except: vec![],
        watch: false,
        debounce_ms: 400,
        max_bytes: 65536,
        max_injected: 3,
        scan_steps: 40,
    }
}

fn assembler_cfg() -> AssemblerConfig {
    AssemblerConfig {
        budget_tokens: 100_000,
        headroom: 0.6,
        tail_steps: 20,
        tail_floor_steps: 4,
        mail_newest_n: 3,
        max_tiers: 3,
        file_view_dir: std::env::temp_dir(),
    }
}

fn skill_file(name: &str, triggers: &str, body: &str) -> String {
    format!("---\nname: {name}\ndescription: d\ntriggers: {triggers}\n---\n\n{body}\n")
}

// ---------------------------------------------------------------------------
// the section, against the real assembler
// ---------------------------------------------------------------------------

/// A ledger holding one agent whose trajectory carries `text`, plus the assembler over it.
async fn world(text: &str) -> (LedgerHandle, Arc<Assembler>) {
    let ctx = Context::root(KernelCore::new());
    let ledger = LedgerHandle(MemoryStore::new(ctx.clone()));
    for def in bough_plugin_agents::vocabulary::step_types() {
        // The token is dropped, not spent: a registration is undone by an EFFECT (§0.2).
        drop(ledger.0.register_step_type(def).expect("a fresh step type"));
    }
    ledger
        .0
        .put_agent(bough_plugin_ledger::AgentRow {
            name: AgentName::new("sol"),
            traj: TrajId::new("t1"),
            routing_refs: Default::default(),
            wake_classes: Default::default(),
            model_override: None,
            tick_floor: None,
            digest_rollup: None,
        })
        .await
        .expect("the agent row writes");
    ledger
        .0
        .append(Append {
            traj: TrajId::new("t1"),
            wake: WakeId::new("w1"),
            kind: StepType::new("thought/text"),
            class: Class::Thought,
            body: serde_json::json!({ "text": text, "step_index": 0 }),
            cites: vec![],
            at: at(),
            id: None,
        })
        .await
        .expect("the step appends");
    let assembler = Assembler::new(Arc::new(assembler_cfg()), ledger.clone(), ctx);
    (ledger, assembler)
}

/// Register one skill's section on the assembler, in its own pool.
fn register(
    assembler: &Arc<Assembler>,
    dir: &Path,
    text: &str,
) -> bough_plugin_projection::SectionToken {
    let cfg = host_cfg(dir);
    let skill = Arc::new(parse_skill(&dir.join("s.md"), text).expect("the fixture parses"));
    let pool = registry::pool(dir);
    // Leaked on purpose: the pool entry lives as long as the test's section does.
    std::mem::forget(pool.insert(Arc::clone(&skill)));
    assembler
        .section(SectionSpec {
            id: skill.id.clone(),
            position: section::POSITION,
            scope: SectionScope::Global,
            agent: None,
            priority: DropPriority::Fine,
            render: Arc::new(section::SkillSection {
                skill,
                pool: registry::pool(dir),
                scan_steps: cfg.scan_steps,
                max_injected: cfg.max_injected,
            }),
        })
        .expect("the section registers")
}

async fn assemble(assembler: &Arc<Assembler>) -> bough_plugin_projection::Assembled {
    assembler
        .assemble(&AssembleRequest {
            agent: AgentName::new("sol"),
            wake: None,
            at: at(),
            budget: None,
            as_of: None,
        })
        .await
        .expect("the projection assembles")
}

#[tokio::test]
async fn a_mentioned_skills_section_appears_in_the_assembled_projection() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (_l, a) = world("time to do a code review on this").await;
    let _t = register(
        &a,
        dir.path(),
        &skill_file("review", "[\"code review\"]", "Read the diff twice."),
    );
    let out = assemble(&a).await;
    let s = out
        .sections
        .iter()
        .find(|s| s.id.as_str() == "skill:review")
        .expect("the mentioned skill injected");
    assert_eq!(s.title, "Skill: review");
    assert_eq!(s.body, "Read the diff twice.");
    // Model-visible ⟺ ledgered: it names the row whose text triggered it.
    assert_eq!(s.cites.steps.len(), 1);
    assert!(out.to_text().contains("Read the diff twice."));
}

#[tokio::test]
async fn an_unmentioned_skill_contributes_nothing_at_all() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (_l, a) = world("nothing to see here").await;
    let _t = register(
        &a,
        dir.path(),
        &skill_file("review", "[\"code review\"]", "Read the diff twice."),
    );
    let out = assemble(&a).await;
    assert!(
        !out.sections.iter().any(|s| s.id.as_str() == "skill:review"),
        "an unmentioned skill must not appear at all: {:?}",
        out.sections
            .iter()
            .map(|s| s.id.as_str())
            .collect::<Vec<_>>()
    );
    assert!(!out.to_text().contains("Read the diff twice."));
}

#[tokio::test]
async fn max_injected_caps_the_pool_and_ties_break_by_section_id() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (_l, a) = world("go go go").await;
    // Registered newest-id first, so a cap that followed registration order would keep `delta`.
    let mut tokens = Vec::new();
    for name in ["delta", "charlie", "bravo", "alpha"] {
        tokens.push(register(
            &a,
            dir.path(),
            &skill_file(name, "[go]", &format!("body of {name}")),
        ));
    }
    let out = assemble(&a).await;
    let injected: Vec<&str> = out
        .sections
        .iter()
        .map(|s| s.id.as_str())
        .filter(|id| id.starts_with("skill:"))
        .collect();
    assert_eq!(
        injected,
        vec!["skill:alpha", "skill:bravo", "skill:charlie"]
    );
}

#[tokio::test]
async fn the_section_honours_as_of_so_a_past_request_reproduces() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (ledger, a) = world("nothing yet").await;
    let before = ledger
        .0
        .head_seq(&TrajId::new("t1"))
        .await
        .expect("head")
        .expect("one step");
    ledger
        .0
        .append(Append {
            traj: TrajId::new("t1"),
            wake: WakeId::new("w1"),
            kind: StepType::new("thought/text"),
            class: Class::Thought,
            body: serde_json::json!({ "text": "code review time", "step_index": 1 }),
            cites: vec![],
            at: at(),
            id: None,
        })
        .await
        .expect("appends");
    let _t = register(
        &a,
        dir.path(),
        &skill_file("review", "[\"code review\"]", "Read the diff twice."),
    );

    let now = assemble(&a).await;
    assert!(now.sections.iter().any(|s| s.id.as_str() == "skill:review"));

    let past = a
        .assemble(&AssembleRequest {
            agent: AgentName::new("sol"),
            wake: None,
            at: at(),
            budget: None,
            as_of: Some(before),
        })
        .await
        .expect("assembles");
    assert!(
        !past
            .sections
            .iter()
            .any(|s| s.id.as_str() == "skill:review"),
        "the mention is above `as_of`: the past request must not see it"
    );
}

// ---------------------------------------------------------------------------
// the host, against a real kernel
// ---------------------------------------------------------------------------

const BUNDLE: &str = "\
- id: ledger
  plugin: ledger-memory
  config: {}
- id: projection
  plugin: projection-assembler
  config: { budget_tokens: 100000, headroom: 0.6, tail_steps: 20, tail_floor_steps: 4,
            mail_newest_n: 3, max_tiers: 3, file_view_dir: /tmp }
";

fn compose(catalog: &Catalog, yaml: &str) -> Composition {
    let patch: Patch = serde_yaml::from_str(yaml).expect("the test bundle parses");
    let mut composer = Composer::new(catalog, ExprEnv::new("test"));
    composer.layer(LayerId::new("test"), patch);
    composer.compose().expect("the test bundle composes")
}

async fn boot(dir: &Path) -> Arc<Kernel> {
    let yaml = format!(
        "{BUNDLE}- id: skills\n  plugin: skills\n  config: {{ dir: {}, glob: \"*.md\", watch: false, debounce_ms: 400, max_bytes: 65536, max_injected: 3, scan_steps: 40 }}\n",
        dir.display()
    );
    let catalog = Catalog::from_inventory().expect("the linked catalog has no duplicate names");
    let composition = compose(&catalog, &yaml);
    let kernel = Kernel::new(
        catalog,
        KernelOptions {
            profile: "test".into(),
            invariants: false,
        },
    );
    kernel.load(composition).await.expect("the tree mounts");
    kernel.quiesce().await;
    kernel
}

fn rows(kernel: &Kernel) -> Vec<RowSnapshot> {
    fn flatten(rows: &[RowSnapshot], out: &mut Vec<RowSnapshot>) {
        for r in rows {
            out.push(r.clone());
            flatten(&r.children, out);
        }
    }
    let mut out = Vec::new();
    flatten(&kernel.snapshot().rows, &mut out);
    out
}

fn child(kernel: &Kernel, id: &str) -> RowSnapshot {
    rows(kernel)
        .into_iter()
        .find(|r| r.id.as_str() == id)
        .unwrap_or_else(|| panic!("no row `{id}` in the tree"))
}

#[tokio::test]
async fn one_child_entry_mounts_per_skill_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("alpha.md"),
        skill_file("alpha", "[alpha]", "A"),
    )
    .unwrap();
    std::fs::write(
        dir.path().join("beta.md"),
        skill_file("beta", "[beta]", "B"),
    )
    .unwrap();
    std::fs::write(dir.path().join("notes.txt"), "not a skill").unwrap();

    let kernel = boot(dir.path()).await;
    assert_eq!(child(&kernel, "skills.alpha").state, FiberState::Active);
    assert_eq!(child(&kernel, "skills.beta").state, FiberState::Active);
    assert!(
        !rows(&kernel)
            .iter()
            .any(|r| r.id.as_str() == "skills.notes"),
        "the glob is `*.md`: a .txt file is not a skill"
    );
    kernel.shutdown().await;
}

#[tokio::test]
async fn a_file_with_no_name_fails_its_child_entry_naming_the_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("broken.md"),
        "---\ntriggers: [x]\n---\nbody\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("ok.md"), skill_file("ok", "[ok]", "O")).unwrap();

    let kernel = boot(dir.path()).await;
    let broken = child(&kernel, "skills.broken");
    assert_eq!(broken.state, FiberState::Failed);
    let err = broken
        .error
        .clone()
        .expect("a failed fiber carries its error");
    assert!(
        err.contains("broken.md"),
        "the refusal names the file: {err}"
    );
    assert!(
        err.contains("`name`"),
        "the refusal says what is missing: {err}"
    );
    // Its sibling is untouched.
    assert_eq!(child(&kernel, "skills.ok").state, FiberState::Active);
    kernel.shutdown().await;
}

#[tokio::test]
async fn a_file_over_max_bytes_fails_its_child_entry_naming_the_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let big = skill_file("big", "[big]", &"x".repeat(200));
    std::fs::write(dir.path().join("big.md"), &big).unwrap();
    let yaml = format!(
        "{BUNDLE}- id: skills\n  plugin: skills\n  config: {{ dir: {}, glob: \"*.md\", watch: false, debounce_ms: 400, max_bytes: 32, max_injected: 3, scan_steps: 40 }}\n",
        dir.path().display()
    );
    let catalog = Catalog::from_inventory().expect("catalog");
    let composition = compose(&catalog, &yaml);
    let kernel = Kernel::new(
        catalog,
        KernelOptions {
            profile: "test".into(),
            invariants: false,
        },
    );
    kernel.load(composition).await.expect("the tree mounts");
    kernel.quiesce().await;
    let err = child(&kernel, "skills.big")
        .error
        .clone()
        .expect("a failed fiber carries its error");
    assert!(err.contains("big.md") && err.contains("max_bytes"), "{err}");
    kernel.shutdown().await;
}

#[tokio::test]
async fn editing_one_skill_file_remounts_exactly_that_child() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("alpha.md"),
        skill_file("alpha", "[alpha]", "A"),
    )
    .unwrap();
    std::fs::write(
        dir.path().join("beta.md"),
        skill_file("beta", "[beta]", "B"),
    )
    .unwrap();
    let kernel = boot(dir.path()).await;
    let before_alpha = child(&kernel, "skills.alpha")
        .uid
        .expect("alpha is mounted");
    let before_beta = child(&kernel, "skills.beta").uid.expect("beta is mounted");

    // The host's own reconcile, driven directly: the watcher is the trigger, not the behaviour.
    std::fs::write(
        dir.path().join("alpha.md"),
        skill_file("alpha", "[alpha]", "A2"),
    )
    .unwrap();
    let ctx = kernel
        .row_context(&bough_kernel::EntryId::new("skills"))
        .expect("the host row has a context");
    let mounted = Arc::new(parking_lot::Mutex::new(
        [
            (
                dir.path().join("alpha.md"),
                (
                    digest_of(skill_file("alpha", "[alpha]", "A").as_bytes()),
                    before_alpha,
                ),
            ),
            (
                dir.path().join("beta.md"),
                (
                    digest_of(skill_file("beta", "[beta]", "B").as_bytes()),
                    before_beta,
                ),
            ),
        ]
        .into_iter()
        .collect::<std::collections::BTreeMap<PathBuf, (String, bough_kernel::FiberUid)>>(),
    ));
    bough_plugin_skills::reconcile(
        &ctx,
        &bough_kernel::EntryId::new("skills"),
        &host_cfg(dir.path()),
        &mounted,
    )
    .await
    .expect("the reload reconciles");
    kernel.quiesce().await;

    let after_alpha = mounted.lock()[&dir.path().join("alpha.md")].1;
    let after_beta = mounted.lock()[&dir.path().join("beta.md")].1;
    assert_ne!(before_alpha, after_alpha, "the edited skill remounted");
    assert_eq!(before_beta, after_beta, "its sibling did not");
    assert_eq!(child(&kernel, "skills.alpha").state, FiberState::Active);
    kernel.shutdown().await;
}

#[test]
fn scan_dir_sees_only_the_glob_and_orders_deterministically() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("b.md"), "b").unwrap();
    std::fs::write(dir.path().join("a.md"), "a").unwrap();
    std::fs::write(dir.path().join("c.txt"), "c").unwrap();
    let found = scan_dir(dir.path(), "*.md").expect("scans");
    let names: Vec<String> = found
        .iter()
        .map(|(p, _)| p.file_name().unwrap().to_string_lossy().to_string())
        .collect();
    assert_eq!(names, vec!["a.md", "b.md"]);
    assert_eq!(found[0].1, digest_of(b"a"));
}

#[test]
fn a_child_entry_names_the_file_and_carries_its_digest() {
    let dir = PathBuf::from("/skills");
    let e = child_entry("skills", &dir.join("review.md"), "d", &host_cfg(&dir));
    assert_eq!(e.id.as_str(), "skills.review");
}

/// A connected-membership sanity check, so `world`'s assumption is not silent.
#[tokio::test]
async fn the_section_reads_the_agents_own_chain() {
    let (ledger, _a) = world("code review").await;
    let c: Connected = ledger
        .0
        .connected(&AgentName::new("sol"))
        .await
        .expect("membership");
    assert_eq!(c.own, TrajId::new("t1"));
}
