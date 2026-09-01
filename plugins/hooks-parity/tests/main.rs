//! Drivability §5: the hook settings both CLIs write are discovered from the CALL's cwd, parsed
//! from both formats, and applied to the tools waterfalls as decisions that only tighten.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use bough_plugin_hooks_parity::settings::{
    call_cwd, discover, filtered, parse_source, source_files,
};
use bough_plugin_hooks_parity::{run_pre, HookState, HooksParityConfig};
use bough_plugin_tools::{Decision, PreExecute, ToolCall, ToolName};

fn cfg() -> HooksParityConfig {
    serde_json::from_value(serde_json::json!({
        "timeout_ms": 5000,
        "max_output_bytes": 65536,
    }))
    .expect("defaults fill in")
}

fn call(name: &str, args: serde_json::Value) -> Arc<ToolCall> {
    Arc::new(ToolCall {
        id: bough_plugin_tools::ToolCallId::new("c1"),
        name: ToolName::new(name),
        args,
        agent: bough_plugin_ledger::AgentName::new("sol"),
        wake: bough_plugin_ledger::WakeId::new("w1"),
        step_index: 0,
    })
}

/// The Claude `settings.json` shape and the Codex `config.toml` shape parse to the same hooks.
#[test]
fn both_formats_parse_to_the_same_hook() {
    let claude = parse_source(
        Path::new("/x/.claude/settings.json"),
        r#"{"hooks": {"PreToolUse": [{"matcher": "Bash", "hooks": [
            {"type": "command", "command": "check.sh", "timeout": 30}]}]}}"#,
    )
    .expect("parses");
    let codex = parse_source(
        Path::new("/x/.codex/config.toml"),
        "[[hooks.PreToolUse]]\nmatcher = \"Bash\"\n\n[[hooks.PreToolUse.hooks]]\ntype = \"command\"\ncommand = \"check.sh\"\ntimeout = 30\n",
    )
    .expect("parses");
    assert_eq!(claude.len(), 1);
    let (c, x) = (&claude[0], &codex[0]);
    assert_eq!(
        (
            c.event.as_str(),
            c.matcher.as_deref(),
            c.command.as_str(),
            c.timeout_ms
        ),
        (
            x.event.as_str(),
            x.matcher.as_deref(),
            x.command.as_str(),
            x.timeout_ms
        ),
    );
    assert_eq!(c.timeout_ms, Some(30_000));
    // A file with no hooks key is simply empty; a broken one is an error the caller warns on.
    assert!(parse_source(Path::new("/x/.claude/settings.json"), "{}")
        .expect("no hooks key is fine")
        .is_empty());
    assert!(parse_source(Path::new("/x/.claude/settings.json"), "{nope").is_err());
}

/// User layer first, then ancestors outermost-first; a home inside the chain is listed once.
#[test]
fn sources_walk_the_ancestors_and_dedupe_the_user_layer() {
    let home = PathBuf::from("/Users/a");
    let out = source_files(Path::new("/Users/a/repos/x"), Some(&home), true, true, true);
    assert_eq!(out[0], home.join(".claude/settings.json"));
    let claude_x = out
        .iter()
        .position(|p| *p == Path::new("/Users/a/repos/x/.claude/settings.json"))
        .expect("the call's own dir is walked");
    let claude_home_dup = out
        .iter()
        .filter(|p| **p == home.join(".claude/settings.json"))
        .count();
    assert_eq!(claude_home_dup, 1, "{out:?}");
    let claude_root = out
        .iter()
        .position(|p| *p == Path::new("/.claude/settings.json"))
        .expect("the filesystem root is walked");
    assert!(claude_root < claude_x, "outermost first: {out:?}");
    assert!(out.iter().any(|p| p.ends_with(".codex/config.toml")));
    // Toggles: no codex, no .codex paths.
    let no_codex = source_files(
        Path::new("/Users/a/repos/x"),
        Some(&home),
        true,
        false,
        true,
    );
    assert!(!no_codex
        .iter()
        .any(|p| p.to_string_lossy().contains(".codex")));
}

