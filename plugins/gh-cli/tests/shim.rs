//! Invariant under test: every `gh` call this crate makes is a spawn of ONE binary with ONE stable
//! argv, and an argv the shim has no fixture for FAILS LOUDLY rather than reaching the network.

use std::time::Duration;

use bough_plugin_gh_cli::{shim::fixture_name, Gh, GhError};

struct Shim {
    dir: tempfile::TempDir,
}

impl Shim {
    fn new() -> Shim {
        Shim {
            dir: tempfile::tempdir().expect("a temp dir"),
        }
    }
    fn fixture(&self, args: &[&str], ext: &str, body: &str) {
        let path = self
            .dir
            .path()
            .join(format!("{}.{ext}", fixture_name(args)));
        std::fs::write(path, body).expect("a fixture");
    }
    fn log(&self) -> std::path::PathBuf {
        self.dir.path().join("argv.log")
    }
    fn gh(&self) -> Gh {
        Gh::new(
            concat!(env!("CARGO_MANIFEST_DIR"), "/../../scripts/fixtures/gh/gh"),
            Duration::from_secs(10),
        )
        .with_env(vec![
            (
                "GH_SHIM_DIR".to_string(),
                self.dir.path().display().to_string(),
            ),
            ("GH_SHIM_LOG".to_string(), self.log().display().to_string()),
        ])
    }
    fn argv(&self) -> Vec<String> {
        std::fs::read_to_string(self.log())
            .unwrap_or_default()
            .lines()
            .map(|l| l.to_string())
            .collect()
    }
}

#[tokio::test]
async fn pr_list_spawns_one_stable_argv_and_parses_in_rust() {
    let shim = Shim::new();
    let args = [
        "pr",
        "list",
        "--repo",
        "o/r",
        "--json",
        "number,title",
        "--limit",
        "50",
    ];
    shim.fixture(&args, "json", r#"[{"number":12,"title":"a PR"}]"#);

    let rows = shim
        .gh()
        .pr_list("o/r", &["number", "title"], 50)
        .await
        .expect("the shim answers");

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["number"], 12);
    assert_eq!(shim.argv(), vec![args.join(" ")]);
    assert!(
        !shim.argv()[0].contains("--jq"),
        "parsing happens in Rust, never in `gh`"
    );
}

#[tokio::test]
async fn an_unplanned_call_fails_loudly_instead_of_reaching_the_network() {
    let shim = Shim::new();
    let err = shim
        .gh()
        .api("repos/o/r/pulls", &[])
        .await
        .expect_err("no fixture");
    match err {
        GhError::Exit { code, stderr, .. } => {
            assert_eq!(code, 42);
            assert!(stderr.contains("no fixture for"), "{stderr}");
        }
        other => panic!("expected a loud exit, got {other}"),
    }
}

#[tokio::test]
async fn an_unparseable_payload_is_bad_json_and_never_a_panic() {
    let shim = Shim::new();
    shim.fixture(&["api", "user"], "json", "{not json");
    let err = shim.gh().whoami().await.expect_err("unparseable");
    assert!(matches!(err, GhError::BadJson { .. }), "{err}");
}

#[tokio::test]
async fn whoami_reads_the_login() {
    let shim = Shim::new();
    shim.fixture(
        &["api", "user"],
        "json",
        r#"{"login":"andrey","type":"User"}"#,
    );
    assert_eq!(shim.gh().whoami().await.expect("a login"), "andrey");
}

#[tokio::test]
async fn a_hanging_gh_times_out_rather_than_hanging_the_sweep() {
    let shim = Shim::new();
    shim.fixture(&["api", "user"], "sleep", "5");
    shim.fixture(&["api", "user"], "json", "{}");
    let gh = Gh::new(
        concat!(env!("CARGO_MANIFEST_DIR"), "/../../scripts/fixtures/gh/gh"),
        Duration::from_millis(200),
    )
    .with_env(vec![(
        "GH_SHIM_DIR".to_string(),
        shim.dir.path().display().to_string(),
    )]);
    let err = gh.whoami().await.expect_err("a timeout");
    assert!(matches!(err, GhError::Timeout { .. }), "{err}");
}

#[tokio::test]
async fn a_missing_binary_is_a_spawn_error() {
    let gh = Gh::new("definitely-not-a-binary-xyz", Duration::from_secs(1));
    let err = gh.whoami().await.expect_err("no such binary");
    assert!(matches!(err, GhError::Spawn { .. }), "{err}");
}
