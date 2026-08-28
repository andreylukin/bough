//! V10 — the record. The three documents this phase is accountable to are checked as DATA, so a
//! decision that quietly changed cannot leave the paperwork saying it did not.
//!
//! Nothing here reads code. That is the point: `REQUIREMENTS.md` §18, the plan's §7 and `BUILD.md`
//! are the artefacts Andrey decides from, and a phase whose record drifts from its build is a
//! phase whose numbers cannot be trusted either.

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the repo root exists")
}

fn doc(rel: &str) -> String {
    let p = repo_root().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("reading {}: {e}", p.display()))
}

/// The text of one `## `-headed section, by its heading prefix.
fn section(text: &str, heading: &str) -> String {
    let start = text
        .find(heading)
        .unwrap_or_else(|| panic!("no section `{heading}`"));
    let rest = &text[start + heading.len()..];
    let end = rest.find("\n## ").unwrap_or(rest.len());
    rest[..end].to_string()
}

/// §18 is the ambiguity referee (the brief: "add Headlong to §18 as reference semantics"). A
/// citation without its design documents is a citation nobody can follow, so the file names are
/// what is asserted, not the repo url alone.
#[test]
fn requirements_18_cites_headlongs_design_docs() {
    let s = section(&doc("REQUIREMENTS.md"), "## 18. Reference");
    assert!(
        s.contains("laude-institute/headlong"),
        "§18 must cite Headlong"
    );
    for design in [
        "trajectory_spec",
        "tiered_memory",
        "monolith_thinker",
        "monolith_backoff",
        "THINKERS_spec",
    ] {
        assert!(s.contains(design), "§18 must name `design/{design}`");
    }
    assert!(
        s.contains("operator") && s.contains("dispatcher"),
        "§18 must record Headlong's stance — the model is an OPERATOR, not a dispatcher — since \
         that is the semantics the spec defers to where it is ambiguous"
    );
    // And the code-mode literature the build actually leans on.
    assert!(s.contains("2402.01030"), "§18 must cite CodeAct");
    assert!(
        s.contains("2602.15945"),
        "§18 must cite the second code-mode paper"
    );
}

/// §7 of the plan is the thing Andrey reads. Each lettered decision must be there, and each must
/// carry evidence rather than a preference — "recommendation" alone is not evidence.
#[test]
fn the_plan_records_the_decisions_for_andrey_with_evidence() {
    let plan = doc("docs/phase-codemode-plan.md");
    let s = section(&plan, "## 7. Decisions for Andrey");
    for decision in [
        "**A. The default consumer.**",
        "**B. Swap-gate policy",
        "**C. Workflow / deterministic-replay row.**",
        "**D. `edit_file`.**",
    ] {
        assert!(s.contains(decision), "§7 must record {decision}");
    }
    assert!(
        s.contains("no action taken") || s.contains("no action"),
        "§7 must say these are RECORDED, not acted on"
    );
    // B names the crates it proposes to stop gating on; a proposal without its list is not one.
    for crate_name in [
        "ledger-memory",
        "llm-replay",
        "agent-loop-scripted",
        "rollups-none",
    ] {
        assert!(
            s.contains(crate_name),
            "decision B must name `{crate_name}`"
        );
    }
    // A points at the numbers, and the numbers exist as a section.
    assert!(
        plan.contains("## 8. Bench results"),
        "the plan must carry the bench results §7.A defers to"
    );
    assert!(
        s.contains("§8") && s.contains("the GO is yours"),
        "§7.A must send the decision to the numbers and leave it with Andrey"
    );
}

/// The one product fact this phase must not have changed: the default consumer. `BUILD.md` is the
/// phase ledger, and this is the line that says the phase measured a swap rather than made one.
#[test]
fn the_build_row_says_the_default_consumer_is_unchanged() {
    let build = doc("BUILD.md");
    let row = build
        .lines()
        .find(|l| l.starts_with("| codemode |"))
        .expect("BUILD.md must carry a `codemode` phase row");
    let lower = row.to_lowercase();
    assert!(
        lower.contains("default"),
        "the codemode row must say what the DEFAULT consumer is: {row}"
    );
    assert!(
        lower.contains("typed") && lower.contains("unchanged"),
        "the codemode row must say the default consumer is the TYPED one and is UNCHANGED: {row}"
    );
    assert!(
        row.contains("profiles/codemode.yml") || row.contains("--profile codemode"),
        "and it must name the only way in: {row}"
    );

    // The profile that is NOT the default must genuinely not be in anyone else's bundle list.
    for profile in ["tui", "headless", "dev"] {
        let p = repo_root().join("profiles").join(format!("{profile}.yml"));
        if let Ok(text) = std::fs::read_to_string(&p) {
            assert!(
                !text.contains("bough-codemode"),
                "`profiles/{profile}.yml` mounts `bough-codemode`; the default consumer is not \
                 unchanged and BUILD.md is wrong"
            );
        }
    }
}