/// The matcher regex is tried against the raw bough name AND the parity alias; the `only` /
/// `except` toggles pick hooks by command substring; the events list gates by event.
#[test]
fn filtering_matches_aliases_and_honours_the_toggles() {
    let defs = parse_source(
        Path::new("/x/.claude/settings.json"),
        r#"{"hooks": {
            "PreToolUse": [
                {"matcher": "Bash", "hooks": [{"type": "command", "command": "guard.sh"}]},
                {"matcher": "Edit|Write", "hooks": [{"type": "command", "command": "fmt.sh"}]},
                {"hooks": [{"type": "command", "command": "always.sh"}]}
            ],
            "PostToolUse": [
                {"matcher": "Bash", "hooks": [{"type": "command", "command": "after.sh"}]}
            ]}}"#,
    )
    .expect("parses");
    fn cmds<'a>(v: Vec<&'a bough_plugin_hooks_parity::settings::HookDef>) -> Vec<&'a str> {
        v.into_iter().map(|d| d.command.as_str()).collect()
    }
    // `bash` only matches "Bash" through its alias; the matcher-less hook always fires.
    assert_eq!(
        cmds(filtered(
            &defs,
            "PreToolUse",
            &["bash", "Bash"],
            &[],
            &[],
            &[]
        )),
        vec!["guard.sh", "always.sh"]
    );
    assert_eq!(
        cmds(filtered(
            &defs,
            "PreToolUse",
            &["write_file", "Write"],
            &[],
            &[],
            &[]
        )),
        vec!["fmt.sh", "always.sh"]
    );
    assert_eq!(
        cmds(filtered(
            &defs,
            "PostToolUse",
            &["bash", "Bash"],
            &[],
            &[],
            &[]
        )),
        vec!["after.sh"]
    );
    // only / except by command substring; events gate.
    assert_eq!(
        cmds(filtered(
            &defs,
            "PreToolUse",
            &["bash", "Bash"],
            &[],
            &["guard".into()],
            &[]
        )),
        vec!["guard.sh"]
    );
    assert_eq!(
        cmds(filtered(
            &defs,
            "PreToolUse",
            &["bash", "Bash"],
            &[],
            &[],
            &["always".into()]
        )),
        vec!["guard.sh"]
    );
    assert!(cmds(filtered(
        &defs,
        "PreToolUse",
        &["bash", "Bash"],
        &["PostToolUse".into()],
        &[],
        &[]
    ))
    .is_empty());
}

/// The call's own directory wins: `args.cwd`, else a path argument's directory, else the
/// workspace.
#[test]
fn the_calls_cwd_comes_from_its_own_arguments() {
    let ws = Path::new("/work");
    assert_eq!(
        call_cwd(&serde_json::json!({"cwd": "/repos/x"}), Some(ws)),
        PathBuf::from("/repos/x")
    );
    assert_eq!(
        call_cwd(&serde_json::json!({"cwd": "sub"}), Some(ws)),
        PathBuf::from("/work/sub")
    );
    assert_eq!(
        call_cwd(
            &serde_json::json!({"path": "/repos/x/src/main.rs"}),
            Some(ws)
        ),
        PathBuf::from("/repos/x/src")
    );
    assert_eq!(
        call_cwd(&serde_json::json!({}), Some(ws)),
        PathBuf::from("/work")
    );
}

