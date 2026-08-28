//! Invariant: the bank is DATA, and it is complete. Every surface entry of the phase's §3 table is
//! exercised by at least one task, every task has a pass predicate no model judgement enters, and
//! every task has a recorded transcript under BOTH arms — a task one arm cannot even attempt would
//! read as a surface difference when it is a missing fixture.

use bough_bench_tools::bank::{self, Coverage};
use bough_bench_tools::run::Arm;

fn tasks() -> Vec<bank::Task> {
    bank::load(&bank::bench_dir().join("bank")).expect("the bank loads")
}

#[test]
fn the_bank_has_at_least_twelve_tasks_covering_every_surface() {
    let tasks = tasks();
    assert!(
        tasks.len() >= 12,
        "the bank has {} tasks; the brief asks for at least twelve",
        tasks.len()
    );

    let mut covered: Vec<Coverage> = tasks.iter().flat_map(|t| t.covers.clone()).collect();
    covered.sort();
    covered.dedup();
    let missing: Vec<Coverage> = Coverage::ALL
        .into_iter()
        .filter(|c| !covered.contains(c))
        .collect();
    assert!(
        missing.is_empty(),
        "a surface entry nobody benches is a surface entry nobody knows the cost of: {missing:?}"
    );
}

#[test]
fn the_bank_covers_the_shapes_the_brief_names() {
    let tasks = tasks();
    let with = |c: Coverage| tasks.iter().filter(|t| t.covers.contains(&c)).count();
    // Three file edits: a create, a hash-anchored patch, a multi-file patch.
    assert!(with(Coverage::Write) >= 1, "a create");
    assert!(with(Coverage::Patch) >= 2, "a patch and a multi-file patch");
    // Two multi-step shell tasks.
    assert!(with(Coverage::Bash) >= 2, "two multi-step shell tasks");
    // A search-then-edit is a task claiming BOTH a command and the file verbs.
    assert!(
        tasks
            .iter()
            .any(|t| t.covers.contains(&Coverage::Bash) && t.covers.contains(&Coverage::Patch)),
        "a search-then-edit"
    );
}

#[test]
fn every_task_id_is_unique_and_every_predicate_is_data() {
    let tasks = tasks();
    let mut ids: Vec<&str> = tasks.iter().map(|t| t.id.as_str()).collect();
    ids.sort();
    let n = ids.len();
    ids.dedup();
    assert_eq!(ids.len(), n, "two tasks share an id");
    for t in &tasks {
        assert!(!t.pass.is_empty(), "{}: no pass predicate", t.id);
        assert!(!t.prompt.trim().is_empty(), "{}: no prompt", t.id);
    }
}

