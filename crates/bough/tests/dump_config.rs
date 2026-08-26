//! V6: `bough --profile tui --dump-config` equals what boots — same tree, same per-row layer
//! annotations, same fingerprint — and the fingerprint moves when a row's config changes.
//!
//! The identity is checked the only way it can be honestly checked: the dump is taken from the
//! REAL binary, and the tree it is compared against is the one a kernel mounted from
//! `compose_for`, the same function the binary called.

use std::path::{Path, PathBuf};
use std::process::Command;

use bough::cli::{Cli, DumpFormat};
use bough::compose::compose_for;
use bough_kernel::{render, Catalog, Kernel, KernelOptions};

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_bough"))
}

/// Sets the process-wide `$BOUGH_HOME`.
///
/// Safe without a lock only because exactly ONE test in this binary calls it
/// (`dump_config_equals_the_booted_tree`); every other test here reaches the launcher through a
/// child process, which is handed `BOUGH_HOME` explicitly and never reads the parent's. A second
/// in-process caller must take a lock first.
fn cli_for(home: &Path, dump: bool, check: bool) -> Cli {
    // The in-process half of the test reads `$BOUGH_HOME` through `bough_util`, which reads the
    // environment on every call; the child process is given the same value explicitly.
    std::env::set_var("BOUGH_HOME", home);
    Cli {
        profile: "tui".into(),
        patches: Vec::new(),
        dump_config: dump,
        dump_format: DumpFormat::Yaml,
        check,
        no_watch: true,
        root: None,
        command: None,
    }
}

fn dump_from_binary(home: &Path) -> String {
    let out = Command::new(bin())
        .args(["--profile", "tui", "--dump-config"])
        .env("BOUGH_HOME", home)
        .output()
        .expect("run bough --dump-config");
    assert!(
        out.status.success(),
        "--dump-config must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("dump is UTF-8")
}

fn fingerprint_line(dump: &str) -> String {
    dump.lines()
        .find(|l| l.to_ascii_lowercase().contains("fingerprint"))
        .unwrap_or_else(|| panic!("the dump must carry the composition fingerprint:\n{dump}"))
        .to_string()
}

#[tokio::test]
async fn dump_config_equals_the_booted_tree() {
    let home = tempfile::tempdir().unwrap();
    let dump = dump_from_binary(home.path());

    let cli = cli_for(home.path(), false, true);
    let catalog = Catalog::from_inventory().unwrap();
    let composition = compose_for(&cli, &catalog).unwrap();
    let kernel = Kernel::new(
        catalog,
        KernelOptions {
            profile: "tui".into(),
            invariants: false,
        },
    );
    kernel.load(composition).await.unwrap();
    kernel.quiesce().await;

    let live = render(
        &kernel
            .composition()
            .expect("the kernel mounted a composition"),
        bough_kernel::DumpFormat::Yaml,
    );

    // Tie the dump to the RUNNING kernel, not merely to the composition it was handed: every
    // enabled row named in the dump is a row that actually reached ACTIVE, and the fingerprint the
    // dump printed is the fingerprint the live tree reports.
    let snapshot = kernel.snapshot();
    assert!(
        dump.contains(snapshot.fingerprint.as_str()),
        "the dumped fingerprint must be the live tree's fingerprint"
    );
    assert!(
        snapshot.unresolved().is_empty(),
        "the dumped tree must be one that fully boots: {:?}",
        snapshot.unresolved()
    );
    for row in &snapshot.rows {
        assert!(
            dump.contains(row.id.as_str()),
            "row {} booted but is missing from the dump:\n{dump}",
            row.id
        );
    }
    kernel.shutdown().await;

    assert_eq!(
        dump, live,
        "--dump-config must be render() of exactly the composition that boots"
    );
}

#[test]
fn dump_config_annotates_the_last_writing_layer() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("bough.patch.yml"),
        "entries:\n  hello.greeter:\n    config: { who: patched, log_level: info }\n",
    )
    .unwrap();

    let dump = dump_from_binary(home.path());
    assert!(dump.contains("hello.greeter"), "{dump}");
    assert!(dump.contains("patched"), "{dump}");
    assert!(
        dump.contains("user"),
        "the row's config must be annotated with the `user` layer that last wrote it:\n{dump}"
    );
    assert!(
        dump.contains("bundle:bough-base"),
        "a row no later layer touched must still name its creating layer:\n{dump}"
    );
}

#[test]
fn dump_config_exits_zero_without_mounting() {
    let home = tempfile::tempdir().unwrap();
    // A patch that makes the tree UNBOOTABLE: the provider is disabled, so `hello.greeter` can
    // never activate. `--check` fails on it; `--dump-config` still exits 0, which is only possible
    // if it never mounted anything.
    std::fs::write(
        home.path().join("bough.patch.yml"),
        "entries:\n  greeting.provider:\n    disabled: true\n",
    )
    .unwrap();

    let dumped = Command::new(bin())
        .args(["--profile", "tui", "--dump-config"])
        .env("BOUGH_HOME", home.path())
        .output()
        .unwrap();
    assert_eq!(dumped.status.code(), Some(0), "--dump-config exits 0");

    let booted = Command::new(bin())
        .args(["--profile", "tui", "--check", "--no-watch"])
        .env("BOUGH_HOME", home.path())
        .output()
        .unwrap();
    assert_eq!(
        booted.status.code(),
        Some(1),
        "the same tree fails to boot, so the dump above cannot have mounted it"
    );
}

#[test]
fn fingerprint_changes_when_a_row_config_changes() {
    let home = tempfile::tempdir().unwrap();
    let patch = home.path().join("bough.patch.yml");

    std::fs::write(
        &patch,
        "entries:\n  hello.greeter:\n    config: { who: one, log_level: info }\n",
    )
    .unwrap();
    let a = fingerprint_line(&dump_from_binary(home.path()));

    std::fs::write(
        &patch,
        "entries:\n  hello.greeter:\n    config: { who: two, log_level: info }\n",
    )
    .unwrap();
    let b = fingerprint_line(&dump_from_binary(home.path()));

    assert_ne!(a, b, "a row's config change must move the fingerprint");
}