/// End to end on a real tree: a `.claude/settings.json` deny hook in the call's cwd denies the
/// call — discovered from THAT directory, not from anywhere the process started — and the `only`
/// toggle turns it off.
#[tokio::test]
async fn a_project_hook_denies_a_call_run_in_its_directory() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(repo.join(".claude")).unwrap();
    std::fs::write(
        repo.join(".claude/settings.json"),
        r#"{"hooks": {"PreToolUse": [{"matcher": "^Bash$", "hooks": [
            {"type": "command",
             "command": "echo '{\"hookSpecificOutput\":{\"hookEventName\":\"PreToolUse\",\"permissionDecision\":\"deny\",\"permissionDecisionReason\":\"repo policy\"}}'"}
        ]}]}}"#,
    )
    .unwrap();
    let st = |cfg: HooksParityConfig| HookState {
        cfg: Arc::new(HooksParityConfig {
            user_layer: false,
            ..cfg
        }),
        ctx: None,
        home: None,
    };
    let repo_s = repo.to_string_lossy().to_string();
    let mut pre = PreExecute::new(
        call(
            "bash",
            serde_json::json!({"command": "rm -rf /", "cwd": repo_s}),
        ),
        bough_plugin_ledger::AgentName::new("sol"),
    );
    run_pre(&st(cfg()), &mut pre).await;
    match pre.decision() {
        Decision::Deny { reason } => assert!(reason.contains("repo policy"), "{reason}"),
        other => panic!("expected a deny, got {other:?}"),
    }
    // A call elsewhere never sees this repo's hook.
    let other_dir = dir.path().to_string_lossy().to_string();
    let mut pre = PreExecute::new(
        call(
            "bash",
            serde_json::json!({"command": "ls", "cwd": other_dir}),
        ),
        bough_plugin_ledger::AgentName::new("sol"),
    );
    run_pre(&st(cfg()), &mut pre).await;
    assert_eq!(*pre.decision(), Decision::Allow);
    // The `only` toggle (no match on the command) turns the hook off.
    let mut pre = PreExecute::new(
        call(
            "bash",
            serde_json::json!({"command": "ls", "cwd": repo.to_string_lossy()}),
        ),
        bough_plugin_ledger::AgentName::new("sol"),
    );
    run_pre(
        &st(HooksParityConfig {
            only: vec!["nothing-matches-this".into()],
            ..cfg()
        }),
        &mut pre,
    )
    .await;
    assert_eq!(*pre.decision(), Decision::Allow);
}

/// Exit code 2 with a stderr reason denies, the way both CLIs document it.
#[tokio::test]
async fn exit_two_denies_with_the_stderr_reason() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("r2");
    std::fs::create_dir_all(repo.join(".claude")).unwrap();
    std::fs::write(
        repo.join(".claude/settings.json"),
        r#"{"hooks": {"PreToolUse": [{"hooks": [
            {"type": "command", "command": "echo 'not in this repo' >&2; exit 2"}]}]}}"#,
    )
    .unwrap();
    let mut pre = PreExecute::new(
        call("bash", serde_json::json!({"cwd": repo.to_string_lossy()})),
        bough_plugin_ledger::AgentName::new("sol"),
    );
    run_pre(
        &HookState {
            cfg: Arc::new(HooksParityConfig {
                user_layer: false,
                ..cfg()
            }),
            ctx: None,
            home: None,
        },
        &mut pre,
    )
    .await;
    match pre.decision() {
        Decision::Deny { reason } => assert!(reason.contains("not in this repo"), "{reason}"),
        other => panic!("expected a deny, got {other:?}"),
    }
}

/// Discovery reads the walked tree: a hook defined in an ANCESTOR of the call's cwd fires too.
#[test]
fn discovery_reads_the_ancestors_of_the_calls_cwd() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join(".codex")).unwrap();
    std::fs::write(
        root.join(".codex/hooks.json"),
        r#"{"hooks": {"PostToolUse": [{"hooks": [{"type": "command", "command": "log.sh"}]}]}}"#,
    )
    .unwrap();
    let deep = root.join("a/b/c");
    std::fs::create_dir_all(&deep).unwrap();
    let defs = discover(&deep, None, true, true, false);
    assert_eq!(defs.len(), 1, "{defs:?}");
    assert_eq!(defs[0].command, "log.sh");
    assert_eq!(defs[0].source, root.join(".codex/hooks.json"));
}
