//! `act('open_pr', target, payload)` from inside a program reaches `actions-github` — the whole
//! path, on the real binary, against the recording `gh` shim.
//!
//! MERGE: phase codemode recorded the `act` bank task as "red by construction — there is no
//! `actions` Provider until Phase 6" (`docs/phase-codemode-plan.md` §8, `docs/codemode-merge-notes
//! .md`). Track B landed those Providers and `bundles/bough-base.yml` mounts `actions.github`, so
//! the claim is testable for the first time and this is the test.
//!
//! Nothing here is mocked at the seam under test. `act` is the bundle's DISPATCH alias over the
//! four action kinds, and the path from the sandbox call to the `gh pr create` argv is the real
//! one: the injected global → `bind.rs` → the tools pipeline → `tool-actions` →
//! `ActionsHandle::execute` → `actions-github` → a spawned binary. Only `gh` itself is a shim, and
//! an UNPLANNED `gh` call is a red test rather than a network request.

use crate::support;

use support::codemode::{answer_round, program_round, Sandbox};

const PR_URL: &str = "https://github.com/andreyl/widget/pull/41";
const REPO: &str = "andreyl/widget";

/// A throwaway directory holding the shim's fixtures and its argv log.
struct Shim {
    dir: tempfile::TempDir,
}

impl Shim {
    fn new() -> Shim {
        let shim = Shim {
            dir: tempfile::tempdir().expect("a temp dir"),
        };
        // The ONLY planned call. `open_pr` puts a marker derived from a runtime idem key into the
        // `--body`, so the argv tail is not knowable here: the fixture is a PREFIX one.
        std::fs::write(
            shim.dir.path().join(format!(
                "{}.prefix.json",
                bough_plugin_gh_cli::shim::fixture_name(&["pr", "create", "--repo", REPO])
            )),
            format!("{PR_URL}\n"),
        )
        .expect("a fixture");
        shim
    }

    fn bin() -> String {
        concat!(env!("CARGO_MANIFEST_DIR"), "/../../scripts/fixtures/gh/gh").to_string()
    }

    fn log(&self) -> std::path::PathBuf {
        self.dir.path().join("argv.log")
    }

    fn argv(&self) -> Vec<String> {
        std::fs::read_to_string(self.log())
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// The row config that points the shipped `actions.github` at this shim. A patch REPLACES a
    /// row's config map, so every field the bundle sets is restated.
    fn patch(&self) -> serde_json::Value {
        serde_json::json!({ "actions.github": { "config": {
            "gh_bin": Shim::bin(),
            "known_bots": ["dependabot[bot]"],
            "timeout_ms": 30000
        }}})
    }

    fn env(&self) -> Vec<(&'static str, String)> {
        vec![
            ("GH_SHIM_DIR", self.dir.path().display().to_string()),
            ("GH_SHIM_LOG", self.log().display().to_string()),
        ]
    }
}

#[test]
fn act_open_pr_from_a_program_reaches_actions_github() {
    let shim = Shim::new();
    let sb = Sandbox::new("act-open-pr");
    let program = format!(
        "const r = await act('open_pr', {REPO:?}, {{head: 'bough/rename-foo', base: 'main', \
         title: 'rename `foo` to `bar`', body: 'As asked.'}}); \
         console.log('PR=' + JSON.stringify(r));"
    );
    let (code, out) = sb.exec_with(
        "open the PR",
        serde_json::json!([program_round("c0", &program), answer_round("opened it.")]),
        shim.patch(),
        &shim.env(),
    );
    assert_eq!(code, 0, "{out}");

    // 1. The program's call is ledgered under the kind it dispatched to, not under `act`.
    let steps = sb.steps();
    let call = steps
        .iter()
        .find(|(k, b)| k == "program/call" && b["name"] == "open_pr")
        .unwrap_or_else(|| {
            panic!(
                "`act('open_pr', …)` must dispatch to `open_pr`: {:?}",
                sb.kinds()
            )
        });
    assert_eq!(call.1["args"]["target"], REPO, "{}", call.1);
    assert_eq!(
        call.1["args"]["payload"]["head"], "bough/rename-foo",
        "{}",
        call.1
    );

    // 2. The journal: intent BEFORE done, both in the ledger (§2.7 item 4).
    let kinds = sb.kinds();
    let intent = kinds.iter().position(|k| k == "action/intent");
    let done = kinds.iter().position(|k| k == "action/done");
    assert!(
        intent.is_some() && done.is_some() && intent < done,
        "the journal must show intent before done: {kinds:?}\n{out}"
    );

    // 3. `gh` really ran, exactly once, with the argv the action asked for.
    let argv = shim.argv();
    assert_eq!(argv.len(), 1, "exactly one `gh` call: {argv:?}");
    assert!(
        argv[0].starts_with(&format!("pr create --repo {REPO}")),
        "the argv is the one `open_pr` builds: {argv:?}"
    );

    // 4. …and the locator came back to the program.
    let console = steps
        .iter()
        .find(|(k, b)| k == "tool/result" && b["name"] == "run")
        .map(|(_, b)| b["content"].as_str().unwrap_or_default().to_string())
        .expect("the `run` call got a result");
    assert!(
        console.contains(PR_URL),
        "the program was handed the PR url: {console}"
    );
}
