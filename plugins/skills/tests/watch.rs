//! V12's skills half, end to end through a real kernel: the child entry a skill FILE mounts
//! contributes a projection section that a mention pulls in, editing that file on disk is picked
//! up by the WATCHER (no hand-driven `reconcile`), exactly one child entry is remounted, and the
//! newly assembled projection carries the new body.
//!
//! `skills.rs` asserts the section against the assembler with the section registered by hand and
//! the reconcile called by hand. This file never calls either: it writes a file and waits.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bough_kernel::{
    Catalog, Composer, Composition, ExprEnv, Kernel, KernelOptions, LayerId, Patch, RowSnapshot,
};
use bough_plugin_ledger::{AgentName, Append, Class, Ledger, StepType, TrajId, WakeId};
use bough_plugin_projection::{AssembleRequest, Projection};
use chrono::{TimeZone, Utc};

const _: (&str, &str, &str, &str) = (
    bough_plugin_skills::PLUGIN_NAME,
    bough_plugin_ledger_memory::PLUGIN_NAME,
    bough_plugin_agents::PLUGIN_NAME,
    bough_plugin_projection_assembler::PLUGIN_NAME,
);

fn skill_file(name: &str, triggers: &str, body: &str) -> String {
    format!("---\nname: {name}\ndescription: d\ntriggers: {triggers}\n---\n\n{body}\n")
}

fn tree(dir: &Path) -> String {
    format!(
        "\
- id: ledger
  plugin: ledger-memory
  config: {{}}
- id: agents
  plugin: agents
  inject: [ledger]
  config: {{}}
- id: projection
  plugin: projection-assembler
  inject: [ledger]
  config: {{ budget_tokens: 100000, headroom: 0.6, tail_steps: 20, tail_floor_steps: 4,
            mail_newest_n: 3, max_tiers: 3, file_view_dir: /tmp }}
- id: skills
  plugin: skills
  inject: [projection, ledger]
  config: {{ dir: {dir}, glob: \"*.md\", watch: true, debounce_ms: 50, max_bytes: 65536,
             max_injected: 3, scan_steps: 40 }}
",
        dir = dir.display()
    )
}

async fn boot(yaml: &str) -> Arc<Kernel> {
    let catalog = Catalog::from_inventory().expect("the linked catalog has no duplicate names");
    let patch: Patch = serde_yaml::from_str(yaml).expect("the test bundle parses");
    let mut composer = Composer::new(&catalog, ExprEnv::new("test"));
    composer.layer(LayerId::new("test"), patch);
    let composition: Composition = composer.compose().expect("the test bundle composes");
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

fn children(kernel: &Kernel) -> Vec<(String, u64)> {
    fn walk(rows: &[RowSnapshot], out: &mut Vec<(String, u64)>) {
        for r in rows {
            if r.id.as_str().starts_with("skills.") {
                out.push((
                    r.id.as_str().to_string(),
                    r.uid.map(|u| u.0).unwrap_or_default(),
                ));
            }
            walk(&r.children, out);
        }
    }
    let mut out = Vec::new();
    walk(&kernel.snapshot().rows, &mut out);
    out.sort();
    out
}

/// The section bodies the assembler produces for `sol`, keyed by section id.
async fn sections(kernel: &Kernel) -> Vec<(String, String)> {
    let p = kernel
        .root()
        .peek_live::<Projection>()
        .expect("the projection seam");
    let out =
        p.0.assemble(&AssembleRequest {
            agent: AgentName::new("sol"),
            wake: None,
            at: Utc.with_ymd_and_hms(2026, 1, 2, 0, 0, 0).unwrap(),
            budget: None,
            as_of: None,
        })
        .await
        .expect("the projection assembles");
    out.sections
        .into_iter()
        .map(|s| (s.id.as_str().to_string(), s.body))
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_mentioned_skill_injects_and_editing_its_file_hot_reloads_exactly_one_child() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("alpha.md"),
        skill_file("alpha", "[\"code review\"]", "Read the diff twice."),
    )
    .unwrap();
    std::fs::write(
        dir.path().join("beta.md"),
        skill_file("beta", "[\"deploy\"]", "Never on a Friday."),
    )
    .unwrap();
    let kernel = boot(&tree(dir.path())).await;

    // One child per file.
    let before = children(&kernel);
    assert_eq!(
        before.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(),
        vec!["skills.alpha", "skills.beta"],
        "{before:?}"
    );

    // A trajectory that MENTIONS alpha's trigger and not beta's.
    let ledger = kernel
        .root()
        .peek_live::<Ledger>()
        .expect("the ledger seam");
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
            body: serde_json::json!({ "text": "time to do a code review", "step_index": 0 }),
            cites: vec![],
            at: Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
            id: None,
        })
        .await
        .expect("the step appends");

    let now = sections(&kernel).await;
    let alpha = now
        .iter()
        .find(|(id, _)| id == "skill:alpha")
        .unwrap_or_else(|| panic!("the mentioned skill injected: {now:?}"));
    assert_eq!(alpha.1, "Read the diff twice.");
    assert!(
        !now.iter().any(|(id, _)| id == "skill:beta"),
        "an unmentioned skill contributes nothing: {now:?}"
    );

    // EDIT THE FILE. Nothing else: the watcher is the only trigger.
    std::fs::write(
        dir.path().join("alpha.md"),
        skill_file("alpha", "[\"code review\"]", "Read the diff three times."),
    )
    .unwrap();

    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let s = sections(&kernel).await;
        if s.iter()
            .any(|(id, body)| id == "skill:alpha" && body == "Read the diff three times.")
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the watcher never reloaded: {s:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // Exactly ONE child entry was reconciled: alpha's fiber is new, beta's is untouched.
    let after = children(&kernel);
    assert_eq!(after.len(), 2, "{after:?}");
    assert_ne!(
        before[0].1, after[0].1,
        "alpha remounted: {before:?} {after:?}"
    );
    assert_eq!(
        before[1].1, after[1].1,
        "beta did not: {before:?} {after:?}"
    );

    kernel.shutdown().await;
}