#[test]
fn every_task_has_a_recorded_transcript_under_both_arms() {
    let dir = bank::bench_dir();
    for t in tasks() {
        for arm in Arm::BOTH {
            let f = dir.join(arm.fixtures()).join(format!("{}.yml", t.id));
            assert!(
                f.is_file(),
                "{} has no recorded transcript for the {} arm ({})",
                t.id,
                arm.label(),
                f.display()
            );
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The arms. A patch layer REPLACES a row's config wholesale (§0.5, `config::patch`: "no deep
// merge"), so a bench-only tweak has to restate the row. These three tests are what stops that
// restatement from becoming a second, divergent configuration of the thing being measured.

fn yaml(rel: &str) -> serde_yaml::Value {
    let path = bank::bench_dir().join(rel);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    serde_yaml::from_str(&text).unwrap_or_else(|e| panic!("{} does not parse: {e}", path.display()))
}

fn entries(doc: &serde_yaml::Value) -> &serde_yaml::Mapping {
    doc.get("entries")
        .and_then(|e| e.as_mapping())
        .expect("an arm patch is `entries: {..}`")
}

/// The shipped bundle, as `{id: entry}` — the same shape a patch layer's `entries` has.
fn bundle_rows(rel: &str) -> serde_yaml::Mapping {
    let path = bank::bench_dir().join("../..").join(rel);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    let rows: serde_yaml::Value = serde_yaml::from_str(&text).expect("the bundle parses");
    let mut out = serde_yaml::Mapping::new();
    for row in rows.as_sequence().expect("a bundle is a sequence") {
        let mut row = row.as_mapping().expect("a row is a mapping").clone();
        let id = row.remove("id").expect("a row has an id");
        out.insert(id, serde_yaml::Value::Mapping(row));
    }
    out
}

/// The arms differ by the CONSUMER and by nothing else: the two patch layers are the same bytes.
/// A bench whose arms tuned the file verbs differently would measure the tuning.
#[test]
fn the_two_arm_patches_are_byte_identical_below_their_comments() {
    let strip = |rel: &str| {
        let path = bank::bench_dir().join(rel);
        std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
            .lines()
            .filter(|l| !l.trim_start().starts_with('#'))
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert_eq!(
        strip("arms/typed.yml"),
        strip("arms/codemode.yml"),
        "the arms must configure the tree identically; the consumer is the only difference, and \
         it comes from the profile document, not from these layers"
    );
    // And the shared block is not empty: two empty files are also identical.
    let typed = yaml("arms/typed.yml");
    assert!(
        entries(&typed).contains_key(serde_yaml::Value::from("tools.operator")),
        "the arms must configure `tools.operator` for the bench to be measurable"
    );
}

/// Which arm is the SHIPPED tree and which one needs a document, since code mode became the
/// default consumer (2026-08-28): the TYPED arm is now the one that names `profiles/typed.yml`,
/// and the codemode arm is `--profile headless` verbatim. This is what keeps a bench arm from
/// silently becoming the other surface.
#[test]
fn the_codemode_arm_comes_from_the_shipped_profile_and_bundle() {
    use bough_bench_tools::run::Arm;
    assert_eq!(
        Arm::Codemode.profile_source(),
        None,
        "code mode is the DEFAULT: its arm is the shipped `headless` tree, unmodified"
    );
    let rel = Arm::Typed
        .profile_source()
        .expect("the typed arm names a profile document");
    let root = bank::bench_dir().join("../..");

    let profile: serde_yaml::Value =
        serde_yaml::from_str(&std::fs::read_to_string(root.join(rel)).expect("the profile"))
            .expect("the profile parses");
    let bundles: Vec<String> = profile
        .get("bundles")
        .and_then(|b| serde_yaml::from_value(b.clone()).ok())
        .expect("the profile lists bundles");
    assert!(
        bundles.contains(&"bough-typed".to_string()),
        "{rel} must mount `bough-typed`, the fallback layer: {bundles:?}"
    );

    // And the shipped `headless` profile is the code-mode one.
    let shipped: serde_yaml::Value = serde_yaml::from_str(
        &std::fs::read_to_string(root.join("profiles/headless.yml")).expect("the profile"),
    )
    .expect("the profile parses");
    let shipped_bundles: Vec<String> = shipped
        .get("bundles")
        .and_then(|b| serde_yaml::from_value(b.clone()).ok())
        .expect("the profile lists bundles");
    assert!(
        shipped_bundles.contains(&"bough-codemode".to_string()),
        "`profiles/headless.yml` must mount `bough-codemode`: {shipped_bundles:?}"
    );

    // The three rows are DECLARED in `bough-base.yml` — enabled, since the GO — so a `--patch` can
    // reach the consumer in either direction (a patch layer configures rows and never creates
    // them). Both halves are pinned: the rows exist and ship ENABLED, and the fallback layer names
    // the consumer.
    let base = bundle_rows("bundles/bough-base.yml");
    for id in ["js", "js.quickjs", "tools.codemode"] {
        let row = base
            .get(serde_yaml::Value::from(id))
            .unwrap_or_else(|| panic!("`{id}` must be declared in bundles/bough-base.yml"));
        assert_eq!(
            row.get("disabled").and_then(|d| d.as_bool()),
            None,
            "`{id}` must ship ENABLED: code mode is the default consumer"
        );
    }

    let fallback: serde_yaml::Value = serde_yaml::from_str(
        &std::fs::read_to_string(root.join("bundles/bough-typed.yml")).expect("the typed bundle"),
    )
    .expect("the typed bundle parses");
    let f_entries = fallback["entries"]
        .as_mapping()
        .expect("the typed bundle is a patch document with `entries`");
    let f_ids: Vec<String> = f_entries
        .keys()
        .map(|k| k.as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(
        f_ids,
        vec!["tools.codemode"],
        "the fallback is ONE field on the consumer row and nothing else"
    );
    assert_eq!(
        f_entries[&serde_yaml::Value::from("tools.codemode")]
            .get("disabled")
            .and_then(|d| d.as_bool()),
        Some(true),
        "the fallback disables the consumer"
    );

    let switch: serde_yaml::Value = serde_yaml::from_str(
        &std::fs::read_to_string(bank::bench_dir().join("../../bundles/bough-codemode.yml"))
            .expect("the codemode bundle"),
    )
    .expect("the codemode bundle parses");
    let entries = switch["entries"]
        .as_mapping()
        .expect("the codemode bundle is a patch document with `entries`");
    let ids: Vec<String> = entries
        .keys()
        .map(|k| k.as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(
        ids,
        vec!["js", "js.quickjs", "tools.codemode"],
        "the codemode bundle enables the seam, its provider and the consumer, and nothing else"
    );
    for (_, v) in entries {
        assert_eq!(
            v.get("disabled").and_then(|d| d.as_bool()),
            Some(false),
            "the codemode bundle is the switch: every entry sets `disabled: false` and nothing else"
        );
        assert_eq!(
            v.as_mapping().map(|m| m.len()),
            Some(1),
            "the codemode bundle must not restate a row's config: it lives in bough-base.yml"
        );
    }
}

/// A restated config must be COMPLETE. `config::patch` replaces the map, so a field the arm forgot
/// is a field the row is deserialized without — which is a boot failure, not a default.
#[test]
fn a_restated_row_names_every_field_the_shipped_bundle_sets() {
    let base = bundle_rows("bundles/bough-base.yml");
    for rel in ["arms/typed.yml", "arms/codemode.yml"] {
        let doc = yaml(rel);
        for (id, entry) in entries(&doc) {
            let Some(shipped) = base.get(id) else {
                continue;
            };
            let shipped = shipped
                .get("config")
                .and_then(|c| c.as_mapping())
                .expect("the shipped row has a config");
            let restated = entry
                .get("config")
                .and_then(|c| c.as_mapping())
                .unwrap_or_else(|| panic!("{rel}: `{id:?}` has no config"));
            for field in shipped.keys() {
                assert!(
                    restated.contains_key(field),
                    "{rel}: `{id:?}` does not restate `{field:?}`; a patch layer replaces the \
                     whole config (§0.5) and the row will fail to deserialize"
                );
            }
        }
    }
}