/// The merge notes are a handoff, not a diary: every entry must say WHERE and WHAT, because the
/// person acting on it is on the other branch.
#[test]
fn the_merge_notes_name_a_file_and_a_hook_for_every_entry() {
    let notes = doc("docs/codemode-merge-notes.md");
    let entries: Vec<&str> = notes.split("\n## ").skip(1).collect();
    assert!(
        entries.len() >= 3,
        "the brief asks for at least three hooks"
    );
    for e in &entries {
        let title = e.lines().next().unwrap_or_default();
        assert!(
            e.contains("**File"),
            "merge note `{title}` names no file to change"
        );
    }
    // The three the brief names by hand.
    assert!(
        notes.contains("ctx.schedule"),
        "the §2.4 `ctx.schedule` handoff must be recorded"
    );
    assert!(
        notes.contains("visible_specs") || notes.contains("conceal"),
        "the concealment hook of §0.1 must be recorded"
    );
}

/// The record's claim "the default is unchanged" is a claim about the BINARY, not about a table in
/// a markdown file, so it is checked against the binary: every shipped profile is composed by the
/// real launcher and must not carry the code-mode consumer, while `--profile codemode` must — that
/// asymmetry is the whole of the "patch-swappable, off by default" promise.
#[test]
fn no_shipped_profile_boots_the_codemode_consumer() {
    let home = tempfile::tempdir().expect("tempdir");
    let dump = |profile: &str| -> String {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_bough"))
            .args(["--profile", profile, "--dump-config"])
            .env("BOUGH_HOME", home.path())
            .output()
            .expect("run bough --dump-config");
        assert!(
            out.status.success(),
            "--profile {profile} --dump-config must exit 0; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).expect("the dump is UTF-8")
    };

    for profile in ["tui", "headless", "dev"] {
        let text = dump(profile);
        assert!(
            text.contains("plugin: tools\n") && text.contains("plugin: tools-baseline"),
            "`--profile {profile}` must still compose the typed tool rows"
        );
        assert!(
            !text.contains("tools-codemode"),
            "`--profile {profile}` composes `tools-codemode`; the DEFAULT consumer is not \
             unchanged and BUILD.md's row is a lie"
        );
    }
    let cm = dump("codemode");
    assert!(
        cm.contains("plugin: tools-codemode"),
        "`--profile codemode` must be the way in; if it is not, the row BUILD.md names does not \
         exist:\n{cm}"
    );
    assert!(
        cm.contains("plugin: tools\n") && cm.contains("plugin: tools-baseline"),
        "and it must be a SECOND consumer of an unchanged seam, not a replacement of it"
    );
}

/// The `$ / bank` figures of one table, keyed by arm, parsed out of a markdown summary table.
fn bank_costs(table: &str) -> Vec<(String, String, String)> {
    table
        .lines()
        .filter(|l| l.contains("live haiku |"))
        .map(|l| {
            let c: Vec<&str> = l.split('|').map(str::trim).collect();
            (c[1].to_string(), c[2].to_string(), c[6].to_string())
        })
        .collect()
}

/// V10 asks for the decisions "with the evidence". A phase row that quotes a bench run the plan has
/// since marked SUPERSEDED is exactly the drift this file exists to catch, so BUILD.md's numbers
/// are checked against the plan's own DECISION table rather than read for plausibility.
#[test]
fn the_build_row_quotes_the_plans_current_decision_table() {
    let plan = doc("docs/phase-codemode-plan.md");
    let decision = section(&plan, "### Live haiku, both arms — the DECISION table");
    let superseded = section(&plan, "### Live haiku, both arms — SUPERSEDED");
    let build = doc("BUILD.md");
    let row = build
        .lines()
        .find(|l| l.starts_with("| codemode |"))
        .expect("BUILD.md must carry a `codemode` phase row");

    let current = bank_costs(&decision);
    assert_eq!(current.len(), 2, "the decision table must carry both arms");
    for (arm, pass, cost) in &current {
        assert!(
            row.contains(cost.trim_start_matches('$')),
            "BUILD.md's codemode row must quote the CURRENT `{arm}` cost {cost}: {row}"
        );
        assert!(
            row.contains(pass.as_str()),
            "BUILD.md's codemode row must quote the CURRENT `{arm}` pass rate {pass}: {row}"
        );
    }
    // And must not present the retired run's numbers as the evidence.
    for (arm, _, cost) in bank_costs(&superseded) {
        let stale = cost.trim_start_matches('$').trim_end_matches('0');
        assert!(
            !row.contains(&format!("@ {stale}")) && !row.contains(&format!("@ ${stale}")),
            "BUILD.md's codemode row still quotes the SUPERSEDED `{arm}` cost {cost}: {row}"
        );
    }
    assert!(
        row.to_lowercase().contains("superseded"),
        "and it must say where the retired table went: {row}"
    );
}
